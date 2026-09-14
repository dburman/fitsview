//! Adding frames together.
//!
//! The signal in a stack grows with the number of frames and the noise grows
//! with its root, so a hundred frames of one field are ten times cleaner than
//! any of them. That only holds if the frames are laid on top of each other
//! correctly: a mount drifts over an hour, and a German equatorial one swings
//! to the other side of its pier at the meridian and delivers the rest of the
//! night rotated by half a turn. Added as they lie, the frames blur.
//!
//! This module is the arithmetic — where each frame belongs, and how to add it
//! — with no opinion about files or folders.

use rayon::prelude::*;

use crate::header::FitsHeader;
use crate::image::FitsImage;
use crate::stars::{Star, StarField};

/// Which side of the mount the telescope was on.
///
/// A German equatorial mount cannot follow a target through the meridian
/// without swinging the tube to the other side of the pier, and the field
/// arrives rotated by half a turn when it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PierSide {
    /// Telescope east of the pier, pointing west.
    East,
    /// Telescope west of the pier, pointing east.
    West,
    /// The header did not say. Assuming a side would be worse than admitting
    /// it: guess wrong and half the night is stacked upside down.
    Unknown,
}

impl PierSide {
    /// Reads the side from a header.
    ///
    /// `PIERSIDE` is what the cameras tested here write, as `East` or `West`.
    /// `SWCREATE`-specific spellings are accepted alongside it because the
    /// keyword is conventional rather than standard.
    #[must_use]
    pub fn from_header(header: &FitsHeader) -> Self {
        let raw = ["PIERSIDE", "PIER_SIDE", "TELESCOP_PIER"]
            .iter()
            .find_map(|k| header.get(k));
        let Some(raw) = raw else {
            return Self::Unknown;
        };

        let cleaned = raw.trim().trim_matches('\'').trim().to_ascii_lowercase();
        match cleaned.as_str() {
            "east" | "e" | "pierEast" | "piereast" => Self::East,
            "west" | "w" | "pierwest" => Self::West,
            _ => Self::Unknown,
        }
    }

    /// Whether a frame taken on this side has to be turned to match one taken
    /// on `reference`.
    ///
    /// Two frames from sides that are both known and different are half a turn
    /// apart. Anything else — the same side, or either side unknown — is left
    /// alone, because turning a frame that did not need it is the same mistake
    /// as failing to turn one that did.
    #[must_use]
    pub fn needs_turning(self, reference: Self) -> bool {
        matches!(
            (reference, self),
            (Self::East, Self::West) | (Self::West, Self::East)
        )
    }
}

/// Where a frame sits relative to the reference frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Alignment {
    /// How far right this frame's sky lies compared with the reference.
    ///
    /// So a star that appears twelve pixels further right than in the reference
    /// gives twelve, and the stack reads this frame at `x + dx`.
    ///
    /// Kept as measured rather than rounded, because a mount does not drift in
    /// whole pixels and the fraction is worth something to whatever adds the
    /// frame up.
    pub dx: f64,
    /// How far down this frame's sky lies compared with the reference.
    pub dy: f64,
    /// Whether it has to be turned half a circle first.
    pub turned: bool,
    /// A further turn, in radians, about the centre of the frame.
    ///
    /// Zero for almost every frame: a tracking mount holds its angle. What it
    /// is for is the meridian flip, where the mount swings to the other side
    /// of the pier and does not come back to exactly half a turn. Half a
    /// degree of residual leaves the middle of the frame lining up and the
    /// edges forty pixels out.
    pub rotation: f64,
    /// How much this frame counts for, against the others.
    ///
    /// One for every frame gives the plain average. Weighting by the inverse
    /// square of a frame's noise is the arrangement that gives the cleanest
    /// result, and on a night whose sky brightened fivefold it is worth some
    /// forty per cent: the late frames carry a quarter of the information the
    /// early ones do, and averaging them equally throws most of that away.
    pub weight: f64,
    /// What to add to every sample to bring this frame's sky to the
    /// reference's.
    ///
    /// The sky brightens and dims through a night — the moon rises, dawn
    /// comes, cloud passes — and a frame taken under a brighter sky sits
    /// bodily above the others. Added as it lies it lifts the result, and,
    /// worse, rejection then treats it as the outlier at every pixel and
    /// throws the whole frame away.
    pub offset: f64,
    /// How many stars agreed on this, which is how much to trust it.
    pub votes: usize,
}

impl Alignment {
    /// The frame is already where it belongs: the reference's own alignment.
    #[must_use]
    pub const fn still() -> Self {
        Self {
            dx: 0.0,
            dy: 0.0,
            turned: false,
            rotation: 0.0,
            weight: 1.0,
            offset: 0.0,
            votes: usize::MAX,
        }
    }

    /// The offset rounded to whole pixels.
    #[must_use]
    pub fn whole(self) -> (i64, i64) {
        #[allow(clippy::cast_possible_truncation)]
        (self.dx.round() as i64, self.dy.round() as i64)
    }

    /// How far the rounded offset is from the measured one, in pixels.
    ///
    /// Zero when the frame happened to land on the grid, and up to about
    /// seven tenths of a pixel when it did not.
    #[must_use]
    pub fn rounding_error(self) -> f64 {
        let (dx, dy) = self.whole();
        #[allow(clippy::cast_precision_loss)]
        ((self.dx - dx as f64).powi(2) + (self.dy - dy as f64).powi(2)).sqrt()
    }
}

/// Fewest stars that must agree before an alignment is believed.
///
/// Three pairs landing on the same offset by chance is unlikely; two is not.
const MINIMUM_VOTES: usize = 4;

/// The share of the stars offered that must agree, as one in this many.
///
/// An absolute floor is not enough on a rich field. Two frames from opposite
/// sides of a meridian flip agree on seven stars out of a hundred, because a
/// flip is a half turn about the optical axis and the real angle is never
/// exactly half a turn: half a degree out displaces a star five thousand
/// pixels from the centre by forty. The middle of the frame lines up and the
/// rest does not, and a translation fitted to those seven would smear
/// everything else.
///
/// Frames that genuinely match agree on more than half of what they are
/// offered, so this rules out that case with room to spare.
const MINIMUM_SHARE: usize = 10;

/// Brightest stars used from each frame.
///
/// The vote is every pairing of one list with the other, so this squares. Two
/// hundred is forty thousand pairings, a few milliseconds against the fifty a
/// frame takes to search, and the brightest of a frame are the ones most
/// likely to appear in both.
///
/// A hundred was not enough for the last frame of one night, taken as the sky
/// brightened ninefold: it held fewer than half the stars of the reference and
/// broader ones, so the brightest hundred of each barely overlapped. Add the
/// meridian flip, where only the middle of the frame matches until the turn is
/// known, and there was nothing left to start from. At two hundred every frame
/// of that night lines up.
const STARS_CONSIDERED: usize = 200;

/// How close two offsets must be to count as the same one, in pixels.
///
/// Wide enough for centroids measured on different frames in different seeing,
/// narrow enough that unrelated pairings do not land in the same bin.
const OFFSET_TOLERANCE: f64 = 2.0;

/// Finds where `frame` sits relative to `reference`.
///
/// Every pairing of a bright star in one frame with a bright star in the other
/// implies an offset. The right pairings all imply the same one and pile up;
/// the wrong ones scatter. The pile is the answer, and no individual pairing
/// has to be correct for it to work.
///
/// `turned` says the frame was taken on the other side of the pier, in which
/// case its stars are turned about the centre of the sensor before the vote.
///
/// Returns `None` when too few pairs agree, which is the honest answer for
/// frames of different fields, for a frame with no stars, and for a night where
/// cloud rolled in.
#[must_use]
pub fn align(
    reference: &StarField,
    frame: &StarField,
    size: (usize, usize),
    turned: bool,
) -> Option<Alignment> {
    let anchors = brightest(reference);
    let moving: Vec<Star> = brightest(frame)
        .into_iter()
        .map(|s| if turned { turn(s, size) } else { s })
        .collect();
    if anchors.len() < MINIMUM_VOTES || moving.len() < MINIMUM_VOTES {
        return None;
    }

    // Every pairing votes for the offset it implies, rounded into whole pixels
    // so that agreeing votes land together.
    let mut votes: std::collections::HashMap<(i64, i64), usize> = std::collections::HashMap::new();
    for anchor in &anchors {
        for star in &moving {
            #[allow(clippy::cast_possible_truncation)]
            let key = (
                (star.x - anchor.x).round() as i64,
                (star.y - anchor.y).round() as i64,
            );
            *votes.entry(key).or_insert(0) += 1;
        }
    }

    // The most popular whole-pixel offset, and then the average of every pair
    // that agrees with it, which recovers the fraction the rounding threw away.
    //
    // Ties are broken by the offset itself rather than by whichever the map
    // happened to yield first: two runs over the same frames must not line
    // them up differently, and a hash map's order is not the same twice.
    let (&(bx, by), _) = votes
        .iter()
        .max_by(|(left_key, left), (right_key, right)| {
            left.cmp(right).then_with(|| right_key.cmp(left_key))
        })?;

    let (mut sum_x, mut sum_y, mut agreed) = (0.0f64, 0.0f64, 0usize);
    for anchor in &anchors {
        for star in &moving {
            let (dx, dy) = (star.x - anchor.x, star.y - anchor.y);
            #[allow(clippy::cast_precision_loss)]
            if (dx - bx as f64).abs() <= OFFSET_TOLERANCE
                && (dy - by as f64).abs() <= OFFSET_TOLERANCE
            {
                sum_x += dx;
                sum_y += dy;
                agreed += 1;
            }
        }
    }
    if agreed < MINIMUM_VOTES {
        // Too few even to start from. Refining needs something to refine.
        return None;
    }

    #[allow(clippy::cast_precision_loss)]
    let shift = (sum_x / agreed as f64, sum_y / agreed as f64);

    // The translation is enough for a frame the mount merely drifted under.
    // Where it is not — after a meridian flip — refining it into a turn as
    // well picks up the stars that a shift alone left behind.
    let refined = refine(&anchors, &moving, size, shift);
    let (dx, dy, rotation, agreed) = refined.unwrap_or((shift.0, shift.1, 0.0, agreed));

    let offered = anchors.len().min(moving.len());
    if agreed < MINIMUM_VOTES.max(offered / MINIMUM_SHARE) {
        return None;
    }

    Some(Alignment {
        dx,
        dy,
        turned,
        rotation,
        // The stars say where the frame is, not how bright its sky was nor how
        // much it should count for. The caller fills those in.
        weight: 1.0,
        offset: 0.0,
        votes: agreed,
    })
}

/// The second pass of a stack, which leaves the outliers out.
///
/// A plain average keeps everything: a satellite crossing one frame in fifty
/// leaves its streak across the result, faint but there, and so does every
/// cosmic ray. Rejection needs to know what ordinary looks like before it can
/// say what is not, and that cannot be known until every frame has been seen —
/// so the frames are read a second time and each sample is judged against what
/// the first pass recorded.
///
/// Each sample is judged against **the other frames**, with itself left out,
/// and in units of **its own frame's noise**. The first version of this had
/// neither, and on a real night it rejected eleven per cent of everything:
///
/// - Judged against a spread it contributed to, a satellite widens the spread
///   enough to excuse itself. Setting each pixel's brightest sample aside cured
///   that, and left every spread too narrow: five times what chance alone
///   rejects, on frames of pure noise.
/// - Judged against one spread for every frame, weighted towards the quiet
///   ones, the noisy frames of a night whose sky brightens fail wholesale. The
///   noisiest frame of that night lost 43 per cent of its samples, nearly all
///   of them ordinary, and the stack came out noisier than with no rejection.
///
/// The brightest of the other frames is set aside as well, so that two
/// outliers in one pixel — a satellite and a cosmic ray — cannot excuse each
/// other.
///
/// The alignments are already known by then, which keeps the second pass
/// cheaper than the first: no stars have to be found again. Each frame must be
/// added with the alignment it had in the first pass, because its sample is
/// taken back out of the figures it went into.
#[derive(Debug, Clone)]
pub struct Rejecting {
    stack: Stack,
    /// What the first pass recorded about every sample position.
    evidence: Evidence,
    /// How far out a sample may sit, in units of its expected scatter, for
    /// each number of degrees of freedom that scatter was measured with —
    /// squared, since the test is made on squares.
    critical: Vec<f64>,
    rejected: usize,
}

impl Rejecting {
    /// Adds a frame, keeping only the samples that agree with the others.
    ///
    /// Returns false, and adds nothing, if the frame is not the shape of the
    /// stack.
    pub fn add(&mut self, image: &FitsImage, alignment: Alignment) -> bool {
        if !self.stack.fits(image) {
            return false;
        }
        let (width, height) = (self.stack.width, self.stack.height);
        let pixels = width * height;
        let noise = channel_noise(image);
        let (evidence, critical) = (&self.evidence, &self.critical);
        let mut rejected = 0usize;

        for (channel, sigma) in noise.iter().enumerate() {
            let plane = channel * pixels;
            let source = &image.data[plane..plane + pixels];
            let inverse = inverse_variance(*sigma);
            let rows: Vec<(&mut [f64], &mut [f32])> = self.stack.total[plane..plane + pixels]
                .chunks_mut(width)
                .zip(self.stack.counted[plane..plane + pixels].chunks_mut(width))
                .collect();

            rejected += rows
                .into_par_iter()
                .enumerate()
                .map(|(y, (total, counted))| {
                    let first = plane + y * width;
                    let mut dropped = 0usize;
                    each_placed(
                        y,
                        width,
                        height,
                        alignment,
                        source,
                        |x, value, quietened| {
                            if evidence.is_outlier(first + x, value, inverse / quietened, critical)
                            {
                                dropped += 1;
                                return;
                            }
                            total[x] += value * alignment.weight;
                            #[allow(clippy::cast_possible_truncation)]
                            {
                                counted[x] += alignment.weight as f32;
                            }
                        },
                    );
                    dropped
                })
                .sum::<usize>();
        }

        self.rejected += rejected;
        self.stack.frames += 1;
        true
    }

    /// How many samples were left out.
    #[must_use]
    pub fn rejected(&self) -> usize {
        self.rejected
    }

    /// How many frames went in.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.stack.frames
    }

    /// The average of what was kept.
    #[must_use]
    pub fn finish(&self, header: FitsHeader) -> FitsImage {
        self.stack.finish(header)
    }
}

/// Which output columns of a row are covered, and where they come from.
///
/// Returns the range of the output row, the slice of the frame that feeds it,
/// and whether that slice is to be read backwards, which is what turning a
/// frame half a circle amounts to once the row has been chosen.
fn row_span(
    y: usize,
    width: usize,
    height: usize,
    alignment: Alignment,
    source: &[f32],
) -> Option<(std::ops::Range<usize>, &[f32], bool)> {
    let (dx, dy) = alignment.whole();
    let last = i64::try_from(width - 1).ok()?;
    let out_y = i64::try_from(y).ok()?;
    let source_y = if alignment.turned {
        i64::try_from(height - 1).ok()? - (out_y + dy)
    } else {
        out_y + dy
    };
    if source_y < 0 || source_y >= i64::try_from(height).ok()? {
        return None;
    }
    #[allow(clippy::cast_sign_loss)]
    let source_row = &source[source_y as usize * width..][..width];

    // The columns whose source column falls on the frame. Turned or not the
    // condition is the same one — that `x + dx` is within the row — because
    // turning reverses the walk without changing which columns it covers.
    let first = (-dx).max(0);
    let upto = (last - dx + 1).min(last + 1).max(0);
    #[allow(clippy::cast_sign_loss)]
    let (first, upto) = (first as usize, (upto as usize).min(width));
    if first >= upto {
        return None;
    }

    let span = upto - first;
    #[allow(clippy::cast_sign_loss)]
    let from = if alignment.turned {
        let source_last = (last - (i64::try_from(first).ok()? + dx)) as usize;
        &source_row[source_last + 1 - span..=source_last]
    } else {
        let source_first = (i64::try_from(first).ok()? + dx) as usize;
        &source_row[source_first..source_first + span]
    };
    Some((first..upto, from, alignment.turned))
}

/// Hands each of a frame's samples that lands in row `y` of the stack to
/// `visit`: its column, its levelled value, and its noise variance as a
/// fraction of the frame's, which is one unless it had to be resampled.
///
/// The one place that decides where a sample goes, so that both passes of a
/// rejecting stack place every sample identically. The second pass takes each
/// sample back out of the figures the first recorded, and a sample placed
/// differently would be taken out of a pixel it never went into. Until this
/// was shared, the first pass resampled a frame with any turn at all — which
/// after a fitted alignment is nearly every frame — and the second placed it
/// to the nearest whole pixel.
///
/// Written as a visitor rather than an iterator so that the common case stays
/// a straight walk along a slice, with the choice of path made once a row.
fn each_placed(
    y: usize,
    width: usize,
    height: usize,
    alignment: Alignment,
    source: &[f32],
    mut visit: impl FnMut(usize, f64, f64),
) {
    let offset = alignment.offset;

    // A turned frame has to be resampled, since its pixels no longer fall on
    // the stack's grid. The softening that costs is the better bargain than
    // leaving the frame out.
    if alignment.rotation.abs() > f64::EPSILON {
        #[allow(clippy::cast_precision_loss)]
        let centre = ((width - 1) as f64 / 2.0, (height - 1) as f64 / 2.0);
        let (c, s) = (alignment.rotation.cos(), alignment.rotation.sin());
        #[allow(clippy::cast_precision_loss)]
        let ay = y as f64 - centre.1;
        for x in 0..width {
            #[allow(clippy::cast_precision_loss)]
            let ax = x as f64 - centre.0;
            let (mut sx, mut sy) = (
                centre.0 + ax * c - ay * s + alignment.dx,
                centre.1 + ax * s + ay * c + alignment.dy,
            );
            if alignment.turned {
                #[allow(clippy::cast_precision_loss)]
                {
                    sx = (width - 1) as f64 - sx;
                    sy = (height - 1) as f64 - sy;
                }
            }
            if let Some((value, quietened)) = sample(source, width, height, sx, sy) {
                visit(x, value + offset, quietened);
            }
        }
        return;
    }

    let Some((out, from, reversed)) = row_span(y, width, height, alignment, source) else {
        return;
    };
    if reversed {
        for (x, value) in out.zip(from.iter().rev()) {
            if value.is_finite() {
                visit(x, f64::from(*value) + offset, 1.0);
            }
        }
    } else {
        for (x, value) in out.zip(from) {
            if value.is_finite() {
                visit(x, f64::from(*value) + offset, 1.0);
            }
        }
    }
}

/// What a sample counts for when it is judged: the inverse square of its
/// frame's noise.
///
/// A frame with no measurable noise — a synthetic one, or a blank — is judged
/// as if its noise were one count, there being nothing better to go on.
fn inverse_variance(noise: f64) -> f64 {
    if noise.is_finite() && noise > 0.0 {
        1.0 / (noise * noise)
    } else {
        1.0
    }
}

/// What a rejecting stack records about each sample position in its first
/// pass: enough to judge any one frame's sample against all the others.
///
/// Sums rather than the samples themselves, since holding every frame is not
/// affordable (see [`Stack`]), and sums are enough to take one sample back out
/// again. Each sample counts by the inverse square of its frame's noise, which
/// is what puts a noisy frame and a quiet one on the same footing.
///
/// Thirty-four bytes a sample: six gigabytes for a stack of 61-megapixel colour
/// frames, which is why it is only kept when rejection was asked for. Held as
/// one record a sample rather than as an array for each figure, because the
/// second pass reads all of them for every sample of every frame, and six
/// streams through memory cost it half as much again as the arithmetic did.
#[derive(Debug, Clone)]
struct Evidence {
    records: Vec<Record>,
    /// How many samples there were, beside the records rather than in them,
    /// where it would round each record up from thirty-two bytes to forty.
    samples: Vec<u16>,
}

/// One sample position's figures.
///
/// In the first pass the three sums are of what each sample counted for, times
/// one, the sample, and the sample squared. Turned into the second pass they
/// are rewritten in place to describe the others once the brightest is set
/// aside: their mean, how far they scatter about it, and what they count for
/// altogether. Setting the brightest aside once, rather than once for every
/// sample judged against it, is most of what the second pass used to cost.
#[derive(Debug, Clone, Copy)]
struct Record {
    /// The weighted sum, becoming the others' mean.
    first: f64,
    /// The weighted sum of squares, becoming the others' scatter about that
    /// mean, which on pure noise is about one for each of them.
    second: f64,
    /// What the samples counted for altogether.
    ///
    /// Double precision, though single looks as if it would do: the scatter is
    /// the small difference between two large figures, and a total rounded to
    /// single precision moves that difference by tens when it should be about
    /// the number of frames.
    total: f64,
    /// The brightest sample, so that it can be set aside.
    brightest: f32,
    /// What the brightest counted for.
    ///
    /// Single precision, unlike the total it comes out of. Taking out a
    /// slightly rounded weight leaves a sliver of that sample behind, at the
    /// same slight weight in all three sums, which is a sample counting for
    /// almost nothing rather than an error in the scatter.
    brightest_inverse: f32,
}

impl Record {
    const EMPTY: Self = Self {
        first: 0.0,
        second: 0.0,
        total: 0.0,
        brightest: f32::NEG_INFINITY,
        brightest_inverse: 0.0,
    };

    /// Takes in one sample, in the first pass.
    fn take(&mut self, samples: &mut u16, value: f64, inverse: f64) {
        self.first += inverse * value;
        self.second += inverse * value * value;
        self.total += inverse;
        *samples = samples.saturating_add(1);
        #[allow(clippy::cast_possible_truncation)]
        let value = value as f32;
        if value > self.brightest {
            self.brightest = value;
            #[allow(clippy::cast_possible_truncation)]
            {
                self.brightest_inverse = inverse as f32;
            }
        }
    }

    /// Sets the brightest sample aside and rewrites the sums as the rest's
    /// mean, scatter and total. Returns how many samples that leaves.
    fn set_brightest_aside(&mut self, samples: u16) -> u16 {
        let (mut first, mut second, mut total, mut others) =
            (self.first, self.second, self.total, samples);
        if self.brightest.is_finite() && others > 0 {
            let (top, inverse) = (f64::from(self.brightest), f64::from(self.brightest_inverse));
            first -= inverse * top;
            second -= inverse * top * top;
            total -= inverse;
            others -= 1;
        }
        if total > 0.0 {
            let mean = first / total;
            self.first = mean;
            self.second = (second - first * mean).max(0.0);
        } else {
            self.first = 0.0;
            self.second = 0.0;
        }
        self.total = total;
        others
    }
}

/// Fewest other samples a sample can be judged against.
///
/// The scatter of the others is measured from them, and from fewer than four —
/// three degrees of freedom — it is so uncertain that the allowance needed to
/// keep ordinary noise is wider than nine deviations, and nothing short of a
/// satellite could be told apart.
const MINIMUM_OTHERS: usize = 4;

impl Evidence {
    fn new(samples: usize) -> Self {
        Self {
            records: vec![Record::EMPTY; samples],
            samples: vec![0; samples],
        }
    }

    /// The rows of one channel, for writing.
    fn rows(
        &mut self,
        plane: usize,
        pixels: usize,
        width: usize,
    ) -> Vec<(&mut [Record], &mut [u16])> {
        let range = plane..plane + pixels;
        self.records[range.clone()]
            .chunks_mut(width)
            .zip(self.samples[range].chunks_mut(width))
            .collect()
    }

    /// Sets every position's brightest sample aside, once, before the second
    /// pass judges anything against it.
    fn set_brightest_aside(&mut self) {
        self.records
            .par_iter_mut()
            .zip(self.samples.par_iter_mut())
            .for_each(|(record, samples)| *samples = record.set_brightest_aside(*samples));
    }

    /// Whether `value`, from a frame whose samples count for `inverse`, is out
    /// of keeping with the other samples recorded at `slot`.
    ///
    /// The others are every sample recorded there but this one, with the
    /// brightest of them set aside. Their mean is the estimate this sample is
    /// compared with; how far they scatter about it, in units of their own
    /// noise, says whether the sky itself varies here — it does across a star,
    /// whose light moves with the seeing — and widens the allowance where it
    /// does. What is allowed is that scatter applied to this sample's own
    /// noise and to the uncertainty of the mean, at the Student's t that keeps
    /// pure noise to the chance the stack was asked for.
    ///
    /// Written out, a sample is out of keeping when
    /// `|value − mean| > t · √(scatter · (1/inverse + 1/total))`. It is tested
    /// here squared and multiplied through by quantities known to be positive,
    /// which is the same decision without a division or a root. This runs for
    /// every sample of every frame.
    fn is_outlier(&self, slot: usize, value: f64, inverse: f64, critical: &[f64]) -> bool {
        let record = &self.records[slot];
        #[allow(clippy::cast_possible_truncation)]
        let is_brightest =
            value as f32 == record.brightest && inverse as f32 == record.brightest_inverse;

        // The record already leaves the brightest out. When this sample is the
        // brightest, that is exactly the others; otherwise it comes out too.
        let others = usize::from(self.samples[slot]);
        let (others, left) = if is_brightest {
            (others, record.total)
        } else {
            (others.saturating_sub(1), record.total - inverse)
        };
        if others < MINIMUM_OTHERS || left <= 0.0 {
            return false;
        }

        let difference = value - record.first;
        let mut scatter = record.second * left;
        if !is_brightest {
            scatter -= inverse * record.total * difference * difference;
        }
        // Distance and scatter both scaled by what the others count for, which
        // is what leaves the comparison free of divisions.
        let distance = record.total * difference;
        #[allow(clippy::cast_precision_loss)]
        let freedom = (others - 1) as f64;
        let allowed = critical.get(others - 1).copied().unwrap_or(f64::INFINITY);
        // An infinite allowance against no scatter is not a number, and the
        // comparison is then false: the sample is kept, as it should be.
        distance * distance * freedom * inverse > allowed * scatter.max(0.0) * (left + inverse)
    }
}

/// What a frame should count for, from its noise and the width of its stars.
///
/// The inverse square of the noise is what makes a stack as clean as it can
/// be: it is the arrangement that minimises the noise of the result. Dividing
/// by the square of the star width as well favours the frames that hold detail
/// rather than merely the quiet ones, which is what anyone stacking is
/// actually after — a still, sharp frame is worth more than a still, soft one.
///
/// Returns `None` when there is nothing to judge by, in which case the frame
/// counts the same as every other.
#[must_use]
pub fn weight_of(noise: f64, fwhm: Option<f64>) -> Option<f64> {
    if !noise.is_finite() || noise <= 0.0 {
        return None;
    }
    let sharpness = match fwhm {
        Some(fwhm) if fwhm.is_finite() && fwhm > 0.0 => fwhm * fwhm,
        _ => 1.0,
    };
    let weight = 1.0 / (noise * noise * sharpness);
    weight.is_finite().then_some(weight)
}

/// The sky level of a frame, for bringing frames to a common one.
///
/// The median of the frame, robustly estimated, which is the sky: stars and
/// nebulosity occupy too little of a frame to move it.
#[must_use]
pub fn sky_level(image: &FitsImage) -> f64 {
    crate::quality::background_and_noise(image).0
}

/// What to add to a frame to bring its sky to `reference`.
///
/// Additive rather than multiplicative because what changes between frames of
/// one night is light added to the sky — the moon, dawn, a passing car — and
/// added light comes off again by subtraction. Cloud dims the stars as well,
/// which needs a scale, and is left for later: a frame dimmed by cloud is
/// usually one to throw away rather than one to rescue.
#[must_use]
pub fn levelling(image: &FitsImage, reference: f64) -> f64 {
    reference - sky_level(image)
}

/// The value at a point between pixels, weighted by how near each one is, and
/// how much that weighting quietened it.
///
/// The second figure is the noise variance of the result as a fraction of a
/// single pixel's: one on a pixel exactly, a quarter exactly between four.
/// Rejection needs it, since a resampled frame is quieter than the frame it
/// came from, and judging it by the noise it had would call ordinary samples
/// from the frames that were not resampled outliers.
///
/// `None` where the point falls off the frame, or where any of the four it sits
/// between is undefined: guessing across a dead pixel would invent signal.
fn sample(source: &[f32], width: usize, height: usize, x: f64, y: f64) -> Option<(f64, f64)> {
    if x < 0.0 || y < 0.0 {
        return None;
    }
    let (fx, fy) = (x.floor(), y.floor());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (ix, iy) = (fx as usize, fy as usize);
    if ix + 1 >= width || iy + 1 >= height {
        return None;
    }

    let (tx, ty) = (x - fx, y - fy);
    let at = |x: usize, y: usize| {
        let value = source[y * width + x];
        if value.is_finite() {
            Some(f64::from(value))
        } else {
            None
        }
    };
    let (a, b, c, d) = (
        at(ix, iy)?,
        at(ix + 1, iy)?,
        at(ix, iy + 1)?,
        at(ix + 1, iy + 1)?,
    );
    let value =
        a * (1.0 - tx) * (1.0 - ty) + b * tx * (1.0 - ty) + c * (1.0 - tx) * ty + d * tx * ty;
    let quietened = ((1.0 - tx).powi(2) + tx * tx) * ((1.0 - ty).powi(2) + ty * ty);
    Some((value, quietened))
}

/// How near a predicted position a star must be to be called the same one.
const MATCH_TOLERANCE: f64 = 3.0;

/// How many times the fit is improved before it is taken as final.
///
/// Each pass matches stars against the current guess and fits a better one to
/// what it found. Three is enough to go from the handful a shift alone catches
/// to everything on the frame; more changes nothing.
const REFINEMENTS: usize = 3;

/// Fits a turn and a shift together, starting from a shift alone.
///
/// A shift fitted to a frame that is also slightly turned matches only the
/// middle, where the turn has not yet moved anything far. Those few matches are
/// enough to estimate the turn, and with the turn known the rest of the frame
/// matches too, which gives a better estimate again.
///
/// Returns the shift, the turn in radians, and how many stars agreed with it,
/// or `None` if refining found nothing better than it started with.
fn refine(
    anchors: &[Star],
    moving: &[Star],
    (width, height): (usize, usize),
    shift: (f64, f64),
) -> Option<(f64, f64, f64, usize)> {
    #[allow(clippy::cast_precision_loss)]
    let centre = ((width - 1) as f64 / 2.0, (height - 1) as f64 / 2.0);
    let (mut dx, mut dy, mut rotation) = (shift.0, shift.1, 0.0f64);
    let mut best: Option<(f64, f64, f64, usize)> = None;

    for _ in 0..REFINEMENTS {
        // Every anchor's predicted place in the other frame, and the nearest
        // star to it if there is one close enough.
        let mut pairs: Vec<(Star, Star)> = Vec::new();
        for anchor in anchors {
            let (px, py) = place(*anchor, centre, dx, dy, rotation);
            let mut nearest: Option<(f64, &Star)> = None;
            for star in moving {
                let distance = ((star.x - px).powi(2) + (star.y - py).powi(2)).sqrt();
                if distance <= MATCH_TOLERANCE && nearest.is_none_or(|(best, _)| distance < best) {
                    nearest = Some((distance, star));
                }
            }
            if let Some((_, star)) = nearest {
                pairs.push((*anchor, *star));
            }
        }
        if pairs.len() < MINIMUM_VOTES {
            break;
        }

        // The turn and shift that best carry the anchors onto their matches.
        #[allow(clippy::cast_precision_loss)]
        let n = pairs.len() as f64;
        let (mut ux, mut uy, mut vx, mut vy) = (0.0, 0.0, 0.0, 0.0);
        for (anchor, star) in &pairs {
            ux += anchor.x - centre.0;
            uy += anchor.y - centre.1;
            vx += star.x - centre.0;
            vy += star.y - centre.1;
        }
        let (ux, uy, vx, vy) = (ux / n, uy / n, vx / n, vy / n);

        let (mut cross, mut dot) = (0.0f64, 0.0f64);
        for (anchor, star) in &pairs {
            let (ax, ay) = (anchor.x - centre.0 - ux, anchor.y - centre.1 - uy);
            let (sx, sy) = (star.x - centre.0 - vx, star.y - centre.1 - vy);
            cross += ax * sy - ay * sx;
            dot += ax * sx + ay * sy;
        }
        rotation = cross.atan2(dot);

        // The shift is whatever is left once the turn is taken out.
        let (c, s) = (rotation.cos(), rotation.sin());
        dx = vx - (ux * c - uy * s);
        dy = vy - (ux * s + uy * c);

        let found = pairs.len();
        if best.is_none_or(|(_, _, _, count)| found > count) {
            best = Some((dx, dy, rotation, found));
        }
    }

    best
}

/// Where an anchor lands in the other frame under a turn and a shift.
fn place(anchor: Star, centre: (f64, f64), dx: f64, dy: f64, rotation: f64) -> (f64, f64) {
    let (c, s) = (rotation.cos(), rotation.sin());
    let (ax, ay) = (anchor.x - centre.0, anchor.y - centre.1);
    (
        centre.0 + ax * c - ay * s + dx,
        centre.1 + ax * s + ay * c + dy,
    )
}

/// A star's position after the frame is turned half a circle about its centre.
fn turn(star: Star, (width, height): (usize, usize)) -> Star {
    #[allow(clippy::cast_precision_loss)]
    Star {
        x: (width - 1) as f64 - star.x,
        y: (height - 1) as f64 - star.y,
        ..star
    }
}

/// The brightest stars of a field, most luminous first.
fn brightest(field: &StarField) -> Vec<Star> {
    let mut stars: Vec<Star> = field.stars.clone();
    stars.sort_unstable_by(|a, b| b.flux.total_cmp(&a.flux));
    stars.truncate(STARS_CONSIDERED);
    stars
}

/// A running total of frames, kept so that no more than one frame is held at a
/// time beyond the totals themselves.
///
/// Forty-seven 61-megapixel frames are eleven gigabytes as floats. Reading them
/// all to take a median is not affordable at this size, so the frames are added
/// one at a time and divided at the end. What each pixel needs is its total and
/// how many frames actually covered it, since aligned frames do not overlap
/// completely and the edges are seen by fewer of them.
#[derive(Debug, Clone)]
pub struct Stack {
    width: usize,
    height: usize,
    channels: usize,
    total: Vec<f64>,
    /// Total weight each pixel has received, which is the count when every
    /// frame counts for one.
    counted: Vec<f32>,
    /// What rejecting outliers needs, kept only when the stack is to reject
    /// them.
    evidence: Option<Evidence>,
    frames: usize,
}

/// How unlikely a sample has to be, as a count of deviations of ordinary
/// Gaussian noise, before it is left out.
///
/// Three keeps everything the sky produced and discards what crossed in front
/// of it: ordinary noise lands that far out 0.27 per cent of the time. With few
/// frames the scatter of the others is itself uncertain, and the allowance
/// widens to keep that chance the same — to nine deviations' worth with five
/// frames, three and a half with twenty.
pub const DEFAULT_CLIP: f64 = 3.0;

/// Fewest frames worth rejecting against.
///
/// With five, only the brightest sample at a pixel can be judged, against the
/// four others; that is still enough to take out a satellite crossing one of
/// them. Below it the plain average is the better answer, and the honest one.
pub const MINIMUM_TO_CLIP: usize = 5;

/// How many row differences go into measuring a channel's noise.
const NOISE_SAMPLES: usize = 1_000_000;

impl Stack {
    /// An empty stack the size of the frames going into it.
    #[must_use]
    pub fn new(width: usize, height: usize, channels: usize) -> Self {
        let samples = width * height * channels;
        Self {
            width,
            height,
            channels,
            total: vec![0.0; samples],
            counted: vec![0.0; samples],
            evidence: None,
            frames: 0,
        }
    }

    /// The same, but recording what a second pass needs to reject outliers.
    #[must_use]
    pub fn rejecting(width: usize, height: usize, channels: usize) -> Self {
        Self {
            evidence: Some(Evidence::new(width * height * channels)),
            ..Self::new(width, height, channels)
        }
    }

    /// Whether a frame is the shape of this stack.
    fn fits(&self, image: &FitsImage) -> bool {
        image.width == self.width && image.height == self.height && image.channels == self.channels
    }

    /// Turns a first pass into the second one that does the rejecting.
    ///
    /// The totals are emptied and reused rather than allocated again: on a
    /// full frame they are gigabytes, and there is no sense holding two sets.
    ///
    /// # Errors
    ///
    /// Hands the stack back unchanged when it was not recording for rejection,
    /// or when fewer than [`MINIMUM_TO_CLIP`] frames went in; its plain average
    /// is then the answer.
    pub fn into_rejecting(mut self, deviations: f64) -> Result<Rejecting, Box<Self>> {
        if self.frames < MINIMUM_TO_CLIP {
            return Err(Box::new(self));
        }
        let Some(mut evidence) = self.evidence.take() else {
            return Err(Box::new(self));
        };
        evidence.set_brightest_aside();

        let chance = normal_tail(deviations);
        let critical = (0..=self.frames)
            .map(|dof| student_critical(dof, chance).powi(2))
            .collect();

        self.total.fill(0.0);
        self.counted.fill(0.0);
        Ok(Rejecting {
            stack: Self { frames: 0, ..self },
            evidence,
            critical,
            rejected: 0,
        })
    }

    /// How many frames have gone in.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Adds a frame at the given alignment.
    ///
    /// A frame that only needs shifting goes to the nearest whole pixel: a
    /// fraction would need the frame resampled, which softens it. One that has
    /// to be turned is resampled anyway.
    ///
    /// Returns false, and adds nothing, if the frame is not the same shape as
    /// the stack — a different sensor, or a different binning, has no business
    /// in it.
    pub fn add(&mut self, image: &FitsImage, alignment: Alignment) -> bool {
        if !self.fits(image) {
            return false;
        }
        let (width, height) = (self.width, self.height);
        let pixels = width * height;
        // Measured only when it will be used: it is a pass over the frame.
        let noise = self.evidence.as_ref().map(|_| channel_noise(image));

        for channel in 0..self.channels {
            let plane = channel * pixels;
            let source = &image.data[plane..plane + pixels];
            let totals = self.total[plane..plane + pixels]
                .chunks_mut(width)
                .zip(self.counted[plane..plane + pixels].chunks_mut(width));

            match (self.evidence.as_mut(), noise.as_ref()) {
                (Some(evidence), Some(noise)) => {
                    let inverse = inverse_variance(noise[channel]);
                    let rows: Vec<_> = totals.zip(evidence.rows(plane, pixels, width)).collect();
                    rows.into_par_iter().enumerate().for_each(
                        |(y, ((total, counted), (records, samples)))| {
                            each_placed(
                                y,
                                width,
                                height,
                                alignment,
                                source,
                                |x, value, quietened| {
                                    total[x] += value * alignment.weight;
                                    #[allow(clippy::cast_possible_truncation)]
                                    {
                                        counted[x] += alignment.weight as f32;
                                    }
                                    records[x].take(&mut samples[x], value, inverse / quietened);
                                },
                            );
                        },
                    );
                }
                _ => {
                    let rows: Vec<_> = totals.collect();
                    rows.into_par_iter()
                        .enumerate()
                        .for_each(|(y, (total, counted))| {
                            each_placed(y, width, height, alignment, source, |x, value, _| {
                                total[x] += value * alignment.weight;
                                #[allow(clippy::cast_possible_truncation)]
                                {
                                    counted[x] += alignment.weight as f32;
                                }
                            });
                        });
                }
            }
        }

        self.frames += 1;
        true
    }

    /// The average of everything added, as an image.
    ///
    /// A pixel no frame covered is undefined rather than zero: it is unknown,
    /// and writing zero would put a black border into the result.
    #[must_use]
    pub fn finish(&self, header: FitsHeader) -> FitsImage {
        let data: Vec<f32> = self
            .total
            .par_iter()
            .zip(&self.counted)
            .map(|(total, count)| {
                if *count <= 0.0 {
                    f32::NAN
                } else {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        (total / f64::from(*count)) as f32
                    }
                }
            })
            .collect();

        let (min, max) = crate::image::finite_min_max(&data);
        FitsImage {
            width: self.width,
            height: self.height,
            channels: self.channels,
            data,
            header,
            min,
            max,
        }
    }
}

/// Each channel's noise, as a standard deviation in counts.
///
/// From second differences along a row — a pixel, less twice the one two
/// along, plus the one two beyond — which cancel a level and a slope exactly.
/// Two apart, because colour reconstruction fills each pixel partly from its
/// neighbours, which then share their noise. Differences rather than the
/// pixels themselves, so that what changes smoothly across the sensor —
/// nebulosity, and the vignetting of a frame without a flat — is not counted
/// as noise; second differences rather than first, so that a steep edge of
/// vignetting is not either. And the median of them, so that stars are not.
///
/// Measured on the frame as it goes into the stack, calibrated and
/// reconstructed, because those are the samples being judged. The one noise
/// figure taken from the raw frame said the last frame of one night was three
/// times as noisy as the first, when in each colour it was between two and two
/// and a half.
#[must_use]
pub fn channel_noise(image: &FitsImage) -> Vec<f64> {
    let (width, height) = (image.width, image.height);
    let pixels = width * height;
    if width < 5 || height == 0 {
        return vec![0.0; image.channels];
    }
    let rows = (NOISE_SAMPLES / (width - 4)).clamp(1, height);
    let stride = (height / rows).max(1);

    (0..image.channels)
        .map(|channel| {
            let plane = &image.data[channel * pixels..(channel + 1) * pixels];
            let mut differences: Vec<f32> = (0..height)
                .step_by(stride)
                .flat_map(|y| {
                    let row = &plane[y * width..(y + 1) * width];
                    row.iter()
                        .zip(&row[2..])
                        .zip(&row[4..])
                        .filter_map(|((a, b), c)| {
                            let curve = a - 2.0 * b + c;
                            curve.is_finite().then_some(curve.abs())
                        })
                })
                .collect();
            if differences.is_empty() {
                return 0.0;
            }
            let middle = differences.len() / 2;
            let (_, median, _) = differences.select_nth_unstable_by(middle, f32::total_cmp);
            // The median absolute difference as a standard deviation, and a
            // second difference, whose variance is six samples', as one.
            f64::from(*median) * 1.482_602_218_505_602 / 6.0_f64.sqrt()
        })
        .collect()
}

/// The chance of ordinary Gaussian noise landing further than `deviations`
/// from its mean, on either side: 0.27 per cent at three.
fn normal_tail(deviations: f64) -> f64 {
    erfc(deviations / std::f64::consts::SQRT_2)
}

/// The complementary error function, good to about one part in ten million.
///
/// The Chebyshev fit from *Numerical Recipes*; the standard library has none.
#[allow(clippy::unreadable_literal)]
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let tail = t
        * (-z * z - 1.26551223
            + t * (1.00002368
                + t * (0.37409196
                    + t * (0.09678418
                        + t * (-0.18628806
                            + t * (0.27886807
                                + t * (-1.13520398
                                    + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))))
            .exp();
    if x >= 0.0 {
        tail
    } else {
        2.0 - tail
    }
}

/// How far out a Student's t sample with `dof` degrees of freedom must land to
/// be as unlikely as `chance`, counting both sides.
///
/// The scatter a sample is judged against is measured from a handful of other
/// frames, and so is itself uncertain; this is the allowance that makes up for
/// it. Infinite with no degrees of freedom, where nothing can be judged.
fn student_critical(dof: usize, chance: f64) -> f64 {
    if dof == 0 || !(chance > 0.0 && chance < 1.0) {
        return f64::INFINITY;
    }
    // Beyond ten thousand the answer is the Gaussian one to a part in a
    // thousand, and the continued fraction below stops converging in the
    // steps it is given.
    #[allow(clippy::cast_precision_loss)]
    let nu = dof.min(10_000) as f64;
    let (mut low, mut high) = (0.0, 1.0);
    while student_tail(high, nu) > chance {
        high *= 2.0;
        if high > 1e12 {
            return f64::INFINITY;
        }
    }
    for _ in 0..100 {
        let middle = 0.5 * (low + high);
        if student_tail(middle, nu) > chance {
            low = middle;
        } else {
            high = middle;
        }
    }
    high
}

/// The chance of a Student's t sample landing further than `t` from zero, on
/// either side.
fn student_tail(t: f64, nu: f64) -> f64 {
    incomplete_beta(nu / 2.0, 0.5, nu / (nu + t * t))
}

/// The regularised incomplete beta function, by continued fraction.
fn incomplete_beta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let front =
        (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        front * beta_fraction(a, b, x) / a
    } else {
        1.0 - front * beta_fraction(b, a, 1.0 - x) / b
    }
}

/// The continued fraction behind [`incomplete_beta`], by Lentz's method.
fn beta_fraction(a: f64, b: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let (sum, plus, minus) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - sum * x / plus;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut fraction = d;
    for m in 1..=300 {
        #[allow(clippy::cast_precision_loss)]
        let m = m as f64;
        let m2 = 2.0 * m;
        let even = m * (b - m) * x / ((minus + m2) * (a + m2));
        d = 1.0 + even * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + even / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        fraction *= d * c;
        let odd = -(a + m) * (sum + m) * x / ((a + m2) * (plus + m2));
        d = 1.0 + odd * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + odd / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let step = d * c;
        fraction *= step;
        if (step - 1.0).abs() < 1e-14 {
            break;
        }
    }
    fraction
}

/// The logarithm of the gamma function, for positive arguments.
///
/// Lanczos's approximation as *Numerical Recipes* gives it, good to about two
/// parts in ten thousand million.
#[allow(clippy::unreadable_literal)]
fn ln_gamma(x: f64) -> f64 {
    const SERIES: [f64; 6] = [
        76.18009172947146,
        -86.50532032941677,
        24.01409824083091,
        -1.231739572450155,
        0.1208650973866179e-2,
        -0.5395239384953e-5,
    ];
    let base = x + 5.5;
    let base = base - (x + 0.5) * base.ln();
    let mut term = x;
    let mut series = 1.000000000190015;
    for coefficient in SERIES {
        term += 1.0;
        series += coefficient / term;
    }
    -base + (2.5066282746310005 * series / x).ln()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_fits_from_bytes;
    use crate::stars::{detect, DetectionParams};
    use crate::testutil::{gaussian_background, synthetic_fits, SyntheticSpec};

    fn header_with(cards: &[(&str, &str)]) -> FitsHeader {
        FitsHeader {
            cards: cards
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    /// A field of stars at the given positions, on noise.
    fn frame(width: usize, height: usize, stars: &[(f64, f64)], seed: u64) -> FitsImage {
        let mut pixels = gaussian_background(width, height, 1000.0, 10.0, seed);
        for (cx, cy) in stars {
            for dy in -6i64..=6 {
                for dx in -6i64..=6 {
                    #[allow(clippy::cast_possible_truncation)]
                    let (x, y) = (*cx as i64 + dx, *cy as i64 + dy);
                    if x < 0 || y < 0 || x as usize >= width || y as usize >= height {
                        continue;
                    }
                    #[allow(clippy::cast_precision_loss)]
                    let r = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)) / 4.0;
                    #[allow(clippy::cast_sign_loss)]
                    {
                        pixels[y as usize * width + x as usize] += 12_000.0 * (-r).exp();
                    }
                }
            }
        }
        let spec = SyntheticSpec::new(width, height, -32);
        read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap()
    }

    /// The positions of a field, shifted, for building a second frame.
    fn shifted(stars: &[(f64, f64)], dx: f64, dy: f64) -> Vec<(f64, f64)> {
        stars.iter().map(|(x, y)| (x + dx, y + dy)).collect()
    }

    /// A scattering of stars that is the same every run.
    fn field_positions() -> Vec<(f64, f64)> {
        let mut state = 0x5eed_1234_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..14)
            .map(|_| {
                #[allow(clippy::cast_precision_loss)]
                (40.0 + (next() % 200) as f64, 40.0 + (next() % 200) as f64)
            })
            .collect()
    }

    #[test]
    fn the_pier_side_is_read_from_the_header() {
        assert_eq!(
            PierSide::from_header(&header_with(&[("PIERSIDE", "'East'")])),
            PierSide::East
        );
        assert_eq!(
            PierSide::from_header(&header_with(&[("PIERSIDE", "'West'")])),
            PierSide::West
        );
        assert_eq!(
            PierSide::from_header(&header_with(&[("PIERSIDE", "WEST")])),
            PierSide::West
        );
    }

    #[test]
    fn a_missing_or_strange_pier_side_is_unknown_rather_than_guessed() {
        // Guessing wrong stacks half a night upside down, which is worse than
        // any honest answer.
        assert_eq!(
            PierSide::from_header(&header_with(&[("OBJECT", "'M42'")])),
            PierSide::Unknown
        );
        assert_eq!(
            PierSide::from_header(&header_with(&[("PIERSIDE", "'Unknown'")])),
            PierSide::Unknown
        );
    }

    #[test]
    fn only_a_known_change_of_side_turns_a_frame() {
        assert!(PierSide::West.needs_turning(PierSide::East));
        assert!(PierSide::East.needs_turning(PierSide::West));
        assert!(!PierSide::East.needs_turning(PierSide::East));
        // Not knowing is not a reason to turn anything.
        assert!(!PierSide::Unknown.needs_turning(PierSide::East));
        assert!(!PierSide::East.needs_turning(PierSide::Unknown));
    }

    #[test]
    fn a_known_shift_is_recovered() {
        let (w, h) = (300usize, 300usize);
        let positions = field_positions();
        let params = DetectionParams::default();

        let reference = detect(&frame(w, h, &positions, 1), &params);
        let moved = detect(&frame(w, h, &shifted(&positions, 12.0, -7.0), 2), &params);

        let alignment = align(&reference, &moved, (w, h), false).expect("an offset");
        assert_eq!(
            alignment.whole(),
            (12, -7),
            "the sky moved twelve right and seven up, and that is what it should say"
        );
        assert!(
            alignment.rounding_error() < 0.3,
            "a whole-pixel shift should be measured as one: {:?}",
            (alignment.dx, alignment.dy)
        );
        assert!(
            alignment.votes >= 8,
            "only {} stars agreed",
            alignment.votes
        );
    }

    #[test]
    fn a_meridian_flip_is_recovered() {
        // The frame the mount hands back after swinging to the other side of
        // the pier: the same sky, turned half a circle.
        let (w, h) = (300usize, 300usize);
        let positions = field_positions();
        let turned_positions: Vec<(f64, f64)> = positions
            .iter()
            .map(|(x, y)| {
                #[allow(clippy::cast_precision_loss)]
                ((w - 1) as f64 - x, (h - 1) as f64 - y)
            })
            .collect();

        let params = DetectionParams::default();
        let reference = detect(&frame(w, h, &positions, 3), &params);
        let flipped = detect(&frame(w, h, &turned_positions, 4), &params);

        // Told about the flip, the frame lines up with no shift left over.
        let alignment = align(&reference, &flipped, (w, h), true).expect("an offset");
        assert!(alignment.turned);
        assert_eq!(alignment.whole(), (0, 0));

        // Not told, it finds nothing worth trusting.
        let confused = align(&reference, &flipped, (w, h), false);
        assert!(
            confused.is_none_or(|a| a.votes < alignment.votes),
            "a flipped frame must not align as well unturned as turned"
        );
    }

    /// The same field turned by `degrees` about the centre of the frame.
    fn rotated(positions: &[(f64, f64)], (w, h): (usize, usize), degrees: f64) -> Vec<(f64, f64)> {
        #[allow(clippy::cast_precision_loss)]
        let centre = ((w - 1) as f64 / 2.0, (h - 1) as f64 / 2.0);
        let radians = degrees.to_radians();
        let (c, s) = (radians.cos(), radians.sin());
        positions
            .iter()
            .map(|(x, y)| {
                let (dx, dy) = (x - centre.0, y - centre.1);
                (centre.0 + dx * c - dy * s, centre.1 + dx * s + dy * c)
            })
            .collect()
    }

    #[test]
    fn a_slight_turn_is_measured_as_well_as_the_shift() {
        // What a mount leaves behind after a meridian flip: a fifth of a
        // degree, which is nothing in the middle of the frame and forty pixels
        // at the corner of a full one. Fitted as a shift alone it matches only
        // the middle.
        let (w, h) = (400usize, 400usize);
        let positions: Vec<(f64, f64)> = (0..30)
            .map(|i| {
                let angle = f64::from(i) * 0.83;
                let radius = 40.0 + f64::from(i) * 4.5;
                (200.0 + radius * angle.cos(), 200.0 + radius * angle.sin())
            })
            .collect();
        let turned_positions = rotated(&positions, (w, h), 0.9);

        let params = DetectionParams::default();
        let reference = detect(&frame(w, h, &positions, 91), &params);
        let moved = detect(&frame(w, h, &turned_positions, 92), &params);

        let alignment = align(&reference, &moved, (w, h), false).expect("an alignment");
        assert!(
            (alignment.rotation.to_degrees() - 0.9).abs() < 0.1,
            "measured {:.3} degrees, expected 0.9",
            alignment.rotation.to_degrees()
        );
        assert!(
            alignment.votes >= positions.len() * 2 / 3,
            "only {} of {} stars agreed once the turn was known",
            alignment.votes,
            positions.len()
        );
    }

    #[test]
    fn a_turned_frame_stacks_without_smearing() {
        // The frames a meridian flip produces: turned, and turned again by a
        // fraction of a degree that a shift cannot absorb.
        let (w, h) = (300usize, 300usize);
        let positions: Vec<(f64, f64)> = (0..12)
            .map(|i| {
                let angle = f64::from(i) * 0.9;
                let radius = 35.0 + f64::from(i) * 7.0;
                (150.0 + radius * angle.cos(), 150.0 + radius * angle.sin())
            })
            .collect();

        let params = DetectionParams::default();
        let straight = frame(w, h, &positions, 93);
        let reference = detect(&straight, &params);
        let skewed = frame(w, h, &rotated(&positions, (w, h), 0.8), 94);
        let found = detect(&skewed, &params);

        let alignment = align(&reference, &found, (w, h), false).expect("an alignment");
        assert!(
            alignment.rotation.abs() > 0.0,
            "a turn should have been found"
        );

        let mut stack = Stack::new(w, h, 1);
        assert!(stack.add(&straight, Alignment::still()));
        assert!(stack.add(&skewed, alignment));

        let stacked = detect(&stack.finish(FitsHeader { cards: Vec::new() }), &params);
        assert_eq!(
            stacked.count(),
            positions.len(),
            "each star should appear once, not twice"
        );
        assert!(
            stacked.roundness.is_some_and(|r| r > 0.6),
            "smearing would show here: {:?}",
            stacked.roundness
        );
    }

    #[test]
    fn an_alignment_only_a_few_stars_agree_on_is_refused() {
        // What a meridian flip looks like when the mount did not come back to
        // exactly half a turn: the middle of the frame lines up and the rest
        // does not. A translation fitted to those few would smear everything
        // else, so it is better to leave the frame out and say so.
        let (w, h) = (400usize, 400usize);
        let params = DetectionParams::default();

        let mut positions = Vec::new();
        let mut turned = Vec::new();
        for i in 0..24 {
            let angle = f64::from(i) * 0.26;
            let radius = 30.0 + f64::from(i) * 6.0;
            let (x, y) = (200.0 + radius * angle.cos(), 200.0 + radius * angle.sin());
            positions.push((x, y));

            // The same field turned by a shade under half a circle: near the
            // centre it lands on itself, further out it does not.
            let residual = 0.06_f64;
            let (dx, dy) = (x - 200.0, y - 200.0);
            let (c, s) = (
                (std::f64::consts::PI + residual).cos(),
                (std::f64::consts::PI + residual).sin(),
            );
            turned.push((200.0 + dx * c - dy * s, 200.0 + dx * s + dy * c));
        }

        let reference = detect(&frame(w, h, &positions, 71), &params);
        let rotated = detect(&frame(w, h, &turned, 72), &params);

        // Turning it back leaves the centre matching and the edges not.
        let attempt = align(&reference, &rotated, (w, h), true);
        assert!(
            attempt.is_none(),
            "a fit only the middle of the frame agrees with must be refused, got {attempt:?}"
        );
    }

    #[test]
    fn the_same_frames_line_up_the_same_way_every_time() {
        // The vote lives in a hash map, whose order is not the same twice, so
        // the most popular offset has to be chosen by something other than
        // which one the map happened to yield first. Two runs over one night
        // must not produce two different stacks.
        let (w, h) = (300usize, 300usize);
        let positions = field_positions();
        let params = DetectionParams::default();
        let reference = detect(&frame(w, h, &positions, 81), &params);
        let moved = detect(&frame(w, h, &shifted(&positions, 4.0, 9.0), 82), &params);

        let first = align(&reference, &moved, (w, h), false).expect("an offset");
        for _ in 0..8 {
            let again = align(&reference, &moved, (w, h), false).expect("an offset");
            assert_eq!(
                (first.dx, first.dy, first.votes),
                (again.dx, again.dy, again.votes)
            );
        }
    }

    #[test]
    fn frames_of_different_fields_do_not_align() {
        let (w, h) = (300usize, 300usize);
        let params = DetectionParams::default();
        let reference = detect(&frame(w, h, &field_positions(), 5), &params);

        let elsewhere: Vec<(f64, f64)> = (0..14)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                (50.0 + (i * 17 % 200) as f64, 60.0 + (i * 29 % 200) as f64)
            })
            .collect();
        let other = detect(&frame(w, h, &elsewhere, 6), &params);

        // It may find something, but it must not find it convincing.
        if let Some(alignment) = align(&reference, &other, (w, h), false) {
            assert!(
                alignment.votes < 8,
                "unrelated fields agreed {} times",
                alignment.votes
            );
        }
    }

    #[test]
    fn an_empty_field_gives_no_alignment() {
        let empty = StarField::default();
        let some = detect(
            &frame(200, 200, &field_positions(), 7),
            &DetectionParams::default(),
        );
        assert!(align(&some, &empty, (200, 200), false).is_none());
        assert!(align(&empty, &some, (200, 200), false).is_none());
    }

    #[test]
    fn stacking_reduces_the_noise_by_the_root_of_the_frames() {
        // The whole claim of the feature, in one assertion.
        let (w, h) = (120usize, 120usize);
        let flat = |seed: u64| {
            let pixels = gaussian_background(w, h, 1000.0, 50.0, seed);
            let spec = SyntheticSpec::new(w, h, -32);
            read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap()
        };

        let noise_of = |image: &FitsImage| {
            let mean: f64 =
                image.data.iter().map(|v| f64::from(*v)).sum::<f64>() / image.data.len() as f64;
            let variance: f64 = image
                .data
                .iter()
                .map(|v| (f64::from(*v) - mean).powi(2))
                .sum::<f64>()
                / image.data.len() as f64;
            variance.sqrt()
        };

        let one = flat(11);
        let single = noise_of(&one);

        let mut stack = Stack::new(w, h, 1);
        let still = Alignment::still();
        for seed in 0..16u64 {
            assert!(stack.add(&flat(100 + seed), still));
        }
        assert_eq!(stack.frames(), 16);

        let stacked = noise_of(&stack.finish(FitsHeader { cards: Vec::new() }));
        let expected = single / 4.0;
        assert!(
            (stacked - expected).abs() < expected * 0.25,
            "sixteen frames should quarter the noise: {single:.1} became {stacked:.1}, \
             expected about {expected:.1}"
        );
    }

    #[test]
    fn a_stack_of_shifted_frames_keeps_its_stars_sharp() {
        // Adding frames where they lie rather than where they belong smears
        // every star into a line, which is the failure this exists to avoid.
        let (w, h) = (200usize, 200usize);
        // More than the fewest votes an alignment is believed on, since this
        // test is about the stacking rather than about that threshold.
        let positions = vec![
            (60.0, 70.0),
            (140.0, 120.0),
            (100.0, 160.0),
            (40.0, 130.0),
            (160.0, 60.0),
            (120.0, 40.0),
        ];
        let params = DetectionParams::default();

        let reference_image = frame(w, h, &positions, 21);
        let reference = detect(&reference_image, &params);
        let single_width = reference.fwhm.expect("a width for one frame");

        let mut aligned = Stack::new(w, h, 1);
        let mut careless = Stack::new(w, h, 1);
        let still = Alignment::still();
        for (index, (dx, dy)) in [(0.0, 0.0), (9.0, -5.0), (-6.0, 8.0), (3.0, 11.0)]
            .into_iter()
            .enumerate()
        {
            #[allow(clippy::cast_possible_truncation)]
            let image = frame(w, h, &shifted(&positions, dx, dy), 30 + index as u64);
            let found = detect(&image, &params);
            let alignment = align(&reference, &found, (w, h), false).expect("an offset");
            assert!(aligned.add(&image, alignment));
            assert!(careless.add(&image, still));
        }

        let aligned_image = aligned.finish(FitsHeader { cards: Vec::new() });
        let careless_image = careless.finish(FitsHeader { cards: Vec::new() });
        let sharp = detect(&aligned_image, &params);
        let smeared = detect(&careless_image, &params);

        assert_eq!(sharp.count(), positions.len(), "every star, once");
        let stacked_width = sharp.fwhm.expect("a width for the stack");
        assert!(
            stacked_width < single_width * 1.3,
            "aligned stacking widened the stars from {single_width:.2} to {stacked_width:.2}"
        );
        // Not aligning shows up in the shape rather than in the width. Each
        // star lands in four places and those four run together into one
        // ragged region, so the detection finds a single source at their
        // centre of gravity: the count does not change, and neither does the
        // width, which is taken from the peak's own profile rather than from
        // the whole region -- the same blend-resistance that makes it a good
        // measure of a real frame. What gives it away is roundness, measured
        // across the region, which collapses when the region is a scatter.
        let round = sharp.roundness.expect("roundness for the aligned stack");
        let ragged = smeared.roundness.expect("roundness for the careless stack");
        assert!(
            ragged < round * 0.75,
            "stacking without aligning should show in the roundness: {round:.2} aligned \
             against {ragged:.2} careless"
        );
    }

    /// A frame of sky alone, at `level`, with noise `sigma`, changed by `change`.
    fn sky_frame_with(
        (w, h): (usize, usize),
        level: f64,
        sigma: f64,
        seed: u64,
        change: impl Fn(&mut [f64]),
    ) -> FitsImage {
        let mut pixels = gaussian_background(w, h, level, sigma, seed);
        change(&mut pixels);
        let spec = SyntheticSpec::new(w, h, -32);
        read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap()
    }

    fn sky_frame(w: usize, h: usize, level: f64, sigma: f64, seed: u64) -> FitsImage {
        sky_frame_with((w, h), level, sigma, seed, |_| {})
    }

    /// The noise of each of ten frames of one real night, first to last, as
    /// the sky brightened threefold.
    const NIGHT: [f64; 10] = [11.9, 11.9, 11.9, 13.3, 13.3, 16.3, 23.7, 26.7, 35.6, 34.1];

    /// Stacks `frames` in two passes, rejecting, and says how many samples
    /// each frame lost.
    fn two_passes(frames: &[FitsImage], placings: &[Alignment]) -> (FitsImage, Vec<usize>) {
        let (w, h, c) = (frames[0].width, frames[0].height, frames[0].channels);
        let mut first = Stack::rejecting(w, h, c);
        for (image, placing) in frames.iter().zip(placings) {
            assert!(first.add(image, *placing));
        }
        let mut second = first.into_rejecting(DEFAULT_CLIP).expect("enough frames");
        let mut lost = Vec::new();
        for (image, placing) in frames.iter().zip(placings) {
            let before = second.rejected();
            assert!(second.add(image, *placing));
            lost.push(second.rejected() - before);
        }
        (second.finish(FitsHeader { cards: Vec::new() }), lost)
    }

    /// How far the stack sits above `level` along row `y`, on average over
    /// the columns `xs`.
    fn row_excess(image: &FitsImage, y: usize, xs: std::ops::Range<usize>, level: f64) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let n = xs.len() as f64;
        xs.map(|x| f64::from(image.data[y * image.width + x]) - level)
            .sum::<f64>()
            / n
    }

    #[test]
    fn ordinary_noise_loses_what_chance_says_however_noisy_each_frame_is() {
        // The first rule rejected eight per cent of pure noise on a night like
        // this, and twenty-eight per cent of its noisiest frame, because it
        // judged every frame by a spread the quiet frames set.
        let (w, h) = (300usize, 300usize);
        let frames: Vec<FitsImage> = NIGHT
            .iter()
            .enumerate()
            .map(|(i, sigma)| sky_frame(w, h, 1000.0, *sigma, 500 + i as u64))
            .collect();
        let (_, lost) = two_passes(&frames, &[Alignment::still(); 10]);

        #[allow(clippy::cast_precision_loss)]
        let share = |n: usize| 100.0 * n as f64 / (w * h) as f64;
        for (i, n) in lost.iter().enumerate() {
            assert!(
                share(*n) < 1.0,
                "frame {i}, noise {}, lost {:.2}%",
                NIGHT[i],
                share(*n)
            );
        }
        let all = share(lost.iter().sum::<usize>()) / 10.0;
        assert!(
            (0.1..0.8).contains(&all),
            "{all:.2}% of pure noise rejected"
        );
    }

    #[test]
    fn how_frames_are_weighted_does_not_change_what_is_rejected() {
        // Weights decide what a frame counts for in the result. Whether a
        // sample is ordinary is a separate question, and the answer to it
        // must not move when the weights do.
        let (w, h) = (150usize, 150usize);
        let frames: Vec<FitsImage> = NIGHT
            .iter()
            .enumerate()
            .map(|(i, sigma)| {
                sky_frame_with((w, h), 1000.0, *sigma, 700 + i as u64, |p| {
                    p[75 * w + 20] += 5000.0 * (i % 3) as f64;
                })
            })
            .collect();
        let equal = [Alignment::still(); 10];
        let weighted: Vec<Alignment> = NIGHT
            .iter()
            .map(|sigma| Alignment {
                weight: weight_of(*sigma, Some(3.0)).expect("a weight"),
                ..Alignment::still()
            })
            .collect();
        assert_eq!(
            two_passes(&frames, &equal).1,
            two_passes(&frames, &weighted).1
        );
    }

    #[test]
    fn a_satellite_and_a_cosmic_ray_in_one_pixel_are_both_left_out() {
        // Two outliers at the same place, one of them in the noisiest frame.
        // Left out one at a time, each would widen the other's allowance
        // enough to be kept.
        let (w, h) = (120usize, 60usize);
        let frames: Vec<FitsImage> = NIGHT
            .iter()
            .enumerate()
            .map(|(i, sigma)| {
                sky_frame_with((w, h), 1000.0, *sigma, 900 + i as u64, |p| {
                    let bump = match i {
                        3 => 20_000.0,
                        8 => 300.0,
                        _ => 0.0,
                    };
                    for x in 10..110 {
                        p[30 * w + x] += bump;
                    }
                })
            })
            .collect();
        let (stacked, _) = two_passes(&frames, &[Alignment::still(); 10]);
        // Kept, the satellite would lift the row by two thousand and the
        // cosmic ray by thirty. The noise of this average is under one.
        let excess = row_excess(&stacked, 30, 10..110, 1000.0);
        assert!(excess.abs() < 5.0, "the row sits {excess:.1} above the sky");
    }

    #[test]
    fn a_faint_trail_across_the_quietest_frame_is_left_out() {
        // A hundred counts is eight deviations of that frame's noise and three
        // of the noisiest one's. It is judged by the noise of the frame it is in.
        let (w, h) = (120usize, 60usize);
        let frames: Vec<FitsImage> = NIGHT
            .iter()
            .enumerate()
            .map(|(i, sigma)| {
                sky_frame_with((w, h), 1000.0, *sigma, 950 + i as u64, |p| {
                    if i == 0 {
                        for x in 10..110 {
                            p[30 * w + x] += 100.0;
                        }
                    }
                })
            })
            .collect();
        let (stacked, _) = two_passes(&frames, &[Alignment::still(); 10]);
        let excess = row_excess(&stacked, 30, 10..110, 1000.0);
        assert!(
            excess.abs() < 3.0,
            "kept, it lifts the row by ten: {excess:.1}"
        );
    }

    #[test]
    fn a_resampled_frame_is_judged_by_the_noise_it_has_once_resampled() {
        // Read from exactly between four pixels, a frame keeps half its noise.
        // Judged by the noise it came with, the frames around it look
        // implausibly calm, and the one frame that was not resampled — the
        // reference — is left out wherever it differs from them.
        let (w, h) = (200usize, 200usize);
        let frames: Vec<FitsImage> = (0..10u64)
            .map(|i| sky_frame(w, h, 1000.0, 20.0, 40 + i))
            .collect();
        let placings: Vec<Alignment> = (0..10)
            .map(|i| {
                if i == 0 {
                    Alignment::still()
                } else {
                    Alignment {
                        dx: 0.5,
                        dy: 0.5,
                        rotation: 1e-9,
                        ..Alignment::still()
                    }
                }
            })
            .collect();
        let (_, lost) = two_passes(&frames, &placings);
        #[allow(clippy::cast_precision_loss)]
        let share = 100.0 * lost[0] as f64 / (w * h) as f64;
        assert!(share < 1.0, "the reference lost {share:.2}% of its samples");
    }

    #[test]
    fn a_frame_that_had_to_be_turned_is_judged_where_it_was_placed() {
        // The first pass resampled a turned frame and the second placed it to
        // the nearest pixel, so each sample was taken out of figures it never
        // went into. Both passes place it the same way now.
        let (w, h) = (200usize, 200usize);
        let frames: Vec<FitsImage> = (0..10u64)
            .map(|i| {
                sky_frame_with((w, h), 1000.0, 15.0, 60 + i, |p| {
                    if i == 4 {
                        for x in 20..180 {
                            p[100 * w + x] += 20_000.0;
                        }
                    }
                })
            })
            .collect();
        let placings: Vec<Alignment> = (0..10)
            .map(|i| Alignment {
                dx: 0.4,
                dy: -0.3,
                rotation: if i == 0 { 0.0 } else { 2e-4 },
                ..Alignment::still()
            })
            .collect();
        let (stacked, lost) = two_passes(&frames, &placings);

        // Away from the trail, pure noise loses what chance says.
        let quiet: usize = lost
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 4)
            .map(|(_, n)| n)
            .sum();
        #[allow(clippy::cast_precision_loss)]
        let share = 100.0 * quiet as f64 / (9 * w * h) as f64;
        assert!(share < 1.0, "{share:.2}% of pure noise rejected");

        // And the trail, spread across two rows by the resampling, is gone.
        for y in 98..=102 {
            let excess = row_excess(&stacked, y, 40..160, 1000.0);
            assert!(
                excess.abs() < 10.0,
                "row {y} sits {excess:.1} above the sky"
            );
        }
    }

    #[test]
    fn a_channel_s_noise_is_measured_through_gradients_and_stars() {
        let (w, h) = (400usize, 300usize);
        let truth = [5.0, 10.0, 20.0];
        let mut pixels = Vec::new();
        for (c, sigma) in truth.iter().enumerate() {
            let mut plane = gaussian_background(w, h, 1000.0, *sigma, 20 + c as u64);
            for y in 0..h {
                for x in 0..w {
                    #[allow(clippy::cast_precision_loss)]
                    let (fx, fy) = (x as f64, y as f64);
                    // Vignetting far steeper than any real frame's, and a
                    // bright field of stars.
                    plane[y * w + x] += fx * 50.0 + fy * 20.0 + (fx - 200.0).powi(2) * 0.01;
                    if (x % 40 < 3) && (y % 30 < 3) {
                        plane[y * w + x] += 30_000.0;
                    }
                }
            }
            pixels.extend(plane);
        }
        let spec = SyntheticSpec::new(w, h, -32).with_channels(3);
        let image = read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap();
        for (measured, truth) in channel_noise(&image).iter().zip(truth) {
            assert!(
                (measured - truth).abs() < truth * 0.05,
                "measured {measured:.2}, truly {truth}"
            );
        }
    }

    #[test]
    fn the_chances_agree_with_the_published_tables() {
        assert!((normal_tail(3.0) - 0.002_699_796).abs() < 1e-7);
        assert!((normal_tail(1.959_964) - 0.05).abs() < 1e-6);
        // Two-sided five per cent points of Student's t.
        for (dof, t) in [
            (1, 12.706_2),
            (2, 4.302_7),
            (3, 3.182_4),
            (4, 2.776_4),
            (5, 2.571_6),
            (10, 2.228_1),
            (30, 2.042_3),
            (120, 1.979_9),
        ] {
            let got = student_critical(dof, 0.05);
            assert!(
                (got - t).abs() < t * 1e-3,
                "{dof} degrees of freedom: {got} against {t}"
            );
        }
        // And at three deviations' chance, as rejection uses them.
        let chance = normal_tail(3.0);
        for (dof, t) in [(3, 9.22), (8, 4.28), (18, 3.48), (98, 3.08)] {
            let got = student_critical(dof, chance);
            assert!(
                (got - t).abs() < 0.01,
                "{dof} degrees of freedom: {got} against {t}"
            );
        }
        assert!((student_critical(1_000_000, chance) - 3.0).abs() < 0.002);
        assert!(student_critical(0, chance).is_infinite());
    }

    #[test]
    fn a_satellite_across_one_frame_is_left_out() {
        // What rejection is for. Averaged in, a trail across one frame in six
        // survives at a sixth of its brightness -- faint, obvious, and ruinous
        // on an image meant to show something fainter still.
        let (w, h) = (100usize, 100usize);
        let still = Alignment::still();
        let plain_frame = |seed: u64| {
            let pixels = gaussian_background(w, h, 1000.0, 15.0, seed);
            let spec = SyntheticSpec::new(w, h, -32);
            read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap()
        };
        let with_trail = |seed: u64| {
            let mut pixels = gaussian_background(w, h, 1000.0, 15.0, seed);
            for x in 10..90 {
                pixels[50 * w + x] += 20_000.0;
            }
            let spec = SyntheticSpec::new(w, h, -32);
            read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap()
        };

        // Six frames, one of them crossed by a satellite.
        let frames: Vec<FitsImage> = (0..6u64)
            .map(|i| {
                if i == 3 {
                    with_trail(90 + i)
                } else {
                    plain_frame(90 + i)
                }
            })
            .collect();

        let mut plain = Stack::new(w, h, 1);
        let mut first = Stack::rejecting(w, h, 1);
        for image in &frames {
            plain.add(image, still);
            first.add(image, still);
        }

        let mut second = first
            .into_rejecting(DEFAULT_CLIP)
            .expect("six frames is enough to clip against");
        for image in &frames {
            second.add(image, still);
        }

        let averaged = plain.finish(FitsHeader { cards: Vec::new() });
        let clipped = second.finish(FitsHeader { cards: Vec::new() });

        // The trail is a sixth of twenty thousand above the sky in the plain
        // average, and gone from the clipped one.
        let on_trail = |image: &FitsImage| f64::from(image.data[50 * w + 50]) - 1000.0;
        assert!(
            on_trail(&averaged) > 2500.0,
            "the plain average should carry the trail: {:.0}",
            on_trail(&averaged)
        );
        assert!(
            on_trail(&clipped) < 300.0,
            "the clipped stack should not: {:.0}",
            on_trail(&clipped)
        );
        assert!(
            second.rejected() >= 80,
            "{} samples left out",
            second.rejected()
        );
    }

    #[test]
    fn rejection_keeps_the_stars_it_is_meant_to_keep() {
        // A cut that removes the satellite and the stars with it would be no
        // use. Stars are in every frame, so nothing about them is an outlier.
        let (w, h) = (140usize, 140usize);
        let positions = field_positions()
            .into_iter()
            .filter(|(x, y)| *x < 120.0 && *y < 120.0)
            .collect::<Vec<_>>();
        let still = Alignment::still();

        let mut first = Stack::rejecting(w, h, 1);
        let frames: Vec<FitsImage> = (0..6u64).map(|i| frame(w, h, &positions, 60 + i)).collect();
        for image in &frames {
            first.add(image, still);
        }
        let mut second = first.into_rejecting(DEFAULT_CLIP).expect("enough frames");
        for image in &frames {
            second.add(image, still);
        }

        let params = DetectionParams::default();
        let before = detect(&frames[0], &params).count();
        let after = detect(&second.finish(FitsHeader { cards: Vec::new() }), &params).count();
        assert_eq!(after, before, "clipping must not eat the stars");
    }

    #[test]
    fn too_few_frames_to_clip_is_admitted_rather_than_guessed() {
        // The spread of four samples is too uncertain to clip against, and
        // clipping against it throws away as much good data as bad.
        let (w, h) = (40usize, 40usize);
        let mut stack = Stack::rejecting(w, h, 1);
        for seed in 0..4u64 {
            let pixels = gaussian_background(w, h, 500.0, 10.0, seed);
            let spec = SyntheticSpec::new(w, h, -32);
            let image = read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap();
            stack.add(&image, Alignment::still());
        }
        let Err(stack) = stack.into_rejecting(DEFAULT_CLIP) else {
            panic!("four frames must fall back on the plain average");
        };
        assert_eq!(stack.frames(), 4, "and the first pass comes back whole");
    }

    #[test]
    fn a_stack_that_was_not_asked_to_reject_cannot() {
        let mut stack = Stack::new(20, 20, 1);
        for seed in 0..8u64 {
            stack.add(&sky_frame(20, 20, 500.0, 10.0, seed), Alignment::still());
        }
        assert!(stack.into_rejecting(DEFAULT_CLIP).is_err());
    }

    #[test]
    fn a_frame_with_a_brighter_sky_is_not_thrown_away_by_the_rejection() {
        // The sky brightens and dims through a night: one of these frames sits
        // four hundred counts above the others, as a frame taken while the
        // moon was up does. That frame is out of keeping with the rest at
        // every pixel, so without something done about it the whole of it is
        // dropped from the result.
        let (w, h) = (80usize, 80usize);
        let still = Alignment::still();
        let sky = |level: f64, seed: u64| {
            let pixels = gaussian_background(w, h, level, 15.0, seed);
            let spec = SyntheticSpec::new(w, h, -32);
            read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap()
        };

        let frames: Vec<FitsImage> = (0..6u64)
            .map(|i| {
                if i == 5 {
                    sky(1400.0, 70 + i)
                } else {
                    sky(1000.0, 70 + i)
                }
            })
            .collect();

        // Every frame brought to the sky of the first before it is added.
        let reference = sky_level(&frames[0]);
        let placings: Vec<Alignment> = frames
            .iter()
            .map(|image| Alignment {
                offset: levelling(image, reference),
                ..still
            })
            .collect();

        let mut first = Stack::rejecting(w, h, 1);
        for (image, placing) in frames.iter().zip(&placings) {
            first.add(image, *placing);
        }
        let mut second = first.into_rejecting(DEFAULT_CLIP).expect("six frames");
        for (image, placing) in frames.iter().zip(&placings) {
            second.add(image, *placing);
        }

        // Nearly every pixel of the bright frame would be rejected, which is
        // one sixth of everything.
        let samples = w * h * frames.len();
        assert!(
            second.rejected() * 20 < samples,
            "{} samples of {samples} rejected: a whole frame has been lost",
            second.rejected()
        );

        // And the result sits at the sky it was levelled to, not somewhere
        // between the two.
        let stacked = second.finish(FitsHeader { cards: Vec::new() });
        let middle = f64::from(stacked.data[40 * w + 40]);
        assert!(
            (middle - reference).abs() < 30.0,
            "the stack should sit at the reference sky of {reference:.0}, not {middle:.0}"
        );
    }

    #[test]
    fn weighting_by_noise_beats_averaging_when_the_sky_brightens() {
        // A night whose sky brightens: the late frames are noisier and carry
        // less, and counting them equally throws away the advantage of the
        // early ones. The claim is that weighting recovers it.
        let (w, h) = (150usize, 150usize);
        let frames: Vec<(FitsImage, f64)> = (0..8u64)
            .map(|i| {
                // Noise from ten to eighty as the sky comes up.
                #[allow(clippy::cast_precision_loss)]
                let noise = 10.0 + i as f64 * 10.0;
                let pixels = gaussian_background(w, h, 1000.0, noise, 300 + i);
                let spec = SyntheticSpec::new(w, h, -32);
                (
                    read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap(),
                    noise,
                )
            })
            .collect();

        let spread_of = |image: &FitsImage| {
            #[allow(clippy::cast_precision_loss)]
            let n = image.data.len() as f64;
            let mean: f64 = image.data.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
            (image
                .data
                .iter()
                .map(|v| (f64::from(*v) - mean).powi(2))
                .sum::<f64>()
                / n)
                .sqrt()
        };

        let mut equal = Stack::new(w, h, 1);
        let mut weighted = Stack::new(w, h, 1);
        for (image, noise) in &frames {
            equal.add(image, Alignment::still());
            weighted.add(
                image,
                Alignment {
                    weight: weight_of(*noise, None).expect("a weight"),
                    ..Alignment::still()
                },
            );
        }

        let plain = spread_of(&equal.finish(FitsHeader { cards: Vec::new() }));
        let better = spread_of(&weighted.finish(FitsHeader { cards: Vec::new() }));
        assert!(
            better < plain * 0.8,
            "weighting should cut the noise well below {plain:.2}, got {better:.2}"
        );
    }

    #[test]
    fn a_weight_needs_something_to_judge_by() {
        assert!(weight_of(0.0, None).is_none(), "no noise, no weight");
        assert!(weight_of(f64::NAN, None).is_none());
        // A sharper frame is worth more than a soft one of the same quiet.
        let sharp = weight_of(10.0, Some(3.0)).expect("a weight");
        let soft = weight_of(10.0, Some(6.0)).expect("a weight");
        assert!(sharp > soft * 3.0, "{sharp} against {soft}");
    }

    #[test]
    fn a_frame_of_another_shape_is_refused() {
        // A different sensor, or a different binning, has no business in a
        // stack, and silently ignoring it would be worse than refusing.
        let mut stack = Stack::new(100, 100, 1);
        let wrong = frame(80, 80, &[(40.0, 40.0)], 41);
        let still = Alignment::still();
        assert!(!stack.add(&wrong, still));
        assert_eq!(stack.frames(), 0);
    }

    #[test]
    fn a_pixel_no_frame_covered_is_undefined_rather_than_black() {
        let (w, h) = (60usize, 60usize);
        let mut stack = Stack::new(w, h, 1);
        let image = frame(w, h, &[(30.0, 30.0)], 42);
        assert!(stack.add(
            &image,
            Alignment {
                dx: 10.0,
                ..Alignment::still()
            }
        ));

        let result = stack.finish(FitsHeader { cards: Vec::new() });
        // The frame's sky lies ten pixels right, so the stack reads past its
        // right edge and that strip is covered by nothing.
        assert!(
            result.data[30 * w + (w - 1)].is_nan(),
            "the strip no frame reached must be undefined, not zero"
        );
        assert!(result.data[30 * w + 30].is_finite());
    }
}

//! Finding and measuring stars.
//!
//! Phase 13's sharpness figure is a proxy: it finds gross problems and says so.
//! Stars are the real measurement. How many there are, how wide they are and
//! how round they are answer the questions an astrophotographer actually asks
//! of a frame, which is whether the focus drifted, whether the mount slipped,
//! and whether the seeing softened.
//!
//! **Most of the work here is rejection, not detection.** Anything bright
//! passes a threshold. Telling a star from a cosmic ray, a satellite trail, a
//! galaxy or a star cut in half by the frame edge is what makes the numbers
//! worth trusting.

use rayon::prelude::*;

use crate::background::BackgroundMap;
use crate::filter;
use crate::image::FitsImage;

/// How hard to look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectionParams {
    /// Deviations above the background a pixel must reach to be considered.
    pub threshold: f64,
    /// Smallest region worth calling a star, in pixels.
    ///
    /// Rejects single hot pixels and cosmic ray hits, which are the most common
    /// thing that is bright and is not a star.
    pub minimum_area: usize,
    /// Largest region worth calling a star.
    ///
    /// Rejects nebulosity and galaxies. It has to be generous: a bright star
    /// at four pixels of standard deviation covers some six hundred pixels
    /// above the threshold, and rejecting it would mean measuring only the
    /// faint stars and reporting a frame as sharper than it is.
    pub maximum_area: usize,
    /// Longest a region may be relative to its width and still be a star.
    ///
    /// Measured on the region's own bounding box rather than on its moments,
    /// because the moment window is bounded and a long trail truncated by it
    /// looks far rounder than it is.
    ///
    /// Satellite trails and aeroplane lights run tens of times longer than they
    /// are wide. Poor tracking does not: it stretches every star by a factor of
    /// two or three, and that is a measurement worth reporting rather than a
    /// detection to discard.
    pub maximum_elongation: f64,
    /// Most stars to keep: the brightest this many.
    ///
    /// Applied after measuring, so that rejected regions do not eat into it
    /// and the stars kept are spread over the whole frame rather than taken
    /// from wherever the scan happened to begin.
    pub limit: usize,
    /// Width of the blur applied before thresholding, in pixels.
    ///
    /// The matched filter. About the size of a star is right: it adds a star's
    /// own pixels together while averaging the noise down, so the threshold
    /// can be stated in deviations and mean something. Zero turns it off,
    /// which makes detection cheaper and much more sensitive to noise.
    pub smoothing: f64,
}

impl Default for DetectionParams {
    fn default() -> Self {
        Self {
            threshold: 5.0,
            minimum_area: 4,
            maximum_area: 2_000,
            maximum_elongation: 5.0,
            // High enough that it does not bind on an ordinary frame: a
            // 61-megapixel broadband exposure holds about ten thousand stars,
            // and a count that saturates cannot be compared with the next
            // frame's, which is most of what the count is for.
            limit: 20_000,
            smoothing: 1.2,
        }
    }
}

/// A fraction of the frame above threshold beyond which grouping is not worth
/// attempting at that threshold.
///
/// This is a guard on cost, not a judgement about the frame. A flat, a badly
/// overexposed frame or one full of cloud can put most of its pixels over any
/// threshold, and joining ten million of them into regions takes seconds and
/// answers nothing.
///
/// A rich broadband frame can cross it honestly: a five-minute luminance
/// exposure of a Milky Way field has nebulosity and thousands of stars, and a
/// fifth of it stands above five deviations. Such a frame is not a failure, so
/// the threshold is raised until the work is bounded rather than the frame
/// being reported as empty.
const MAX_BRIGHT_FRACTION: f64 = 0.20;

/// Most regions to measure, however many the frame holds.
///
/// A guard on cost rather than a choice about the answer: measuring is
/// parallel and cheap per star, but a frame of pure gradient can produce
/// millions of regions, and there is no sense measuring those.
const MAX_CANDIDATES: usize = 200_000;

/// How far the threshold may be raised when a frame is too bright to group.
///
/// Eight times the asked-for threshold is enough for the brightest real frame
/// tested and still finite; past that the frame is a flat or a fog, and there
/// is nothing to find.
const MAX_THRESHOLD_SCALE: f64 = 8.0;

/// One detected star.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Star {
    /// Centroid column, in image pixels, to sub-pixel precision.
    pub x: f64,
    /// Centroid row, in the frame's own storage order.
    pub y: f64,
    /// Total signal above the background.
    pub flux: f64,
    /// Full width at half maximum, in pixels.
    pub fwhm: f64,
    /// 1.0 is circular; a trailed star tends towards 0.
    pub roundness: f64,
    /// Whether the peak is flat, meaning the star ran out of range.
    pub saturated: bool,
    /// How far the brightest pixel stood above the sky.
    ///
    /// What decides whether a width measured from this star means anything. A
    /// star barely above the noise has its half-maximum contour sitting in the
    /// noise, so the region stops early and the width comes out too small.
    pub peak: f64,
}

/// What a frame's stars say about it.
#[derive(Debug, Clone, PartialEq)]
pub struct StarField {
    /// Every star that survived the filters.
    pub stars: Vec<Star>,
    /// Median width, over the unsaturated stars.
    ///
    /// `None` when too few were found for a median to mean anything.
    pub fwhm: Option<f64>,
    /// Median roundness, over the unsaturated stars.
    pub roundness: Option<f64>,
    /// How many ran out of range.
    pub saturated: usize,
    /// Whether more stars were found than the limit allowed, so that only the
    /// brightest are reported.
    ///
    /// A capped count says "at least this many" and cannot be compared with a
    /// neighbouring frame's, which is what counts are mostly used for.
    pub capped: bool,
    /// What the threshold had to be multiplied by to make the frame
    /// searchable.
    ///
    /// One for almost every frame. Above one when the frame was so bright that
    /// grouping at the asked-for threshold would have cost seconds — a rich
    /// broadband exposure does this honestly — and the figures then describe
    /// the brighter stars only. Worth saying rather than hiding, because it
    /// explains a count that cannot be compared with a neighbouring frame's.
    pub threshold_scale: f64,
}

impl Default for StarField {
    /// Nothing found, at the threshold that was asked for.
    fn default() -> Self {
        Self {
            stars: Vec::new(),
            fwhm: None,
            roundness: None,
            saturated: 0,
            threshold_scale: 1.0,
            capped: false,
        }
    }
}

impl StarField {
    /// Whether the frame was too bright to search at the threshold asked for.
    #[must_use]
    pub fn threshold_was_raised(&self) -> bool {
        self.threshold_scale > 1.0
    }

    /// How many stars were found.
    #[must_use]
    pub fn count(&self) -> usize {
        self.stars.len()
    }

    /// Whether nothing was found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stars.is_empty()
    }
}

/// How far above the noise a star must stand for its width to be believed.
///
/// The width comes from where the profile falls to half its height, and for a
/// star ten times the noise that half-height is five times the noise — a level
/// the noise itself crosses often enough to stop the measurement early and
/// report the star as narrower than it is. A deep stack finds tens of
/// thousands of such stars, and taking a median over all of them said the
/// stack was sharper than any frame that went into it, which cannot be.
///
/// A hundred puts the half-height fifty deviations up, where noise does not
/// reach at all. It was set at twenty first, which is enough for a single
/// frame — the answer there does not move between twenty and four hundred —
/// but not for a stack of three dozen, where the measured width was still
/// climbing at a hundred: a deep stack finds so many faint stars that they
/// carry the median on their own.
const MINIMUM_PEAK_FOR_WIDTH: f64 = 100.0;

/// Fewer stars than this and a median says more about luck than the frame.
const MINIMUM_FOR_A_MEDIAN: usize = 3;

/// Finds and measures the stars in a frame.
///
/// The frame should be calibrated but **not** debayered: a colour mosaic has
/// its own entry point in [`detect_mosaic`], because every star in a mosaic
/// would otherwise be found four times, once per filter site.
#[must_use]
pub fn detect(image: &FitsImage, params: &DetectionParams) -> StarField {
    if image.width < 3 || image.height < 3 {
        return StarField::default();
    }
    // A colour image — a stacked result, or a frame from a camera that writes
    // three planes — is searched on its luminance. Running on one plane would
    // throw away two thirds of the signal, and running on all three would find
    // every star three times.
    if image.channels == 3 {
        let Some(grey) = luminance(image) else {
            return StarField::default();
        };
        return detect(&grey, params);
    }
    if image.channels != 1 {
        return StarField::default();
    }

    // The frame the threshold is applied to: blurred, so that a star's pixels
    // are added together and the noise is averaged down. Stars are measured on
    // the original further below, because a blur widens whatever it touches.
    let filtered = if params.smoothing > 0.0 {
        Some(filter::gaussian_blur(image, params.smoothing))
    } else {
        None
    };
    let searched = match filtered.as_ref() {
        Some(data) => &FitsImage {
            width: image.width,
            height: image.height,
            channels: 1,
            data: data.clone(),
            header: image.header.clone(),
            min: image.min,
            max: image.max,
        },
        None => image,
    };

    // The background tile by tile rather than once for the frame, so that a
    // light pollution gradient does not put one side of the frame over the
    // threshold everywhere at once.
    //
    // Measured on the frame as it came, never on the blurred one: see
    // `filter::noise_attenuation`. A normalised blur leaves the background
    // where it was and shrinks the noise by a known factor, so the threshold
    // is scaled by that factor instead of being measured again.
    let sky = BackgroundMap::measure_level_and_noise(searched, image);
    let (_, noise) = sky.typical();
    if noise <= 0.0 {
        // No noise means no scale to threshold against.
        return StarField::default();
    }
    let base = params.threshold * filter::noise_attenuation(params.smoothing);

    // A frame too bright to group at the asked-for threshold is searched at a
    // higher one rather than reported as empty. Each attempt costs only the
    // counting pass, which stops before anything is collected.
    let mut scale = 1.0f64;
    let bright = loop {
        match bright_pixels(searched, &sky, base * scale) {
            Some(bright) => break bright,
            None if scale < MAX_THRESHOLD_SCALE => scale *= 2.0,
            None => {
                log::debug!("nothing to find: bright at every threshold tried");
                return StarField::default();
            }
        }
    };
    let regions = group(&bright, image.width);

    // The cheap rejections first, in one pass, so that the expensive
    // measurement is only ever done on regions that could be stars. Taking
    // them in order keeps the result the same from one run to the next when
    // the limit bites.
    let candidates: Vec<&[usize]> = regions
        .iter()
        .filter(|pixels| {
            pixels.len() >= params.minimum_area
                && pixels.len() <= params.maximum_area
                // Half a star has a shape that is a lie.
                && !touches_edge(pixels, image.width, image.height)
                // A line, not a point: a satellite or an aeroplane.
                && elongation(pixels, image.width) <= params.maximum_elongation
        })
        .take(MAX_CANDIDATES)
        .collect();

    // Each star is measured from its own window and nothing is shared between
    // them, so this is the one part of the work that divides perfectly.
    let mut stars: Vec<Star> = candidates
        .par_iter()
        .filter_map(|pixels| {
            // The background under this star, not the frame's average: on a
            // frame with a gradient they differ by more than a faint star is
            // worth.
            let first = pixels[0];
            let (local, _) = sky.at(first % image.width, first / image.width);
            measure_star(image, pixels, f64::from(local))
        })
        .collect();

    // The brightest, when there are more than asked for. Sorting on flux
    // rather than stopping at the first `limit` found is what keeps the answer
    // from describing one corner of the frame.
    let capped = stars.len() > params.limit;
    if capped {
        stars.sort_unstable_by(|a, b| b.flux.total_cmp(&a.flux));
        stars.truncate(params.limit);
    }

    summarise(stars, noise, scale, capped)
}

/// Finds and measures the stars in a one-shot colour mosaic.
///
/// Detection runs on the green sites, which are half the pixels of a Bayer
/// sensor and the closest thing to luminance it offers. Positions and widths
/// are scaled back to the mosaic's own coordinates, so a caller sees the frame
/// it gave.
#[must_use]
pub fn detect_mosaic(
    image: &FitsImage,
    pattern: crate::debayer::BayerPattern,
    params: &DetectionParams,
) -> StarField {
    let Some(green) = green_channel(image, pattern) else {
        return StarField::default();
    };
    // The green image is half size, so a star there is half as wide, and the
    // area thresholds have to come down with it.
    let scaled = DetectionParams {
        minimum_area: (params.minimum_area / 4).max(2),
        maximum_area: (params.maximum_area / 4).max(4),
        ..*params
    };

    let mut field = detect(&green, &scaled);
    for star in &mut field.stars {
        star.x = star.x * 2.0 + 0.5;
        star.y = star.y * 2.0 + 0.5;
    }

    // The widths are measured again at full resolution, where the sensor
    // actually recorded them.
    //
    // Doubling the width found on the half-size green image is arithmetically
    // right and practically wrong. A star three pixels across on the sensor is
    // barely one and a half on that image, which is below what a grid can
    // describe: its half-maximum region comes to one or two pixels, fewer than
    // a width is measured from, so those stars are dropped and only the broad
    // ones are left to take a median of. Measured that way a real frame read
    // 5.05 pixels where its own pixels, read off by hand, say 3.5.
    // The sky the widths are measured against comes from the green image that
    // found the stars, not from the mosaic as a whole: green pixels sit above
    // red and blue ones, and a level taken across all three would be too low.
    let (background, noise) = crate::quality::background_and_noise(&green);
    remeasure_widths(&mut field, image, pattern, background, noise);
    field
}

/// The middle value of a list, or nothing when there are too few to mean
/// anything.
fn middle_value(values: &mut [f64]) -> Option<f64> {
    if values.len() < MINIMUM_FOR_A_MEDIAN {
        return None;
    }
    let middle = values.len() / 2;
    values.select_nth_unstable_by(middle, |a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });
    Some(values[middle])
}

/// Measures each star's width again, on the sensor's own green pixels.
///
/// Half the pixels of a colour sensor are green, in a chequer, so they sample
/// the sky at the sensor's full spacing along the diagonals. Growing the
/// half-maximum region over those pixels alone measures the star where it was
/// actually recorded: no interpolation, and no filter pattern in the way.
/// Each green pixel stands for two of the sensor's, which is how the count
/// becomes an area.
fn remeasure_widths(
    field: &mut StarField,
    image: &FitsImage,
    pattern: crate::debayer::BayerPattern,
    background: f64,
    noise: f64,
) {
    // A green pixel is read as it stands; anything else takes the mean of its
    // green neighbours. Worked out where it is asked for rather than across
    // the frame, since only a few hundred places around each star are.
    let green = |x: usize, y: usize| green_value(image, pattern, x, y);
    let size = (image.width, image.height);
    let widths: Vec<f64> = field
        .stars
        .par_iter_mut()
        .filter_map(|star| {
            let fwhm = width_near(&green, size, star.x, star.y, background)?;
            star.fwhm = fwhm;
            // The same rule as everywhere else: a star too near the noise has
            // its half-height in the noise, and its width is not evidence.
            let solid = star.peak >= MINIMUM_PEAK_FOR_WIDTH * noise;
            (solid && !star.saturated).then_some(fwhm)
        })
        .collect();

    let mut widths = widths;
    field.fwhm = middle_value(&mut widths);
}

/// A green pixel as it stands, or the mean of the green pixels around it.
///
/// Half a colour sensor's pixels are green, in a chequer; the rest take the
/// mean of their four green neighbours. That samples the sky at the sensor's
/// own spacing with no filter pattern left in it, which is what a width has to
/// be measured on: reading the green pixels alone means stepping along the
/// diagonals at one and a half pixels a time, and a star three pixels across
/// is then measured too coarsely to be measured well. Read that way a real
/// star came to 3.5 pixels where every-pixel sampling of the same star said
/// 3.05.
fn green_value(
    image: &FitsImage,
    pattern: crate::debayer::BayerPattern,
    x: usize,
    y: usize,
) -> Option<f64> {
    use crate::debayer::Colour;
    if x >= image.width || y >= image.height {
        return None;
    }
    if pattern.colour_at(x, y) == Colour::Green {
        let value = f64::from(image.data[y * image.width + x]);
        return value.is_finite().then_some(value);
    }

    let mut total = 0.0f64;
    let mut count = 0u8;
    for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
        let (nx, ny) = (x as i64 + dx, y as i64 + dy);
        if nx < 0 || ny < 0 {
            continue;
        }
        #[allow(clippy::cast_sign_loss)]
        let (nx, ny) = (nx as usize, ny as usize);
        if nx >= image.width || ny >= image.height {
            continue;
        }
        let value = f64::from(image.data[ny * image.width + nx]);
        if value.is_finite() {
            total += value;
            count += 1;
        }
    }
    (count > 0).then(|| total / f64::from(count))
}

/// The width of the star near `(x, y)`, measured from its own peak.
///
/// The values come from a sampler rather than an array, so that a colour
/// sensor's green pixels can have their gaps filled where the walk actually
/// looks — a few hundred places around each star — instead of across the
/// whole frame. Filling the frame cost seventy per cent of the time detection
/// takes on a 61-megapixel image, for values that were never read.
fn width_near(
    value: &impl Fn(usize, usize) -> Option<f64>,
    size: (usize, usize),
    x: f64,
    y: f64,
    background: f64,
) -> Option<f64> {
    let (width, height) = size;
    if x < 0.0 || y < 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (cx, cy) = (x.round() as usize, y.round() as usize);
    if cx + 4 >= width || cy + 4 >= height || cx < 4 || cy < 4 {
        return None;
    }

    // The brightest pixel near where the star was found, not the pixel its
    // centroid happens to land on: the width is measured against half the
    // height, so starting anywhere dimmer sets the mark too low.
    let (mut peak, mut peak_at) = (f64::NEG_INFINITY, (cx, cy));
    for ny in cy - 3..=cy + 3 {
        for nx in cx - 3..=cx + 3 {
            if let Some(found) = value(nx, ny) {
                if found > peak {
                    peak = found;
                    peak_at = (nx, ny);
                }
            }
        }
    }
    let height_above_sky = peak - background;
    if !height_above_sky.is_finite() || height_above_sky <= 0.0 {
        return None;
    }
    crossing_width(value, size, peak_at, background, height_above_sky)
}

/// One plane holding the brightness of a three-plane colour image./// One plane holding the brightness of a three-plane colour image.
///
/// The plain mean of the three, not a weighted luminance: the weights that
/// suit human vision are wrong for a telescope, where a red star's photons
/// count as much as a green one's.
fn luminance(image: &FitsImage) -> Option<FitsImage> {
    let pixels = image.width.checked_mul(image.height)?;
    if image.data.len() < pixels * 3 {
        return None;
    }
    let (red, green, blue) = (
        &image.data[0..pixels],
        &image.data[pixels..pixels * 2],
        &image.data[pixels * 2..pixels * 3],
    );

    let mut data = vec![f32::NAN; pixels];
    data.par_iter_mut()
        .zip(red.par_iter().zip(green).zip(blue))
        .for_each(|(out, ((r, g), b))| {
            let mut total = 0.0f32;
            let mut count = 0u8;
            for value in [*r, *g, *b] {
                if value.is_finite() {
                    total += value;
                    count += 1;
                }
            }
            if count > 0 {
                *out = total / f32::from(count);
            }
        });

    let (min, max) = crate::image::finite_min_max(&data);
    Some(FitsImage {
        width: image.width,
        height: image.height,
        channels: 1,
        data,
        header: image.header.clone(),
        min,
        max,
    })
}

/// A half-size image of the green sites of a mosaic.
///
/// Each 2x2 tile has two greens; averaging them is both a reasonable estimate
/// and a small amount of noise reduction.
fn green_channel(image: &FitsImage, pattern: crate::debayer::BayerPattern) -> Option<FitsImage> {
    use crate::debayer::Colour;
    if image.channels != 1 || image.width < 2 || image.height < 2 {
        return None;
    }
    let (width, height) = (image.width / 2, image.height / 2);
    if width == 0 || height == 0 {
        return None;
    }

    let mut data = vec![f32::NAN; width * height];
    // A row of the half-size image reads two rows of the mosaic, and no row
    // depends on another.
    data.par_chunks_mut(width).enumerate().for_each(|(y, out)| {
        for (x, slot) in out.iter_mut().enumerate() {
            let mut total = 0.0f64;
            let mut count = 0u32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let (sx, sy) = (x * 2 + dx, y * 2 + dy);
                    if pattern.colour_at(sx, sy) != Colour::Green {
                        continue;
                    }
                    let value = image.data[sy * image.width + sx];
                    if value.is_finite() {
                        total += f64::from(value);
                        count += 1;
                    }
                }
            }
            if count > 0 {
                #[allow(clippy::cast_possible_truncation)]
                {
                    *slot = (total / f64::from(count)) as f32;
                }
            }
        }
    });

    let (min, max) = crate::image::finite_min_max(&data);
    Some(FitsImage {
        width,
        height,
        channels: 1,
        data,
        header: image.header.clone(),
        min,
        max,
    })
}

/// The indices of every pixel above the threshold.
///
/// Returns `None` when so much of the frame is bright that it cannot be a star
/// field, which saves labelling a flat or a cloud-ruined frame pixel by pixel.
fn bright_pixels(image: &FitsImage, sky: &BackgroundMap, deviations: f64) -> Option<Vec<usize>> {
    let cap = {
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        {
            (image.data.len() as f64 * MAX_BRIGHT_FRACTION) as usize
        }
    };

    // Counted before anything is collected. Counting is the same scan without
    // the allocation, and it keeps the bail-out honest: a flat, or a frame
    // full of cloud, can put millions of pixels over any threshold, and
    // building that list before noticing would cost more memory than the
    // image.
    let width = image.width.max(1);
    let bright_count: usize = image
        .data
        .par_chunks(width)
        .enumerate()
        .map(|(y, row)| {
            let mut thresholds = Vec::new();
            sky.row_thresholds(y, deviations, row.len(), &mut thresholds);
            row.iter()
                .zip(&thresholds)
                .filter(|(v, t)| v.is_finite() && *v > t)
                .count()
        })
        .sum();
    if bright_count > cap {
        log::debug!("more than {MAX_BRIGHT_FRACTION} of the frame is bright; not a star field");
        return None;
    }

    // Rayon's `collect` keeps the order of an ordered iterator, and the
    // grouping below depends on the indices ascending.
    Some(
        image
            .data
            .par_chunks(width)
            .enumerate()
            .flat_map_iter(move |(y, row)| {
                let mut thresholds = Vec::new();
                sky.row_thresholds(y, deviations, row.len(), &mut thresholds);
                row.iter()
                    .zip(thresholds)
                    .enumerate()
                    .filter(|(_, (v, t))| v.is_finite() && **v > *t)
                    .map(move |(x, _)| y * width + x)
            })
            .collect(),
    )
}

/// Bright pixels arranged so that each region's own are next to each other.
///
/// One allocation rather than a vector per region, which matters when a rich
/// field holds thousands of them.
struct Regions {
    /// Every bright pixel, region by region.
    order: Vec<usize>,
    /// Where each region begins in `order`, with a final entry at the end.
    starts: Vec<usize>,
}

impl Regions {
    /// Each region's pixels, in ascending index order.
    fn iter(&self) -> impl Iterator<Item = &[usize]> {
        self.starts.windows(2).map(|w| &self.order[w[0]..w[1]])
    }
}

/// Groups bright pixels into connected regions, eight-connected.
///
/// Works over the sparse set rather than the whole frame: a star field puts a
/// fraction of a percent of its pixels above threshold, so this is thousands of
/// steps rather than tens of millions.
///
/// The neighbours of a pixel are found by walking the previous row in step with
/// the current one rather than by looking each one up in a map. Both are
/// sorted, so the walk never goes backwards, and the pass costs a couple of
/// comparisons per pixel instead of a hash. Collecting the results is a
/// counting sort over the roots for the same reason: the roots are small
/// integers, so nothing needs hashing at all.
fn group(bright: &[usize], width: usize) -> Regions {
    let n = bright.len();
    let mut parent: Vec<usize> = (0..n).collect();

    // Rows are contiguous runs, because the indices ascend.
    let mut row_start = 0usize;
    let mut previous: std::ops::Range<usize> = 0..0;
    let mut previous_row: Option<usize> = None;

    while row_start < n {
        let y = bright[row_start] / width;
        let mut row_end = row_start + 1;
        while row_end < n && bright[row_end] < (y + 1) * width {
            row_end += 1;
        }

        // The first row has nothing above it, and `None == None` would say it
        // did.
        let above = y > 0 && previous_row == Some(y - 1);
        let mut cursor = previous.start;
        for slot in row_start..row_end {
            let index = bright[slot];
            let x = index - y * width;

            // The pixel to the left, when there is one and it really is to the
            // left rather than the last pixel of the row above.
            if x > 0 && slot > row_start && bright[slot - 1] + 1 == index {
                union(&mut parent, slot, slot - 1);
            }

            if !above {
                continue;
            }
            // The three above, clamped so that column 0 is never joined to the
            // last column of the row above.
            let lo = index - width - usize::from(x > 0);
            let hi = index - width + usize::from(x + 1 < width);
            while cursor < previous.end && bright[cursor] < lo {
                cursor += 1;
            }
            let mut peek = cursor;
            while peek < previous.end && bright[peek] <= hi {
                union(&mut parent, slot, peek);
                peek += 1;
            }
        }

        previous = row_start..row_end;
        previous_row = Some(y);
        row_start = row_end;
    }

    // A counting sort over the roots. Regions come out ordered by their first
    // pixel, so the same frame always yields the same stars in the same order,
    // which a hash map's iteration order did not guarantee.
    let mut count = vec![0usize; n];
    let roots: Vec<usize> = (0..n)
        .map(|slot| {
            let root = find(&mut parent, slot);
            count[root] += 1;
            root
        })
        .collect();

    let mut starts = Vec::new();
    let mut offset = vec![0usize; n];
    let mut total = 0usize;
    for (root, size) in count.iter().enumerate() {
        if *size > 0 {
            offset[root] = total;
            starts.push(total);
            total += size;
        }
    }
    starts.push(total);

    let mut order = vec![0usize; n];
    for (slot, root) in roots.iter().enumerate() {
        order[offset[*root]] = bright[slot];
        offset[*root] += 1;
    }

    Regions { order, starts }
}

/// Union-find root, with path compression.
fn find(parent: &mut [usize], mut slot: usize) -> usize {
    while parent[slot] != slot {
        parent[slot] = parent[parent[slot]];
        slot = parent[slot];
    }
    slot
}

/// Joins two regions.
fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        parent[ra] = rb;
    }
}

/// How long a region is relative to its width, from its bounding box.
fn elongation(pixels: &[usize], width: usize) -> f64 {
    let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0usize, usize::MAX, 0usize);
    for index in pixels {
        let (x, y) = (index % width, index / width);
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    #[allow(clippy::cast_precision_loss)]
    let (w, h) = ((x1 - x0 + 1) as f64, (y1 - y0 + 1) as f64);
    w.max(h) / w.min(h).max(1.0)
}

/// Whether a region touches the frame's border.
fn touches_edge(pixels: &[usize], width: usize, height: usize) -> bool {
    pixels.iter().any(|index| {
        let (x, y) = (index % width, index / width);
        x == 0 || y == 0 || x + 1 >= width || y + 1 >= height
    })
}

/// Measures one region.
///
/// The centroid comes from the thresholded pixels, which is where the signal
/// is. The width comes from a window around it, including pixels **below** the
/// threshold: measuring only what crosses the threshold would clip the wings of
/// every star and report them all as narrower than they are.
fn measure_star(image: &FitsImage, pixels: &[usize], background: f64) -> Option<Star> {
    let width = image.width;

    let mut flux = 0.0f64;
    let mut sum_x = 0.0f64;
    let mut sum_y = 0.0f64;
    let mut peak = f64::NEG_INFINITY;
    let mut peak_at = 0usize;
    let mut at_peak = 0usize;

    for index in pixels {
        let value = f64::from(image.data[*index]) - background;
        if value <= 0.0 {
            continue;
        }
        let (x, y) = ((index % width) as f64, (index / width) as f64);
        flux += value;
        sum_x += x * value;
        sum_y += y * value;

        if value > peak {
            peak = value;
            peak_at = *index;
            at_peak = 1;
        } else if (value - peak).abs() < f64::EPSILON {
            at_peak += 1;
        }
    }
    if flux <= 0.0 {
        return None;
    }

    let (cx, cy) = (sum_x / flux, sum_y / flux);

    // A flat top means the star ran out of range: its width is understated and
    // its centroid unreliable, so it is counted but not measured.
    let saturated = at_peak >= 3;

    // How much the brightest pixel stands above the ring around it. A star is
    // spread across many pixels by the atmosphere and the optics, so its
    // neighbours are nearly as bright as its centre; a hot pixel or a cosmic
    // ray has everything in one pixel and almost nothing beside it.
    //
    // The blur that makes faint stars detectable also spreads a single hot
    // pixel into a blob several pixels across, which is enough to pass a test
    // on area alone. On a frame taken with the cover on, that turned tens of
    // thousands of hot pixels into reported stars. This is measured on the
    // frame as it came, where a spike is still a spike.
    if !saturated && ring_fraction(image, peak_at, background) < MINIMUM_RING_FRACTION {
        return None;
    }

    // Outwards from the peak to where the profile falls through half its
    // height, interpolating between pixels.
    //
    // Counting the pixels above half instead was tried, and cannot see finer
    // than the grid: a region of twelve pixels says the radius is 1.95 whether
    // it is 1.6 or 2.2, and a well focused star covers few enough pixels for
    // that to be the largest error in the answer. Interpolating is what a
    // person does reading the numbers off a frame, and it agrees with them
    // where counting did not.
    //
    // Second moments over a window were tried before either, and were worse
    // again: they measured the window rather than the star.
    // A star whose edge cannot be found — because it runs off the frame, or
    // into a neighbour — is still a star. It is counted and left unmeasured,
    // as a saturated one is, rather than being made to disappear.
    let plain = |x: usize, y: usize| {
        let value = f64::from(image.data[y * image.width + x]);
        value.is_finite().then_some(value)
    };
    let fwhm = crossing_width(
        &plain,
        (image.width, image.height),
        (peak_at % image.width, peak_at / image.width),
        background,
        peak,
    )
    .unwrap_or(f64::NAN);

    // Roundness from the whole detected region rather than from the few pixels
    // above half the peak. Shape needs area: the half-maximum region of a well
    // sampled star is three or four pixels, and the shape of three pixels is
    // the shape of the grid they sit on, which came out the same for every
    // star on every frame. The detected region is ten times that and stretches
    // with the star when the mount slips, which is the thing worth seeing.
    let roundness = region_roundness(pixels, image.width);

    Some(Star {
        x: cx,
        y: cy,
        flux,
        fwhm,
        roundness,
        saturated,
        peak,
    })
}

/// Least of its peak that a star's immediate neighbours must carry.
///
/// The atmosphere and the optics spread a star over many pixels, so the ring
/// around its centre holds nearly as much as the centre does — nine tenths for
/// a well sampled star. A hot pixel or a cosmic ray has nothing beside it.
///
/// Stated as the ring over the peak rather than the peak over the ring, which
/// is the same test but for the arithmetic: the peak of a detected star is a
/// large, well determined number, while the ring of a faint one sits close to
/// the background, and dividing by it turned ordinary noise into an enormous
/// ratio. That rejected the faint stars the matched filter exists to find —
/// nine detections in ten on a narrowband frame.
const MINIMUM_RING_FRACTION: f64 = 0.3;

/// How much of the peak the eight pixels around it carry, above the
/// background. Near one for anything the sky produced, near zero for a defect.
fn ring_fraction(image: &FitsImage, peak_at: usize, background: f64) -> f64 {
    let (width, height) = (image.width, image.height);
    let (x, y) = (peak_at % width, peak_at / width);
    if x == 0 || y == 0 || x + 1 >= width || y + 1 >= height {
        // Nothing to compare against; the edge filter has this case anyway.
        return 1.0;
    }

    let mut total = 0.0f64;
    let mut count = 0u32;
    for dy in [-1i64, 0, 1] {
        for dx in [-1i64, 0, 1] {
            if dx == 0 && dy == 0 {
                continue;
            }
            #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
            let index = ((y as i64 + dy) as usize) * width + (x as i64 + dx) as usize;
            let value = f64::from(image.data[index]) - background;
            if value.is_finite() {
                total += value;
                count += 1;
            }
        }
    }
    if count == 0 {
        return 1.0;
    }

    let centre = f64::from(image.data[peak_at]) - background;
    if centre <= 0.0 {
        // Not above the background at all on the frame as it came.
        return 0.0;
    }
    (total / f64::from(count)) / centre
}

/// The width of a star, from where its profile falls to half its height.
///
/// Walks out from the brightest pixel along the four directions of the grid,
/// finds where each crosses half the height above the sky by straight-line
/// interpolation between the two pixels either side, and takes twice the mean
/// of those four distances.
///
/// `None` when fewer than three of the four find an edge, which means the star
/// runs off the frame or into a neighbour and its width would be a guess.
fn crossing_width(
    value: &impl Fn(usize, usize) -> Option<f64>,
    (width, height): (usize, usize),
    peak_at: (usize, usize),
    background: f64,
    peak: f64,
) -> Option<f64> {
    let (px, py) = peak_at;
    let level = background + peak / 2.0;

    let mut radii = Vec::with_capacity(4);
    for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
        let mut previous = background + peak;
        let mut crossing = None;
        for step in 1..=CROSSING_STEPS {
            #[allow(clippy::cast_possible_wrap)]
            let (nx, ny) = (px as i64 + dx * step as i64, py as i64 + dy * step as i64);
            if nx < 0 || ny < 0 {
                break;
            }
            #[allow(clippy::cast_sign_loss)]
            let (nx, ny) = (nx as usize, ny as usize);
            if nx >= width || ny >= height {
                break;
            }
            let Some(value) = value(nx, ny) else { break };

            if value < level {
                let span = previous - value;
                let fraction = if span > 0.0 {
                    (previous - level) / span
                } else {
                    0.5
                };
                #[allow(clippy::cast_precision_loss)]
                {
                    crossing = Some((step - 1) as f64 + fraction);
                }
                break;
            }
            previous = value;
        }
        if let Some(radius) = crossing {
            radii.push(radius);
        }
    }

    if radii.len() < 3 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let mean = radii.iter().sum::<f64>() / radii.len() as f64;
    (mean > 0.0).then_some(mean * 2.0)
}

/// How far from a peak a star's edge is looked for, in pixels.
const CROSSING_STEPS: usize = 30;

fn region_roundness(pixels: &[usize], width: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let count = pixels.len() as f64;
    if count < 2.0 {
        return 0.0;
    }

    let (mut sum_x, mut sum_y) = (0.0f64, 0.0f64);
    for index in pixels {
        #[allow(clippy::cast_precision_loss)]
        {
            sum_x += (index % width) as f64;
            sum_y += (index / width) as f64;
        }
    }
    let (cx, cy) = (sum_x / count, sum_y / count);

    let (mut xx, mut yy, mut xy) = (0.0f64, 0.0f64, 0.0f64);
    for index in pixels {
        #[allow(clippy::cast_precision_loss)]
        let (dx, dy) = ((index % width) as f64 - cx, (index / width) as f64 - cy);
        xx += dx * dx;
        yy += dy * dy;
        xy += dx * dy;
    }
    let (xx, yy, xy) = (xx / count, yy / count, xy / count);

    let difference = ((xx - yy).powi(2) + 4.0 * xy * xy).sqrt();
    let major = (xx + yy + difference) / 2.0;
    let minor = (xx + yy - difference) / 2.0;
    if major > 0.0 {
        (minor.max(0.0) / major).sqrt()
    } else {
        0.0
    }
}

/// Turns a list of stars into the summary a frame is judged by.
fn summarise(stars: Vec<Star>, noise: f64, threshold_scale: f64, capped: bool) -> StarField {
    let saturated = stars.iter().filter(|s| s.saturated).count();

    // Only stars with signal enough for a width to mean anything. See
    // `MINIMUM_PEAK_FOR_WIDTH`: without this a deep stack reports itself
    // sharper than the frames it was made from.
    let solid = |s: &&Star| s.peak >= MINIMUM_PEAK_FOR_WIDTH * noise;

    let mut widths: Vec<f64> = stars
        .iter()
        .filter(solid)
        .filter(|s| !s.saturated && s.fwhm.is_finite())
        .map(|s| s.fwhm)
        .collect();
    let mut roundnesses: Vec<f64> = stars
        .iter()
        .filter(solid)
        .filter(|s| !s.saturated && s.roundness.is_finite())
        .map(|s| s.roundness)
        .collect();

    let median = |values: &mut Vec<f64>| -> Option<f64> {
        if values.len() < MINIMUM_FOR_A_MEDIAN {
            return None;
        }
        let middle = values.len() / 2;
        values.select_nth_unstable_by(middle, |a, b| {
            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
        });
        Some(values[middle])
    };

    StarField {
        fwhm: median(&mut widths),
        roundness: median(&mut roundnesses),
        saturated,
        threshold_scale,
        capped,
        stars,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_fits_from_bytes;
    use crate::testutil::{gaussian_background, synthetic_fits, SyntheticSpec};

    fn image(width: usize, height: usize, pixels: &[f64]) -> FitsImage {
        let spec = SyntheticSpec::new(width, height, -32);
        read_fits_from_bytes(&synthetic_fits(&spec, pixels).unwrap()).unwrap()
    }

    /// Adds a Gaussian star, optionally elongated and optionally clipped.
    struct StarSpec {
        cx: f64,
        cy: f64,
        peak: f64,
        sigma_x: f64,
        sigma_y: f64,
        clip: Option<f64>,
    }

    fn add_star(pixels: &mut [f64], width: usize, star: &StarSpec) {
        let StarSpec {
            cx,
            cy,
            peak,
            sigma_x,
            sigma_y,
            clip,
        } = *star;
        let reach = (4.0 * sigma_x.max(sigma_y)).ceil() as i64;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let (x, y) = (cx as i64 + dx, cy as i64 + dy);
                if x < 0 || y < 0 || x as usize >= width {
                    continue;
                }
                let index = y as usize * width + x as usize;
                if index >= pixels.len() {
                    continue;
                }
                let ex = (x as f64 - cx).powi(2) / (2.0 * sigma_x * sigma_x);
                let ey = (y as f64 - cy).powi(2) / (2.0 * sigma_y * sigma_y);
                pixels[index] += peak * (-(ex + ey)).exp();
                if let Some(limit) = clip {
                    pixels[index] = pixels[index].min(limit);
                }
            }
        }
    }

    /// A sky with `count` identical round stars on a grid.
    fn star_field(width: usize, height: usize, count: usize, sigma: f64) -> Vec<f64> {
        let mut pixels = gaussian_background(width, height, 1000.0, 15.0, 42);
        let spacing = 40;
        let mut placed = 0;
        let mut y = 30;
        while y < height - 30 && placed < count {
            let mut x = 30;
            while x < width - 30 && placed < count {
                add_star(
                    &mut pixels,
                    width,
                    &StarSpec {
                        cx: x as f64,
                        cy: y as f64,
                        peak: 8000.0,
                        sigma_x: sigma,
                        sigma_y: sigma,
                        clip: None,
                    },
                );
                placed += 1;
                x += spacing;
            }
            y += spacing;
        }
        pixels
    }

    /// Groups bright pixels the obvious way: a flood fill from each unvisited
    /// pixel, over a set. Slow, and plainly correct, which is the point.
    fn group_by_flood_fill(bright: &[usize], width: usize) -> Vec<Vec<usize>> {
        use std::collections::HashSet;
        let all: HashSet<usize> = bright.iter().copied().collect();
        let mut seen: HashSet<usize> = HashSet::new();
        let mut out = Vec::new();

        for start in bright {
            if seen.contains(start) {
                continue;
            }
            let mut region = Vec::new();
            let mut stack = vec![*start];
            seen.insert(*start);
            while let Some(index) = stack.pop() {
                region.push(index);
                let (x, y) = ((index % width) as i64, (index / width) as i64);
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        let (nx, ny) = (x + dx, y + dy);
                        if nx < 0 || ny < 0 || nx as usize >= width {
                            continue;
                        }
                        let neighbour = ny as usize * width + nx as usize;
                        if all.contains(&neighbour) && seen.insert(neighbour) {
                            stack.push(neighbour);
                        }
                    }
                }
            }
            region.sort_unstable();
            out.push(region);
        }
        out.sort();
        out
    }

    #[test]
    fn grouping_agrees_with_a_flood_fill() {
        // The fast grouping walks two sorted rows in step and collects the
        // result by counting. Neither resembles the obvious algorithm, so it
        // is checked against the obvious algorithm on a scattering of shapes.
        let (w, h) = (61usize, 37usize);
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for _ in 0..40 {
            let mut bright: Vec<usize> = (0..w * h).filter(|_| next() % 5 == 0).collect();
            bright.sort_unstable();

            let mine: Vec<Vec<usize>> = {
                let regions = group(&bright, w);
                let mut v: Vec<Vec<usize>> = regions.iter().map(<[usize]>::to_vec).collect();
                for region in &mut v {
                    region.sort_unstable();
                }
                v.sort();
                v
            };
            assert_eq!(mine, group_by_flood_fill(&bright, w));
        }
    }

    #[test]
    fn a_region_does_not_wrap_around_the_end_of_a_row() {
        // The last pixel of one row and the first of the next are neighbours
        // in memory and nowhere near each other on the sky. Joining them would
        // merge two stars into one long one and lose both.
        let width = 10;
        let bright = vec![
            9,  // (9, 0)
            10, // (0, 1)
        ];
        let regions = group(&bright, width);
        assert_eq!(regions.iter().count(), 2, "opposite edges are not adjacent");
    }

    #[test]
    fn the_same_frame_always_gives_the_same_stars() {
        // Grouping used to hand back regions in a hash map's order, so which
        // stars survived the limit could change from one run to the next.
        let (w, h) = (400usize, 400usize);
        let pixels = star_field(w, h, 60, 2.0);
        let image = image(w, h, &pixels);
        let params = DetectionParams {
            limit: 20,
            ..DetectionParams::default()
        };

        let first = detect(&image, &params);
        assert_eq!(
            first.count(),
            20,
            "the limit should bite for this to prove anything"
        );
        for _ in 0..4 {
            assert_eq!(detect(&image, &params).stars, first.stars);
        }
    }

    #[test]
    fn a_field_of_known_stars_is_found() {
        let (w, h) = (300, 300);
        let pixels = star_field(w, h, 20, 2.0);
        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        assert_eq!(field.count(), 20, "expected every star to be found");
    }

    #[test]
    fn centroids_land_on_the_stars() {
        let (w, h) = (120, 120);
        let mut pixels = gaussian_background(w, h, 1000.0, 10.0, 3);
        // Deliberately off the pixel grid, to check sub-pixel precision.
        add_star(
            &mut pixels,
            w,
            &StarSpec {
                cx: 40.3,
                cy: 60.7,
                peak: 9000.0,
                sigma_x: 2.0,
                sigma_y: 2.0,
                clip: None,
            },
        );

        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        assert_eq!(field.count(), 1);
        let star = field.stars[0];
        assert!(
            (star.x - 40.3).abs() < 0.5 && (star.y - 60.7).abs() < 0.5,
            "centroid was ({:.2}, {:.2})",
            star.x,
            star.y
        );
    }

    #[test]
    fn the_measured_width_follows_the_real_width() {
        // The claim the whole feature rests on. Exactness is not the point;
        // ordering is, since frames are compared with each other.
        let (w, h) = (140, 140);
        let mut widths = Vec::new();
        for sigma in [1.5, 2.5, 4.0] {
            let mut pixels = gaussian_background(w, h, 1000.0, 8.0, 5);
            add_star(
                &mut pixels,
                w,
                &StarSpec {
                    cx: 70.0,
                    cy: 70.0,
                    peak: 12_000.0,
                    sigma_x: sigma,
                    sigma_y: sigma,
                    clip: None,
                },
            );
            let field = detect(&image(w, h, &pixels), &DetectionParams::default());
            assert_eq!(field.count(), 1, "sigma {sigma}");
            widths.push((sigma, field.stars[0].fwhm));
        }

        for (sigma, measured) in &widths {
            // A Gaussian's full width at half maximum is this multiple of its
            // standard deviation.
            let expected = 2.354_820_045_030_949 * sigma;
            assert!(
                (measured - expected).abs() < expected * 0.25,
                "sigma {sigma}: measured {measured:.2}, expected about {expected:.2}"
            );
        }
        assert!(
            widths[0].1 < widths[1].1 && widths[1].1 < widths[2].1,
            "width must increase with the star's size: {widths:?}"
        );
    }

    #[test]
    fn a_blurred_frame_measures_wider_stars() {
        // What a user actually does with this: compare two frames.
        let (w, h) = (200, 200);
        let sharp = detect(
            &image(w, h, &star_field(w, h, 8, 1.6)),
            &DetectionParams::default(),
        );
        let soft = detect(
            &image(w, h, &star_field(w, h, 8, 3.2)),
            &DetectionParams::default(),
        );

        let (a, b) = (sharp.fwhm.unwrap(), soft.fwhm.unwrap());
        assert!(
            b > a,
            "blurred stars should measure wider: {a:.2} then {b:.2}"
        );
    }

    #[test]
    fn hot_pixels_and_cosmic_rays_are_not_stars() {
        // The most common bright thing that is not a star.
        let (w, h) = (100, 100);
        let mut pixels = gaussian_background(w, h, 1000.0, 10.0, 7);
        for i in (0..pixels.len()).step_by(97) {
            pixels[i] = 60_000.0; // single-pixel spikes
        }
        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        assert_eq!(field.count(), 0, "found {} spurious stars", field.count());
    }

    #[test]
    fn a_trailed_star_is_less_round_than_a_circular_one() {
        let (w, h) = (120, 120);

        let mut round = gaussian_background(w, h, 1000.0, 8.0, 11);
        add_star(
            &mut round,
            w,
            &StarSpec {
                cx: 60.0,
                cy: 60.0,
                peak: 10_000.0,
                sigma_x: 2.0,
                sigma_y: 2.0,
                clip: None,
            },
        );
        let round_field = detect(&image(w, h, &round), &DetectionParams::default());

        let mut trailed = gaussian_background(w, h, 1000.0, 8.0, 11);
        add_star(
            &mut trailed,
            w,
            &StarSpec {
                cx: 60.0,
                cy: 60.0,
                peak: 10_000.0,
                sigma_x: 6.0,
                sigma_y: 2.0,
                clip: None,
            },
        );
        let trailed_field = detect(&image(w, h, &trailed), &DetectionParams::default());

        assert_eq!(round_field.count(), 1);
        assert_eq!(trailed_field.count(), 1);
        let (r, t) = (
            round_field.stars[0].roundness,
            trailed_field.stars[0].roundness,
        );
        assert!(r > 0.8, "a round star should be round: {r:.2}");
        assert!(t < 0.6, "a trailed star should not be: {t:.2}");
    }

    #[test]
    fn saturated_stars_are_counted_but_not_measured() {
        // A flat top understates a star's width, so including it would make an
        // overexposed frame look sharper than it is.
        let (w, h) = (200, 200);
        let mut pixels = gaussian_background(w, h, 1000.0, 8.0, 13);
        // Four unsaturated stars, and two clipped flat.
        for (i, x) in [40, 80, 120, 160].iter().enumerate() {
            add_star(
                &mut pixels,
                w,
                &StarSpec {
                    cx: *x as f64,
                    cy: 60.0,
                    peak: 9000.0,
                    sigma_x: 2.0,
                    sigma_y: 2.0,
                    clip: None,
                },
            );
            let _ = i;
        }
        add_star(
            &mut pixels,
            w,
            &StarSpec {
                cx: 60.0,
                cy: 140.0,
                peak: 40_000.0,
                sigma_x: 3.0,
                sigma_y: 3.0,
                clip: Some(20_000.0),
            },
        );
        add_star(
            &mut pixels,
            w,
            &StarSpec {
                cx: 140.0,
                cy: 140.0,
                peak: 40_000.0,
                sigma_x: 3.0,
                sigma_y: 3.0,
                clip: Some(20_000.0),
            },
        );

        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        assert_eq!(field.saturated, 2, "both clipped stars should be flagged");
        assert!(field.count() >= 6);
        assert!(
            field.fwhm.is_some(),
            "the unsaturated ones still give a width"
        );
    }

    #[test]
    fn stars_cut_by_the_frame_edge_are_ignored() {
        // Half a star has a shape that is a lie.
        let (w, h) = (100, 100);
        let mut pixels = gaussian_background(w, h, 1000.0, 8.0, 17);
        add_star(
            &mut pixels,
            w,
            &StarSpec {
                cx: 1.0,
                cy: 50.0,
                peak: 9000.0,
                sigma_x: 2.0,
                sigma_y: 2.0,
                clip: None,
            },
        );
        add_star(
            &mut pixels,
            w,
            &StarSpec {
                cx: 50.0,
                cy: 1.0,
                peak: 9000.0,
                sigma_x: 2.0,
                sigma_y: 2.0,
                clip: None,
            },
        );
        add_star(
            &mut pixels,
            w,
            &StarSpec {
                cx: 50.0,
                cy: 50.0,
                peak: 9000.0,
                sigma_x: 2.0,
                sigma_y: 2.0,
                clip: None,
            },
        );

        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        assert_eq!(field.count(), 1, "only the one away from the edge counts");
        assert!((field.stars[0].x - 50.0).abs() < 1.0);
    }

    #[test]
    fn a_satellite_trail_is_not_counted_as_a_star() {
        // A line across the frame, which is what a satellite or an aeroplane
        // leaves. Extreme elongation is what separates it from bad tracking.
        let (w, h) = (160, 160);
        let mut pixels = gaussian_background(w, h, 1000.0, 8.0, 37);
        for x in 20..140 {
            for dy in -1i64..=1 {
                let y = (80 + dy) as usize;
                pixels[y * w + x] += 9000.0;
            }
        }
        add_star(
            &mut pixels,
            w,
            &StarSpec {
                cx: 60.0,
                cy: 30.0,
                peak: 9000.0,
                sigma_x: 2.0,
                sigma_y: 2.0,
                clip: None,
            },
        );

        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        assert_eq!(field.count(), 1, "only the real star should survive");
        assert!(
            (field.stars[0].y - 30.0).abs() < 2.0,
            "and it is the one off the trail"
        );
    }

    #[test]
    fn poor_tracking_is_measured_rather_than_discarded() {
        // Every star stretched moderately is exactly the case the numbers exist
        // to report. Rejecting them would hide the problem instead of showing
        // it.
        let (w, h) = (200, 200);
        let mut pixels = gaussian_background(w, h, 1000.0, 8.0, 41);
        for (x, y) in [(50.0, 50.0), (110.0, 60.0), (70.0, 130.0), (150.0, 140.0)] {
            add_star(
                &mut pixels,
                w,
                &StarSpec {
                    cx: x,
                    cy: y,
                    peak: 9000.0,
                    sigma_x: 4.5,
                    sigma_y: 2.0,
                    clip: None,
                },
            );
        }

        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        assert_eq!(field.count(), 4, "trailed stars are still stars");
        let roundness = field.roundness.expect("four stars give a median");
        assert!(
            roundness < 0.75,
            "the stretching should show in the roundness: {roundness:.2}"
        );
    }

    #[test]
    fn an_empty_sky_yields_no_stars() {
        let (w, h) = (150, 150);
        let field = detect(
            &image(w, h, &gaussian_background(w, h, 1000.0, 20.0, 19)),
            &DetectionParams::default(),
        );
        assert_eq!(field.count(), 0, "found {} in pure noise", field.count());
        assert_eq!(field.fwhm, None);
    }

    #[test]
    fn degenerate_frames_do_not_panic() {
        for (w, h, pixels) in [
            (1usize, 1usize, vec![5.0]),
            (3, 3, vec![7.0; 9]),
            (20, 20, vec![f64::NAN; 400]),
            (20, 20, vec![500.0; 400]),
        ] {
            let field = detect(&image(w, h, &pixels), &DetectionParams::default());
            assert!(field.count() <= 1, "{w}x{h}");
        }
    }

    #[test]
    fn a_faint_star_survives_the_test_that_rejects_hot_pixels() {
        // Both are a few deviations above the background, so amplitude alone
        // cannot separate them: the star is spread over its neighbours and the
        // hot pixel is not. Written because the first form of this test
        // divided by the neighbours rather than by the peak, and a faint
        // star's neighbours sit close enough to the background that ordinary
        // noise made the ratio enormous — it threw away nine detections in ten
        // on a real narrowband frame.
        let (w, h) = (200usize, 200usize);
        let mut pixels = gaussian_background(w, h, 1000.0, 10.0, 51);

        // Faint: a peak only six deviations up, which is what the matched
        // filter exists to find.
        add_star(
            &mut pixels,
            w,
            &StarSpec {
                cx: 60.0,
                cy: 60.0,
                peak: 60.0,
                sigma_x: 2.0,
                sigma_y: 2.0,
                clip: None,
            },
        );
        // Bright, for contrast.
        add_star(
            &mut pixels,
            w,
            &StarSpec {
                cx: 140.0,
                cy: 140.0,
                peak: 9000.0,
                sigma_x: 2.0,
                sigma_y: 2.0,
                clip: None,
            },
        );
        // A hot pixel of the same height as the bright star.
        pixels[100 * w + 100] += 9000.0;

        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        let near = |x: f64, y: f64| {
            field
                .stars
                .iter()
                .any(|s| (s.x - x).abs() < 2.0 && (s.y - y).abs() < 2.0)
        };

        assert!(near(140.0, 140.0), "the bright star must be found");
        assert!(near(60.0, 60.0), "and so must the faint one");
        assert!(
            !near(100.0, 100.0),
            "but the hot pixel must not be counted as a star"
        );
    }

    #[test]
    fn the_limit_keeps_the_brightest_and_says_that_it_did() {
        // It used to stop at the first regions the scan reached, which took
        // them all from the top of the frame and let rejected regions eat the
        // budget: a good narrowband frame reported seventy-six stars out of
        // nearly three thousand, which is what a frame shot with the cover on
        // looks like.
        let (w, h) = (400usize, 400usize);
        let pixels = star_field(w, h, 60, 2.0);
        let image = image(w, h, &pixels);

        let all = detect(&image, &DetectionParams::default());
        assert!(!all.capped);
        let limited = detect(
            &image,
            &DetectionParams {
                limit: 10,
                ..DetectionParams::default()
            },
        );

        assert_eq!(limited.count(), 10);
        assert!(limited.capped, "a capped count must say so");

        let faintest_kept = limited
            .stars
            .iter()
            .map(|s| s.flux)
            .fold(f64::INFINITY, f64::min);
        let brighter_dropped = all.stars.iter().filter(|s| s.flux > faintest_kept).count();
        assert!(
            brighter_dropped <= 10,
            "the ten kept must be the ten brightest; {brighter_dropped} brighter ones were dropped"
        );

        // And they must not all come from one corner, which is what taking
        // them in scan order did.
        let rows: Vec<f64> = limited.stars.iter().map(|s| s.y).collect();
        let spread = rows.iter().cloned().fold(f64::MIN, f64::max)
            - rows.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            spread > f64::from(u16::try_from(h).unwrap()) / 4.0,
            "the stars kept span only {spread} rows of {h}"
        );
    }

    #[test]
    fn a_colour_image_is_searched_on_its_brightness() {
        // A stacked result is three planes. Reporting nothing for it made a
        // perfectly good image look like an empty one.
        let (w, h) = (200usize, 200usize);
        let mut mono = gaussian_background(w, h, 500.0, 8.0, 41);
        let places = [(50usize, 60usize), (120, 80), (160, 150), (70, 170)];
        for (cx, cy) in places {
            add_star(
                &mut mono,
                w,
                &StarSpec {
                    cx: cx as f64,
                    cy: cy as f64,
                    peak: 9000.0,
                    sigma_x: 2.0,
                    sigma_y: 2.0,
                    clip: None,
                },
            );
        }

        // The same sky in three planes, the star redder than the background.
        let mut colour = Vec::with_capacity(w * h * 3);
        for weight in [1.4f64, 1.0, 0.6] {
            colour.extend(mono.iter().map(|v| v * weight));
        }
        let spec = SyntheticSpec::new(w, h, -32).with_channels(3);
        let image = read_fits_from_bytes(&synthetic_fits(&spec, &colour).unwrap()).unwrap();
        assert_eq!(image.channels, 3, "the test needs a three-plane image");

        let field = detect(&image, &DetectionParams::default());
        assert_eq!(field.count(), places.len(), "every star, once each");
        assert!(field.fwhm.is_some());
    }

    #[test]
    fn a_frame_too_bright_to_search_is_searched_higher_rather_than_given_up_on() {
        // A rich broadband exposure has nebulosity and thousands of stars, and
        // honestly puts a fifth of itself above five deviations. Reporting it
        // as empty made it look exactly like a frame taken with the cover on.
        let (w, h) = (300usize, 300usize);
        let mut pixels = gaussian_background(w, h, 1000.0, 10.0, 31);

        // A broad glow over most of the frame, with real stars on top of it.
        for y in 0..h {
            for x in 0..w {
                if x > 20 && x < 280 && y > 20 && y < 280 {
                    pixels[y * w + x] += 400.0;
                }
            }
        }
        for (cx, cy) in [(60usize, 60usize), (150, 90), (220, 200), (100, 240)] {
            add_star(
                &mut pixels,
                w,
                &StarSpec {
                    cx: cx as f64,
                    cy: cy as f64,
                    peak: 20_000.0,
                    sigma_x: 2.0,
                    sigma_y: 2.0,
                    clip: None,
                },
            );
        }

        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        assert!(
            field.count() >= 4,
            "the stars on the glow must still be found, got {}",
            field.count()
        );
        assert!(
            field.threshold_was_raised(),
            "and the frame must say the threshold was raised"
        );
    }

    #[test]
    fn an_ordinary_frame_does_not_report_a_raised_threshold() {
        let (w, h) = (300usize, 300usize);
        let pixels = star_field(w, h, 20, 2.0);
        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        assert!(!field.threshold_was_raised());
        assert!((field.threshold_scale - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_frame_that_is_mostly_bright_is_not_treated_as_a_star_field() {
        // A flat, or a frame ruined by cloud. Labelling it would cost a great
        // deal and answer nothing.
        let (w, h) = (200, 200);
        let mut pixels = gaussian_background(w, h, 1000.0, 10.0, 23);
        for value in pixels.iter_mut().take(w * h / 2) {
            *value += 50_000.0;
        }
        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        assert_eq!(field.count(), 0);
    }

    #[test]
    fn the_star_limit_is_respected() {
        let (w, h) = (300, 300);
        let pixels = star_field(w, h, 30, 2.0);
        let params = DetectionParams {
            limit: 5,
            ..DetectionParams::default()
        };
        let field = detect(&image(w, h, &pixels), &params);
        assert!(field.count() <= 5, "found {}", field.count());
    }

    #[test]
    fn a_median_needs_more_than_a_couple_of_stars() {
        let (w, h) = (100, 100);
        let mut pixels = gaussian_background(w, h, 1000.0, 8.0, 29);
        add_star(
            &mut pixels,
            w,
            &StarSpec {
                cx: 50.0,
                cy: 50.0,
                peak: 9000.0,
                sigma_x: 2.0,
                sigma_y: 2.0,
                clip: None,
            },
        );
        let field = detect(&image(w, h, &pixels), &DetectionParams::default());
        assert_eq!(field.count(), 1);
        assert_eq!(field.fwhm, None, "one star is not a median");
    }

    #[test]
    fn a_mosaic_reports_the_width_the_sensor_actually_recorded() {
        use crate::debayer::{BayerPattern, Colour};
        // A colour sensor records a star on half its pixels. Measuring on a
        // half-size image built from those and doubling the answer is
        // arithmetically right and practically wrong: a star three pixels
        // across is one and a half there, too few for a grid to describe, so
        // the narrow ones are dropped and the median comes from the broad
        // ones. On a real frame that read 5.05 pixels where the frame's own
        // pixels, read off by hand, said 3.5.
        let (w, h) = (300usize, 300usize);
        let pattern = BayerPattern::Rggb;

        // From four pixels across upwards. Narrower than that and the stars
        // are not found at all on a colour sensor, for the reason given in
        // `detect_mosaic`: the half-size image it searches samples a three
        // pixel star at one and a half, and a perfect Gaussian that narrow
        // looks like a hot pixel to the filter that rejects hot pixels. Real
        // stars of that width are found, being less sharply peaked than a
        // Gaussian, but it is close to the edge of what this can do.
        for sigma in [1.8f64, 2.6, 3.4] {
            let mut scene = gaussian_background(w, h, 1000.0, 12.0, 55);
            for (cx, cy) in [
                (70usize, 80usize),
                (150, 90),
                (100, 190),
                (200, 200),
                (60, 160),
            ] {
                for dy in -9i64..=9 {
                    for dx in -9i64..=9 {
                        let (x, y) = (cx as i64 + dx, cy as i64 + dy);
                        if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                            continue;
                        }
                        #[allow(clippy::cast_precision_loss)]
                        let r = ((dx * dx + dy * dy) as f64) / (2.0 * sigma * sigma);
                        #[allow(clippy::cast_sign_loss)]
                        {
                            scene[y as usize * w + x as usize] += 30_000.0 * (-r).exp();
                        }
                    }
                }
            }

            let mosaic: Vec<f64> = (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    match pattern.colour_at(x, y) {
                        Colour::Green => scene[i],
                        _ => scene[i] * 0.6,
                    }
                })
                .collect();

            let field = detect_mosaic(&image(w, h, &mosaic), pattern, &DetectionParams::default());
            let truth = 2.354_820_045_030_949 * sigma;
            let measured = field.fwhm.unwrap_or_else(|| {
                panic!(
                    "no width for a star {truth:.2} pixels across, of which {} were found",
                    field.count()
                )
            });
            assert!(
                (measured - truth).abs() < truth * 0.2,
                "sigma {sigma}: measured {measured:.2} px against a true {truth:.2}"
            );
        }
    }

    #[test]
    fn a_colour_mosaic_finds_each_star_once_rather_than_four_times() {
        use crate::debayer::BayerPattern;
        let (w, h) = (200, 200);
        let pattern = BayerPattern::Rggb;

        // A star field, then sampled through the filter grid.
        let scene = star_field(w, h, 6, 2.5);
        let mosaic: Vec<f64> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                // Green sites keep the signal; the others are dimmer, as a real
                // sensor's would be for a neutral star.
                match pattern.colour_at(x, y) {
                    crate::debayer::Colour::Green => scene[i],
                    _ => scene[i] * 0.6,
                }
            })
            .collect();

        let field = detect_mosaic(&image(w, h, &mosaic), pattern, &DetectionParams::default());
        assert!(
            (5..=7).contains(&field.count()),
            "expected about six stars, found {}",
            field.count()
        );
    }

    #[test]
    fn mosaic_positions_come_back_in_the_mosaic_coordinates() {
        use crate::debayer::BayerPattern;
        let (w, h) = (160, 160);
        let mut scene = gaussian_background(w, h, 1000.0, 8.0, 31);
        add_star(
            &mut scene,
            w,
            &StarSpec {
                cx: 80.0,
                cy: 100.0,
                peak: 12_000.0,
                sigma_x: 3.0,
                sigma_y: 3.0,
                clip: None,
            },
        );
        let pattern = BayerPattern::Rggb;

        let field = detect_mosaic(&image(w, h, &scene), pattern, &DetectionParams::default());
        assert_eq!(field.count(), 1);
        let star = field.stars[0];
        assert!(
            (star.x - 80.0).abs() < 3.0 && (star.y - 100.0).abs() < 3.0,
            "position came back as ({:.1}, {:.1})",
            star.x,
            star.y
        );
    }
}

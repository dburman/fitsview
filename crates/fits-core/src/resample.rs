//! Laying a frame onto another's grid at a fraction of a pixel.
//!
//! A mount does not drift in whole pixels, so almost every frame of a stack
//! belongs somewhere between the pixels of the reference. There were two ways
//! of putting it there, and both cost the picture:
//!
//! - **To the nearest whole pixel**, which is sharp but up to seven tenths of a
//!   pixel out, and a stack of frames each a different fraction out is a stack
//!   of slightly doubled stars.
//! - **By blending the four pixels around the point**, which is in the right
//!   place but blurs every star, and quietens the noise by an amount that
//!   depends on where each pixel landed, which leaves a faint grid in the sky.
//!
//! This uses the Lanczos kernel over six pixels either way instead, the
//! standard in astronomical registration: in the right place, and very nearly
//! as sharp as the frame itself.
//!
//! **Two passes rather than one.** Six by six is thirty-six pixels for every
//! pixel placed. Any shift and turn can instead be done exactly as a pass along
//! the rows of the frame followed by one down the columns of the result, which
//! is six and six. The weights come from a table rather than from sines, since
//! the fraction a pixel lands at changes slowly across a frame.
//!
//! **Straight lines stay straight.** The kernel as usually written does not
//! reproduce a straight line: a quarter of the way between two pixels it
//! reads from 0.2303 of the way, and a star placed there lands a fiftieth of a
//! pixel short. The weights are corrected by the least change that makes them
//! reproduce a line exactly, once, in the table.
//!
//! **Clamped against rings.** The kernel has negative lobes, and a bright star
//! under one of them digs a dark ring around itself — five per cent of its
//! peak for a star one pixel wide, nothing measurable for a star three pixels
//! wide. A dip below both of the two nearest pixels is a ring when it is
//! deeper than [`RING_DEVIATIONS`] of the frame's noise, and the blend of those
//! two is used there instead. Anything shallower cannot be told from the noise,
//! and clamping it would blur the noise as the blend does.

use std::sync::OnceLock;

use rayon::prelude::*;

/// Where each output pixel comes from in the source: the source point for
/// output `(x, y)` is `(a·x + b·y + e, c·x + d·y + f)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    /// How the source column changes along an output row.
    pub a: f64,
    /// How the source column changes down an output column.
    pub b: f64,
    /// The source column of output `(0, 0)`.
    pub e: f64,
    /// How the source row changes along an output row.
    pub c: f64,
    /// How the source row changes down an output column.
    pub d: f64,
    /// The source row of output `(0, 0)`.
    pub f: f64,
}

impl Affine {
    /// Where output `(x, y)` comes from.
    #[must_use]
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.b * y + self.e,
            self.c * x + self.d * y + self.f,
        )
    }
}

/// Pixels each side of the point the kernel reaches.
const RADIUS: usize = 3;

/// Pixels the kernel reads along each pass.
const TAPS: usize = 2 * RADIUS;

/// Steps between two pixels at which the weights are worked out.
///
/// A two-thousandth of a pixel, which is far finer than any alignment is known
/// to.
const STEPS: usize = 2048;

/// How deep a dip below both of the nearest two pixels must be, in deviations
/// of the frame's noise, to be taken for a ring rather than the noise.
///
/// Three, because noise alone almost never dips that far below its
/// neighbours: see the tests below for how rarely.
pub const RING_DEVIATIONS: f32 = 3.0;

/// The kernel's weights at every step between two pixels, worked out once.
struct Kernel {
    /// The weights of the six pixels, the nearest two in the middle, summing
    /// to one.
    weights: Vec<[f32; TAPS]>,
    /// The sum of their squares: how much of a pixel's noise survives being
    /// placed at that step.
    kept: Vec<f64>,
}

fn kernel() -> &'static Kernel {
    static KERNEL: OnceLock<Kernel> = OnceLock::new();
    KERNEL.get_or_init(|| {
        let mut weights = Vec::with_capacity(STEPS + 1);
        let mut kept = Vec::with_capacity(STEPS + 1);
        for step in 0..=STEPS {
            #[allow(clippy::cast_precision_loss)]
            let t = step as f64 / STEPS as f64;
            let mut raw = [0.0f64; TAPS];
            for (k, weight) in raw.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let offset = k as f64 - (RADIUS - 1) as f64;
                *weight = lanczos(t - offset);
            }
            let corrected = reproducing_a_line(raw, t);
            let mut row = [0.0f32; TAPS];
            for (out, weight) in row.iter_mut().zip(corrected) {
                #[allow(clippy::cast_possible_truncation)]
                {
                    *out = weight as f32;
                }
            }
            kept.push(corrected.iter().map(|w| w * w).sum());
            weights.push(row);
        }
        Kernel { weights, kept }
    })
}

/// `raw` weights changed as little as possible so that they sum to one and
/// place a straight line exactly: the six pixels, weighted, sit `t` of the way
/// from the third to the fourth.
///
/// The least change in the sense of the sum of squares, which spreads it over
/// all six in proportion to how far each is from the middle. At a whole pixel
/// and at a half the kernel already does both, and nothing changes.
fn reproducing_a_line(raw: [f64; TAPS], t: f64) -> [f64; TAPS] {
    let sum: f64 = raw.iter().sum();
    let mut weights = raw.map(|w| w / sum);
    // Where the six sit, the third at zero.
    #[allow(clippy::cast_precision_loss)]
    let place = |k: usize| k as f64 - (RADIUS - 1) as f64;
    let reads_from: f64 = (0..TAPS).map(|k| weights[k] * place(k)).sum();
    #[allow(clippy::cast_precision_loss)]
    let (places, squares) =
        (0..TAPS).fold((0.0, 0.0), |(p, q), k| (p + place(k), q + place(k).powi(2)));
    // Adding α + β·place(k) to each: α·6 + β·places keeps the sum, and
    // α·places + β·squares moves the line by what it is out.
    #[allow(clippy::cast_precision_loss)]
    let count = TAPS as f64;
    let beta = (t - reads_from) / (squares - places * places / count);
    let alpha = -beta * places / count;
    for (k, weight) in weights.iter_mut().enumerate() {
        *weight += alpha + beta * place(k);
    }
    weights
}

/// The Lanczos kernel of radius three.
fn lanczos(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let radius = RADIUS as f64;
    if x.abs() >= radius {
        return 0.0;
    }
    let px = std::f64::consts::PI * x;
    radius * px.sin() * (px / radius).sin() / (px * px)
}

/// The table step nearest a fraction between zero and one.
#[inline(always)]
fn step_of(t: f64) -> usize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let step = (t * STEPS as f64).round() as usize;
    step.min(STEPS)
}

/// How much of a pixel's noise variance survives being placed at `(x, y)` in
/// the source, as a fraction: one on a pixel, about 0.6 midway between four.
///
/// Worked out from the kernel as if it were never clamped. Clamping keeps
/// less, but it happens beside bright stars, where the scatter between frames
/// is far larger than the noise anyway.
#[must_use]
pub fn noise_kept(x: f64, y: f64) -> f64 {
    let kept = &kernel().kept;
    kept[step_of(x - x.floor())] * kept[step_of(y - y.floor())]
}

/// Interpolates six pixels by `weights`, the point lying `t` of the way from
/// the third to the fourth. Also says whether it had to be
/// clamped, which it is when the result dips below both of the nearest two by
/// more than `ring`.
///
/// `None` if any of the six is undefined: guessing across a dead pixel would
/// invent signal.
// Always inlined: it runs for every pixel of both passes, and left to the
// compiler it was inlined in one build and called in another, which was the
// difference between placement costing half a second and three.
#[inline(always)]
fn interpolate(
    taps: &[f32; TAPS],
    weights: &[f32; TAPS],
    t: f32,
    ring: f32,
) -> Option<(f32, bool)> {
    // Whether any of the six is undefined is asked of the sum rather than of
    // each, which is the same question — an undefined or infinite pixel leaves
    // the sum undefined or infinite whatever it is weighted by, zero included
    // — asked once instead of six times, for every pixel of every frame.
    let sum = taps[0] * weights[0]
        + taps[1] * weights[1]
        + taps[2] * weights[2]
        + taps[3] * weights[3]
        + taps[4] * weights[4]
        + taps[5] * weights[5];
    if !sum.is_finite() {
        return None;
    }
    let (near, far) = (taps[RADIUS - 1], taps[RADIUS]);
    if sum < near.min(far) - ring {
        Some((near + (far - near) * t, true))
    } else {
        Some((sum, false))
    }
}

/// How deep a dip is taken for a ring in a frame whose noise is `noise`.
///
/// A frame whose noise is unknown, or nothing, is never clamped: there is no
/// telling a ring from the noise, and nothing real is lost by keeping the
/// kernel.
fn ring_depth(noise: f64) -> f32 {
    if noise.is_finite() && noise > 0.0 {
        #[allow(clippy::cast_possible_truncation)]
        {
            RING_DEVIATIONS * noise as f32
        }
    } else {
        f32::INFINITY
    }
}

/// The value at `at` along a line of `len` pixels read through `pixel`.
fn along(pixel: impl Fn(usize) -> f32, len: usize, at: f64, ring: f32) -> Option<(f32, bool)> {
    #[allow(clippy::cast_precision_loss)]
    if !(at >= (RADIUS - 1) as f64 && at < (len - RADIUS) as f64) {
        return None;
    }
    let floor = at.floor();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let first = floor as usize + 1 - RADIUS;
    let t = at - floor;
    let mut taps = [0.0f32; TAPS];
    for (k, tap) in taps.iter_mut().enumerate() {
        *tap = pixel(first + k);
    }
    #[allow(clippy::cast_possible_truncation)]
    interpolate(&taps, &kernel().weights[step_of(t)], t as f32, ring)
}

/// Fills `out[x]` with `row` read at `base + across·x`.
///
/// For almost every frame `across` is one, or minus one for a frame from the
/// far side of a meridian flip, to within a few parts in ten thousand million:
/// the fraction each pixel lands at is then the same all along the row, to far
/// better than the table resolves. One set of weights serves the whole row,
/// and the kernel slides along it a pixel at a time — forwards, or backwards
/// for a flipped frame — with nothing to look up. Only a frame turned by a
/// measurable angle, where the fraction drifts along the row, is worked out
/// pixel by pixel.
fn resample_line(row: &[f32], out: &mut [f32], base: f64, across: f64, ring: f32) {
    let len = row.len();
    let sign = if across < 0.0 { -1.0 } else { 1.0 };
    #[allow(clippy::cast_precision_loss)]
    let drift = (across - sign).abs() * len as f64 * STEPS as f64;
    if drift > 0.25 {
        for (x, value) in out.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let at = base + across * x as f64;
            if let Some((placed, _)) = along(|i| row[i], len, at, ring) {
                *value = placed;
            }
        }
        return;
    }

    let floor = base.floor();
    let t = base - floor;
    let weights = &kernel().weights[step_of(t)];
    #[allow(clippy::cast_possible_truncation)]
    let (floor, t) = (floor as i64, t as f32);
    let Ok(last) = i64::try_from(len) else {
        return;
    };
    #[allow(clippy::cast_possible_wrap)]
    let radius = RADIUS as i64;
    for (x, value) in (0i64..).zip(out.iter_mut()) {
        // The pixel the point lies just past: the third of the six.
        let near = if sign > 0.0 { floor + x } else { floor - x };
        let first = near + 1 - radius;
        if first < 0 || near + radius >= last {
            continue;
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let taps: &[f32; TAPS] = row[first as usize..][..TAPS]
            .try_into()
            .expect("six pixels");
        if let Some((placed, _)) = interpolate(taps, weights, t, ring) {
            *value = placed;
        }
    }
}

/// A frame half placed: resampled along its rows, and ready to be read down
/// its columns one output row at a time.
///
/// Holding the half-way stage costs one plane of the frame, and reading the
/// output a row at a time means the finished frame is never held at all: each
/// row goes straight into the stack.
#[derive(Debug)]
pub struct Resampler {
    width: usize,
    height: usize,
    affine: Affine,
    /// How deep a dip is taken for a ring.
    ring: f32,
    /// Row `v` of the source, resampled to the output's columns.
    between: Vec<f32>,
}

impl Resampler {
    /// Resamples `source`, whose noise is `noise`, along its rows for placing
    /// by `affine`.
    ///
    /// `None` for a turn of more than about sixty degrees, which the two passes
    /// cannot do — a frame's rows would run down the output's columns — and for
    /// a frame too small to hold the kernel. No frame of a stack is turned
    /// that far other than by the half turn of a meridian flip, which is not
    /// a problem: it reverses the rows without turning them.
    #[must_use]
    pub fn new(
        source: &[f32],
        width: usize,
        height: usize,
        affine: Affine,
        noise: f64,
    ) -> Option<Self> {
        if affine.d.abs() < 0.5 || width <= TAPS || height <= TAPS {
            return None;
        }
        // For output column x and source row v: the output row y at which
        // that column meets row v, and so the source column to read there.
        let across = affine.a - affine.b * affine.c / affine.d;
        let down = affine.b / affine.d;
        let start = affine.e - affine.b * affine.f / affine.d;
        let ring = ring_depth(noise);

        let mut between = vec![f32::NAN; width * height];
        between
            .par_chunks_mut(width)
            .enumerate()
            .for_each(|(v, out)| {
                let row = &source[v * width..][..width];
                #[allow(clippy::cast_precision_loss)]
                let base = start + down * v as f64;
                resample_line(row, out, base, across, ring);
            });

        Some(Self {
            width,
            height,
            affine,
            ring,
            between,
        })
    }

    /// Hands each placed pixel of output row `y` to `visit`, with its column,
    /// its value, and how much of its noise variance survived, from
    /// [`noise_kept`].
    ///
    /// A pixel whose source point falls within three pixels of the frame's
    /// edge, or near an undefined one, is not handed over.
    pub fn row(&self, y: usize, mut visit: impl FnMut(usize, f32, f64)) {
        let (width, height, affine, ring) = (self.width, self.height, self.affine, self.ring);
        let between = &self.between;
        let kernel = kernel();
        // Along an output row the source point moves by (a, c) a pixel.
        #[allow(clippy::cast_precision_loss)]
        let (row_x, row_y) = (
            affine.b * y as f64 + affine.e,
            affine.d * y as f64 + affine.f,
        );
        #[allow(clippy::cast_precision_loss)]
        let highest = (height - RADIUS) as f64;
        for x in 0..width {
            #[allow(clippy::cast_precision_loss)]
            let (sx, sy) = (row_x + affine.a * x as f64, row_y + affine.c * x as f64);
            #[allow(clippy::cast_precision_loss)]
            if !(sy >= (RADIUS - 1) as f64 && sy < highest) {
                continue;
            }
            let floor = sy.floor();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let first = (floor as usize + 1 - RADIUS) * width + x;
            let mut taps = [0.0f32; TAPS];
            for (k, tap) in taps.iter_mut().enumerate() {
                *tap = between[first + k * width];
            }
            let t = sy - floor;
            let step = step_of(t);
            #[allow(clippy::cast_possible_truncation)]
            if let Some((value, _)) = interpolate(&taps, &kernel.weights[step], t as f32, ring) {
                visit(
                    x,
                    value,
                    kernel.kept[step_of(sx - sx.floor())] * kernel.kept[step],
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Places a whole frame whose noise is `noise`, for looking at the result.
    fn placed_with(
        source: &[f32],
        width: usize,
        height: usize,
        affine: Affine,
        noise: f64,
    ) -> Vec<f32> {
        let resampler = Resampler::new(source, width, height, affine, noise).expect("placeable");
        let mut out = vec![f32::NAN; width * height];
        for y in 0..height {
            resampler.row(y, |x, value, _| out[y * width + x] = value);
        }
        out
    }

    /// Places a frame with no noise to speak of, and so no clamping.
    fn placed(source: &[f32], width: usize, height: usize, affine: Affine) -> Vec<f32> {
        placed_with(source, width, height, affine, 0.0)
    }

    /// The same by blending the four nearest pixels, as stacking used to.
    fn blended(source: &[f32], width: usize, height: usize, affine: Affine) -> Vec<f32> {
        let mut out = vec![f32::NAN; width * height];
        for y in 0..height {
            for x in 0..width {
                #[allow(clippy::cast_precision_loss)]
                let (sx, sy) = affine.apply(x as f64, y as f64);
                if sx < 0.0 || sy < 0.0 {
                    continue;
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let (ix, iy) = (sx as usize, sy as usize);
                if ix + 1 >= width || iy + 1 >= height {
                    continue;
                }
                let (tx, ty) = (sx - sx.floor(), sy - sy.floor());
                let at = |x: usize, y: usize| f64::from(source[y * width + x]);
                let value = at(ix, iy) * (1.0 - tx) * (1.0 - ty)
                    + at(ix + 1, iy) * tx * (1.0 - ty)
                    + at(ix, iy + 1) * (1.0 - tx) * ty
                    + at(ix + 1, iy + 1) * tx * ty;
                #[allow(clippy::cast_possible_truncation)]
                {
                    out[y * width + x] = value as f32;
                }
            }
        }
        out
    }

    fn shift(dx: f64, dy: f64) -> Affine {
        Affine {
            a: 1.0,
            b: 0.0,
            e: dx,
            c: 0.0,
            d: 1.0,
            f: dy,
        }
    }

    /// A turn about the middle of a `size` frame, then a shift.
    fn turn(size: f64, angle: f64, dx: f64, dy: f64) -> Affine {
        let (c, s) = (angle.cos(), angle.sin());
        let centre = (size - 1.0) / 2.0;
        Affine {
            a: c,
            b: -s,
            e: centre - centre * c + centre * s + dx,
            c: s,
            d: c,
            f: centre - centre * s - centre * c + dy,
        }
    }

    /// A star of standard deviation `sigma` on a sky of `sky`.
    fn star(size: usize, (cx, cy): (f64, f64), sigma: f64, peak: f64, sky: f64) -> Vec<f32> {
        (0..size * size)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let (x, y) = ((i % size) as f64, (i / size) as f64);
                let r2 = (x - cx).powi(2) + (y - cy).powi(2);
                #[allow(clippy::cast_possible_truncation)]
                {
                    (sky + peak * (-r2 / (2.0 * sigma * sigma)).exp()) as f32
                }
            })
            .collect()
    }

    /// Centroid and width (standard deviation) of the light above `sky`.
    fn moments(image: &[f32], size: usize, sky: f64) -> ((f64, f64), f64) {
        let (mut total, mut sx, mut sy) = (0.0, 0.0, 0.0);
        for (i, v) in image.iter().enumerate() {
            if !v.is_finite() {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let (x, y) = ((i % size) as f64, (i / size) as f64);
            let light = f64::from(*v) - sky;
            total += light;
            sx += light * x;
            sy += light * y;
        }
        let (cx, cy) = (sx / total, sy / total);
        let mut spread = 0.0;
        for (i, v) in image.iter().enumerate() {
            if !v.is_finite() {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let (x, y) = ((i % size) as f64, (i / size) as f64);
            let light = f64::from(*v) - sky;
            spread += light * ((x - cx).powi(2) + (y - cy).powi(2));
        }
        ((cx, cy), (spread / total / 2.0).sqrt())
    }

    /// Deterministic Gaussian noise.
    fn noise(len: usize, sigma: f64, level: f64, seed: u64) -> Vec<f32> {
        let mut state = seed | 1;
        let mut uniform = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            #[allow(clippy::cast_precision_loss)]
            {
                ((state >> 11) as f64 + 0.5) / (1u64 << 53) as f64
            }
        };
        (0..len)
            .map(|_| {
                let (u, v) = (uniform(), uniform());
                let normal = (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos();
                #[allow(clippy::cast_possible_truncation)]
                {
                    (level + sigma * normal) as f32
                }
            })
            .collect()
    }

    #[test]
    fn a_whole_pixel_shift_moves_the_frame_and_changes_nothing() {
        let size = 40;
        let source = noise(size * size, 10.0, 1000.0, 3);
        let out = placed(&source, size, size, shift(3.0, -2.0));
        for y in 10..30 {
            for x in 10..30 {
                assert_eq!(
                    out[y * size + x],
                    source[(y - 2) * size + x + 3],
                    "at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn a_star_lands_where_it_belongs_at_its_own_width() {
        // Placed by fractions of a pixel, the star has to move by exactly that
        // much and stay the width it was. Blending the four nearest pixels
        // widens it most midway between them; this must not.
        let size = 64;
        let sigma = 1.2;
        let truth = star(size, (32.0, 32.0), sigma, 20_000.0, 1000.0);
        let ((_, _), own) = moments(&truth, size, 1000.0);
        for (dx, dy) in [(0.25, 0.0), (0.5, 0.5), (0.7, -0.3), (-0.45, 0.15)] {
            let out = placed(&truth, size, size, shift(dx, dy));
            let ((cx, cy), width) = moments(&out, size, 1000.0);
            assert!(
                (cx - (32.0 - dx)).abs() < 0.002,
                "{dx},{dy}: centred at {cx}"
            );
            assert!(
                (cy - (32.0 - dy)).abs() < 0.002,
                "{dx},{dy}: centred at {cy}"
            );
            let wider = width / own - 1.0;
            let ((_, _), soft) = moments(&blended(&truth, size, size, shift(dx, dy)), size, 1000.0);
            let blend_wider = soft / own - 1.0;
            assert!(wider.abs() < 0.01, "{dx},{dy}: {:.2}% wider", 100.0 * wider);
            if dx.abs() > 0.2 {
                assert!(
                    blend_wider > 4.0 * wider.abs(),
                    "{dx},{dy}: blending widened it {:.2}%, this {:.2}%",
                    100.0 * blend_wider,
                    100.0 * wider
                );
            }
        }
    }

    #[test]
    fn a_turn_and_the_meridian_flip_land_the_star_where_the_arithmetic_says() {
        let size = 96;
        let at = (30.3, 61.8);
        let source = star(size, at, 1.3, 20_000.0, 1000.0);
        #[allow(clippy::cast_precision_loss)]
        let last = (size - 1) as f64;
        for (angle, flip) in [(0.004, false), (-0.02, false), (0.003, true)] {
            let mut affine = turn(size as f64, angle, 0.37, -0.61);
            if flip {
                affine = Affine {
                    a: -affine.a,
                    b: -affine.b,
                    e: last - affine.e,
                    c: -affine.c,
                    d: -affine.d,
                    f: last - affine.f,
                };
            }
            let out = placed(&source, size, size, affine);
            let ((cx, cy), _) = moments(&out, size, 1000.0);
            let (sx, sy) = affine.apply(cx, cy);
            assert!(
                (sx - at.0).abs() < 0.02 && (sy - at.1).abs() < 0.02,
                "angle {angle}, flipped {flip}: came from {sx:.3},{sy:.3}, not {at:?}"
            );
        }
    }

    #[test]
    fn smooth_sky_is_reproduced_far_better_than_by_blending() {
        // Nebulosity, a gradient: what blending smooths and this should not.
        let size = 80;
        let wave = |x: f64, y: f64| 1000.0 + 300.0 * (x / 5.0).sin() * (y / 7.0).cos();
        let source: Vec<f32> = (0..size * size)
            .map(|i| {
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                {
                    wave((i % size) as f64, (i / size) as f64) as f32
                }
            })
            .collect();
        let affine = turn(size as f64, 0.01, 0.4, 0.3);
        let error = |out: &[f32]| {
            let mut worst = 0.0f64;
            for y in 12..size - 12 {
                for x in 12..size - 12 {
                    #[allow(clippy::cast_precision_loss)]
                    let (sx, sy) = affine.apply(x as f64, y as f64);
                    worst = worst.max((f64::from(out[y * size + x]) - wave(sx, sy)).abs());
                }
            }
            worst
        };
        let sharp = error(&placed(&source, size, size, affine));
        let soft = error(&blended(&source, size, size, affine));
        assert!(sharp < 0.2, "off by {sharp:.3} of 300");
        assert!(soft > 10.0 * sharp, "blending {soft:.3}, this {sharp:.3}");
    }

    #[test]
    fn a_star_sharper_than_any_real_one_digs_no_dark_ring() {
        // The case clamping is for: a star so undersampled that most of its
        // light is in one pixel, placed midway between pixels. Unclamped it
        // digs a ring thousands of counts deep.
        let size = 48;
        let (sky, noise) = (1000.0, 10.0);
        let source = star(size, (24.0, 24.0), 0.5, 50_000.0, sky);
        let lowest = |out: Vec<f32>| {
            f64::from(
                out.iter()
                    .filter(|v| v.is_finite())
                    .fold(f32::INFINITY, |a, v| a.min(*v)),
            )
        };
        let unclamped = lowest(placed(&source, size, size, shift(0.5, 0.5)));
        assert!(
            unclamped < sky - 1000.0,
            "the kernel alone dips to {unclamped}"
        );
        let clamped = lowest(placed_with(&source, size, size, shift(0.5, 0.5), noise));
        assert!(
            clamped > sky - 2.0 * f64::from(RING_DEVIATIONS) * noise,
            "a ring to {clamped} on a sky of {sky}, noise {noise}"
        );
    }

    #[test]
    fn a_star_as_wide_as_a_real_one_is_not_clamped_at_all() {
        // Three pixels across, as a night at 1.8 arcseconds a pixel gives:
        // the kernel's ring is too shallow to see, so nothing is given up.
        let size = 48;
        let source = star(size, (24.0, 24.0), 1.2, 50_000.0, 1000.0);
        let plain = placed(&source, size, size, shift(0.3, 0.6));
        let clamped = placed_with(&source, size, size, shift(0.3, 0.6), 10.0);
        // Bit for bit, so that the undefined edges compare as equal.
        let bits = |image: &[f32]| image.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        assert!(
            bits(&plain) == bits(&clamped),
            "clamping changed a star of real width"
        );
    }

    #[test]
    fn ordinary_noise_is_almost_never_clamped() {
        // Clamped too readily, the kernel turns into the blend it replaces.
        let len = 200_000;
        for level in [1000.0, 0.0] {
            let source = noise(len, 20.0, level, 17);
            let mut clamped = 0usize;
            let mut tried = 0usize;
            for (i, t) in [(0usize, 0.5), (1, 0.25), (2, 0.8)]
                .iter()
                .cycle()
                .take(len / 10)
            {
                let first = (i * 997 + tried * 7) % (len - TAPS);
                let mut taps = [0.0f32; TAPS];
                taps.copy_from_slice(&source[first..first + TAPS]);
                #[allow(clippy::cast_possible_truncation)]
                let (_, was) = interpolate(
                    &taps,
                    &kernel().weights[step_of(*t)],
                    *t as f32,
                    ring_depth(20.0),
                )
                .expect("finite");
                clamped += usize::from(was);
                tried += 1;
            }
            #[allow(clippy::cast_precision_loss)]
            let share = 100.0 * clamped as f64 / tried as f64;
            assert!(share < 0.2, "sky at {level}: {share:.3}% of noise clamped");
        }
    }

    #[test]
    fn the_weights_place_a_straight_line_exactly_at_every_step() {
        for step in [0, 1, 300, 512, 1024, 1500, 2047, 2048] {
            let weights = &kernel().weights[step];
            #[allow(clippy::cast_precision_loss)]
            let t = step as f64 / STEPS as f64;
            let sum: f64 = weights.iter().map(|w| f64::from(*w)).sum();
            #[allow(clippy::cast_precision_loss)]
            let reads: f64 = weights
                .iter()
                .enumerate()
                .map(|(k, w)| f64::from(*w) * (k as f64 - 2.0))
                .sum();
            assert!(
                (sum - 1.0).abs() < 1e-6,
                "step {step}: weights sum to {sum}"
            );
            assert!(
                (reads - t).abs() < 1e-6,
                "step {step}: reads from {reads}, not {t}"
            );
        }
    }

    #[test]
    fn the_noise_kept_is_the_noise_that_survives() {
        // Rejection judges a placed frame by this, so it has to be right.
        let size = 300;
        let source = noise(size * size, 20.0, 1000.0, 99);
        for (dx, dy) in [(0.5, 0.5), (0.25, 0.0), (0.1, 0.7)] {
            let affine = shift(dx, dy);
            let out = placed_with(&source, size, size, affine, 20.0);
            let values: Vec<f64> = out
                .iter()
                .filter(|v| v.is_finite())
                .map(|v| f64::from(*v))
                .collect();
            #[allow(clippy::cast_precision_loss)]
            let n = values.len() as f64;
            let mean = values.iter().sum::<f64>() / n;
            let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
            let measured = variance / 400.0;
            let predicted = noise_kept(dx, dy);
            assert!(
                (measured - predicted).abs() < 0.03,
                "{dx},{dy}: kept {measured:.3} of the noise, predicted {predicted:.3}"
            );
        }
        assert!((noise_kept(3.0, 7.0) - 1.0).abs() < 1e-9);
        assert!(noise_kept(0.5, 0.5) > 0.55, "{}", noise_kept(0.5, 0.5));
    }

    #[test]
    fn nothing_is_made_up_near_the_edge_or_an_undefined_pixel() {
        let size = 40;
        let mut source = noise(size * size, 10.0, 1000.0, 5);
        source[20 * size + 20] = f32::NAN;
        let out = placed(&source, size, size, shift(0.5, 0.5));
        // Every output pixel reading within three of the dead one is undefined.
        for y in 17..21 {
            for x in 17..21 {
                assert!(
                    out[y * size + x].is_nan(),
                    "{x},{y} = {}",
                    out[y * size + x]
                );
            }
        }
        assert!(out[5 * size + 5].is_finite());
        // And none reaching past the edge is handed over.
        assert!(out[size - 1].is_nan(), "read past the right edge");
        assert!(out[(size - 1) * size].is_nan(), "read past the bottom edge");
    }
}

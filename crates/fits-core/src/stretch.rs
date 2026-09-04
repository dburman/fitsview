//! Automatic screen stretch.
//!
//! A raw astronomical frame is almost black. The interesting signal sits a
//! little above the background, and the background itself occupies a tiny
//! fraction of the range, so a linear mapping shows nothing. This module
//! implements the midtone transfer function auto-stretch that astronomy tools
//! conventionally use, which puts the background at a chosen brightness and
//! pulls the faint signal above it into view.
//!
//! Nothing here changes the pixel data. The stretch affects display only, so
//! calibration and statistics keep working on the real values.

use rayon::prelude::*;

use crate::image::FitsImage;

/// Largest number of samples used to estimate the background.
///
/// The median and deviation of a million samples describe a frame just as well
/// as those of twenty-four million, at a twentieth of the cost.
pub const MAX_SAMPLES: usize = 1_000_000;

/// Entries in a lookup table: one per distinct 16-bit input level.
pub const LUT_LEN: usize = 65_536;

/// A prepared lookup table mapping a quantised sample to a display byte.
pub type Lut = [u8; LUT_LEN];

/// Scales the median absolute deviation to a standard-deviation equivalent for
/// normally distributed data.
/// Turns a median absolute deviation into a standard deviation, for the
/// normal distribution the sky background approximates.
pub const MAD_TO_SIGMA: f64 = 1.482_602_218_505_602;

/// How the stretch is chosen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StretchParams {
    /// How far below the median, in deviations, the black point is placed.
    ///
    /// Negative, because the black point belongs below the background. The
    /// conventional value clips the bottom of the noise without eating signal.
    pub shadows_clip: f32,
    /// Where the background should end up, from 0 for black to 1 for white.
    pub target_bg: f32,
    /// Whether every colour channel shares one stretch.
    ///
    /// Linked — the default — measures the channels together and applies the
    /// same curve to each, so the camera's colour response survives and a red
    /// nebula stays red. A one-shot colour frame then looks green, because that
    /// is what the sensor recorded: twice as many green sites, a higher green
    /// response, and light pollution weighted towards green.
    ///
    /// Unlinked measures each channel on its own, which puts all three
    /// backgrounds at the same brightness and so renders the sky neutral grey.
    /// It is a colour-correction operation rather than a viewing one — it
    /// weakens real colour along with the cast — but it is how the colour of a
    /// frame is judged before stacking.
    ///
    /// Has no effect on a mono frame, which has one channel either way.
    pub linked: bool,
}

impl Default for StretchParams {
    fn default() -> Self {
        Self {
            shadows_clip: -2.8,
            target_bg: 0.25,
            linked: true,
        }
    }
}

/// A computed stretch for one channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stretch {
    /// Normalised input level that maps to black.
    pub shadows: f32,
    /// Midtone balance, between 0 and 1.
    pub midtones: f32,
    /// Normalised input level that maps to white.
    pub highlights: f32,
}

impl Default for Stretch {
    fn default() -> Self {
        Self::identity()
    }
}

impl Stretch {
    /// The stretch that changes nothing: a straight line from black to white.
    ///
    /// Used when a channel carries nothing to measure, so that an unusual image
    /// still displays rather than turning black.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            shadows: 0.0,
            midtones: 0.5,
            highlights: 1.0,
        }
    }

    /// Whether this stretch leaves the image untouched.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        (self.shadows - 0.0).abs() < f32::EPSILON
            && (self.midtones - 0.5).abs() < f32::EPSILON
            && (self.highlights - 1.0).abs() < f32::EPSILON
    }

    /// Applies this stretch to one normalised sample, returning a normalised
    /// result.
    #[must_use]
    pub fn apply(&self, normalised: f32) -> f32 {
        let span = f64::from(self.highlights) - f64::from(self.shadows);
        if span <= 0.0 {
            return 0.0;
        }
        let t = ((f64::from(normalised) - f64::from(self.shadows)) / span).clamp(0.0, 1.0);
        #[allow(clippy::cast_possible_truncation)]
        {
            mtf(f64::from(self.midtones), t) as f32
        }
    }
}

/// The midtone transfer function.
///
/// `m` is the midtone balance in `(0, 1)`; `x` is the input in `[0, 1]`.
///
/// The property the auto-stretch depends on is that this function inverts its
/// own arguments: if `t = mtf(m, x)` then `mtf(t, x) = m`. That is how
/// [`compute_stretch`] solves for the midtone that puts the background where it
/// is wanted, instead of inverting the function by hand.
///
/// ```
/// # use fits_core::stretch::mtf;
/// // A midtone of one half is the identity.
/// assert!((mtf(0.5, 0.3) - 0.3).abs() < 1e-12);
/// // The arguments swap roles.
/// let t = mtf(0.25, 0.0654206);
/// assert!((mtf(t, 0.0654206) - 0.25).abs() < 1e-9);
/// ```
#[must_use]
pub fn mtf(m: f64, x: f64) -> f64 {
    if !x.is_finite() || x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    if !m.is_finite() || m <= 0.0 {
        return 1.0;
    }
    if m >= 1.0 {
        return 0.0;
    }
    // At exactly one half the denominator's x term vanishes and the function is
    // the identity. Special-cased to avoid relying on the arithmetic.
    if (m - 0.5).abs() < f64::EPSILON {
        return x;
    }
    let denominator = ((2.0 * m - 1.0) * x) - m;
    if denominator == 0.0 {
        return x;
    }
    (((m - 1.0) * x) / denominator).clamp(0.0, 1.0)
}

/// Background statistics for one channel, in normalised units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Background {
    /// Median sample value.
    pub median: f64,
    /// Median absolute deviation, scaled to a standard-deviation equivalent.
    pub sigma: f64,
    /// How many finite samples contributed.
    pub samples: usize,
}

/// Measures the background of a slice of normalised samples.
///
/// Uses a subsample of at most [`MAX_SAMPLES`] values and
/// `select_nth_unstable`, which finds a median in linear time. A full sort of
/// twenty-four million values would dominate the cost of opening an image.
///
/// Non-finite samples are skipped.
#[must_use]
pub fn measure_background(normalised: &[f64]) -> Background {
    let stride = (normalised.len() / MAX_SAMPLES).max(1);
    let mut sample: Vec<f64> = normalised
        .iter()
        .step_by(stride)
        .copied()
        .filter(|v| v.is_finite())
        .collect();

    if sample.is_empty() {
        return Background {
            median: 0.0,
            sigma: 0.0,
            samples: 0,
        };
    }

    let median = median_of(&mut sample);

    let mut deviations: Vec<f64> = sample.iter().map(|v| (v - median).abs()).collect();
    let mad = median_of(&mut deviations);

    Background {
        median,
        sigma: mad * MAD_TO_SIGMA,
        samples: sample.len(),
    }
}

/// Median of a slice, reordering it in the process.
///
/// The slice must be non-empty and contain only finite values.
fn median_of(values: &mut [f64]) -> f64 {
    let middle = values.len() / 2;
    values.select_nth_unstable_by(middle, |a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });
    values[middle]
}

/// Computes the stretch to apply to each channel of an image.
///
/// Returns one [`Stretch`] per channel: length 1 for mono, 3 for colour.
///
/// By default **every channel gets the same stretch, measured from all of them
/// together.** That is deliberate: measuring each colour plane separately puts
/// every channel's background at the same brightness, which is another way of
/// saying it divides out the camera's colour response and renders a red nebula
/// grey. Astronomy tools call this the linked variant, and it is the right
/// default for looking at an image.
///
/// [`StretchParams::linked`] turns it off, measuring each channel separately.
/// That neutralises the sky, which is how the colour of a frame is judged, at
/// the cost of the real colour along with the cast.
///
/// Normalisation always uses the image's overall range, so that the levels the
/// channels are compared against remain the same ones.
#[must_use]
pub fn compute_stretch(image: &FitsImage, params: &StretchParams) -> Vec<Stretch> {
    let channels = image.channels.max(1);
    let span = f64::from(image.max) - f64::from(image.min);
    if span <= 0.0 {
        return vec![Stretch::identity(); channels];
    }
    let low = f64::from(image.min);
    let normalise = |v: f32| (f64::from(v) - low) / span;

    // A mono frame has one channel whichever variant is asked for.
    if params.linked || channels == 1 {
        // One measurement across the whole image, colour planes included.
        let normalised: Vec<f64> = image.data.par_iter().map(|&v| normalise(v)).collect();
        let stretch = stretch_for(&measure_background(&normalised), params);
        return vec![stretch; channels];
    }

    // One measurement per plane. The planes are stored one after another, so
    // each is a contiguous run of the data.
    let pixels = image.width * image.height;
    (0..channels)
        .map(|plane| {
            let start = plane * pixels;
            let end = (start + pixels).min(image.data.len());
            if start >= end {
                return Stretch::identity();
            }
            let normalised: Vec<f64> = image.data[start..end]
                .par_iter()
                .map(|&v| normalise(v))
                .collect();
            stretch_for(&measure_background(&normalised), params)
        })
        .collect()
}

/// Turns measured background statistics into a stretch.
///
/// This is the heart of the algorithm and is separated out so the arithmetic
/// can be tested against known numbers without building an image.
#[must_use]
pub fn stretch_for(background: &Background, params: &StretchParams) -> Stretch {
    if background.samples == 0 {
        return Stretch::identity();
    }

    let median = background.median;
    let clip = f64::from(params.shadows_clip);
    let target = f64::from(params.target_bg).clamp(0.001, 0.999);

    let shadows = (median + clip * background.sigma).clamp(0.0, 1.0);
    let highlights = 1.0;

    let span = highlights - shadows;
    if span <= 0.0 {
        return Stretch::identity();
    }

    // Rescale the median into the shadows-to-highlights window BEFORE solving
    // for the midtone. Solving with the raw difference instead is the classic
    // mistake here: it is silent, and it leaves the background about three
    // hundredths too bright. The worked example in the tests pins this.
    let x0 = (median - shadows) / span;
    if !x0.is_finite() || x0 <= 0.0 || x0 >= 1.0 {
        // No measurable background, as in a constant frame. A straight ramp is
        // more useful than an arbitrary curve.
        return Stretch::identity();
    }

    let midtones = mtf(target, x0).clamp(0.001, 0.999);

    #[allow(clippy::cast_possible_truncation)]
    Stretch {
        shadows: shadows as f32,
        midtones: midtones as f32,
        highlights: highlights as f32,
    }
}

/// Builds the lookup table for a stretch.
///
/// Index by the normalised sample quantised to 16 bits; the value is the
/// display byte. Evaluating the transfer function per pixel would cost a
/// division and several multiplications on every one of twenty-four million
/// samples; through the table it is one multiply, one cast and one index.
///
/// Boxed because the table is 64 KB and has no business on the stack.
#[must_use]
pub fn build_lut(stretch: &Stretch) -> Box<Lut> {
    let mut lut = vec![0u8; LUT_LEN].into_boxed_slice();
    for (index, slot) in lut.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let normalised = index as f32 / (LUT_LEN - 1) as f32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            *slot = (stretch.apply(normalised) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
    // The length is fixed above, so this cannot fail.
    lut.try_into().unwrap_or_else(|_| Box::new([0u8; LUT_LEN]))
}

/// Quantises a normalised sample to a lookup table index.
#[must_use]
pub fn lut_index(normalised: f32) -> usize {
    if !normalised.is_finite() {
        return 0;
    }
    #[allow(clippy::cast_precision_loss)]
    let scaled = normalised.clamp(0.0, 1.0) * (LUT_LEN - 1) as f32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        (scaled.round() as usize).min(LUT_LEN - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_fits_from_bytes;
    use crate::testutil::{gaussian_background, synthetic_fits, SyntheticSpec};
    use approx::assert_relative_eq;

    /// Builds a mono image from physical values.
    fn image(width: usize, height: usize, pixels: &[f64]) -> FitsImage {
        let spec = SyntheticSpec::new(width, height, -32);
        let bytes = synthetic_fits(&spec, pixels).unwrap();
        read_fits_from_bytes(&bytes).unwrap()
    }

    #[test]
    fn a_midtone_of_one_half_is_the_identity() {
        for x in [0.0, 0.1, 0.25, 0.5, 0.75, 1.0] {
            assert_relative_eq!(mtf(0.5, x), x, epsilon = 1e-12);
        }
    }

    #[test]
    fn the_transfer_function_swaps_its_arguments() {
        // The property the whole algorithm rests on: solving for the midtone is
        // a matter of calling the function with the arguments exchanged.
        let mut worst: f64 = 0.0;
        let mut rng = crate::testutil::Prng::new(7);
        for _ in 0..20_000 {
            let x = rng.next_f64().mul_add(0.9998, 0.0001);
            let target = rng.next_f64().mul_add(0.98, 0.01);
            let m = mtf(target, x);
            worst = worst.max((mtf(m, x) - target).abs());
        }
        assert!(worst < 1e-9, "worst error was {worst}");
    }

    #[test]
    fn the_transfer_function_is_monotonic_and_bounded() {
        for m in [0.05, 0.25, 0.5, 0.75, 0.95] {
            let mut previous = -1.0;
            for i in 0..=1000 {
                let x = f64::from(i) / 1000.0;
                let y = mtf(m, x);
                assert!((0.0..=1.0).contains(&y), "mtf({m}, {x}) = {y}");
                assert!(y >= previous - 1e-12, "not monotonic at m={m}, x={x}");
                previous = y;
            }
        }
    }

    #[test]
    fn the_transfer_function_handles_nonsense_without_panicking() {
        for m in [f64::NAN, f64::INFINITY, -1.0, 0.0, 1.0, 2.0] {
            for x in [f64::NAN, f64::INFINITY, -1.0, 0.0, 1.0, 2.0] {
                let y = mtf(m, x);
                assert!((0.0..=1.0).contains(&y), "mtf({m}, {x}) = {y}");
            }
        }
    }

    #[test]
    fn the_worked_example_from_the_plan_reproduces_exactly() {
        // Background median 0.20, sigma 0.02, with the default parameters.
        // These numbers are in README.md; if this test changes, change them too.
        let background = Background {
            median: 0.20,
            sigma: 0.02,
            samples: 1000,
        };
        let stretch = stretch_for(&background, &StretchParams::default());

        assert_relative_eq!(stretch.shadows, 0.144, epsilon = 1e-6);
        assert_relative_eq!(stretch.highlights, 1.0, epsilon = 1e-6);
        assert_relative_eq!(stretch.midtones, 0.173_554, epsilon = 1e-6);

        // The median, put through the whole pipeline, lands on the target.
        let out = stretch.apply(0.20);
        assert_relative_eq!(out, 0.25, epsilon = 1e-6);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let byte = (out * 255.0).round() as u8;
        assert_eq!(byte, 64);
    }

    #[test]
    fn solving_without_the_rescale_would_give_the_wrong_answer() {
        // Guards the step that is easy to skip and silent when skipped. Solving
        // with the raw difference rather than the rescaled one puts the
        // background near 0.282 instead of 0.25.
        let (median, shadows) = (0.20_f64, 0.144_f64);
        let wrong_midtone = mtf(0.25, median - shadows);
        let wrong_result = mtf(wrong_midtone, (median - shadows) / (1.0 - shadows));

        assert!(
            (wrong_result - 0.25).abs() > 0.03,
            "the wrong method should be visibly wrong, got {wrong_result}"
        );
        assert_relative_eq!(wrong_result, 0.282_297, epsilon = 1e-5);
    }

    #[test]
    fn the_background_of_a_noisy_frame_maps_to_the_target() {
        // The end-to-end claim, on data shaped like a real sky background.
        let (w, h) = (400, 400);
        let pixels = gaussian_background(w, h, 1200.0, 40.0, 3);
        let img = image(w, h, &pixels);

        let stretch = compute_stretch(&img, &StretchParams::default());
        assert_eq!(stretch.len(), 1);

        // Put the median physical value through the same normalisation the
        // display uses, then through the stretch.
        let mut sorted = pixels.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median_physical = sorted[sorted.len() / 2];
        let normalised =
            (median_physical - f64::from(img.min)) / (f64::from(img.max) - f64::from(img.min));

        #[allow(clippy::cast_possible_truncation)]
        let out = stretch[0].apply(normalised as f32);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let byte = (out * 255.0).round() as u8;
        let target_byte = (StretchParams::default().target_bg * 255.0).round() as u8;

        assert!(
            byte.abs_diff(target_byte) <= 3,
            "background landed at {byte}, wanted about {target_byte}"
        );
    }

    #[test]
    fn a_stretch_brightens_the_background_substantially() {
        // The reason the feature exists: linearly, a sky background is nearly
        // black.
        let (w, h) = (200, 200);
        let mut pixels = gaussian_background(w, h, 1000.0, 30.0, 5);
        pixels[0] = 60_000.0; // one bright star sets the top of the range
        let img = image(w, h, &pixels);

        let normalised = (1000.0 - f64::from(img.min)) / (f64::from(img.max) - f64::from(img.min));
        #[allow(clippy::cast_possible_truncation)]
        let linear_byte = (normalised * 255.0).round() as u8;
        assert!(linear_byte < 10, "expected a nearly black linear view");

        let stretch = compute_stretch(&img, &StretchParams::default());
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let stretched_byte = (stretch[0].apply(normalised as f32) * 255.0).round() as u8;
        assert!(
            stretched_byte > 50,
            "the stretch should lift the background, got {stretched_byte}"
        );
    }

    #[test]
    fn a_constant_image_falls_back_to_a_straight_ramp() {
        let img = image(8, 8, &vec![500.0; 64]);
        let stretch = compute_stretch(&img, &StretchParams::default());
        assert_eq!(stretch.len(), 1);
        assert!(
            stretch[0].is_identity(),
            "expected the identity, got {:?}",
            stretch[0]
        );
    }

    #[test]
    fn an_all_nan_image_does_not_panic_and_yields_the_identity() {
        let img = image(8, 8, &vec![f64::NAN; 64]);
        let stretch = compute_stretch(&img, &StretchParams::default());
        assert!(stretch[0].is_identity(), "got {:?}", stretch[0]);
    }

    #[test]
    fn a_single_pixel_image_does_not_panic() {
        let img = image(1, 1, &[42.0]);
        let stretch = compute_stretch(&img, &StretchParams::default());
        assert_eq!(stretch.len(), 1);
        assert!(stretch[0].midtones.is_finite());
    }

    #[test]
    fn nan_pixels_do_not_disturb_the_measurement() {
        let (w, h) = (100, 100);
        let mut pixels = gaussian_background(w, h, 800.0, 20.0, 11);
        let clean = compute_stretch(&image(w, h, &pixels), &StretchParams::default())[0];

        // Scatter undefined pixels through the frame.
        for i in (0..pixels.len()).step_by(7) {
            pixels[i] = f64::NAN;
        }
        let with_nan = compute_stretch(&image(w, h, &pixels), &StretchParams::default())[0];

        assert_relative_eq!(clean.shadows, with_nan.shadows, epsilon = 0.02);
        assert_relative_eq!(clean.midtones, with_nan.midtones, epsilon = 0.02);
    }

    /// A colour image whose channels sit at different levels, the way a
    /// one-shot colour sensor records the sky: green highest, red lowest.
    fn unbalanced_colour(width: usize, height: usize) -> FitsImage {
        let pixels = width * height;
        let mut data = Vec::with_capacity(pixels * 3);
        for level in [1000.0f64, 1900.0, 1500.0] {
            for i in 0..pixels {
                #[allow(clippy::cast_precision_loss)]
                data.push(level + (i % 7) as f64);
            }
        }
        let spec = SyntheticSpec::new(width, height, -32).with_channels(3);
        read_fits_from_bytes(&synthetic_fits(&spec, &data).unwrap()).unwrap()
    }

    #[test]
    fn an_unlinked_stretch_puts_every_background_at_the_same_brightness() {
        // What the option is for: the green cast of a one-shot colour frame is
        // the sensor telling the truth, and this is how it is set aside to
        // judge the colour of what was captured.
        let image = unbalanced_colour(32, 32);
        let params = StretchParams {
            linked: false,
            ..StretchParams::default()
        };
        let stretches = compute_stretch(&image, &params);
        assert_eq!(stretches.len(), 3);

        let luts: Vec<_> = stretches.iter().map(build_lut).collect();
        let sample = |plane: usize| {
            let pixels = image.width * image.height;
            // The plane's median pixel, which is the background: the values
            // run level..level+6, so the fourth is the middle one.
            let value = image.data[plane * pixels + 3];
            let span = f64::from(image.max) - f64::from(image.min);
            let normalised = (f64::from(value) - f64::from(image.min)) / span;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let index = (normalised * 65535.0).round() as usize;
            luts[plane][index.min(65535)]
        };

        let (red, green, blue) = (sample(0), sample(1), sample(2));
        let spread = red.max(green).max(blue) - red.min(green).min(blue);
        assert!(
            spread <= 8,
            "the three backgrounds should land together: {red} {green} {blue}"
        );
    }

    #[test]
    fn a_linked_stretch_keeps_the_channels_apart() {
        // The other half of the claim: linked must not neutralise, or the
        // option would mean nothing and real colour would be lost by default.
        let image = unbalanced_colour(32, 32);
        let stretches = compute_stretch(&image, &StretchParams::default());
        assert_eq!(stretches[0], stretches[1]);
        assert_eq!(stretches[1], stretches[2]);
    }

    #[test]
    fn unlinking_does_nothing_to_a_mono_frame() {
        let mut pixels = vec![1000.0f64; 32 * 32];
        pixels[10] = 5000.0;
        let spec = SyntheticSpec::new(32, 32, -32);
        let image = read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap();

        let linked = compute_stretch(&image, &StretchParams::default());
        let unlinked = compute_stretch(
            &image,
            &StretchParams {
                linked: false,
                ..StretchParams::default()
            },
        );
        assert_eq!(linked, unlinked);
    }

    #[test]
    fn every_colour_channel_gets_the_same_stretch() {
        // Measuring each plane separately would put all three backgrounds at
        // the same brightness, which neutralises the colour: a red nebula would
        // render grey. One measurement across the whole image keeps the ratios.
        let (w, h) = (16, 16);
        let mut pixels = vec![0.0; w * h * 3];
        for (i, v) in pixels.iter_mut().enumerate() {
            // Each plane sits at a very different level.
            *v = 1000.0 * (i / (w * h) + 1) as f64;
        }
        let spec = SyntheticSpec::new(w, h, -32).with_channels(3);
        let bytes = synthetic_fits(&spec, &pixels).unwrap();
        let img = read_fits_from_bytes(&bytes).unwrap();

        let stretch = compute_stretch(&img, &StretchParams::default());
        assert_eq!(stretch.len(), 3);
        assert_eq!(stretch[0], stretch[1], "channels must share one stretch");
        assert_eq!(stretch[1], stretch[2], "channels must share one stretch");
    }

    #[test]
    fn a_stretch_preserves_the_order_of_the_colour_channels() {
        let (w, h) = (32, 32);
        let mut pixels = vec![0.0; w * h * 3];
        pixels[..w * h].copy_from_slice(&gaussian_background(w, h, 4000.0, 50.0, 41));
        pixels[w * h..2 * w * h].copy_from_slice(&gaussian_background(w, h, 1500.0, 50.0, 42));
        pixels[2 * w * h..].copy_from_slice(&gaussian_background(w, h, 800.0, 50.0, 43));

        let spec = SyntheticSpec::new(w, h, -32).with_channels(3);
        let bytes = synthetic_fits(&spec, &pixels).unwrap();
        let img = read_fits_from_bytes(&bytes).unwrap();

        let stretch = compute_stretch(&img, &StretchParams::default());
        let span = f64::from(img.max) - f64::from(img.min);
        let low = f64::from(img.min);
        #[allow(clippy::cast_possible_truncation)]
        let at = |physical: f64| stretch[0].apply(((physical - low) / span) as f32);

        assert!(
            at(4000.0) > at(1500.0) && at(1500.0) > at(800.0),
            "the channel order must survive: {} {} {}",
            at(4000.0),
            at(1500.0),
            at(800.0)
        );
    }

    #[test]
    fn the_lookup_table_is_monotonic_across_every_entry() {
        // A table that dips would show as banding or inverted patches.
        let background = Background {
            median: 0.2,
            sigma: 0.02,
            samples: 1000,
        };
        let lut = build_lut(&stretch_for(&background, &StretchParams::default()));
        let mut previous = 0u8;
        for (index, value) in lut.iter().enumerate() {
            assert!(
                *value >= previous,
                "table dips at {index}: {previous} then {value}"
            );
            previous = *value;
        }
        assert_eq!(lut[LUT_LEN - 1], 255, "white should stay white");
        assert_eq!(lut[0], 0, "black should stay black");
    }

    #[test]
    fn the_identity_table_is_a_straight_ramp() {
        let lut = build_lut(&Stretch::identity());
        assert_eq!(lut[0], 0);
        assert_eq!(lut[LUT_LEN - 1], 255);
        assert_eq!(lut[LUT_LEN / 2], 128);
    }

    #[test]
    fn the_table_agrees_with_evaluating_the_stretch_directly() {
        let background = Background {
            median: 0.15,
            sigma: 0.03,
            samples: 1000,
        };
        let stretch = stretch_for(&background, &StretchParams::default());
        let lut = build_lut(&stretch);

        for step in 0..=100 {
            #[allow(clippy::cast_precision_loss)]
            let x = step as f32 / 100.0;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let direct = (stretch.apply(x) * 255.0).round() as u8;
            let through_table = lut[lut_index(x)];
            assert!(
                direct.abs_diff(through_table) <= 1,
                "at {x}: direct {direct}, table {through_table}"
            );
        }
    }

    #[test]
    fn table_indexing_stays_in_range_for_any_input() {
        for x in [
            -100.0,
            -0.0,
            0.0,
            0.5,
            1.0,
            100.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            let index = lut_index(x);
            assert!(index < LUT_LEN, "index {index} for input {x}");
        }
    }

    #[test]
    fn background_measurement_of_an_empty_slice_is_safe() {
        let background = measure_background(&[]);
        assert_eq!(background.samples, 0);
        assert!(stretch_for(&background, &StretchParams::default()).is_identity());
    }

    #[test]
    fn background_measurement_subsamples_a_large_frame() {
        // Twenty-four million values must not be sorted in full.
        let data: Vec<f64> = (0..2_000_000)
            .map(|i| f64::from(i % 1000) / 1000.0)
            .collect();
        let background = measure_background(&data);
        assert!(
            background.samples <= MAX_SAMPLES,
            "used {} samples",
            background.samples
        );
        assert!(background.median > 0.0);
    }

    #[test]
    fn the_target_background_setting_moves_the_result() {
        let background = Background {
            median: 0.2,
            sigma: 0.02,
            samples: 1000,
        };
        let dim = stretch_for(
            &background,
            &StretchParams {
                target_bg: 0.10,
                ..Default::default()
            },
        );
        let bright = stretch_for(
            &background,
            &StretchParams {
                target_bg: 0.40,
                ..Default::default()
            },
        );
        assert!(
            bright.apply(0.2) > dim.apply(0.2),
            "a higher target should give a brighter background"
        );
        assert_relative_eq!(dim.apply(0.2), 0.10, epsilon = 1e-5);
        assert_relative_eq!(bright.apply(0.2), 0.40, epsilon = 1e-5);
    }

    #[test]
    fn the_shadow_clip_setting_moves_the_black_point() {
        let background = Background {
            median: 0.2,
            sigma: 0.02,
            samples: 1000,
        };
        let gentle = stretch_for(
            &background,
            &StretchParams {
                shadows_clip: -5.0,
                ..Default::default()
            },
        );
        let aggressive = stretch_for(
            &background,
            &StretchParams {
                shadows_clip: -1.0,
                ..Default::default()
            },
        );
        assert!(
            aggressive.shadows > gentle.shadows,
            "clipping closer to the median should raise the black point"
        );
    }

    #[test]
    fn extreme_parameters_do_not_produce_a_broken_stretch() {
        let background = Background {
            median: 0.2,
            sigma: 0.02,
            samples: 1000,
        };
        for shadows_clip in [-100.0, -0.0, 0.0, 100.0, f32::NAN] {
            for target_bg in [-1.0, 0.0, 0.5, 1.0, 2.0, f32::NAN] {
                let s = stretch_for(
                    &background,
                    &StretchParams {
                        shadows_clip,
                        target_bg,
                        ..StretchParams::default()
                    },
                );
                assert!(
                    s.shadows.is_finite() && s.midtones.is_finite() && s.highlights.is_finite(),
                    "clip {shadows_clip}, target {target_bg} gave {s:?}"
                );
                assert!((0.0..=1.0).contains(&s.apply(0.5)));
            }
        }
    }
}

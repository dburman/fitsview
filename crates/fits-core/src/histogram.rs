//! How a frame's samples are distributed.
//!
//! An astronomical frame is mostly sky at one level, with a thin tail of stars
//! running to the top of the range. Seeing that distribution is how you tell a
//! frame that is merely faint from one that is clipped, and how you judge what
//! the stretch is doing rather than guessing at it.

use crate::image::FitsImage;
use crate::stretch::MAX_SAMPLES;

/// Number of buckets. Enough to see the shape, few enough to draw.
pub const BINS: usize = 256;

/// The distribution of a frame's samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Histogram {
    /// How many samples fell in each bucket, from `low` to `high`.
    pub bins: Vec<u32>,
    /// The most any single bucket holds, for scaling a drawing.
    pub peak: u32,
    /// How many finite samples were counted.
    pub counted: u64,
}

impl Histogram {
    /// An empty distribution, for a frame with nothing to measure.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            bins: vec![0; BINS],
            peak: 0,
            counted: 0,
        }
    }

    /// Whether anything was counted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.counted == 0
    }

    /// The height of a bucket relative to the tallest, from 0 to 1.
    ///
    /// Square-rooted, because a sky background puts almost every sample into a
    /// handful of buckets and a linear plot would show one spike and nothing
    /// else. The square root keeps the stars visible without the dishonesty of
    /// a logarithm, which would make an empty bucket look occupied.
    #[must_use]
    pub fn height(&self, bin: usize) -> f32 {
        let Some(count) = self.bins.get(bin) else {
            return 0.0;
        };
        if self.peak == 0 || *count == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            (*count as f32 / self.peak as f32).sqrt()
        }
    }
}

/// Counts a frame's samples into buckets, between its own extremes.
///
/// Uses the same subsampling as the stretch, so this costs little on top of
/// opening an image. Non-finite samples are skipped rather than counted
/// anywhere, since they represent no measurement at all.
#[must_use]
pub fn compute(image: &FitsImage) -> Histogram {
    let span = f64::from(image.max) - f64::from(image.min);
    if image.data.is_empty() || span <= 0.0 {
        return Histogram::empty();
    }
    let low = f64::from(image.min);

    let stride = (image.data.len() / MAX_SAMPLES).max(1);
    let mut bins = vec![0u32; BINS];
    let mut counted = 0u64;

    for value in image.data.iter().step_by(stride) {
        if !value.is_finite() {
            continue;
        }
        let position = (f64::from(*value) - low) / span;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bin = ((position * BINS as f64) as usize).min(BINS - 1);
        bins[bin] += 1;
        counted += 1;
    }

    let peak = bins.iter().copied().max().unwrap_or(0);
    Histogram {
        bins,
        peak,
        counted,
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

    #[test]
    fn every_finite_sample_is_counted_exactly_once() {
        let pixels: Vec<f64> = (0..1024).map(f64::from).collect();
        let h = compute(&image(32, 32, &pixels));

        assert_eq!(h.counted, 1024);
        let total: u64 = h.bins.iter().map(|c| u64::from(*c)).sum();
        assert_eq!(total, h.counted, "the bins must account for every sample");
    }

    #[test]
    fn undefined_samples_are_not_counted_anywhere() {
        let mut pixels: Vec<f64> = (0..256).map(f64::from).collect();
        for i in (0..256).step_by(4) {
            pixels[i] = f64::NAN;
        }
        let h = compute(&image(16, 16, &pixels));

        assert_eq!(h.counted, 192, "a quarter of the samples were undefined");
        let total: u64 = h.bins.iter().map(|c| u64::from(*c)).sum();
        assert_eq!(total, 192);
    }

    #[test]
    fn a_frame_of_only_undefined_samples_is_empty_rather_than_a_panic() {
        let h = compute(&image(8, 8, &vec![f64::NAN; 64]));
        assert!(h.is_empty());
        assert_eq!(h.peak, 0);
        // Drawing an empty histogram must be safe too.
        assert!((h.height(0) - 0.0).abs() < f32::EPSILON);
        assert!((h.height(BINS - 1) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_constant_frame_is_a_single_spike() {
        // Its min and max fall back to 0 and 1 rather than being equal, since
        // `finite_min_max` guards callers against dividing by zero. Every
        // sample therefore lands in one bin, which is an honest picture of a
        // frame that holds one value.
        let h = compute(&image(8, 8, &vec![7.0; 64]));
        assert_eq!(h.counted, 64);
        let occupied = h.bins.iter().filter(|c| **c > 0).count();
        assert_eq!(occupied, 1, "one value belongs in one bucket");
        assert_eq!(h.peak, 64);
    }

    #[test]
    fn the_darkest_and_brightest_samples_land_at_the_ends() {
        let pixels = vec![0.0, 50.0, 100.0, 100.0];
        let h = compute(&image(2, 2, &pixels));
        assert_eq!(h.bins[0], 1, "the darkest sample belongs in the first bin");
        assert_eq!(h.bins[BINS - 1], 2, "the brightest belong in the last");
    }

    #[test]
    fn a_sky_background_makes_one_tall_peak() {
        // The shape every astronomical frame has, and the reason the drawing is
        // square-rooted rather than linear.
        let (w, h) = (200, 200);
        let mut pixels = gaussian_background(w, h, 1000.0, 20.0, 4);
        pixels[0] = 60_000.0; // one star setting the top of the range
        let hist = compute(&image(w, h, &pixels));

        assert!(hist.counted > 0);
        let tallest = hist
            .bins
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| **c)
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            tallest < 20,
            "the sky should sit near the bottom of the range"
        );
        assert!(
            f64::from(hist.peak) > 0.5 * hist.counted as f64,
            "most samples should fall in one bucket"
        );
    }

    #[test]
    fn heights_are_between_zero_and_one_and_the_peak_is_one() {
        let (w, h) = (64, 64);
        let hist = compute(&image(w, h, &gaussian_background(w, h, 500.0, 30.0, 8)));
        let mut tallest = 0.0f32;
        for bin in 0..BINS {
            let height = hist.height(bin);
            assert!((0.0..=1.0).contains(&height), "bin {bin} gave {height}");
            tallest = tallest.max(height);
        }
        assert!(
            (tallest - 1.0).abs() < f32::EPSILON,
            "the peak should reach 1"
        );
    }

    #[test]
    fn asking_for_a_bin_that_does_not_exist_is_safe() {
        let hist = compute(&image(4, 4, &(0..16).map(f64::from).collect::<Vec<_>>()));
        assert!((hist.height(BINS) - 0.0).abs() < f32::EPSILON);
        assert!((hist.height(usize::MAX) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_large_frame_is_subsampled_rather_than_counted_in_full() {
        let (w, h) = (2000, 1000);
        let hist = compute(&image(w, h, &gaussian_background(w, h, 800.0, 25.0, 6)));
        assert!(
            hist.counted <= MAX_SAMPLES as u64,
            "counted {} samples",
            hist.counted
        );
        assert!(hist.counted > 0);
    }
}

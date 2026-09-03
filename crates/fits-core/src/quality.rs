//! Measuring what a frame looks like, so bad ones can be found without
//! inspecting every one by eye.
//!
//! Going through two hundred frames is the job this application exists for, and
//! looking at each in turn is slow. A number per frame lets the ones ruined by
//! cloud, wind or a tracking error sort themselves to the top.
//!
//! **These are comparisons within a folder, not absolute measures.** They depend
//! on exposure, gain, the target and the sky, so a value means something only
//! next to the other frames from the same session. Nothing here is a star
//! measurement; that needs star detection, which is a larger piece of work.

use crate::image::FitsImage;
use crate::stretch::{measure_background, MAX_SAMPLES};

/// What a frame looks like, for comparing it with its neighbours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quality {
    /// Median sample value: the sky background.
    ///
    /// Cloud, moonlight and dawn all raise it. The single most useful number
    /// for finding frames worth discarding.
    pub background: f64,

    /// Spread of the background, from the median absolute deviation.
    pub noise: f64,

    /// How much structure the frame holds, relative to its own noise.
    ///
    /// The mean squared difference between horizontally adjacent samples,
    /// divided by what that difference would be for noise alone. A value near
    /// 1 means the frame is almost entirely noise; higher means real
    /// structure. Blur, cloud and tracking errors all reduce it.
    ///
    /// It is a proxy, not a focus measurement. It finds gross problems, not
    /// small differences in focus.
    pub sharpness: f64,
}

impl Quality {
    /// A measurement of nothing, for a frame with no usable samples.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            background: 0.0,
            noise: 0.0,
            sharpness: 0.0,
        }
    }
}

/// Measures a frame.
///
/// Uses the same subsampling the stretch does, so this costs little on top of
/// what opening an image already pays.
///
/// Measure the **raw** frame, before calibration and debayering, so that
/// numbers stay comparable however the display happens to be configured.
#[must_use]
pub fn measure(image: &FitsImage) -> Quality {
    if image.data.is_empty() || image.width == 0 {
        return Quality::none();
    }

    // The background statistics, in the image's own units rather than
    // normalised, so they can be compared with a neighbouring frame.
    let sample: Vec<f64> = subsample(image);
    if sample.is_empty() {
        return Quality::none();
    }
    let background = measure_background(&sample);

    let gradient = mean_squared_gradient(image);
    // For uncorrelated noise of deviation s, the mean squared difference
    // between neighbours is 2s^2. Dividing by that puts a frame of pure noise
    // at about 1, and anything with real structure above it.
    let noise_floor = 2.0 * background.sigma * background.sigma;
    let sharpness = if noise_floor > 0.0 && gradient.is_finite() {
        gradient / noise_floor
    } else {
        0.0
    };

    Quality {
        background: background.median,
        noise: background.sigma,
        sharpness,
    }
}

/// Up to [`MAX_SAMPLES`] finite samples, evenly spread through the frame.
fn subsample(image: &FitsImage) -> Vec<f64> {
    let stride = (image.data.len() / MAX_SAMPLES).max(1);
    image
        .data
        .iter()
        .step_by(stride)
        .filter(|v| v.is_finite())
        .map(|v| f64::from(*v))
        .collect()
}

/// Mean squared difference between horizontally adjacent samples.
///
/// Rows are subsampled rather than pixels, because taking every k-th pixel
/// would destroy the adjacency the measure depends on.
fn mean_squared_gradient(image: &FitsImage) -> f64 {
    if image.width < 2 || image.height == 0 {
        return 0.0;
    }
    // Enough rows for a million samples, and at least one.
    let wanted_rows = (MAX_SAMPLES / image.width).max(1);
    let stride = (image.height / wanted_rows).max(1);

    let mut total = 0.0f64;
    let mut count = 0u64;

    for y in (0..image.height).step_by(stride) {
        let row = &image.data[y * image.width..(y + 1) * image.width];
        for pair in row.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if a.is_finite() && b.is_finite() {
                let difference = f64::from(b) - f64::from(a);
                total += difference * difference;
                count += 1;
            }
        }
    }

    if count == 0 {
        0.0
    } else {
        #[allow(clippy::cast_precision_loss)]
        {
            total / count as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_fits_from_bytes;
    use crate::testutil::{gaussian_background, synthetic_fits, Prng, SyntheticSpec};

    fn image(width: usize, height: usize, pixels: &[f64]) -> FitsImage {
        let spec = SyntheticSpec::new(width, height, -32);
        read_fits_from_bytes(&synthetic_fits(&spec, pixels).unwrap()).unwrap()
    }

    /// A sky background with stars on it.
    fn starfield(width: usize, height: usize, stars: usize, seed: u64) -> Vec<f64> {
        let mut pixels = gaussian_background(width, height, 1000.0, 20.0, seed);
        let mut rng = Prng::new(seed.wrapping_add(1));
        for _ in 0..stars {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let x = (rng.next_f64() * (width - 6) as f64) as usize + 3;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let y = (rng.next_f64() * (height - 6) as f64) as usize + 3;
            pixels[y * width + x] += 40_000.0;
        }
        pixels
    }

    /// Blurs with a 3x3 box, which is what defocus or poor tracking looks like.
    fn blurred(width: usize, height: usize, pixels: &[f64]) -> Vec<f64> {
        let mut out = pixels.to_vec();
        for y in 1..height - 1 {
            for x in 1..width - 1 {
                let mut total = 0.0;
                for dy in 0..3 {
                    for dx in 0..3 {
                        total += pixels[(y + dy - 1) * width + x + dx - 1];
                    }
                }
                out[y * width + x] = total / 9.0;
            }
        }
        out
    }

    #[test]
    fn a_blurred_frame_measures_less_sharp_than_the_same_frame_unblurred() {
        // The claim the whole measure rests on. If this does not hold, the
        // number is decoration.
        let (w, h) = (200, 200);
        let sharp = starfield(w, h, 300, 5);
        let soft = blurred(w, h, &sharp);

        let sharp_q = measure(&image(w, h, &sharp));
        let soft_q = measure(&image(w, h, &soft));

        assert!(
            soft_q.sharpness < sharp_q.sharpness,
            "blurring should reduce sharpness: sharp {:.3}, soft {:.3}",
            sharp_q.sharpness,
            soft_q.sharpness
        );
    }

    #[test]
    fn blurring_twice_measures_less_sharp_than_blurring_once() {
        // The measure should be graded rather than merely different.
        let (w, h) = (160, 160);
        let sharp = starfield(w, h, 200, 9);
        let once = blurred(w, h, &sharp);
        let twice = blurred(w, h, &once);

        let a = measure(&image(w, h, &sharp)).sharpness;
        let b = measure(&image(w, h, &once)).sharpness;
        let c = measure(&image(w, h, &twice)).sharpness;
        assert!(
            a > b && b > c,
            "expected a graded response: {a:.3} {b:.3} {c:.3}"
        );
    }

    #[test]
    fn a_frame_of_pure_noise_measures_about_one() {
        // The normalisation is chosen so noise alone sits near 1, which is what
        // makes a value comparable between frames of different brightness.
        let (w, h) = (256, 256);
        let q = measure(&image(w, h, &gaussian_background(w, h, 1000.0, 30.0, 3)));
        assert!(
            (q.sharpness - 1.0).abs() < 0.15,
            "pure noise measured {:.3}, expected about 1",
            q.sharpness
        );
    }

    #[test]
    fn a_frame_with_stars_measures_above_pure_noise() {
        let (w, h) = (200, 200);
        let empty = measure(&image(w, h, &gaussian_background(w, h, 1000.0, 20.0, 11)));
        let stars = measure(&image(w, h, &starfield(w, h, 400, 11)));
        assert!(
            stars.sharpness > empty.sharpness,
            "stars {:.3} should exceed empty sky {:.3}",
            stars.sharpness,
            empty.sharpness
        );
    }

    #[test]
    fn a_raised_background_is_detected_and_barely_moves_the_sharpness() {
        // Cloud and moonlight raise the background. That must show up, and it
        // must not be confused with the frame becoming blurred.
        let (w, h) = (200, 200);
        let base = starfield(w, h, 300, 7);
        let hazy: Vec<f64> = base.iter().map(|v| v + 5_000.0).collect();

        let clear = measure(&image(w, h, &base));
        let cloudy = measure(&image(w, h, &hazy));

        assert!(
            cloudy.background > clear.background + 4_000.0,
            "the raised background should show: {:.0} against {:.0}",
            cloudy.background,
            clear.background
        );
        assert!(
            (cloudy.sharpness - clear.sharpness).abs() < clear.sharpness * 0.1,
            "a constant offset should barely change sharpness: {:.3} against {:.3}",
            cloudy.sharpness,
            clear.sharpness
        );
    }

    #[test]
    fn the_background_is_reported_in_the_frames_own_units() {
        // So it can be compared with a neighbouring frame directly.
        let (w, h) = (64, 64);
        let q = measure(&image(w, h, &gaussian_background(w, h, 1234.0, 10.0, 2)));
        assert!(
            (q.background - 1234.0).abs() < 5.0,
            "background was {:.1}",
            q.background
        );
        assert!((q.noise - 10.0).abs() < 2.0, "noise was {:.1}", q.noise);
    }

    #[test]
    fn undefined_pixels_do_not_disturb_the_measurement() {
        let (w, h) = (128, 128);
        let clean = starfield(w, h, 150, 13);
        let mut holed = clean.clone();
        for i in (0..holed.len()).step_by(17) {
            holed[i] = f64::NAN;
        }

        let a = measure(&image(w, h, &clean));
        let b = measure(&image(w, h, &holed));
        assert!(
            (a.background - b.background).abs() < 20.0,
            "background moved from {:.1} to {:.1}",
            a.background,
            b.background
        );
        assert!(b.sharpness.is_finite() && b.sharpness > 0.0);
    }

    #[test]
    fn degenerate_frames_do_not_panic() {
        for (w, h, pixels) in [
            (1usize, 1usize, vec![5.0]),
            (2, 2, vec![7.0; 4]),
            (4, 4, vec![f64::NAN; 16]),
            (8, 1, vec![0.0; 8]),
        ] {
            let q = measure(&image(w, h, &pixels));
            assert!(q.background.is_finite(), "{w}x{h}");
            assert!(q.noise.is_finite() && q.noise >= 0.0, "{w}x{h}");
            assert!(q.sharpness.is_finite() && q.sharpness >= 0.0, "{w}x{h}");
        }
    }

    #[test]
    fn a_constant_frame_has_no_noise_and_no_structure() {
        let q = measure(&image(16, 16, &vec![500.0; 256]));
        assert!((q.background - 500.0).abs() < 0.01);
        assert!(q.noise.abs() < 0.01);
        assert!(q.sharpness.abs() < 0.01, "sharpness was {}", q.sharpness);
    }

    #[test]
    fn measuring_is_deterministic() {
        let (w, h) = (100, 100);
        let pixels = starfield(w, h, 100, 21);
        let a = measure(&image(w, h, &pixels));
        let b = measure(&image(w, h, &pixels));
        assert_eq!(a, b);
    }
}

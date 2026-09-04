//! The matched filter: a small blur applied before looking for stars.
//!
//! Thresholding raw pixels asks each pixel on its own whether it is bright,
//! which is the wrong question. A star is not one bright pixel; it is a small
//! round group of them, and the noise is not. Blurring with a kernel about the
//! size of a star adds up the star's own pixels while averaging the noise
//! down, so a real star stands further above the noise afterwards than before
//! and a single hot pixel stands far less.
//!
//! This is why source detection is conventionally quoted at a few deviations:
//! the threshold is applied to the filtered frame, not the raw one. Without
//! this step a threshold has to be raised until it rejects noise by brute
//! force, which throws away the faint stars with it.
//!
//! Stars are **measured** on the original frame. A blur widens everything it
//! touches, and a width measured on a blurred frame would be the star and the
//! kernel together.

use rayon::prelude::*;

use crate::image::FitsImage;

/// Half-width of the kernel, in standard deviations.
///
/// Beyond three deviations a Gaussian contributes less than a hundredth of a
/// per cent, which no threshold can see.
const REACH: f64 = 3.0;

/// Blurs a frame with a Gaussian of standard deviation `sigma`.
///
/// The kernel is separable, so this is two one-dimensional passes rather than
/// one two-dimensional one: for a radius of three that is fourteen
/// multiplications a pixel instead of forty-nine.
///
/// Undefined pixels are skipped rather than spread: each output is the
/// weighted mean of the finite inputs under the kernel, so a frame with dead
/// pixels blurs without growing holes around them. A pixel with nothing finite
/// anywhere near it stays undefined.
#[must_use]
pub fn gaussian_blur(image: &FitsImage, sigma: f64) -> Vec<f32> {
    let (width, height) = (image.width, image.height);
    if sigma <= 0.0 || width == 0 || height == 0 {
        return image.data.clone();
    }

    let kernel = gaussian_kernel(sigma);
    let radius = kernel.len() / 2;

    // Which rows hold an undefined pixel. Almost always none of them, and
    // knowing that lets both passes drop the test from their inner loops: a
    // branch on every tap is what stops a compiler vectorising them, and there
    // are eighteen taps per pixel.
    let ragged: Vec<bool> = image
        .data
        .par_chunks(width)
        .map(|row| row.iter().any(|v| !v.is_finite()))
        .collect();

    // Horizontal, then vertical, each row independent. Both passes split the
    // row into an interior, where the whole kernel fits and the loop can run
    // without a single bounds decision, and the two edges, which are a few
    // pixels each and take the careful path.
    let mut across = vec![f32::NAN; image.data.len()];
    across
        .par_chunks_mut(width)
        .enumerate()
        .for_each(|(y, out)| {
            let row = &image.data[y * width..y * width + width];
            blur_row(row, out, &kernel, radius, ragged[y]);
        });

    let mut down = vec![f32::NAN; image.data.len()];
    // The vertical pass adds whole rows together rather than walking each
    // column. A column runs down memory with the width of the frame between
    // its pixels, so reading one costs a cache miss per pixel; adding row into
    // row reads every input in order and lets the processor vectorise it.
    down.par_chunks_mut(width).enumerate().for_each_init(
        || vec![0.0f32; width],
        |weights, (y, out)| {
            out.fill(0.0);
            weights.fill(0.0);

            let first = y.saturating_sub(radius);
            let last = (y + radius).min(height - 1);
            // The horizontal pass can only have written an undefined pixel
            // into a row that had one within reach.
            let clean = !ragged[first..=last].iter().any(|r| *r);

            if clean {
                // Every tap is a number, so the weight is the same for the
                // whole row and the loop is a plain multiply-and-add.
                let mut total_weight = 0.0f32;
                for row in first..=last {
                    let k = kernel[radius + row - y];
                    total_weight += k;
                    let source = &across[row * width..row * width + width];
                    for (slot, value) in out.iter_mut().zip(source) {
                        *slot += *value * k;
                    }
                }
                for slot in out.iter_mut() {
                    *slot /= total_weight;
                }
                return;
            }

            weights.fill(0.0);
            for row in first..=last {
                let k = kernel[radius + row - y];
                let source = &across[row * width..row * width + width];
                for ((slot, weight), value) in out.iter_mut().zip(weights.iter_mut()).zip(source) {
                    if value.is_finite() {
                        *slot += *value * k;
                        *weight += k;
                    }
                }
            }

            for (slot, weight) in out.iter_mut().zip(weights.iter()) {
                *slot = if *weight > 0.0 {
                    *slot / *weight
                } else {
                    f32::NAN
                };
            }
        },
    );
    down
}

/// One row of the horizontal pass.
fn blur_row(row: &[f32], out: &mut [f32], kernel: &[f32], radius: usize, ragged: bool) {
    let width = row.len();
    if width <= radius * 2 + 1 {
        for (x, slot) in out.iter_mut().enumerate() {
            *slot = weighted(row, x, radius, kernel);
        }
        return;
    }

    for (x, slot) in out.iter_mut().enumerate().take(radius) {
        *slot = weighted(row, x, radius, kernel);
    }
    for (x, slot) in out.iter_mut().enumerate().skip(width - radius) {
        *slot = weighted(row, x, radius, kernel);
    }

    // The interior, where every tap is inside the row. When the row holds no
    // undefined pixel the weights sum to one by construction, so this is a
    // plain dot product with nothing to test and nothing to divide.
    if ragged {
        for x in radius..width - radius {
            let window = &row[x - radius..=x + radius];
            let mut total = 0.0f32;
            let mut weight = 0.0f32;
            for (value, k) in window.iter().zip(kernel) {
                if value.is_finite() {
                    total += *value * *k;
                    weight += *k;
                }
            }
            out[x] = if weight > 0.0 {
                total / weight
            } else {
                f32::NAN
            };
        }
    } else {
        for x in radius..width - radius {
            let window = &row[x - radius..=x + radius];
            let mut total = 0.0f32;
            for (value, k) in window.iter().zip(kernel) {
                total += *value * *k;
            }
            out[x] = total;
        }
    }
}

/// Weighted mean of the finite values under the kernel, along a slice.
///
/// The careful path, for pixels near an end where part of the kernel hangs off
/// the frame.
fn weighted(values: &[f32], at: usize, radius: usize, kernel: &[f32]) -> f32 {
    let mut total = 0.0f32;
    let mut weight = 0.0f32;
    let first = at.saturating_sub(radius);
    let last = (at + radius).min(values.len() - 1);
    for position in first..=last {
        let value = values[position];
        if value.is_finite() {
            let k = kernel[radius + position - at];
            total += value * k;
            weight += k;
        }
    }
    if weight > 0.0 {
        total / weight
    } else {
        f32::NAN
    }
}

/// How much the blur shrinks the noise, as a factor on its deviation.
///
/// For noise that is independent pixel to pixel, a weighted mean has the
/// deviation of its inputs times the root of the sum of its squared weights.
/// The two-dimensional kernel is the outer product of the one-dimensional one,
/// so the sum of its squares is the square of the one-dimensional sum, and the
/// root of that is the sum itself.
///
/// This is why the noise cannot simply be measured on the blurred frame: a
/// blur makes neighbouring pixels alike, and every estimator that leans on
/// neighbours — including the one this crate uses, which differences them to
/// escape gradients — reads that likeness as an absence of noise and returns
/// almost zero. Measure on the frame as it came, then scale.
#[must_use]
pub fn noise_attenuation(sigma: f64) -> f64 {
    if sigma <= 0.0 {
        return 1.0;
    }
    gaussian_kernel(sigma)
        .iter()
        .map(|k| f64::from(*k) * f64::from(*k))
        .sum()
}

/// A normalised one-dimensional Gaussian kernel.
fn gaussian_kernel(sigma: f64) -> Vec<f32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let radius = ((REACH * sigma).ceil() as usize).max(1);
    let mut kernel: Vec<f32> = Vec::with_capacity(radius * 2 + 1);
    let mut total = 0.0f64;
    for offset in -(radius as i64)..=(radius as i64) {
        #[allow(clippy::cast_precision_loss)]
        let d = offset as f64;
        let weight = (-d * d / (2.0 * sigma * sigma)).exp();
        total += weight;
        #[allow(clippy::cast_possible_truncation)]
        kernel.push(weight as f32);
    }
    #[allow(clippy::cast_possible_truncation)]
    let total = total as f32;
    for weight in &mut kernel {
        *weight /= total;
    }
    kernel
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

    /// Noise deviation of a slice, from neighbouring differences.
    fn noise_of(values: &[f32], width: usize) -> f64 {
        let mut total = 0.0f64;
        let mut count = 0u32;
        for row in values.chunks(width) {
            for pair in row.windows(2) {
                if pair[0].is_finite() && pair[1].is_finite() {
                    let d = f64::from(pair[1] - pair[0]);
                    total += d * d;
                    count += 1;
                }
            }
        }
        (total / f64::from(count) / 2.0).sqrt()
    }

    #[test]
    fn the_kernel_sums_to_one() {
        // Anything else would change the brightness of the whole frame.
        for sigma in [0.5, 1.0, 1.5, 3.0] {
            let kernel = gaussian_kernel(sigma);
            let total: f32 = kernel.iter().sum();
            assert!(
                (total - 1.0).abs() < 1e-5,
                "sigma {sigma} summed to {total}"
            );
            assert_eq!(kernel.len() % 2, 1, "the kernel must have a centre");
        }
    }

    #[test]
    fn blurring_leaves_a_flat_field_flat() {
        let (w, h) = (64usize, 64usize);
        let pixels = vec![500.0f64; w * h];
        let blurred = gaussian_blur(&image(w, h, &pixels), 1.5);
        for value in &blurred {
            assert!(
                (value - 500.0).abs() < 0.01,
                "a flat field came back as {value}"
            );
        }
    }

    #[test]
    fn a_star_stands_further_above_the_noise_after_filtering() {
        // The claim the whole module rests on. A star of a given brightness
        // must be more detectable after the filter, not less.
        let (w, h) = (256usize, 256usize);
        let mut pixels = gaussian_background(w, h, 1000.0, 20.0, 21);
        let (cx, cy) = (128.0f64, 128.0f64);
        for dy in -8i64..=8 {
            for dx in -8i64..=8 {
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let index = (cy as i64 + dy) as usize * w + (cx as i64 + dx) as usize;
                #[allow(clippy::cast_precision_loss)]
                let r = (dx * dx + dy * dy) as f64;
                pixels[index] += 60.0 * (-r / 8.0).exp();
            }
        }
        let frame = image(w, h, &pixels);

        let raw_noise = noise_of(&frame.data, w);
        let raw_peak = f64::from(frame.data[128 * w + 128]) - 1000.0;

        let blurred = gaussian_blur(&frame, 1.5);
        let filtered_noise = noise_of(&blurred, w);
        let filtered_peak = f64::from(blurred[128 * w + 128]) - 1000.0;

        let before = raw_peak / raw_noise;
        let after = filtered_peak / filtered_noise;
        assert!(
            after > before * 1.5,
            "signal to noise went from {before:.2} to {after:.2}; the filter must improve it"
        );
    }

    #[test]
    fn a_single_hot_pixel_is_suppressed() {
        // The other half of the claim: what the filter does to a spike that is
        // not a star. Hot pixels and cosmic rays are the commonest thing that
        // is bright and is not a star.
        let (w, h) = (128usize, 128usize);
        let mut pixels = vec![1000.0f64; w * h];
        pixels[64 * w + 64] = 6000.0;
        let frame = image(w, h, &pixels);

        let blurred = gaussian_blur(&frame, 1.5);
        let before = 5000.0;
        let after = f64::from(blurred[64 * w + 64]) - 1000.0;
        assert!(
            after < before * 0.15,
            "a hot pixel kept {after} of its {before}; the filter should flatten it"
        );
    }

    #[test]
    fn undefined_pixels_do_not_spread() {
        // A dead pixel must not eat a hole the size of the kernel out of the
        // frame around it.
        let (w, h) = (64usize, 64usize);
        let mut pixels = vec![800.0f64; w * h];
        pixels[32 * w + 32] = f64::NAN;
        let blurred = gaussian_blur(&image(w, h, &pixels), 1.5);

        for (offset, name) in [(1usize, "next to"), (3, "near")] {
            let value = blurred[32 * w + 32 + offset];
            assert!(
                value.is_finite() && (value - 800.0).abs() < 1.0,
                "the pixel {name} an undefined one came back as {value}"
            );
        }
    }

    #[test]
    fn a_blur_of_nothing_returns_the_frame_unchanged() {
        let (w, h) = (32usize, 32usize);
        let pixels = gaussian_background(w, h, 100.0, 3.0, 22);
        let frame = image(w, h, &pixels);
        assert_eq!(gaussian_blur(&frame, 0.0), frame.data);
    }

    #[test]
    fn the_blur_is_symmetric() {
        // An off-by-one in the kernel offsets would shift every star by a
        // pixel, which would quietly bias every centroid.
        let (w, h) = (65usize, 65usize);
        let mut pixels = vec![0.0f64; w * h];
        pixels[32 * w + 32] = 1000.0;
        let blurred = gaussian_blur(&image(w, h, &pixels), 2.0);

        for offset in 1..=4usize {
            let left = blurred[32 * w + 32 - offset];
            let right = blurred[32 * w + 32 + offset];
            let up = blurred[(32 - offset) * w + 32];
            let down = blurred[(32 + offset) * w + 32];
            assert!((left - right).abs() < 1e-4, "{left} vs {right}");
            assert!((up - down).abs() < 1e-4, "{up} vs {down}");
            assert!((left - up).abs() < 1e-4, "not round: {left} vs {up}");
        }
    }
}

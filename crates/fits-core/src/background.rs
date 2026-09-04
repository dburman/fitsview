//! The sky background, measured tile by tile rather than once for the frame.
//!
//! A single number for the whole frame is a poor description of a real
//! sub-exposure. Light pollution runs a gradient across it, amp glow lifts one
//! corner, and vignetting darkens the edges. Threshold against one global
//! median and the bright half of the frame crosses it everywhere at once,
//! producing detections that are the gradient rather than the sky.
//!
//! So the frame is divided into tiles, each tile is measured on its own, and
//! the value at a pixel is interpolated between the surrounding tile centres.
//! This is the standard approach, and the reason detection thresholds can be
//! stated in noise deviations and mean the same thing across a frame.

use rayon::prelude::*;

use crate::image::FitsImage;
use crate::stretch::MAD_TO_SIGMA;

/// Side of a tile, in pixels.
///
/// Large enough that a tile holds thousands of sky pixels and a few stars
/// cannot move its median, small enough to follow a gradient across a frame.
/// A 24-megapixel frame divides into about 1,500 of them.
pub const TILE: usize = 128;

/// Most samples taken from one tile.
///
/// A median of a thousand values sits within a few hundredths of a deviation
/// of the median of all sixteen thousand, and the sampling is what keeps the
/// whole map cheap.
const SAMPLES_PER_TILE: usize = 1_024;

/// The background and noise across a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundMap {
    /// Tiles across.
    across: usize,
    /// Tiles down.
    down: usize,
    /// Median of each tile, row by row.
    median: Vec<f32>,
    /// Noise deviation of each tile, row by row.
    sigma: Vec<f32>,
    /// Median of the tile medians, for tiles that held nothing.
    typical_median: f32,
    /// Median of the tile deviations.
    typical_sigma: f32,
}

impl BackgroundMap {
    /// Measures a frame.
    #[must_use]
    pub fn measure(image: &FitsImage) -> Self {
        Self::build(image, image, TILE)
    }

    /// Measures the level of one frame and the noise of another.
    ///
    /// They differ when the frame being searched has been filtered. The
    /// background has to describe the frame the threshold is applied to, or
    /// the two disagree by more than the threshold itself; the noise has to be
    /// measured where it is still independent pixel to pixel, which a filter
    /// destroys. On an undebayered colour mosaic the gap is wide enough to
    /// put the whole frame over the threshold at once.
    #[must_use]
    pub fn measure_level_and_noise(level: &FitsImage, noise: &FitsImage) -> Self {
        Self::build(level, noise, TILE)
    }

    /// Measures a frame with a given tile size, for tests.
    #[must_use]
    pub fn with_tile(image: &FitsImage, tile: usize) -> Self {
        Self::build(image, image, tile)
    }

    fn build(level: &FitsImage, noise_from: &FitsImage, tile: usize) -> Self {
        let image = level;
        let tile = tile.max(8);
        let across = image.width.div_ceil(tile).max(1);
        let down = image.height.div_ceil(tile).max(1);

        // Each tile is independent, which is what makes this cheap enough to
        // do at all.
        let measured: Vec<(f32, f32)> = (0..across * down)
            .into_par_iter()
            .map(|index| {
                let (tx, ty) = (index % across, index / across);
                let (median, _) = measure_tile(level, tx * tile, ty * tile, tile);
                let (_, sigma) = measure_tile(noise_from, tx * tile, ty * tile, tile);
                (median, sigma)
            })
            .collect();

        let median: Vec<f32> = measured.iter().map(|(m, _)| *m).collect();
        let sigma: Vec<f32> = measured.iter().map(|(_, s)| *s).collect();
        let typical_median = middle(&median);
        let typical_sigma = middle(&sigma);

        // A tile of nothing but undefined pixels takes the frame's own figure
        // rather than dragging its neighbourhood to zero.
        let median = median
            .into_iter()
            .map(|v| if v.is_finite() { v } else { typical_median })
            .collect();
        let sigma = sigma
            .into_iter()
            .map(|v| {
                if v.is_finite() && v > 0.0 {
                    v
                } else {
                    typical_sigma
                }
            })
            .collect();

        Self {
            across,
            down,
            median,
            sigma,
            typical_median,
            typical_sigma,
        }
    }

    /// The background and noise at a pixel, interpolated between tile centres.
    #[must_use]
    pub fn at(&self, x: usize, y: usize) -> (f32, f32) {
        let (ax, x0, x1) = self.axis(x, self.across);
        let (ay, y0, y1) = self.axis(y, self.down);

        let corner = |cx: usize, cy: usize| {
            let index = cy * self.across + cx;
            (self.median[index], self.sigma[index])
        };
        let (m00, s00) = corner(x0, y0);
        let (m10, s10) = corner(x1, y0);
        let (m01, s01) = corner(x0, y1);
        let (m11, s11) = corner(x1, y1);

        let blend = |a: f32, b: f32, c: f32, d: f32| {
            let top = a + (b - a) * ax;
            let bottom = c + (d - c) * ax;
            top + (bottom - top) * ay
        };
        (blend(m00, m10, m01, m11), blend(s00, s10, s01, s11))
    }

    /// Fills `out` with the detection threshold for every pixel of row `y`.
    ///
    /// A row at a time, because the vertical interpolation is the same for
    /// every pixel in it: doing both axes per pixel would repeat that work
    /// twenty-four million times on a full frame.
    pub fn row_thresholds(&self, y: usize, deviations: f64, width: usize, out: &mut Vec<f32>) {
        out.clear();
        out.reserve(width);
        #[allow(clippy::cast_possible_truncation)]
        let deviations = deviations as f32;
        let (ay, y0, y1) = self.axis(y, self.down);

        for x in 0..width {
            let (ax, x0, x1) = self.axis(x, self.across);
            let corner = |cx: usize, cy: usize| {
                let index = cy * self.across + cx;
                (self.median[index], self.sigma[index])
            };
            let (m00, s00) = corner(x0, y0);
            let (m10, s10) = corner(x1, y0);
            let (m01, s01) = corner(x0, y1);
            let (m11, s11) = corner(x1, y1);
            let blend = |a: f32, b: f32, c: f32, d: f32| {
                let top = a + (b - a) * ax;
                let bottom = c + (d - c) * ax;
                top + (bottom - top) * ay
            };
            out.push(blend(m00, m10, m01, m11) + deviations * blend(s00, s10, s01, s11));
        }
    }

    /// The frame's overall background and noise, for callers that want one
    /// number: the median over the tiles, which a gradient cannot skew the way
    /// it skews a mean.
    #[must_use]
    pub fn typical(&self) -> (f64, f64) {
        (
            f64::from(self.typical_median),
            f64::from(self.typical_sigma),
        )
    }

    /// The two tile indices either side of a pixel, and how far between them
    /// it lies. Tile centres sit half a tile in from the frame's edge, so
    /// pixels beyond the outermost centres take that tile's own value.
    fn axis(&self, position: usize, tiles: usize) -> (f32, usize, usize) {
        #[allow(clippy::cast_precision_loss)]
        let centred = (position as f32 + 0.5) / TILE as f32 - 0.5;
        if centred <= 0.0 {
            return (0.0, 0, 0);
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let low = centred.floor() as usize;
        if low + 1 >= tiles {
            return (0.0, tiles - 1, tiles - 1);
        }
        #[allow(clippy::cast_precision_loss)]
        (centred - low as f32, low, low + 1)
    }
}

/// Median and noise of one tile, or non-finite values when it held nothing.
fn measure_tile(image: &FitsImage, x0: usize, y0: usize, tile: usize) -> (f32, f32) {
    let x1 = (x0 + tile).min(image.width);
    let y1 = (y0 + tile).min(image.height);
    if x0 >= x1 || y0 >= y1 {
        return (f32::NAN, f32::NAN);
    }

    // Enough samples for a stable median, however large the tile is.
    let area = (x1 - x0) * (y1 - y0);
    let stride = (area / SAMPLES_PER_TILE).max(1).isqrt().max(1);

    let mut sample: Vec<f32> = Vec::with_capacity(SAMPLES_PER_TILE + 8);
    for y in (y0..y1).step_by(stride) {
        let row = &image.data[y * image.width..y * image.width + image.width];
        for value in row[x0..x1].iter().step_by(stride) {
            if value.is_finite() {
                sample.push(*value);
            }
        }
    }
    if sample.is_empty() {
        return (f32::NAN, f32::NAN);
    }

    let median = middle(&sample);

    // Noise from the difference between pixels rather than from the spread
    // about the median. The spread includes whatever gradient runs through the
    // tile, and reports a frame with light pollution across it as far noisier
    // than it is; differencing cancels any smooth gradient and leaves the
    // noise.
    //
    // Two apart, not adjacent. On a colour sensor's undebayered mosaic the
    // pixel next door is under a different filter, so differencing neighbours
    // measures the gap between red and green sensitivity and calls it noise:
    // on a real frame from such a camera it read the noise as ninety times its
    // true value, which pushed the detection threshold past most of the stars.
    // Two apart is the same colour, and for a mono sensor is just as
    // independent as one apart.
    let mut differences: Vec<f32> = Vec::with_capacity(SAMPLES_PER_TILE + 8);
    for y in (y0..y1).step_by(stride) {
        let row = &image.data[y * image.width + x0..y * image.width + x1];
        for pair in row.windows(3) {
            if pair[0].is_finite() && pair[2].is_finite() {
                differences.push((pair[2] - pair[0]).abs());
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    let sigma = if differences.is_empty() {
        // Nothing to difference: fall back on the spread, gradient and all.
        let mut deviations: Vec<f32> = sample.iter().map(|v| (v - median).abs()).collect();
        middle_mut(&mut deviations) * MAD_TO_SIGMA as f32
    } else {
        // The difference of two samples has root-two the deviation of one.
        middle_mut(&mut differences) * (MAD_TO_SIGMA / std::f64::consts::SQRT_2) as f32
    };
    (median, sigma)
}

/// Median of a slice, leaving it alone.
fn middle(values: &[f32]) -> f32 {
    let mut copy: Vec<f32> = values.iter().copied().filter(|v| v.is_finite()).collect();
    middle_mut(&mut copy)
}

/// Median of a slice, reordering it in the process.
fn middle_mut(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return f32::NAN;
    }
    let middle = values.len() / 2;
    values.select_nth_unstable_by(middle, |a, b| a.total_cmp(b));
    values[middle]
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
    fn a_flat_sky_measures_the_same_everywhere() {
        let (w, h) = (512usize, 512usize);
        let pixels = gaussian_background(w, h, 1000.0, 10.0, 11);
        let map = BackgroundMap::measure(&image(w, h, &pixels));

        for (x, y) in [(0usize, 0usize), (255, 255), (511, 511), (100, 400)] {
            let (background, noise) = map.at(x, y);
            assert!(
                (background - 1000.0).abs() < 4.0,
                "background at ({x}, {y}) was {background}"
            );
            assert!(
                (noise - 10.0).abs() < 2.0,
                "noise at ({x}, {y}) was {noise}"
            );
        }
    }

    #[test]
    fn a_gradient_is_followed_rather_than_averaged() {
        // The point of the whole module: light pollution across a frame must
        // not make one side look like signal.
        let (w, h) = (512usize, 512usize);
        let mut pixels = gaussian_background(w, h, 1000.0, 10.0, 12);
        for y in 0..h {
            for x in 0..w {
                pixels[y * w + x] += 4.0 * x as f64;
            }
        }
        let map = BackgroundMap::measure(&image(w, h, &pixels));

        // Between the outermost tile centres the map follows the gradient
        // closely. Beyond them it holds the edge tile's own value, which is
        // deliberate: extrapolating a gradient past the last measurement
        // invents numbers.
        for x in [128usize, 200, 256, 320, 384] {
            let (measured, _) = map.at(x, 256);
            #[allow(clippy::cast_precision_loss)]
            let truth = 1000.0 + 4.0 * x as f32;
            assert!(
                (measured - truth).abs() < 30.0,
                "at x={x} the map said {measured}, the sky was {truth}"
            );
        }
        assert!(
            map.at(20, 256).0 < map.at(490, 256).0,
            "the map must still rise from left to right"
        );

        // And the noise, which the gradient must not inflate: a global
        // estimate over this frame would report a deviation of hundreds.
        let (_, noise) = map.at(256, 256);
        assert!(noise < 40.0, "the gradient inflated the noise to {noise}");
    }

    #[test]
    fn the_map_is_smooth_across_a_tile_boundary() {
        // A step at every tile edge would show up as a ring of detections.
        let (w, h) = (512usize, 512usize);
        let mut pixels = gaussian_background(w, h, 1000.0, 8.0, 13);
        for y in 0..h {
            for x in 0..w {
                pixels[y * w + x] += 3.0 * x as f64;
            }
        }
        let map = BackgroundMap::measure(&image(w, h, &pixels));

        let mut worst: f32 = 0.0;
        for x in 1..w {
            let step = (map.at(x, 256).0 - map.at(x - 1, 256).0).abs();
            worst = worst.max(step);
        }
        assert!(worst < 8.0, "the map jumps by {worst} between neighbours");
    }

    #[test]
    fn stars_do_not_move_the_background_they_sit_on() {
        // A median is used rather than a mean precisely so that this holds.
        let (w, h) = (256usize, 256usize);
        let mut plain = gaussian_background(w, h, 500.0, 5.0, 14);
        let flat = BackgroundMap::measure(&image(w, h, &plain));

        for star in 0..40 {
            let (cx, cy) = (7 + (star % 8) * 30, 7 + (star / 8) * 30);
            for dy in 0..5 {
                for dx in 0..5 {
                    plain[(cy + dy) * w + cx + dx] += 20_000.0;
                }
            }
        }
        let starry = BackgroundMap::measure(&image(w, h, &plain));

        let (before, _) = flat.at(128, 128);
        let (after, _) = starry.at(128, 128);
        assert!(
            (before - after).abs() < 3.0,
            "stars moved the background from {before} to {after}"
        );
    }

    #[test]
    fn a_frame_of_undefined_pixels_does_not_panic() {
        let (w, h) = (64usize, 64usize);
        let pixels = vec![f64::NAN; w * h];
        let map = BackgroundMap::measure(&image(w, h, &pixels));
        let (background, noise) = map.at(32, 32);
        assert!(
            !background.is_nan() || background.is_nan(),
            "any answer will do; not panicking is the test"
        );
        let _ = noise;
    }

    #[test]
    fn a_frame_smaller_than_one_tile_still_measures() {
        let (w, h) = (40usize, 30usize);
        let pixels = gaussian_background(w, h, 700.0, 6.0, 15);
        let map = BackgroundMap::measure(&image(w, h, &pixels));
        let (background, noise) = map.at(20, 15);
        assert!((background - 700.0).abs() < 5.0, "{background}");
        assert!((noise - 6.0).abs() < 3.0, "{noise}");
    }
}

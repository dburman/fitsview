//! Turning a decoded FITS image into something the GPU can draw.
//!
//! Three things happen here, and each is a place bugs hide:
//!
//! 1. **The vertical flip.** FITS stores the bottom row of the image first.
//!    Screen coordinates put row 0 at the top. The flip happens here and
//!    nowhere else, so that calibration frames, which are never displayed,
//!    stay aligned with the lights they are subtracted from.
//! 2. **Downsampling.** A 24-megapixel texture is wasteful when the window is
//!    1400 pixels wide, and exceeds what some GPUs will accept. Large images
//!    are box-filtered down by an integer factor for display only; the
//!    full-resolution samples stay in memory for statistics and calibration.
//! 3. **Mapping samples to bytes.** Non-finite samples become black rather
//!    than poisoning the arithmetic.

use std::sync::Arc;

use egui::{Color32, ColorImage};
use fits_core::stretch::{self, Lut, StretchParams};
use fits_core::FitsImage;
use rayon::prelude::*;

/// Largest texture edge we will hand to the GPU.
///
/// Every desktop GPU in use accepts 8192, and most accept 16384, but there is
/// no benefit to uploading more detail than a screen can show.
pub const MAX_TEXTURE_EDGE: usize = 4096;

/// Maps sample values onto the 0..=255 display range.
///
/// Two modes. Linear spreads the image's finite range evenly, which is honest
/// but shows almost nothing on a raw astronomical frame. Stretched puts the sky
/// background at a chosen brightness through a per-channel lookup table, which
/// is what makes the faint signal visible.
#[derive(Debug, Clone)]
pub struct Mapping {
    /// Sample value that maps to the bottom of the range.
    low: f32,
    /// Sample value that maps to the top of the range.
    high: f32,
    /// One lookup table per channel when stretching, empty when linear.
    ///
    /// Shared rather than copied, because a table is 64 KB and the mapping is
    /// cloned for every texture rebuild.
    luts: Arc<Vec<Box<Lut>>>,
}

impl Mapping {
    /// A linear ramp across an image's finite range.
    #[must_use]
    pub fn linear(image: &FitsImage) -> Self {
        Self::range(image.min, image.max)
    }

    /// A linear ramp between two explicit values.
    #[must_use]
    pub fn range(low: f32, high: f32) -> Self {
        Self {
            low,
            high,
            luts: Arc::new(Vec::new()),
        }
    }

    /// An automatic stretch computed from the image.
    ///
    /// The tables are built once here rather than per pixel, which is what
    /// keeps a stretched redraw as cheap as a linear one.
    #[must_use]
    pub fn stretched(image: &FitsImage, params: &StretchParams) -> Self {
        let luts = stretch::compute_stretch(image, params)
            .iter()
            .map(stretch::build_lut)
            .collect();
        Self {
            low: image.min,
            high: image.max,
            luts: Arc::new(luts),
        }
    }

    /// Whether this mapping applies a stretch.
    #[must_use]
    pub fn is_stretched(&self) -> bool {
        !self.luts.is_empty()
    }

    /// Maps one sample of the given channel to a display byte.
    ///
    /// Non-finite samples map to black. A `NaN` reaching this unhandled would
    /// otherwise produce an arbitrary byte and speckle the image.
    #[must_use]
    pub fn to_u8(&self, sample: f32, channel: usize) -> u8 {
        if !sample.is_finite() {
            return 0;
        }
        let span = self.high - self.low;
        if span <= 0.0 {
            return 0;
        }
        let normalised = ((sample - self.low) / span).clamp(0.0, 1.0);

        if let Some(lut) = self.luts.get(channel).or_else(|| self.luts.first()) {
            return lut[stretch::lut_index(normalised)];
        }

        // 255.0 rather than 256.0 so that `high` maps exactly to 255.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            (normalised * 255.0).round() as u8
        }
    }
}

/// The integer factor by which an image must shrink to fit within `max_edge`.
///
/// Always at least 1. Returns the smallest factor that works rather than
/// rounding up to a power of two, because a box filter handles any factor and
/// a smaller factor keeps more detail.
///
/// ```
/// # use fitsview::texture::downsample_factor;
/// assert_eq!(downsample_factor(1000, 800, 4096), 1);
/// assert_eq!(downsample_factor(6000, 4000, 4096), 2);
/// ```
#[must_use]
pub fn downsample_factor(width: usize, height: usize, max_edge: usize) -> usize {
    if max_edge == 0 {
        return 1;
    }
    let longest = width.max(height);
    if longest <= max_edge {
        return 1;
    }
    // Round up so the result really does fit.
    longest.div_ceil(max_edge).max(1)
}

/// Dimensions after downsampling by `factor`.
///
/// Never returns zero for a non-empty input, so a very small image with a large
/// factor still produces a drawable texture.
#[must_use]
pub fn downsampled_size(width: usize, height: usize, factor: usize) -> (usize, usize) {
    let f = factor.max(1);
    (width.div_ceil(f).max(1), height.div_ceil(f).max(1))
}

/// Builds a texture-ready image: flipped, downsampled and mapped to bytes.
///
/// `factor` of 1 means no downsampling. Use [`downsample_factor`] to choose it.
#[must_use]
pub fn to_color_image(image: &FitsImage, mapping: &Mapping, factor: usize) -> ColorImage {
    let factor = factor.max(1);
    let (out_w, out_h) = downsampled_size(image.width, image.height, factor);

    let mut pixels = vec![Color32::BLACK; out_w * out_h];

    // One row of output at a time, in parallel. Each task reads only its own
    // source rows, so there is no sharing to reason about.
    pixels
        .par_chunks_mut(out_w)
        .enumerate()
        .for_each(|(out_y, row)| {
            for (out_x, slot) in row.iter_mut().enumerate() {
                *slot = sample_block(image, mapping, out_x, out_y, factor, out_h);
            }
        });

    ColorImage::new([out_w, out_h], pixels)
}

/// Averages the source block that maps to one output pixel, then colours it.
///
/// `out_y` counts from the top of the screen; FITS rows count from the bottom,
/// so the flip happens in the row index computed here.
fn sample_block(
    image: &FitsImage,
    mapping: &Mapping,
    out_x: usize,
    out_y: usize,
    factor: usize,
    out_h: usize,
) -> Color32 {
    // Flip: the topmost output row corresponds to the last FITS row.
    let flipped_out_y = out_h - 1 - out_y;
    let src_x0 = out_x * factor;
    let src_y0 = flipped_out_y * factor;

    let mut channel_bytes = [0u8; 3];
    for (c, byte) in channel_bytes.iter_mut().enumerate().take(image.channels) {
        let mut total = 0.0f64;
        let mut counted = 0u32;
        for dy in 0..factor {
            let y = src_y0 + dy;
            if y >= image.height {
                break;
            }
            for dx in 0..factor {
                let x = src_x0 + dx;
                if x >= image.width {
                    break;
                }
                if let Some(v) = image.sample(x, y, c) {
                    // Averaging in the sample domain, then mapping, keeps a
                    // single hot pixel from dominating a whole output block.
                    if v.is_finite() {
                        total += f64::from(v);
                        counted += 1;
                    }
                }
            }
        }
        *byte = if counted == 0 {
            // Every contributing sample was NaN, so the block has no data.
            0
        } else {
            #[allow(clippy::cast_possible_truncation)]
            mapping.to_u8((total / f64::from(counted)) as f32, c)
        };
    }

    if image.channels >= 3 {
        Color32::from_rgb(channel_bytes[0], channel_bytes[1], channel_bytes[2])
    } else {
        Color32::from_gray(channel_bytes[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::read_fits_from_bytes;
    use fits_core::testutil::{synthetic_fits, SyntheticSpec};

    /// Builds an image from physical values, row-major in FITS order, meaning
    /// the first row given is the bottom of the picture.
    fn image(width: usize, height: usize, pixels: &[f64]) -> FitsImage {
        let spec = SyntheticSpec::new(width, height, -32);
        let bytes = synthetic_fits(&spec, pixels).unwrap();
        read_fits_from_bytes(&bytes).unwrap()
    }

    #[test]
    fn linear_mapping_spans_the_image_range() {
        let img = image(2, 1, &[10.0, 20.0]);
        let m = Mapping::linear(&img);
        assert_eq!(m.to_u8(10.0, 0), 0);
        assert_eq!(m.to_u8(20.0, 0), 255);
        assert_eq!(m.to_u8(15.0, 0), 128);
    }

    #[test]
    fn mapping_clamps_out_of_range_samples() {
        let m = Mapping::range(0.0, 100.0);
        assert_eq!(m.to_u8(-50.0, 0), 0);
        assert_eq!(m.to_u8(1000.0, 0), 255);
    }

    #[test]
    fn non_finite_samples_map_to_black() {
        let m = Mapping::range(0.0, 100.0);
        assert_eq!(m.to_u8(f32::NAN, 0), 0);
        assert_eq!(m.to_u8(f32::INFINITY, 0), 0);
        assert_eq!(m.to_u8(f32::NEG_INFINITY, 0), 0);
    }

    #[test]
    fn a_zero_width_range_does_not_divide_by_zero() {
        let m = Mapping::range(5.0, 5.0);
        assert_eq!(m.to_u8(5.0, 0), 0);
        let m = Mapping::range(10.0, 0.0);
        assert_eq!(m.to_u8(5.0, 0), 0);
    }

    #[test]
    fn the_image_is_flipped_exactly_once() {
        // This is the test that catches the classic upside-down bug. The first
        // FITS row is the bottom of the picture, so a bright first row must
        // appear in the LAST row of the texture.
        let img = image(2, 2, &[100.0, 100.0, 0.0, 0.0]);
        let m = Mapping::linear(&img);
        let ci = to_color_image(&img, &m, 1);

        assert_eq!(ci.size, [2, 2]);
        // Texture row 0 is the top of the screen: the dark FITS row.
        assert_eq!(ci.pixels[0], Color32::from_gray(0));
        assert_eq!(ci.pixels[1], Color32::from_gray(0));
        // Texture row 1 is the bottom: the bright FITS row.
        assert_eq!(ci.pixels[2], Color32::from_gray(255));
        assert_eq!(ci.pixels[3], Color32::from_gray(255));
    }

    #[test]
    fn horizontal_order_is_not_flipped() {
        // Only the vertical axis differs between FITS and screen order.
        let img = image(3, 1, &[0.0, 128.0, 255.0]);
        let m = Mapping::range(0.0, 255.0);
        let ci = to_color_image(&img, &m, 1);
        assert_eq!(ci.pixels[0], Color32::from_gray(0));
        assert_eq!(ci.pixels[2], Color32::from_gray(255));
    }

    #[test]
    fn flipping_a_tall_image_reverses_every_row() {
        let height = 5;
        let pixels: Vec<f64> = (0..height).map(|y| y as f64 * 60.0).collect();
        let img = image(1, height, &pixels);
        let m = Mapping::range(0.0, 240.0);
        let ci = to_color_image(&img, &m, 1);
        // FITS row 4 (brightest) must be at texture row 0.
        for (out_y, px) in ci.pixels.iter().enumerate() {
            let fits_row = height - 1 - out_y;
            let expected = m.to_u8(fits_row as f32 * 60.0, 0);
            assert_eq!(*px, Color32::from_gray(expected), "output row {out_y}");
        }
    }

    #[test]
    fn downsample_factor_leaves_small_images_alone() {
        assert_eq!(downsample_factor(100, 100, 4096), 1);
        assert_eq!(downsample_factor(4096, 4096, 4096), 1);
    }

    #[test]
    fn downsample_factor_shrinks_large_images_enough_to_fit() {
        for (w, h) in [(6000, 4000), (9000, 100), (100, 20000), (16384, 16384)] {
            let f = downsample_factor(w, h, 4096);
            let (ow, oh) = downsampled_size(w, h, f);
            assert!(
                ow <= 4096 && oh <= 4096,
                "{w}x{h} -> {ow}x{oh} (factor {f})"
            );
        }
    }

    #[test]
    fn downsample_factor_never_returns_zero() {
        assert_eq!(downsample_factor(0, 0, 4096), 1);
        assert_eq!(downsample_factor(100, 100, 0), 1);
        assert!(downsample_factor(usize::MAX, 1, 4096) >= 1);
    }

    #[test]
    fn downsampling_averages_a_block() {
        // A 2x2 image of 0, 100, 200, 300 averages to 150.
        let img = image(2, 2, &[0.0, 100.0, 200.0, 300.0]);
        let m = Mapping::range(0.0, 300.0);
        let ci = to_color_image(&img, &m, 2);
        assert_eq!(ci.size, [1, 1]);
        assert_eq!(ci.pixels[0], Color32::from_gray(m.to_u8(150.0, 0)));
    }

    #[test]
    fn downsampling_handles_a_ragged_edge() {
        // 3x3 downsampled by 2 gives 2x2, where the right and bottom blocks are
        // partial. Reading past the edge would panic or produce garbage.
        let pixels: Vec<f64> = (0..9).map(|i| i as f64).collect();
        let img = image(3, 3, &pixels);
        let m = Mapping::range(0.0, 8.0);
        let ci = to_color_image(&img, &m, 2);
        assert_eq!(ci.size, [2, 2]);
        assert!(ci.pixels.iter().all(|p| p.a() == 255));
    }

    #[test]
    fn a_block_of_only_nan_becomes_black_rather_than_arbitrary() {
        let img = image(2, 1, &[f64::NAN, 50.0]);
        let m = Mapping::range(0.0, 100.0);
        let ci = to_color_image(&img, &m, 1);
        assert_eq!(ci.pixels[0], Color32::from_gray(0));
        assert_eq!(ci.pixels[1], Color32::from_gray(m.to_u8(50.0, 0)));
    }

    #[test]
    fn nan_does_not_drag_down_the_average_of_a_downsampled_block() {
        // Averaging NaN as if it were zero would darken the block. Only the
        // finite samples should count.
        let img = image(2, 2, &[100.0, 100.0, f64::NAN, 100.0]);
        let m = Mapping::range(0.0, 100.0);
        let ci = to_color_image(&img, &m, 2);
        assert_eq!(ci.pixels[0], Color32::from_gray(255));
    }

    #[test]
    fn colour_images_produce_rgb_texels() {
        let spec = SyntheticSpec::new(1, 1, -32).with_channels(3);
        let bytes = synthetic_fits(&spec, &[0.0, 128.0, 255.0]).unwrap();
        let img = read_fits_from_bytes(&bytes).unwrap();
        let m = Mapping::range(0.0, 255.0);
        let ci = to_color_image(&img, &m, 1);
        assert_eq!(ci.pixels[0], Color32::from_rgb(0, 128, 255));
    }

    #[test]
    fn texture_size_matches_the_pixel_count() {
        for (w, h, f) in [(10, 10, 1), (10, 10, 3), (6000, 4000, 2), (1, 1, 8)] {
            let pixels = vec![1.0f64; w * h];
            let img = image(w, h, &pixels);
            let m = Mapping::linear(&img);
            let ci = to_color_image(&img, &m, f);
            assert_eq!(
                ci.size[0] * ci.size[1],
                ci.pixels.len(),
                "{w}x{h} factor {f}"
            );
            assert_eq!(
                ci.size,
                [downsampled_size(w, h, f).0, downsampled_size(w, h, f).1]
            );
        }
    }
}

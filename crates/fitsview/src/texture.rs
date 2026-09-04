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

use egui::{Color32, ColorImage, Rect, Vec2};
use fits_core::stretch::{self, Lut, Stretch, StretchParams};
use fits_core::FitsImage;
use rayon::prelude::*;

use crate::view::ViewState;

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
    /// The stretch the tables were built from, kept so the histogram can mark
    /// the black point and midtone without measuring the image a second time.
    stretch: Option<Stretch>,
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
            stretch: None,
        }
    }

    /// An automatic stretch computed from the image.
    ///
    /// The tables are built once here rather than per pixel, which is what
    /// keeps a stretched redraw as cheap as a linear one.
    #[must_use]
    pub fn stretched(image: &FitsImage, params: &StretchParams) -> Self {
        let stretches = stretch::compute_stretch(image, params);
        let luts = stretches.iter().map(stretch::build_lut).collect();
        // With one stretch shared by every channel the first describes them
        // all. Unlinked, the three differ, and there is no single curve to
        // report: anything drawn from one of them would be wrong for the other
        // two, so the histogram goes without its markers rather than showing
        // markers that belong to the red channel alone.
        let shared = stretches
            .first()
            .copied()
            .filter(|first| stretches.iter().all(|s| s == first));

        Self {
            low: image.min,
            high: image.max,
            luts: Arc::new(luts),
            stretch: shared,
        }
    }

    /// The stretch this mapping applies, if it applies one.
    ///
    /// Measuring an image's background costs 12 ms on a full frame, so anything
    /// else that needs the same answer takes it from here rather than working
    /// it out again.
    #[must_use]
    pub fn stretch(&self) -> Option<Stretch> {
        self.stretch
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

/// Detail textures are built in tiles of this many image pixels.
///
/// Rounding the visible region out to a grid means panning within a tile reuses
/// the upload instead of rebuilding it every frame of a drag.
pub const DETAIL_TILE: usize = 256;

/// A rectangle of an image, in display coordinates, where row 0 is the top of
/// the picture rather than the first row stored in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetailRegion {
    /// Left edge, in image pixels.
    pub x: usize,
    /// Top edge, in image pixels, counting down from the top of the picture.
    pub y: usize,
    /// Width in image pixels.
    pub width: usize,
    /// Height in image pixels.
    pub height: usize,
}

impl DetailRegion {
    /// Number of pixels the region covers.
    #[must_use]
    pub fn area(&self) -> usize {
        self.width.saturating_mul(self.height)
    }

    /// Where the region sits on screen, given the current view.
    #[must_use]
    pub fn screen_rect(&self, view: &ViewState) -> Rect {
        #[allow(clippy::cast_precision_loss)]
        let min = Vec2::new(self.x as f32, self.y as f32);
        #[allow(clippy::cast_precision_loss)]
        let max = Vec2::new((self.x + self.width) as f32, (self.y + self.height) as f32);
        Rect::from_min_max(view.image_to_screen(min), view.image_to_screen(max))
    }
}

/// The part of an image worth uploading at full resolution, if any.
///
/// The overview texture is the image shrunk by `factor`, so each of its texels
/// covers `factor * zoom` screen pixels. Once that exceeds one, the overview is
/// being magnified and real detail is no longer visible: a 6000 x 4000 frame
/// shrunk by two and shown at "100%" is a two-times blur, not the image.
///
/// Returns `None` while the overview is good enough, which is the common case
/// and costs nothing. Otherwise returns the visible region, rounded out to
/// [`DETAIL_TILE`] and clamped to the image, so the work is bounded by the size
/// of the window rather than the size of the image.
#[must_use]
pub fn detail_region_for(
    image_size: (usize, usize),
    factor: usize,
    view: &ViewState,
    viewport: Rect,
) -> Option<DetailRegion> {
    let (width, height) = image_size;
    if width == 0 || height == 0 || factor <= 1 {
        // Nothing was thrown away, so there is no more detail to fetch.
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let overview_scale = 1.0 / factor as f32;
    if !view.zoom.is_finite() || view.zoom <= overview_scale {
        return None;
    }

    #[allow(clippy::cast_precision_loss)]
    let image = Vec2::new(width as f32, height as f32);
    let on_screen = view.image_rect(image).intersect(viewport);
    if on_screen.width() <= 0.0 || on_screen.height() <= 0.0 {
        // Scrolled entirely out of view.
        return None;
    }

    let top_left = view.screen_to_image(on_screen.min);
    let bottom_right = view.screen_to_image(on_screen.max);

    // Round out to the tile grid, then clamp, so a small pan asks for the same
    // region and the texture is reused.
    let floor_tile = |v: f32, limit: usize| -> usize {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let p = v.max(0.0) as usize;
        (p / DETAIL_TILE * DETAIL_TILE).min(limit)
    };
    let ceil_tile = |v: f32, limit: usize| -> usize {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let p = v.max(0.0).ceil() as usize;
        p.div_ceil(DETAIL_TILE)
            .saturating_mul(DETAIL_TILE)
            .min(limit)
    };

    let x0 = floor_tile(top_left.x, width);
    let y0 = floor_tile(top_left.y, height);
    let x1 = ceil_tile(bottom_right.x, width).max(x0);
    let y1 = ceil_tile(bottom_right.y, height).max(y0);

    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(DetailRegion {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
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

/// Builds a texture for part of an image, at full resolution.
///
/// One texel per image pixel, so this is what makes a true 1:1 view possible.
/// The vertical flip is applied here as it is for the whole image: row 0 of the
/// result is the top of the picture, which is the last row stored in the file.
#[must_use]
pub fn to_color_image_region(
    image: &FitsImage,
    mapping: &Mapping,
    region: &DetailRegion,
) -> ColorImage {
    let width = region.width.min(image.width.saturating_sub(region.x));
    let height = region.height.min(image.height.saturating_sub(region.y));
    if width == 0 || height == 0 {
        return ColorImage::new([1, 1], vec![Color32::BLACK]);
    }

    let plane = image.width * image.height;
    let mut pixels = vec![Color32::BLACK; width * height];

    pixels
        .par_chunks_mut(width)
        .enumerate()
        .for_each(|(row, out)| {
            // Display row `region.y + row` counts from the top of the picture,
            // and the file stores the bottom row first.
            let display_y = region.y + row;
            let source_y = image.height - 1 - display_y;
            let base = source_y * image.width + region.x;

            for (column, slot) in out.iter_mut().enumerate() {
                let index = base + column;
                let mut channel_bytes = [0u8; 3];
                for (c, byte) in channel_bytes.iter_mut().enumerate().take(image.channels) {
                    let value = image.data[c * plane + index];
                    *byte = mapping.to_u8(value, c);
                }
                *slot = if image.channels >= 3 {
                    Color32::from_rgb(channel_bytes[0], channel_bytes[1], channel_bytes[2])
                } else {
                    Color32::from_gray(channel_bytes[0])
                };
            }
        });

    ColorImage::new([width, height], pixels)
}

/// Averages the source block that maps to one output pixel, then colours it.
///
/// `out_y` counts from the top of the screen; FITS rows count from the bottom,
/// so the flip happens in the row index computed here.
///
/// Indexes the sample data directly rather than through [`FitsImage::sample`].
/// The bounds it would check are already established by the loop, and at
/// 24 megapixels that check runs a hundred million times.
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

    let plane = image.width * image.height;
    let x_end = (src_x0 + factor).min(image.width);
    let y_end = (src_y0 + factor).min(image.height);

    let mut channel_bytes = [0u8; 3];
    for (c, byte) in channel_bytes.iter_mut().enumerate().take(image.channels) {
        let base = c * plane;
        let mut total = 0.0f64;
        let mut counted = 0u32;

        for y in src_y0..y_end {
            let row = base + y * image.width;
            // The row slice is in range by construction, so the loop below
            // needs no per-sample bounds check.
            let samples = &image.data[row + src_x0..row + x_end];
            for value in samples {
                // Averaging in the sample domain, then mapping, keeps a single
                // hot pixel from dominating a whole output block.
                if value.is_finite() {
                    total += f64::from(*value);
                    counted += 1;
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

    /// A view fitted to a viewport, then zoomed about its centre.
    fn view_at(image: (usize, usize), viewport: Rect, zoom: f32) -> ViewState {
        #[allow(clippy::cast_precision_loss)]
        let size = Vec2::new(image.0 as f32, image.1 as f32);
        let mut view = ViewState::fit(size, viewport);
        view.set_zoom_about_centre(viewport, zoom);
        view
    }

    fn window() -> Rect {
        Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1400.0, 900.0))
    }

    #[test]
    fn no_detail_is_needed_while_the_overview_is_sharp_enough() {
        // The overview of a 6000x4000 frame is half size, so it holds real
        // detail up to 50% zoom. Below that it is being shrunk, not magnified.
        let image = (6000, 4000);
        for zoom in [0.05, 0.2, 0.4, 0.49] {
            let view = view_at(image, window(), zoom);
            assert_eq!(
                detail_region_for(image, 2, &view, window()),
                None,
                "zoom {zoom} should not need a detail texture"
            );
        }
    }

    #[test]
    fn detail_is_needed_once_the_overview_is_being_magnified() {
        let image = (6000, 4000);
        for zoom in [0.6, 1.0, 4.0] {
            let view = view_at(image, window(), zoom);
            assert!(
                detail_region_for(image, 2, &view, window()).is_some(),
                "zoom {zoom} magnifies the overview, so detail is needed"
            );
        }
    }

    #[test]
    fn an_image_that_was_never_shrunk_never_needs_detail() {
        // Factor 1 means the overview is already every pixel.
        let image = (800, 600);
        for zoom in [0.5, 1.0, 8.0] {
            let view = view_at(image, window(), zoom);
            assert_eq!(detail_region_for(image, 1, &view, window()), None);
        }
    }

    #[test]
    fn the_region_stays_inside_the_image() {
        let image = (6000, 4000);
        // Zoomed in and panned hard against each corner in turn.
        for (dx, dy) in [(-9000.0, -9000.0), (9000.0, 9000.0), (0.0, 0.0)] {
            let mut view = view_at(image, window(), 2.0);
            view.pan(Vec2::new(dx, dy));
            if let Some(region) = detail_region_for(image, 2, &view, window()) {
                assert!(
                    region.x + region.width <= image.0,
                    "region runs off the right: {region:?}"
                );
                assert!(
                    region.y + region.height <= image.1,
                    "region runs off the bottom: {region:?}"
                );
                assert!(region.width > 0 && region.height > 0);
            }
        }
    }

    #[test]
    fn a_small_pan_reuses_the_same_region() {
        // Otherwise the texture is rebuilt on every frame of a drag.
        let image = (6000, 4000);
        let mut view = view_at(image, window(), 2.0);
        let first = detail_region_for(image, 2, &view, window()).expect("needs detail");

        // A few screen pixels, well inside one tile at this zoom.
        view.pan(Vec2::new(3.0, -2.0));
        let after = detail_region_for(image, 2, &view, window()).expect("still needs detail");
        assert_eq!(first, after, "a small pan should not rebuild the texture");
    }

    #[test]
    fn a_large_pan_asks_for_a_different_region() {
        let image = (6000, 4000);
        let mut view = view_at(image, window(), 2.0);
        let first = detail_region_for(image, 2, &view, window()).expect("needs detail");

        view.pan(Vec2::new(-1200.0, 0.0));
        let after = detail_region_for(image, 2, &view, window()).expect("needs detail");
        assert_ne!(first, after, "panning a long way should move the region");
    }

    #[test]
    fn a_viewport_larger_than_the_image_covers_the_whole_image() {
        let image = (600, 400);
        let view = view_at(image, window(), 2.0);
        if let Some(region) = detail_region_for(image, 2, &view, window()) {
            assert_eq!(region.x, 0);
            assert_eq!(region.y, 0);
            assert_eq!(region.width, image.0);
            assert_eq!(region.height, image.1);
        }
    }

    #[test]
    fn the_region_is_bounded_by_the_window_not_the_image() {
        // The whole point: a bigger image must not mean a bigger upload.
        let small = (6000, 4000);
        let huge = (30_000, 20_000);
        let view_small = view_at(small, window(), 1.0);
        let view_huge = view_at(huge, window(), 1.0);

        let a = detail_region_for(small, 2, &view_small, window()).expect("detail");
        let b = detail_region_for(huge, 8, &view_huge, window()).expect("detail");

        assert!(
            b.area() < small.0 * small.1,
            "a 600 megapixel image asked for {} pixels",
            b.area()
        );
        // Both are about a window's worth, give or take the tile rounding.
        let window_pixels = 1400 * 900;
        for region in [a, b] {
            assert!(
                region.area() < window_pixels * 4,
                "region {region:?} is far larger than the window"
            );
        }
    }

    #[test]
    fn an_image_scrolled_out_of_sight_needs_no_detail() {
        let image = (6000, 4000);
        let mut view = view_at(image, window(), 2.0);
        view.pan(Vec2::new(100_000.0, 100_000.0));
        assert_eq!(detail_region_for(image, 2, &view, window()), None);
    }

    #[test]
    fn a_degenerate_image_or_view_is_handled() {
        let view = view_at((10, 10), window(), 2.0);
        assert_eq!(detail_region_for((0, 0), 2, &view, window()), None);
        let mut broken = view;
        broken.zoom = f32::NAN;
        assert_eq!(detail_region_for((6000, 4000), 2, &broken, window()), None);
    }

    #[test]
    fn a_fitted_view_of_a_full_frame_needs_no_detail() {
        // The common case on opening an image: fitted to the window, far below
        // the point where the overview is magnified. Uploading detail here
        // would be pure waste on every file the user steps to.
        let image = (6000, 4000);
        let view = ViewState::fit(Vec2::new(6000.0, 4000.0), window());
        assert!(view.zoom < 0.5, "a fitted full frame is well under 1:1");
        assert_eq!(detail_region_for(image, 2, &view, window()), None);
    }

    #[test]
    fn a_detail_texture_is_one_texel_per_image_pixel() {
        let img = image(
            64,
            64,
            &(0..4096).map(|i| f64::from(i % 256)).collect::<Vec<_>>(),
        );
        let region = DetailRegion {
            x: 8,
            y: 16,
            width: 32,
            height: 24,
        };
        let rendered = to_color_image_region(&img, &Mapping::linear(&img), &region);
        assert_eq!(rendered.size, [32, 24]);
        assert_eq!(rendered.pixels.len(), 32 * 24);
    }

    #[test]
    fn a_detail_texture_is_flipped_the_same_way_as_the_overview() {
        // The bright first FITS row is the bottom of the picture, so a region
        // covering the bottom must be the bright one.
        let img = image(
            4,
            4,
            &[
                100.0, 100.0, 100.0, 100.0, // FITS row 0: the bottom
                0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            ],
        );
        let m = Mapping::range(0.0, 100.0);

        let bottom = to_color_image_region(
            &img,
            &m,
            &DetailRegion {
                x: 0,
                y: 3,
                width: 4,
                height: 1,
            },
        );
        assert_eq!(
            bottom.pixels[0],
            Color32::from_gray(255),
            "bottom is bright"
        );

        let top = to_color_image_region(
            &img,
            &m,
            &DetailRegion {
                x: 0,
                y: 0,
                width: 4,
                height: 1,
            },
        );
        assert_eq!(top.pixels[0], Color32::from_gray(0), "top is dark");
    }

    #[test]
    fn a_detail_texture_agrees_with_the_overview_where_they_overlap() {
        // At factor 1 the whole-image builder is already full resolution, so a
        // region of it must match the same pixels.
        let pixels: Vec<f64> = (0..1024).map(|i| f64::from(i % 251)).collect();
        let img = image(32, 32, &pixels);
        let m = Mapping::linear(&img);

        let whole = to_color_image(&img, &m, 1);
        let region = DetailRegion {
            x: 5,
            y: 7,
            width: 11,
            height: 9,
        };
        let part = to_color_image_region(&img, &m, &region);

        for row in 0..region.height {
            for column in 0..region.width {
                let from_part = part.pixels[row * region.width + column];
                let from_whole = whole.pixels[(region.y + row) * 32 + region.x + column];
                assert_eq!(from_part, from_whole, "at ({column}, {row})");
            }
        }
    }

    #[test]
    fn a_region_running_past_the_edge_is_trimmed_rather_than_panicking() {
        let img = image(8, 8, &vec![1.0; 64]);
        let rendered = to_color_image_region(
            &img,
            &Mapping::linear(&img),
            &DetailRegion {
                x: 6,
                y: 6,
                width: 100,
                height: 100,
            },
        );
        assert_eq!(rendered.size, [2, 2]);
    }

    #[test]
    fn a_region_entirely_outside_the_image_yields_something_drawable() {
        let img = image(8, 8, &vec![1.0; 64]);
        let rendered = to_color_image_region(
            &img,
            &Mapping::linear(&img),
            &DetailRegion {
                x: 99,
                y: 99,
                width: 4,
                height: 4,
            },
        );
        assert_eq!(rendered.size, [1, 1], "never an empty texture");
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

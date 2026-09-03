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

use std::collections::HashMap;

use crate::image::FitsImage;
use crate::quality;

/// Turns a Gaussian's standard deviation into a full width at half maximum.
const FWHM_PER_SIGMA: f64 = 2.354_820_045_030_949;

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
    /// Most stars to measure, so a rich field cannot cost unbounded time.
    pub limit: usize,
}

impl Default for DetectionParams {
    fn default() -> Self {
        Self {
            threshold: 5.0,
            minimum_area: 4,
            maximum_area: 2_000,
            maximum_elongation: 5.0,
            limit: 5_000,
        }
    }
}

/// A fraction of the frame above threshold beyond which it is not a star field.
///
/// A flat, a badly overexposed frame or one full of cloud can put most of its
/// pixels above any threshold. Labelling those would cost a great deal and
/// answer nothing.
const MAX_BRIGHT_FRACTION: f64 = 0.10;

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
}

/// What a frame's stars say about it.
#[derive(Debug, Clone, Default, PartialEq)]
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
}

impl StarField {
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

/// Fewer stars than this and a median says more about luck than the frame.
const MINIMUM_FOR_A_MEDIAN: usize = 3;

/// Finds and measures the stars in a frame.
///
/// The frame should be calibrated but **not** debayered: a colour mosaic has
/// its own entry point in [`detect_mosaic`], because every star in a mosaic
/// would otherwise be found four times, once per filter site.
#[must_use]
pub fn detect(image: &FitsImage, params: &DetectionParams) -> StarField {
    if image.width < 3 || image.height < 3 || image.channels != 1 {
        return StarField::default();
    }

    let measured = quality::measure(image);
    if measured.noise <= 0.0 {
        // No noise means no scale to threshold against.
        return StarField::default();
    }
    let threshold = measured.background + params.threshold * measured.noise;

    let Some(bright) = bright_pixels(image, threshold) else {
        return StarField::default();
    };
    let regions = group(&bright, image.width);

    let mut stars: Vec<Star> = Vec::new();
    for pixels in regions.values() {
        if stars.len() >= params.limit {
            break;
        }
        if pixels.len() < params.minimum_area || pixels.len() > params.maximum_area {
            continue;
        }
        if touches_edge(pixels, image.width, image.height) {
            // Half a star has a shape that is a lie.
            continue;
        }
        if elongation(pixels, image.width) > params.maximum_elongation {
            // A line, not a point: a satellite or an aeroplane.
            continue;
        }
        if let Some(star) = measure_star(image, pixels, measured.background) {
            stars.push(star);
        }
    }

    summarise(stars)
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
        star.fwhm *= 2.0;
    }
    field.fwhm = field.fwhm.map(|w| w * 2.0);
    field
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
    for y in 0..height {
        for x in 0..width {
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
                    data[y * width + x] = (total / f64::from(count)) as f32;
                }
            }
        }
    }

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
fn bright_pixels(image: &FitsImage, threshold: f64) -> Option<Vec<usize>> {
    #[allow(clippy::cast_possible_truncation)]
    let threshold = threshold as f32;
    let mut bright = Vec::new();
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

    for (index, value) in image.data.iter().enumerate() {
        if value.is_finite() && *value > threshold {
            bright.push(index);
            if bright.len() > cap {
                log::debug!(
                    "more than {MAX_BRIGHT_FRACTION} of the frame is bright; not a star field"
                );
                return None;
            }
        }
    }
    Some(bright)
}

/// Groups bright pixels into connected regions, eight-connected.
///
/// Works over the sparse set rather than the whole frame: a star field puts a
/// fraction of a percent of its pixels above threshold, so this is thousands of
/// lookups rather than tens of millions.
fn group(bright: &[usize], width: usize) -> HashMap<usize, Vec<usize>> {
    let position: HashMap<usize, usize> = bright
        .iter()
        .enumerate()
        .map(|(slot, index)| (*index, slot))
        .collect();

    let mut parent: Vec<usize> = (0..bright.len()).collect();
    for (slot, index) in bright.iter().enumerate() {
        let (x, y) = (index % width, index / width);
        // Only the neighbours already visited, which is enough to connect
        // everything by the time the scan finishes.
        for (dx, dy) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0)] {
            let (nx, ny) = (x as i64 + dx, y as i64 + dy);
            if nx < 0 || ny < 0 {
                continue;
            }
            #[allow(clippy::cast_sign_loss)]
            let neighbour = ny as usize * width + nx as usize;
            #[allow(clippy::cast_sign_loss)]
            if nx as usize >= width {
                continue;
            }
            if let Some(other) = position.get(&neighbour) {
                union(&mut parent, slot, *other);
            }
        }
    }

    let mut regions: HashMap<usize, Vec<usize>> = HashMap::new();
    for (slot, index) in bright.iter().enumerate() {
        let root = find(&mut parent, slot);
        regions.entry(root).or_default().push(*index);
    }
    regions
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

    // A window wide enough to hold the wings, from the region's own extent.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let radius = ((pixels.len() as f64).sqrt().ceil() as usize).clamp(3, 16);
    let (moment_xx, moment_yy, moment_xy, weight) = moments(image, cx, cy, radius, background);
    if weight <= 0.0 {
        return None;
    }

    let variance_x = moment_xx / weight;
    let variance_y = moment_yy / weight;
    let covariance = moment_xy / weight;

    let mean_variance = (variance_x + variance_y) / 2.0;
    if mean_variance <= 0.0 {
        return None;
    }
    let fwhm = FWHM_PER_SIGMA * mean_variance.sqrt();

    // The axes of the intensity distribution, from the eigenvalues of its
    // covariance. Their ratio is how round the star is.
    let difference = ((variance_x - variance_y).powi(2) + 4.0 * covariance * covariance).sqrt();
    let major = (variance_x + variance_y + difference) / 2.0;
    let minor = (variance_x + variance_y - difference) / 2.0;
    let roundness = if major > 0.0 {
        (minor.max(0.0) / major).sqrt()
    } else {
        0.0
    };

    Some(Star {
        x: cx,
        y: cy,
        flux,
        fwhm,
        roundness,
        saturated,
    })
}

/// Second central moments of the background-subtracted signal about a centre.
fn moments(
    image: &FitsImage,
    cx: f64,
    cy: f64,
    radius: usize,
    background: f64,
) -> (f64, f64, f64, f64) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (ix, iy) = (cx.round() as usize, cy.round() as usize);
    let x0 = ix.saturating_sub(radius);
    let y0 = iy.saturating_sub(radius);
    let x1 = (ix + radius + 1).min(image.width);
    let y1 = (iy + radius + 1).min(image.height);

    let (mut xx, mut yy, mut xy, mut weight) = (0.0, 0.0, 0.0, 0.0);
    for y in y0..y1 {
        for x in x0..x1 {
            let value = f64::from(image.data[y * image.width + x]) - background;
            if !value.is_finite() || value <= 0.0 {
                continue;
            }
            let (dx, dy) = (x as f64 - cx, y as f64 - cy);
            xx += value * dx * dx;
            yy += value * dy * dy;
            xy += value * dx * dy;
            weight += value;
        }
    }
    (xx, yy, xy, weight)
}

/// Turns a list of stars into the summary a frame is judged by.
fn summarise(stars: Vec<Star>) -> StarField {
    let saturated = stars.iter().filter(|s| s.saturated).count();

    let mut widths: Vec<f64> = stars
        .iter()
        .filter(|s| !s.saturated && s.fwhm.is_finite())
        .map(|s| s.fwhm)
        .collect();
    let mut roundnesses: Vec<f64> = stars
        .iter()
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
            let expected = FWHM_PER_SIGMA * sigma;
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

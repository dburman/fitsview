//! Taking a smooth gradient out of an image.
//!
//! A frame's sky is rarely even. Light pollution brightens one side, the moon
//! another, and vignetting that a flat did not quite remove darkens the corners.
//! None of it is the target, and all of it makes the target hard to judge.
//!
//! The gradient is measured where there is only sky, and a smooth surface
//! fitted through it: the frame is cut into boxes, each box's sky taken as the
//! median of what is left when the stars are clipped away, and a polynomial of
//! low order fitted through those, dropping any box that sits well above the
//! surface — which is where nebulosity or a galaxy is — and fitting again.
//! The surface is then taken off, and the typical sky level put back so that a
//! stretch still has a sky to set.
//!
//! **It cannot tell a gradient from a nebula as big as the frame.** A cloud of
//! gas that fills the field is as smooth as light pollution, and some of it
//! comes out with the gradient. A low order, and the dropping of boxes that
//! stand above the rest, keep that to what is larger than a good part of the
//! frame.

use rayon::prelude::*;

use crate::image::FitsImage;

/// How the gradient is found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Boxes across the longer side of the frame.
    pub boxes: usize,
    /// The highest power of the surface: 1 is a tilted plane, 2 a bowl or a
    /// saddle, 3 a little more. Higher bends round nebulosity and takes it out.
    pub degree: usize,
    /// How far above the surface a box may sit, in deviations of the boxes'
    /// scatter about it, before it is taken for something in the sky.
    pub rejection: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            boxes: 32,
            degree: 2,
            rejection: 2.0,
        }
    }
}

/// A fitted gradient, one surface for each channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    width: usize,
    height: usize,
    degree: usize,
    /// Each channel's coefficients, for the powers [`terms`] lists.
    coefficients: Vec<Vec<f64>>,
    /// Each channel's typical sky, put back once the surface is taken off.
    level: Vec<f64>,
    /// How many boxes each channel's surface was fitted through.
    pub boxes_used: Vec<usize>,
}

/// The powers of x and y, in order, for a surface of `degree`.
fn terms(degree: usize) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    for total in 0..=degree {
        for i in 0..=total {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            out.push(((total - i) as i32, i as i32));
        }
    }
    out
}

/// A pixel's place across the frame, from −1 to 1, which keeps the fit well
/// conditioned whatever the frame's size.
fn across(position: f64, size: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let last = (size.max(2) - 1) as f64;
    2.0 * position / last - 1.0
}

/// Fits the gradient of `image`, or `None` for a frame too small, or too
/// empty, to measure one in.
#[must_use]
pub fn fit(image: &FitsImage, settings: &Settings) -> Option<Model> {
    let (width, height) = (image.width, image.height);
    let side = (width.max(height) / settings.boxes.max(2)).max(4);
    let (across_x, down_y) = (width / side, height / side);
    let terms = terms(settings.degree);
    if across_x * down_y < 2 * terms.len() {
        return None;
    }
    let plane = width * height;

    let mut coefficients = Vec::with_capacity(image.channels);
    let mut level = Vec::with_capacity(image.channels);
    let mut boxes_used = Vec::with_capacity(image.channels);
    for channel in 0..image.channels {
        let data = &image.data[channel * plane..(channel + 1) * plane];
        let boxes: Vec<(f64, f64, f64)> = (0..across_x * down_y)
            .into_par_iter()
            .filter_map(|k| {
                let (bx, by) = (k % across_x, k / across_x);
                let sky = box_sky(data, width, bx * side, by * side, side)?;
                #[allow(clippy::cast_precision_loss)]
                let (cx, cy) = (
                    (bx * side) as f64 + side as f64 / 2.0,
                    (by * side) as f64 + side as f64 / 2.0,
                );
                Some((across(cx, width), across(cy, height), sky))
            })
            .collect();
        let (fitted, kept) = fit_rejecting(&boxes, &terms, settings.rejection)?;
        // The typical sky is the median of the boxes the surface went through:
        // what the sky is, without what lifts or dims it.
        let mut skies: Vec<f64> = kept.iter().map(|&i| boxes[i].2).collect();
        let middle = skies.len() / 2;
        skies.select_nth_unstable_by(middle, f64::total_cmp);
        level.push(skies[middle]);
        boxes_used.push(kept.len());
        coefficients.push(fitted);
    }

    Some(Model {
        width,
        height,
        degree: settings.degree,
        coefficients,
        level,
        boxes_used,
    })
}

/// A box's sky: the median of its pixels once the stars are clipped away.
///
/// Up to about a thousand pixels of the box are looked at, evenly spread.
/// `None` for a box more than half undefined.
fn box_sky(data: &[f32], width: usize, x0: usize, y0: usize, side: usize) -> Option<f64> {
    let step = ((side * side) / 1024).max(1);
    let stride = (step as f64).sqrt().ceil();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let stride = (stride as usize).max(1);
    let mut values: Vec<f32> = Vec::with_capacity(1100);
    let mut looked = 0usize;
    for y in (y0..y0 + side).step_by(stride) {
        let row = &data[y * width..(y + 1) * width];
        for x in (x0..x0 + side).step_by(stride) {
            looked += 1;
            let v = row[x];
            if v.is_finite() {
                values.push(v);
            }
        }
    }
    if values.len() * 2 < looked {
        return None;
    }
    // Twice: the median and its spread, then the median of what is not
    // bright above that — stars, and the bright core of anything else.
    for _ in 0..2 {
        let middle = values.len() / 2;
        values.select_nth_unstable_by(middle, f32::total_cmp);
        let median = values[middle];
        let mut deviations: Vec<f32> = values.iter().map(|v| (v - median).abs()).collect();
        let (_, spread, _) = deviations.select_nth_unstable_by(middle, f32::total_cmp);
        let limit = median + 3.0 * 1.4826 * *spread;
        values.retain(|v| *v <= limit);
        if values.is_empty() {
            return None;
        }
    }
    let middle = values.len() / 2;
    values.select_nth_unstable_by(middle, f32::total_cmp);
    Some(f64::from(values[middle]))
}

/// Fits a surface through `boxes`, dropping those far from it and fitting
/// again until none are dropped. Returns the coefficients and which boxes
/// were kept, or `None` if too few are left to fit.
fn fit_rejecting(
    boxes: &[(f64, f64, f64)],
    terms: &[(i32, i32)],
    rejection: f64,
) -> Option<(Vec<f64>, Vec<usize>)> {
    let mut kept: Vec<usize> = (0..boxes.len()).collect();
    let mut fitted = solve(boxes, &kept, terms)?;
    for _ in 0..10 {
        let residuals: Vec<f64> = kept
            .iter()
            .map(|&i| boxes[i].2 - surface(&fitted, terms, boxes[i].0, boxes[i].1))
            .collect();
        let mut absolute: Vec<f64> = residuals.iter().map(|r| r.abs()).collect();
        let middle = absolute.len() / 2;
        absolute.select_nth_unstable_by(middle, f64::total_cmp);
        let scatter = 1.4826 * absolute[middle];
        if scatter <= 0.0 {
            break;
        }
        // Above the surface is what is in the sky, and goes readily; below it
        // is rarer, a dark lane or a box beside the frame's edge, and has to be
        // further out to go.
        let before = kept.len();
        kept = kept
            .iter()
            .zip(&residuals)
            .filter(|(_, r)| **r <= rejection * scatter && **r >= -2.0 * rejection * scatter)
            .map(|(&i, _)| i)
            .collect();
        if kept.len() < 2 * terms.len() {
            return None;
        }
        fitted = solve(boxes, &kept, terms)?;
        if kept.len() == before {
            break;
        }
    }
    Some((fitted, kept))
}

/// The surface at `(x, y)`.
fn surface(coefficients: &[f64], terms: &[(i32, i32)], x: f64, y: f64) -> f64 {
    coefficients
        .iter()
        .zip(terms)
        .map(|(c, (i, j))| c * x.powi(*i) * y.powi(*j))
        .sum()
}

/// Least squares through the `kept` boxes, by the normal equations, which for
/// a handful of terms over coordinates from −1 to 1 are well enough
/// conditioned.
fn solve(boxes: &[(f64, f64, f64)], kept: &[usize], terms: &[(i32, i32)]) -> Option<Vec<f64>> {
    let n = terms.len();
    let mut matrix = vec![vec![0.0f64; n + 1]; n];
    for &k in kept {
        let (x, y, value) = boxes[k];
        let basis: Vec<f64> = terms.iter().map(|(i, j)| x.powi(*i) * y.powi(*j)).collect();
        for r in 0..n {
            for c in 0..n {
                matrix[r][c] += basis[r] * basis[c];
            }
            matrix[r][n] += basis[r] * value;
        }
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..n {
        let pivot =
            (col..n).max_by(|a, b| matrix[*a][col].abs().total_cmp(&matrix[*b][col].abs()))?;
        if matrix[pivot][col].abs() < 1e-12 {
            return None;
        }
        matrix.swap(col, pivot);
        for row in col + 1..n {
            let factor = matrix[row][col] / matrix[col][col];
            let (above, below) = matrix.split_at_mut(row);
            for (target, source) in below[0][col..].iter_mut().zip(&above[col][col..]) {
                *target -= factor * source;
            }
        }
    }
    let mut out = vec![0.0; n];
    for row in (0..n).rev() {
        let known: f64 = (row + 1..n).map(|c| matrix[row][c] * out[c]).sum();
        out[row] = (matrix[row][n] - known) / matrix[row][row];
    }
    Some(out)
}

/// `image` with `model` taken off and the typical sky put back.
///
/// Worked a row at a time: along a row the surface is a polynomial in x alone,
/// whose coefficients are found once for the row, so each pixel costs a few
/// multiplications rather than one for every term.
#[must_use]
pub fn remove(image: &FitsImage, model: &Model) -> FitsImage {
    let (width, height) = (image.width, image.height);
    if (width, height) != (model.width, model.height) || image.channels != model.coefficients.len()
    {
        return image.clone();
    }
    let terms = terms(model.degree);
    let plane = width * height;
    let mut data = image.data.clone();
    for (channel, (coefficients, level)) in model.coefficients.iter().zip(&model.level).enumerate()
    {
        data[channel * plane..(channel + 1) * plane]
            .par_chunks_mut(width)
            .enumerate()
            .for_each(|(y, row)| {
                #[allow(clippy::cast_precision_loss)]
                let v = across(y as f64, height);
                // The row's polynomial in x: one coefficient per power.
                let mut in_x = vec![0.0f64; model.degree + 1];
                for (c, (i, j)) in coefficients.iter().zip(&terms) {
                    #[allow(clippy::cast_sign_loss)]
                    {
                        in_x[*i as usize] += c * v.powi(*j);
                    }
                }
                for (x, value) in row.iter_mut().enumerate() {
                    if !value.is_finite() {
                        continue;
                    }
                    #[allow(clippy::cast_precision_loss)]
                    let u = across(x as f64, width);
                    let gradient = in_x.iter().rev().fold(0.0, |sum, c| sum * u + c);
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        *value = (f64::from(*value) - gradient + level) as f32;
                    }
                }
            });
    }
    let (min, max) = crate::image::finite_min_max(&data);
    FitsImage {
        width,
        height,
        channels: image.channels,
        data,
        header: image.header.clone(),
        min,
        max,
    }
}

/// `image` with its gradient taken out, or unchanged if none could be measured.
#[must_use]
pub fn flatten(image: &FitsImage, settings: &Settings) -> FitsImage {
    match fit(image, settings) {
        Some(model) => remove(image, &model),
        None => image.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::FitsHeader;
    use crate::testutil::gaussian_background;

    const W: usize = 300;
    const H: usize = 200;

    fn image(channels: usize, value: impl Fn(usize, usize, usize) -> f64) -> FitsImage {
        let data: Vec<f32> = (0..channels)
            .flat_map(|c| (0..W * H).map(move |k| (c, k)))
            .map(|(c, k)| {
                #[allow(clippy::cast_possible_truncation)]
                {
                    value(c, k % W, k / W) as f32
                }
            })
            .collect();
        let (min, max) = crate::image::finite_min_max(&data);
        FitsImage {
            width: W,
            height: H,
            channels,
            data,
            header: FitsHeader::default(),
            min,
            max,
        }
    }

    /// Light pollution from one side and vignetting a flat did not take out.
    fn gradient(x: usize, y: usize) -> f64 {
        let (u, v) = (across(x as f64, W), across(y as f64, H));
        80.0 * u + 60.0 * v - 100.0 * (u * u + v * v)
    }

    /// Stars at a few places, four pixels across.
    fn stars(x: usize, y: usize) -> f64 {
        [(50.0, 40.0), (220.0, 150.0), (150.0, 100.0), (270.0, 30.0)]
            .iter()
            .map(|(sx, sy): &(f64, f64)| {
                let r2 = (x as f64 - sx).powi(2) + (y as f64 - sy).powi(2);
                3_000.0 * (-r2 / 8.0).exp()
            })
            .sum()
    }

    /// The median of each 20-pixel block's median: how even the sky is.
    fn block_medians(image: &FitsImage, channel: usize) -> Vec<f64> {
        let plane = &image.data[channel * W * H..(channel + 1) * W * H];
        let mut out = Vec::new();
        for by in 0..H / 20 {
            for bx in 0..W / 20 {
                let mut v: Vec<f32> = (0..20)
                    .flat_map(|y| (0..20).map(move |x| plane[(by * 20 + y) * W + bx * 20 + x]))
                    .collect();
                let m = v.len() / 2;
                out.push(f64::from(*v.select_nth_unstable_by(m, f32::total_cmp).1));
            }
        }
        out
    }

    fn spread(values: &[f64]) -> f64 {
        let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        hi - lo
    }

    #[test]
    fn the_surface_has_the_terms_its_degree_asks_for() {
        assert_eq!(terms(1).len(), 3);
        assert_eq!(terms(2).len(), 6);
        assert_eq!(terms(3).len(), 10);
    }

    #[test]
    fn a_tilt_and_a_bowl_come_out_and_the_stars_stay() {
        let noise = gaussian_background(W, H, 0.0, 5.0, 3);
        let before = image(1, |_, x, y| {
            1000.0 + gradient(x, y) + stars(x, y) + noise[y * W + x]
        });
        assert!(
            spread(&block_medians(&before, 0)) > 150.0,
            "the test needs a gradient"
        );

        let after = flatten(&before, &Settings::default());
        let uneven = spread(&block_medians(&before, 0));
        let even = spread(&block_medians(&after, 0));
        // Blocks holding a star's wings sit a little higher whatever is done,
        // so what is asked is that nineteen parts in twenty of the variation
        // go, and that a few counts at most are left.
        assert!(
            even < 10.0 && even < uneven / 20.0,
            "the sky still varies by {even:.1} of {uneven:.1}"
        );

        // A star stands as far above its sky as it did.
        let star =
            |im: &FitsImage| f64::from(im.data[100 * W + 150]) - f64::from(im.data[100 * W + 165]);
        assert!(
            (star(&after) / star(&before) - 1.0).abs() < 0.02,
            "{} against {}",
            star(&after),
            star(&before)
        );
    }

    #[test]
    fn a_nebula_smaller_than_the_frame_is_mostly_kept() {
        // Faint, soft, an eighth of the frame across: what a gradient fit must
        // not eat.
        let noise = gaussian_background(W, H, 0.0, 3.0, 5);
        let nebula = |x: usize, y: usize| {
            let r2 = (x as f64 - 150.0).powi(2) + (y as f64 - 100.0).powi(2);
            60.0 * (-r2 / (2.0 * 25.0f64.powi(2))).exp()
        };
        let before = image(1, |_, x, y| {
            1000.0 + gradient(x, y) + nebula(x, y) + noise[y * W + x]
        });
        let after = flatten(&before, &Settings::default());
        let at = |x: usize, y: usize| f64::from(after.data[y * W + x]);
        let middle = (145..155)
            .flat_map(|x| (95..105).map(move |y| (x, y)))
            .map(|(x, y)| at(x, y))
            .sum::<f64>()
            / 100.0;
        let away = (20..30)
            .flat_map(|x| (95..105).map(move |y| (x, y)))
            .map(|(x, y)| at(x, y))
            .sum::<f64>()
            / 100.0;
        let kept = (middle - away) / 60.0;
        assert!(
            kept > 0.8,
            "only {:.0}% of the nebula is left",
            100.0 * kept
        );
    }

    #[test]
    fn an_even_sky_is_left_alone() {
        let noise = gaussian_background(W, H, 0.0, 5.0, 7);
        let before = image(1, |_, x, y| 1000.0 + noise[y * W + x]);
        let after = flatten(&before, &Settings::default());
        let worst = before
            .data
            .iter()
            .zip(&after.data)
            .map(|(a, b)| (f64::from(*a) - f64::from(*b)).abs())
            .fold(0.0, f64::max);
        assert!(worst < 2.0, "changed by up to {worst:.2}");
    }

    #[test]
    fn each_colour_loses_its_own_gradient_and_keeps_its_own_level() {
        let noise = gaussian_background(W, H, 0.0, 4.0, 9);
        let levels = [900.0, 1400.0, 700.0];
        let before = image(3, |c, x, y| {
            let tilt = match c {
                0 => 1.0,
                1 => -0.5,
                _ => 2.0,
            };
            levels[c] + tilt * gradient(x, y) + noise[y * W + x]
        });
        let after = flatten(&before, &Settings::default());
        let typical = |blocks: &[f64]| {
            let mut sorted = blocks.to_vec();
            sorted.sort_by(f64::total_cmp);
            sorted[sorted.len() / 2]
        };
        for c in 0..levels.len() {
            let blocks = block_medians(&after, c);
            assert!(
                spread(&blocks) < 8.0,
                "channel {c} still varies by {:.1}",
                spread(&blocks)
            );
            // Each colour's sky stays where its own typical sky was: the
            // colour balance of the frame is not changed.
            let was = typical(&block_medians(&before, c));
            let is = typical(&blocks);
            assert!(
                (is - was).abs() < 5.0,
                "channel {c} sits at {is:.0}, was {was:.0}"
            );
        }
    }

    #[test]
    fn undefined_pixels_stay_undefined_and_a_tiny_frame_is_left_as_it_is() {
        let noise = gaussian_background(W, H, 0.0, 5.0, 11);
        let mut before = image(1, |_, x, y| 1000.0 + gradient(x, y) + noise[y * W + x]);
        before.data[5 * W + 5] = f32::NAN;
        let after = flatten(&before, &Settings::default());
        assert!(after.data[5 * W + 5].is_nan());

        let tiny = FitsImage {
            width: 8,
            height: 8,
            channels: 1,
            data: vec![1.0; 64],
            header: FitsHeader::default(),
            min: 1.0,
            max: 1.0,
        };
        assert!(fit(&tiny, &Settings::default()).is_none());
        assert_eq!(flatten(&tiny, &Settings::default()).data, tiny.data);
    }
}

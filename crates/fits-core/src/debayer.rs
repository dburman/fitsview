//! Reconstructing colour from a one-shot colour camera's mosaic.
//!
//! A one-shot colour sensor is monochrome under a grid of tiny colour filters,
//! so a raw frame is a single channel in which each pixel measured only red,
//! green or blue. This module reconstructs the two missing channels at every
//! pixel by interpolation.
//!
//! **That invents detail**, which is fine for looking at an image and wrong for
//! measuring one. Calibration therefore happens on the mosaic, before this, and
//! exported files stay as mosaics. See [`crate::calib::calibrate`].

use rayon::prelude::*;

use crate::header::FitsHeader;
use crate::image::FitsImage;

/// Which filter sits over a pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    /// Red.
    Red,
    /// Green, of which a Bayer tile has two.
    Green,
    /// Blue.
    Blue,
}

impl Colour {
    /// The plane this colour occupies in a three-channel image.
    #[must_use]
    pub const fn plane(self) -> usize {
        match self {
            Colour::Red => 0,
            Colour::Green => 1,
            Colour::Blue => 2,
        }
    }
}

/// The 2x2 filter tile, named by its pixels read across then down.
///
/// `Rggb` means red at the top left, green at the top right and bottom left,
/// blue at the bottom right, where "top" is the first row **as stored in the
/// file**. FITS stores the bottom row of an image first, and capture programs
/// disagree about which end `BAYERPAT` refers to, which is why
/// [`BayerPattern::flipped_rows`] exists and the interface offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BayerPattern {
    /// Red, green / green, blue.
    Rggb,
    /// Blue, green / green, red.
    Bggr,
    /// Green, red / blue, green.
    Grbg,
    /// Green, blue / red, green.
    Gbrg,
}

impl BayerPattern {
    /// Every pattern, for offering a choice.
    pub const ALL: [BayerPattern; 4] = [
        BayerPattern::Rggb,
        BayerPattern::Bggr,
        BayerPattern::Grbg,
        BayerPattern::Gbrg,
    ];

    /// The name a header would use.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            BayerPattern::Rggb => "RGGB",
            BayerPattern::Bggr => "BGGR",
            BayerPattern::Grbg => "GRBG",
            BayerPattern::Gbrg => "GBRG",
        }
    }

    /// Parses a `BAYERPAT` value, ignoring case, quotes and padding.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let cleaned: String = value
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .collect::<String>()
            .to_ascii_uppercase();
        match cleaned.as_str() {
            "RGGB" => Some(BayerPattern::Rggb),
            "BGGR" => Some(BayerPattern::Bggr),
            "GRBG" => Some(BayerPattern::Grbg),
            "GBRG" => Some(BayerPattern::Gbrg),
            _ => None,
        }
    }

    /// The tile, as four colours read across then down.
    #[must_use]
    const fn tile(self) -> [Colour; 4] {
        match self {
            BayerPattern::Rggb => [Colour::Red, Colour::Green, Colour::Green, Colour::Blue],
            BayerPattern::Bggr => [Colour::Blue, Colour::Green, Colour::Green, Colour::Red],
            BayerPattern::Grbg => [Colour::Green, Colour::Red, Colour::Blue, Colour::Green],
            BayerPattern::Gbrg => [Colour::Green, Colour::Blue, Colour::Red, Colour::Green],
        }
    }

    /// Which filter sits over the pixel at these coordinates.
    #[must_use]
    pub const fn colour_at(self, x: usize, y: usize) -> Colour {
        self.tile()[(y % 2) * 2 + (x % 2)]
    }

    /// The pattern seen when the tile is shifted by this many pixels.
    ///
    /// Shifting a Bayer tile always yields another Bayer tile, which is why the
    /// `XBAYROFF` and `YBAYROFF` offsets need no special handling anywhere
    /// else: they simply select a different pattern.
    #[must_use]
    pub fn shifted(self, dx: usize, dy: usize) -> Self {
        let at = |x: usize, y: usize| self.colour_at(x + dx, y + dy);
        Self::from_tile([at(0, 0), at(1, 0), at(0, 1), at(1, 1)]).unwrap_or(self)
    }

    /// The pattern seen when the rows are read in the opposite order.
    ///
    /// The fix for a file whose `BAYERPAT` refers to the sensor's top-left
    /// while FITS stores the bottom row first. The symptom of needing it is an
    /// image in the wrong colours, not a broken one.
    #[must_use]
    pub fn flipped_rows(self) -> Self {
        let tile = self.tile();
        Self::from_tile([tile[2], tile[3], tile[0], tile[1]]).unwrap_or(self)
    }

    /// The pattern matching a tile, if it is a valid Bayer arrangement.
    fn from_tile(tile: [Colour; 4]) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.tile() == tile)
    }
}

/// Why a mosaic could not be reconstructed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DebayerError {
    /// The image already has colour channels.
    #[error("this image already has {channels} channels, so it is not a mosaic")]
    NotAMosaic {
        /// How many channels it has.
        channels: usize,
    },

    /// The image is too small for a 2x2 tile to mean anything.
    #[error("an image of {width}x{height} is too small to debayer")]
    TooSmall {
        /// Width in pixels.
        width: usize,
        /// Height in pixels.
        height: usize,
    },
}

/// Reads the Bayer pattern a header declares, if it declares one.
///
/// Applies `XBAYROFF` and `YBAYROFF` when present, since a shifted tile is
/// simply a different pattern.
#[must_use]
pub fn detect(header: &FitsHeader) -> Option<BayerPattern> {
    let pattern = header
        .get("BAYERPAT")
        .or_else(|| header.get("COLORTYP"))
        .and_then(BayerPattern::parse)?;

    let offset = |key: &str| {
        header
            .get_i64(key)
            .and_then(|v| usize::try_from(v.rem_euclid(2)).ok())
            .unwrap_or(0)
    };
    Some(pattern.shifted(offset("XBAYROFF"), offset("YBAYROFF")))
}

/// Reconstructs three channels from a single-channel mosaic.
///
/// Each pixel keeps its own measurement, and each missing channel is the mean
/// of the neighbours in the surrounding 3x3 that carry it. That is bilinear
/// demosaicing. It degrades gracefully at the edges, where fewer neighbours
/// exist, and undefined pixels contribute nothing rather than poisoning their
/// neighbours.
///
/// # Errors
///
/// Returns [`DebayerError::NotAMosaic`] for an image that already has colour,
/// and [`DebayerError::TooSmall`] for one smaller than a tile.
pub fn debayer(image: &FitsImage, pattern: BayerPattern) -> Result<FitsImage, DebayerError> {
    if image.channels != 1 {
        return Err(DebayerError::NotAMosaic {
            channels: image.channels,
        });
    }
    if image.width < 2 || image.height < 2 {
        return Err(DebayerError::TooSmall {
            width: image.width,
            height: image.height,
        });
    }

    let (width, height) = (image.width, image.height);
    let plane = width * height;
    let mut data = vec![f32::NAN; plane * 3];

    // Split into the three planes so each row can be written without sharing.
    let (red, rest) = data.split_at_mut(plane);
    let (green, blue) = rest.split_at_mut(plane);

    red.par_chunks_mut(width)
        .zip(green.par_chunks_mut(width))
        .zip(blue.par_chunks_mut(width))
        .enumerate()
        .for_each(|(y, ((red_row, green_row), blue_row))| {
            for x in 0..width {
                let [r, g, b] = reconstruct(image, pattern, x, y);
                red_row[x] = r;
                green_row[x] = g;
                blue_row[x] = b;
            }
        });

    let (min, max) = crate::image::finite_min_max(&data);
    Ok(FitsImage {
        width,
        height,
        channels: 3,
        data,
        header: image.header.clone(),
        min,
        max,
    })
}

/// The three channels at one pixel.
fn reconstruct(image: &FitsImage, pattern: BayerPattern, x: usize, y: usize) -> [f32; 3] {
    let own = pattern.colour_at(x, y);
    let mut out = [f32::NAN; 3];

    // The measurement this pixel actually made.
    let measured = image.data[y * image.width + x];
    if measured.is_finite() {
        out[own.plane()] = measured;
    }

    // The other two channels come from the neighbours that carry them.
    let mut sums = [0.0f64; 3];
    let mut counts = [0u32; 3];

    let x0 = x.saturating_sub(1);
    let y0 = y.saturating_sub(1);
    let x1 = (x + 1).min(image.width - 1);
    let y1 = (y + 1).min(image.height - 1);

    for ny in y0..=y1 {
        for nx in x0..=x1 {
            if nx == x && ny == y {
                continue;
            }
            let value = image.data[ny * image.width + nx];
            if !value.is_finite() {
                continue;
            }
            let plane = pattern.colour_at(nx, ny).plane();
            sums[plane] += f64::from(value);
            counts[plane] += 1;
        }
    }

    for plane in 0..3 {
        if plane == own.plane() {
            continue;
        }
        if counts[plane] > 0 {
            #[allow(clippy::cast_possible_truncation)]
            {
                out[plane] = (sums[plane] / f64::from(counts[plane])) as f32;
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_fits_from_bytes;
    use crate::testutil::{synthetic_fits, SyntheticSpec};

    /// A mono image from physical values.
    fn mosaic(width: usize, height: usize, pixels: &[f64]) -> FitsImage {
        let spec = SyntheticSpec::new(width, height, -32);
        read_fits_from_bytes(&synthetic_fits(&spec, pixels).unwrap()).unwrap()
    }

    /// The sample at (x, y) of a channel.
    fn at(image: &FitsImage, x: usize, y: usize, channel: usize) -> f32 {
        image.data[channel * image.width * image.height + y * image.width + x]
    }

    #[test]
    fn each_pattern_names_its_own_tile() {
        assert_eq!(BayerPattern::Rggb.colour_at(0, 0), Colour::Red);
        assert_eq!(BayerPattern::Rggb.colour_at(1, 0), Colour::Green);
        assert_eq!(BayerPattern::Rggb.colour_at(0, 1), Colour::Green);
        assert_eq!(BayerPattern::Rggb.colour_at(1, 1), Colour::Blue);

        assert_eq!(BayerPattern::Bggr.colour_at(0, 0), Colour::Blue);
        assert_eq!(BayerPattern::Bggr.colour_at(1, 1), Colour::Red);

        assert_eq!(BayerPattern::Grbg.colour_at(1, 0), Colour::Red);
        assert_eq!(BayerPattern::Grbg.colour_at(0, 1), Colour::Blue);

        assert_eq!(BayerPattern::Gbrg.colour_at(1, 0), Colour::Blue);
        assert_eq!(BayerPattern::Gbrg.colour_at(0, 1), Colour::Red);
    }

    #[test]
    fn the_tile_repeats_every_two_pixels() {
        for pattern in BayerPattern::ALL {
            for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                assert_eq!(pattern.colour_at(x, y), pattern.colour_at(x + 2, y));
                assert_eq!(pattern.colour_at(x, y), pattern.colour_at(x, y + 2));
                assert_eq!(pattern.colour_at(x, y), pattern.colour_at(x + 8, y + 6));
            }
        }
    }

    #[test]
    fn shifting_by_one_turns_a_pattern_into_another() {
        assert_eq!(BayerPattern::Rggb.shifted(1, 0), BayerPattern::Grbg);
        assert_eq!(BayerPattern::Rggb.shifted(0, 1), BayerPattern::Gbrg);
        assert_eq!(BayerPattern::Rggb.shifted(1, 1), BayerPattern::Bggr);
    }

    #[test]
    fn shifting_twice_returns_to_the_start() {
        for pattern in BayerPattern::ALL {
            assert_eq!(pattern.shifted(2, 0), pattern);
            assert_eq!(pattern.shifted(0, 2), pattern);
            assert_eq!(pattern.shifted(1, 0).shifted(1, 0), pattern);
            assert_eq!(pattern.shifted(1, 1).shifted(1, 1), pattern);
        }
    }

    #[test]
    fn flipping_the_rows_swaps_the_tile_and_undoes_itself() {
        assert_eq!(BayerPattern::Rggb.flipped_rows(), BayerPattern::Gbrg);
        assert_eq!(BayerPattern::Bggr.flipped_rows(), BayerPattern::Grbg);
        for pattern in BayerPattern::ALL {
            assert_eq!(pattern.flipped_rows().flipped_rows(), pattern);
        }
    }

    #[test]
    fn pattern_names_parse_however_a_header_writes_them() {
        for (text, expected) in [
            ("RGGB", BayerPattern::Rggb),
            ("rggb", BayerPattern::Rggb),
            ("'RGGB    '", BayerPattern::Rggb),
            (" BGGR ", BayerPattern::Bggr),
            ("GrBg", BayerPattern::Grbg),
        ] {
            assert_eq!(BayerPattern::parse(text), Some(expected), "input {text:?}");
        }
        for bad in ["", "XYZW", "RGB", "RGGBB"] {
            assert_eq!(BayerPattern::parse(bad), None, "input {bad:?}");
        }
    }

    #[test]
    fn a_name_round_trips_through_parsing() {
        for pattern in BayerPattern::ALL {
            assert_eq!(BayerPattern::parse(pattern.name()), Some(pattern));
        }
    }

    fn header(cards: &[(&str, &str)]) -> FitsHeader {
        FitsHeader {
            cards: cards
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_header_declaring_a_pattern_is_detected() {
        assert_eq!(
            detect(&header(&[("BAYERPAT", "RGGB")])),
            Some(BayerPattern::Rggb)
        );
        assert_eq!(detect(&header(&[("OBJECT", "M31")])), None);
    }

    #[test]
    fn bayer_offsets_select_a_different_pattern() {
        // A shifted tile is another tile, so offsets need no special handling.
        let shifted = detect(&header(&[
            ("BAYERPAT", "RGGB"),
            ("XBAYROFF", "1"),
            ("YBAYROFF", "0"),
        ]));
        assert_eq!(shifted, Some(BayerPattern::Grbg));

        let both = detect(&header(&[
            ("BAYERPAT", "RGGB"),
            ("XBAYROFF", "1"),
            ("YBAYROFF", "1"),
        ]));
        assert_eq!(both, Some(BayerPattern::Bggr));
    }

    #[test]
    fn even_offsets_change_nothing_and_odd_ones_wrap() {
        let at_offset = |x: &str, y: &str| {
            detect(&header(&[
                ("BAYERPAT", "RGGB"),
                ("XBAYROFF", x),
                ("YBAYROFF", y),
            ]))
        };
        assert_eq!(at_offset("2", "4"), Some(BayerPattern::Rggb));
        assert_eq!(at_offset("3", "0"), Some(BayerPattern::Grbg));
        // A negative offset still lands on a valid tile.
        assert_eq!(at_offset("-1", "0"), Some(BayerPattern::Grbg));
    }

    /// Builds a mosaic by sampling a colour image through a filter pattern.
    fn mosaic_from(
        width: usize,
        height: usize,
        pattern: BayerPattern,
        colour: impl Fn(usize, usize) -> [f64; 3],
    ) -> FitsImage {
        let pixels: Vec<f64> = (0..width * height)
            .map(|i| {
                let (x, y) = (i % width, i / width);
                colour(x, y)[pattern.colour_at(x, y).plane()]
            })
            .collect();
        mosaic(width, height, &pixels)
    }

    #[test]
    fn a_flat_field_of_one_colour_comes_back_as_that_colour() {
        // The simplest possible check, and the one that catches an edge bug:
        // every pixel, corners included, should read the same.
        let (w, h) = (16, 16);
        let source = [1000.0, 400.0, 200.0];
        let image = mosaic_from(w, h, BayerPattern::Rggb, |_, _| source);

        let out = debayer(&image, BayerPattern::Rggb).unwrap();
        assert_eq!(out.channels, 3);

        for y in 0..h {
            for x in 0..w {
                for (plane, expected) in source.iter().enumerate() {
                    let got = at(&out, x, y, plane);
                    #[allow(clippy::cast_possible_truncation)]
                    let expected = *expected as f32;
                    assert!(
                        (got - expected).abs() < 0.01,
                        "at ({x}, {y}) plane {plane}: got {got}, wanted {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_pattern_reconstructs_its_own_mosaic() {
        let (w, h) = (12, 12);
        let source = [900.0, 500.0, 100.0];
        for pattern in BayerPattern::ALL {
            let image = mosaic_from(w, h, pattern, |_, _| source);
            let out = debayer(&image, pattern).unwrap();
            for (plane, expected) in source.iter().enumerate() {
                #[allow(clippy::cast_possible_truncation)]
                let expected = *expected as f32;
                assert!(
                    (at(&out, 5, 5, plane) - expected).abs() < 0.01,
                    "{} plane {plane}",
                    pattern.name()
                );
            }
        }
    }

    #[test]
    fn a_smooth_colour_gradient_is_recovered_approximately() {
        // Interpolation cannot be exact, but it should be close on data that
        // varies slowly, which is what a real sky background looks like.
        let (w, h) = (32, 32);
        #[allow(clippy::cast_precision_loss)]
        let colour = |x: usize, y: usize| {
            [
                1000.0 + x as f64 * 4.0,
                600.0 + y as f64 * 2.0,
                300.0 + (x + y) as f64,
            ]
        };
        let image = mosaic_from(w, h, BayerPattern::Rggb, colour);
        let out = debayer(&image, BayerPattern::Rggb).unwrap();

        // Away from the edges, where all neighbours exist.
        for y in 2..h - 2 {
            for x in 2..w - 2 {
                let wanted = colour(x, y);
                for (plane, expected) in wanted.iter().enumerate() {
                    let got = f64::from(at(&out, x, y, plane));
                    assert!(
                        (got - expected).abs() < 6.0,
                        "at ({x}, {y}) plane {plane}: got {got}, wanted {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_wrong_pattern_gives_a_visibly_wrong_answer() {
        // Otherwise the pattern choice would be decorative, and the flip
        // control could not be judged by looking at the image.
        let (w, h) = (16, 16);
        let source = [1000.0, 400.0, 100.0];
        let image = mosaic_from(w, h, BayerPattern::Rggb, |_, _| source);

        let right = debayer(&image, BayerPattern::Rggb).unwrap();
        let wrong = debayer(&image, BayerPattern::Bggr).unwrap();

        // Red and blue swap, which is exactly the symptom described in the plan.
        assert!((at(&right, 5, 5, 0) - 1000.0).abs() < 0.01);
        assert!(
            (at(&wrong, 5, 5, 0) - 100.0).abs() < 0.01,
            "expected red and blue to swap, got {}",
            at(&wrong, 5, 5, 0)
        );
    }

    #[test]
    fn undefined_pixels_do_not_poison_their_neighbours() {
        let (w, h) = (8, 8);
        let mut pixels: Vec<f64> = vec![500.0; w * h];
        pixels[3 * w + 3] = f64::NAN;
        let image = mosaic(w, h, &pixels);

        let out = debayer(&image, BayerPattern::Rggb).unwrap();

        // The undefined pixel has no measurement of its own colour.
        let own = BayerPattern::Rggb.colour_at(3, 3).plane();
        assert!(at(&out, 3, 3, own).is_nan(), "an undefined pixel stays so");

        // Its neighbours are still reconstructed from the pixels that remain.
        for plane in 0..3 {
            let got = at(&out, 5, 5, plane);
            assert!((got - 500.0).abs() < 0.01, "plane {plane} was {got}");
        }
    }

    #[test]
    fn a_colour_image_is_refused_rather_than_debayered_twice() {
        let spec = SyntheticSpec::new(4, 4, -32).with_channels(3);
        let colour = read_fits_from_bytes(&synthetic_fits(&spec, &[1.0; 48]).unwrap()).unwrap();
        assert_eq!(
            debayer(&colour, BayerPattern::Rggb).unwrap_err(),
            DebayerError::NotAMosaic { channels: 3 }
        );
    }

    #[test]
    fn an_image_smaller_than_a_tile_is_refused() {
        assert!(matches!(
            debayer(&mosaic(1, 1, &[1.0]), BayerPattern::Rggb).unwrap_err(),
            DebayerError::TooSmall { .. }
        ));
    }

    #[test]
    fn the_dimensions_are_kept_and_the_statistics_recomputed() {
        let (w, h) = (10, 6);
        let image = mosaic_from(w, h, BayerPattern::Rggb, |_, _| [800.0, 400.0, 200.0]);
        let out = debayer(&image, BayerPattern::Rggb).unwrap();

        assert_eq!((out.width, out.height, out.channels), (w, h, 3));
        assert_eq!(out.data.len(), w * h * 3);
        assert!((out.min - 200.0).abs() < 0.01, "min was {}", out.min);
        assert!((out.max - 800.0).abs() < 0.01, "max was {}", out.max);
    }

    #[test]
    fn the_header_travels_with_the_reconstructed_image() {
        let spec = SyntheticSpec::new(4, 4, -32).with_card("OBJECT", "'M42     '");
        let image = read_fits_from_bytes(&synthetic_fits(&spec, &[100.0; 16]).unwrap()).unwrap();
        let out = debayer(&image, BayerPattern::Rggb).unwrap();
        assert_eq!(out.header.get("OBJECT"), Some("M42"));
    }
}

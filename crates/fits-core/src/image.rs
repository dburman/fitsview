//! Image geometry, pixel conversion and statistics.
//!
//! Everything decodes to `f32`, which covers every `BITPIX` this viewer
//! supports and keeps the display and calibration paths simple.

use rayon::prelude::*;

use crate::error::FitsError;
use crate::header::FitsHeader;

/// Pixels handed to one rayon task. Large enough that scheduling overhead is
/// negligible, small enough that a modest image still spreads across cores.
const CHUNK: usize = 65_536;

/// A decoded FITS image.
///
/// `data` is in native FITS order, which means the first row is the **bottom**
/// of the image. The display layer flips it; calibration frames must not be
/// flipped, or they would stop lining up with the lights.
#[derive(Debug, Clone)]
pub struct FitsImage {
    /// Width in pixels, `NAXIS1`.
    pub width: usize,
    /// Height in pixels, `NAXIS2`.
    pub height: usize,
    /// 1 for mono, 3 for RGB.
    pub channels: usize,
    /// `width * height * channels` samples, plane-major when `channels == 3`.
    pub data: Vec<f32>,
    /// The header this image was decoded from.
    pub header: FitsHeader,
    /// Smallest finite sample, or 0.0 if there are none.
    pub min: f32,
    /// Largest finite sample, or 1.0 if there are none.
    pub max: f32,
}

impl FitsImage {
    /// Total number of samples, across all channels.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the image has no samples at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The sample at `(x, y)` in channel `c`, or `None` if out of bounds.
    ///
    /// Row 0 is the bottom of the image, matching FITS order.
    #[must_use]
    pub fn sample(&self, x: usize, y: usize, c: usize) -> Option<f32> {
        if x >= self.width || y >= self.height || c >= self.channels {
            return None;
        }
        self.data
            .get(c * self.width * self.height + y * self.width + x)
            .copied()
    }
}

/// Everything needed to decode a data block, validated once so the conversion
/// loop can assume it is correct.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    /// `NAXIS1`.
    pub width: usize,
    /// `NAXIS2`.
    pub height: usize,
    /// 1 or 3.
    pub channels: usize,
    /// One of 8, 16, 32, 64, -32, -64.
    pub bitpix: i64,
    /// Additive scaling term, `BZERO`, default 0.0.
    pub bzero: f64,
    /// Multiplicative scaling term, `BSCALE`, default 1.0.
    pub bscale: f64,
    /// `BLANK`, the integer value meaning "no data", already scaled to the
    /// physical value it will hold after conversion. Integer `BITPIX` only.
    pub blank_as_f32: Option<f32>,
}

/// The six `BITPIX` values the standard defines.
const LEGAL_BITPIX: [i64; 6] = [8, 16, 32, 64, -32, -64];

impl Geometry {
    /// Reads and validates geometry from a header.
    ///
    /// # Errors
    ///
    /// - [`FitsError::UnsupportedBitpix`] for a `BITPIX` outside the six legal values.
    /// - [`FitsError::BadHeader`] if `NAXIS` or an axis length is missing or unparseable.
    /// - [`FitsError::UnsupportedGeometry`] for shapes this viewer does not display,
    ///   such as a 1-D spectrum or a cube with 4 planes.
    /// - [`FitsError::DimensionOverflow`] if the axis lengths multiply out beyond `usize`.
    pub fn from_header(h: &FitsHeader) -> Result<Self, FitsError> {
        let bitpix = h
            .get_i64("BITPIX")
            .ok_or_else(|| FitsError::BadHeader("missing or unparseable BITPIX".into()))?;
        if !LEGAL_BITPIX.contains(&bitpix) {
            return Err(FitsError::UnsupportedBitpix(bitpix));
        }

        let naxis = h
            .get_i64("NAXIS")
            .ok_or_else(|| FitsError::BadHeader("missing or unparseable NAXIS".into()))?;
        if !(2..=3).contains(&naxis) {
            return Err(FitsError::UnsupportedGeometry(format!(
                "NAXIS is {naxis}; only 2-D images and 3-plane cubes are supported"
            )));
        }

        let axis = |n: i64| -> Result<usize, FitsError> {
            let key = format!("NAXIS{n}");
            let v = h
                .get_i64(&key)
                .ok_or_else(|| FitsError::BadHeader(format!("missing or unparseable {key}")))?;
            usize::try_from(v)
                .ok()
                .filter(|&v| v > 0)
                .ok_or_else(|| FitsError::UnsupportedGeometry(format!("{key} is {v}, must be > 0")))
        };

        let width = axis(1)?;
        let height = axis(2)?;
        let channels = if naxis == 3 { axis(3)? } else { 1 };
        if channels != 1 && channels != 3 {
            return Err(FitsError::UnsupportedGeometry(format!(
                "NAXIS3 is {channels}; only 1 or 3 planes are supported"
            )));
        }

        // Reject anything that would overflow before we try to allocate for it.
        width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(channels))
            .ok_or_else(|| {
                FitsError::DimensionOverflow(format!("{width} x {height} x {channels}"))
            })?;

        let bzero = h.get_f64("BZERO").unwrap_or(0.0);
        let bscale = h.get_f64("BSCALE").unwrap_or(1.0);
        if !bzero.is_finite() || !bscale.is_finite() || bscale == 0.0 {
            return Err(FitsError::BadHeader(format!(
                "BZERO {bzero} / BSCALE {bscale} are not usable"
            )));
        }

        // BLANK only applies to integer BITPIX, and is expressed in raw units,
        // so scale it the same way the pixels will be scaled.
        let blank_as_f32 = if bitpix > 0 {
            h.get_i64("BLANK").map(|raw| {
                #[allow(clippy::cast_precision_loss)]
                let scaled = bzero + bscale * (raw as f64);
                #[allow(clippy::cast_possible_truncation)]
                let v = scaled as f32;
                v
            })
        } else {
            None
        };

        Ok(Geometry {
            width,
            height,
            channels,
            bitpix,
            bzero,
            bscale,
            blank_as_f32,
        })
    }

    /// Total samples across all channels.
    #[must_use]
    pub fn pixel_count(&self) -> usize {
        // Validated not to overflow in `from_header`.
        self.width
            .saturating_mul(self.height)
            .saturating_mul(self.channels)
    }

    /// Bytes each raw sample occupies in the file.
    #[must_use]
    pub fn bytes_per_pixel(&self) -> usize {
        (self.bitpix.unsigned_abs() / 8) as usize
    }

    /// Bytes the data block occupies, before block padding.
    ///
    /// Returns `None` on overflow.
    #[must_use]
    pub fn data_len(&self) -> Option<usize> {
        self.pixel_count().checked_mul(self.bytes_per_pixel())
    }
}

/// Converts a raw big-endian data block into `f32` samples.
///
/// The `BITPIX` match is hoisted out of the pixel loop deliberately: branching
/// per pixel costs meaningfully more than branching per chunk, and each arm
/// below is a tight loop the optimiser can vectorise.
///
/// `out` must have exactly `geom.pixel_count()` elements and `raw` exactly
/// `geom.data_len()` bytes; the reader guarantees both.
pub fn convert_pixels(geom: &Geometry, raw: &[u8], out: &mut [f32]) {
    debug_assert_eq!(out.len(), geom.pixel_count());
    debug_assert_eq!(Some(raw.len()), geom.data_len());

    let bpp = geom.bytes_per_pixel();
    let (bzero, bscale) = (geom.bzero, geom.bscale);

    // Unsigned 16-bit, written as signed with BZERO 32768, is what almost every
    // astronomy camera produces. Handling it in one step avoids a second pass.
    let fast_u16 = geom.bitpix == 16 && bzero == 32768.0 && bscale == 1.0;
    let needs_scaling = !fast_u16 && (bzero != 0.0 || bscale != 1.0);
    let blank = geom.blank_as_f32;

    out.par_chunks_mut(CHUNK)
        .zip(raw.par_chunks(CHUNK * bpp))
        .for_each(|(dst, src)| {
            convert_chunk(geom.bitpix, fast_u16, src, dst);

            if needs_scaling {
                for d in dst.iter_mut() {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        *d = (bzero + bscale * f64::from(*d)) as f32;
                    }
                }
            }

            // BLANK means "no data". NaN is how the rest of the pipeline spells
            // that, and every statistic already skips non-finite values.
            if let Some(b) = blank {
                for d in dst.iter_mut() {
                    if *d == b {
                        *d = f32::NAN;
                    }
                }
            }
        });
}

/// Converts one chunk. Split out so each `BITPIX` arm is its own tight loop.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn convert_chunk(bitpix: i64, fast_u16: bool, src: &[u8], dst: &mut [f32]) {
    // `as_chunks` yields fixed-size arrays rather than slices, so each
    // `from_be_bytes` takes the array whole: no indexing, no bounds checks, and
    // the compiler can see the width is constant. The remainder is always empty
    // here, because the reader sizes `src` as an exact multiple.
    match bitpix {
        8 => {
            for (d, s) in dst.iter_mut().zip(src.iter()) {
                *d = f32::from(*s);
            }
        }
        16 if fast_u16 => {
            let (chunks, _) = src.as_chunks::<2>();
            for (d, s) in dst.iter_mut().zip(chunks) {
                *d = (i32::from(i16::from_be_bytes(*s)) + 32_768) as f32;
            }
        }
        16 => {
            let (chunks, _) = src.as_chunks::<2>();
            for (d, s) in dst.iter_mut().zip(chunks) {
                *d = f32::from(i16::from_be_bytes(*s));
            }
        }
        32 => {
            let (chunks, _) = src.as_chunks::<4>();
            for (d, s) in dst.iter_mut().zip(chunks) {
                *d = i32::from_be_bytes(*s) as f32;
            }
        }
        64 => {
            let (chunks, _) = src.as_chunks::<8>();
            for (d, s) in dst.iter_mut().zip(chunks) {
                *d = i64::from_be_bytes(*s) as f32;
            }
        }
        -32 => {
            let (chunks, _) = src.as_chunks::<4>();
            for (d, s) in dst.iter_mut().zip(chunks) {
                *d = f32::from_be_bytes(*s);
            }
        }
        -64 => {
            let (chunks, _) = src.as_chunks::<8>();
            for (d, s) in dst.iter_mut().zip(chunks) {
                *d = f64::from_be_bytes(*s) as f32;
            }
        }
        // `Geometry::from_header` rejects every other value, so this is dead
        // code. Filling with NaN keeps it non-panicking in a worker thread.
        _ => dst.fill(f32::NAN),
    }
}

/// Smallest and largest finite samples.
///
/// Non-finite values are skipped. A single `NaN` reaching a naive `min`/`max`
/// would make the whole image render blank, so this is the only statistic the
/// rest of the crate uses.
///
/// Returns `(0.0, 1.0)` when nothing is finite or every sample is identical, so
/// that callers can always divide by `max - min` without checking.
#[must_use]
pub fn finite_min_max(data: &[f32]) -> (f32, f32) {
    let (lo, hi) = data
        .par_iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(
            || (f32::INFINITY, f32::NEG_INFINITY),
            |(lo, hi), v| (lo.min(v), hi.max(v)),
        )
        .reduce(
            || (f32::INFINITY, f32::NEG_INFINITY),
            |a, b| (a.0.min(b.0), a.1.max(b.1)),
        );

    if lo.is_finite() && hi.is_finite() && hi > lo {
        (lo, hi)
    } else {
        (0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(cards: &[(&str, &str)]) -> FitsHeader {
        FitsHeader {
            cards: cards
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    fn mono_2x2(bitpix: i64) -> FitsHeader {
        header(&[
            ("BITPIX", &bitpix.to_string()),
            ("NAXIS", "2"),
            ("NAXIS1", "2"),
            ("NAXIS2", "2"),
        ])
    }

    #[test]
    fn geometry_reads_a_mono_image() {
        let g = Geometry::from_header(&mono_2x2(16)).unwrap();
        assert_eq!((g.width, g.height, g.channels), (2, 2, 1));
        assert_eq!(g.pixel_count(), 4);
        assert_eq!(g.bytes_per_pixel(), 2);
        assert_eq!(g.data_len(), Some(8));
        assert_eq!((g.bzero, g.bscale), (0.0, 1.0));
    }

    #[test]
    fn geometry_reads_a_three_plane_cube() {
        let h = header(&[
            ("BITPIX", "16"),
            ("NAXIS", "3"),
            ("NAXIS1", "4"),
            ("NAXIS2", "3"),
            ("NAXIS3", "3"),
        ]);
        let g = Geometry::from_header(&h).unwrap();
        assert_eq!(g.channels, 3);
        assert_eq!(g.pixel_count(), 36);
    }

    #[test]
    fn bytes_per_pixel_covers_every_bitpix() {
        for (bitpix, bytes) in [(8, 1), (16, 2), (32, 4), (64, 8), (-32, 4), (-64, 8)] {
            let g = Geometry::from_header(&mono_2x2(bitpix)).unwrap();
            assert_eq!(g.bytes_per_pixel(), bytes, "bitpix {bitpix}");
        }
    }

    #[test]
    fn illegal_bitpix_is_rejected() {
        for bad in [0, 7, 24, -16, 128] {
            let err = Geometry::from_header(&mono_2x2(bad)).unwrap_err();
            assert!(
                matches!(err, FitsError::UnsupportedBitpix(b) if b == bad),
                "bitpix {bad} gave {err:?}"
            );
        }
    }

    #[test]
    fn missing_keywords_are_reported_as_bad_header() {
        let err = Geometry::from_header(&header(&[("NAXIS", "2")])).unwrap_err();
        assert!(matches!(err, FitsError::BadHeader(_)), "got {err:?}");

        let err = Geometry::from_header(&header(&[("BITPIX", "16")])).unwrap_err();
        assert!(matches!(err, FitsError::BadHeader(_)), "got {err:?}");

        // NAXIS says 2 but NAXIS2 is absent.
        let h = header(&[("BITPIX", "16"), ("NAXIS", "2"), ("NAXIS1", "10")]);
        let err = Geometry::from_header(&h).unwrap_err();
        assert!(matches!(err, FitsError::BadHeader(_)), "got {err:?}");
    }

    #[test]
    fn unsupported_shapes_are_rejected() {
        // A 1-D spectrum.
        let h = header(&[("BITPIX", "16"), ("NAXIS", "1"), ("NAXIS1", "10")]);
        assert!(matches!(
            Geometry::from_header(&h).unwrap_err(),
            FitsError::UnsupportedGeometry(_)
        ));

        // A 4-plane cube.
        let h = header(&[
            ("BITPIX", "16"),
            ("NAXIS", "3"),
            ("NAXIS1", "2"),
            ("NAXIS2", "2"),
            ("NAXIS3", "4"),
        ]);
        assert!(matches!(
            Geometry::from_header(&h).unwrap_err(),
            FitsError::UnsupportedGeometry(_)
        ));

        // A zero-length axis.
        let h = header(&[
            ("BITPIX", "16"),
            ("NAXIS", "2"),
            ("NAXIS1", "0"),
            ("NAXIS2", "5"),
        ]);
        assert!(matches!(
            Geometry::from_header(&h).unwrap_err(),
            FitsError::UnsupportedGeometry(_)
        ));
    }

    #[test]
    fn absurd_dimensions_overflow_instead_of_allocating() {
        let big = usize::MAX.to_string();
        let h = header(&[
            ("BITPIX", "16"),
            ("NAXIS", "2"),
            ("NAXIS1", &big),
            ("NAXIS2", &big),
        ]);
        let err = Geometry::from_header(&h).unwrap_err();
        // Either the value does not fit in i64 (BadHeader) or the product
        // overflows; both are refusals rather than an allocation attempt.
        assert!(
            matches!(
                err,
                FitsError::DimensionOverflow(_)
                    | FitsError::BadHeader(_)
                    | FitsError::UnsupportedGeometry(_)
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn a_zero_bscale_is_rejected() {
        let h = header(&[
            ("BITPIX", "16"),
            ("NAXIS", "2"),
            ("NAXIS1", "2"),
            ("NAXIS2", "2"),
            ("BSCALE", "0.0"),
        ]);
        assert!(matches!(
            Geometry::from_header(&h).unwrap_err(),
            FitsError::BadHeader(_)
        ));
    }

    /// Converts `raw` under `geom` and returns the samples.
    fn convert(geom: &Geometry, raw: &[u8]) -> Vec<f32> {
        let mut out = vec![0.0; geom.pixel_count()];
        convert_pixels(geom, raw, &mut out);
        out
    }

    #[test]
    fn converts_unsigned_eight_bit() {
        let g = Geometry::from_header(&mono_2x2(8)).unwrap();
        assert_eq!(convert(&g, &[0, 1, 128, 255]), vec![0.0, 1.0, 128.0, 255.0]);
    }

    #[test]
    fn converts_signed_sixteen_bit() {
        let g = Geometry::from_header(&mono_2x2(16)).unwrap();
        let raw: Vec<u8> = [-32768i16, -1, 0, 32767]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        assert_eq!(convert(&g, &raw), vec![-32768.0, -1.0, 0.0, 32767.0]);
    }

    #[test]
    fn the_unsigned_sixteen_bit_fast_path_matches_the_general_path() {
        // BZERO 32768 means the data is really u16. This is the common case,
        // and it has a dedicated branch, so it must agree with the slow path.
        let fast = header(&[
            ("BITPIX", "16"),
            ("NAXIS", "2"),
            ("NAXIS1", "2"),
            ("NAXIS2", "2"),
            ("BZERO", "32768.0"),
            ("BSCALE", "1.0"),
        ]);
        // Same scaling, expressed so the fast path does not trigger.
        let slow = header(&[
            ("BITPIX", "16"),
            ("NAXIS", "2"),
            ("NAXIS1", "2"),
            ("NAXIS2", "2"),
            ("BZERO", "32768.0"),
            ("BSCALE", "1.0000000001"),
        ]);
        let gf = Geometry::from_header(&fast).unwrap();
        let gs = Geometry::from_header(&slow).unwrap();

        let raw: Vec<u8> = [-32768i16, -1, 0, 32767]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();

        let got = convert(&gf, &raw);
        assert_eq!(got, vec![0.0, 32767.0, 32768.0, 65535.0]);

        // The general path agrees to within the tiny BSCALE difference.
        for (a, b) in got.iter().zip(convert(&gs, &raw).iter()) {
            assert!((a - b).abs() < 1.0, "{a} vs {b}");
        }
    }

    #[test]
    fn converts_thirty_two_bit_integers_and_floats() {
        let gi = Geometry::from_header(&mono_2x2(32)).unwrap();
        let raw: Vec<u8> = [-1i32, 0, 1, 1_000_000]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        assert_eq!(convert(&gi, &raw), vec![-1.0, 0.0, 1.0, 1_000_000.0]);

        let gf = Geometry::from_header(&mono_2x2(-32)).unwrap();
        let raw: Vec<u8> = [-1.5f32, 0.0, 0.25, 1e10]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        assert_eq!(convert(&gf, &raw), vec![-1.5, 0.0, 0.25, 1e10]);
    }

    #[test]
    fn converts_sixty_four_bit_integers_and_floats() {
        let gi = Geometry::from_header(&mono_2x2(64)).unwrap();
        let raw: Vec<u8> = [-1i64, 0, 1, 1_000_000]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        assert_eq!(convert(&gi, &raw), vec![-1.0, 0.0, 1.0, 1_000_000.0]);

        let gf = Geometry::from_header(&mono_2x2(-64)).unwrap();
        let raw: Vec<u8> = [-1.5f64, 0.0, 0.25, 2.0]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        assert_eq!(convert(&gf, &raw), vec![-1.5, 0.0, 0.25, 2.0]);
    }

    #[test]
    fn bzero_and_bscale_are_applied() {
        let h = header(&[
            ("BITPIX", "8"),
            ("NAXIS", "2"),
            ("NAXIS1", "2"),
            ("NAXIS2", "2"),
            ("BZERO", "10.0"),
            ("BSCALE", "2.0"),
        ]);
        let g = Geometry::from_header(&h).unwrap();
        // physical = 10 + 2 * raw
        assert_eq!(convert(&g, &[0, 1, 2, 3]), vec![10.0, 12.0, 14.0, 16.0]);
    }

    #[test]
    fn blank_pixels_become_nan() {
        let h = header(&[
            ("BITPIX", "16"),
            ("NAXIS", "2"),
            ("NAXIS1", "2"),
            ("NAXIS2", "2"),
            ("BLANK", "-32768"),
        ]);
        let g = Geometry::from_header(&h).unwrap();
        let raw: Vec<u8> = [-32768i16, 1, 2, 3]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let got = convert(&g, &raw);
        assert!(got[0].is_nan(), "expected NaN, got {}", got[0]);
        assert_eq!(&got[1..], &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn blank_is_ignored_for_floating_point_bitpix() {
        // BLANK is only defined for integer BITPIX. A float image that happens
        // to carry the keyword must not have pixels silently blanked.
        let h = header(&[
            ("BITPIX", "-32"),
            ("NAXIS", "2"),
            ("NAXIS1", "2"),
            ("NAXIS2", "2"),
            ("BLANK", "0"),
        ]);
        let g = Geometry::from_header(&h).unwrap();
        assert_eq!(g.blank_as_f32, None);
    }

    #[test]
    fn conversion_spans_multiple_rayon_chunks() {
        // Larger than CHUNK, so the parallel path really splits, and any
        // misalignment between the two zipped iterators would show up here.
        let n = CHUNK * 2 + 1234;
        let h = header(&[
            ("BITPIX", "16"),
            ("NAXIS", "2"),
            ("NAXIS1", &n.to_string()),
            ("NAXIS2", "1"),
            ("BZERO", "32768.0"),
        ]);
        let g = Geometry::from_header(&h).unwrap();
        let raw: Vec<u8> = (0..n)
            .flat_map(|i| ((i % 65536) as u16).wrapping_sub(32768).to_be_bytes())
            .collect();
        let got = convert(&g, &raw);
        assert_eq!(got.len(), n);
        for (i, v) in got.iter().enumerate() {
            assert_eq!(*v, (i % 65536) as f32, "sample {i}");
        }
    }

    #[test]
    fn finite_min_max_ignores_nan_and_infinities() {
        let data = [1.0, f32::NAN, 5.0, f32::INFINITY, -3.0, f32::NEG_INFINITY];
        assert_eq!(finite_min_max(&data), (-3.0, 5.0));
    }

    #[test]
    fn finite_min_max_falls_back_when_nothing_is_usable() {
        assert_eq!(finite_min_max(&[]), (0.0, 1.0));
        assert_eq!(finite_min_max(&[f32::NAN, f32::NAN]), (0.0, 1.0));
        // A constant image has no range to stretch; the fallback keeps callers
        // from dividing by zero.
        assert_eq!(finite_min_max(&[7.0, 7.0, 7.0]), (0.0, 1.0));
    }

    #[test]
    fn finite_min_max_works_across_chunks() {
        let mut data = vec![0.0f32; CHUNK * 3];
        data[CHUNK * 2 + 7] = -100.0;
        data[5] = 100.0;
        assert_eq!(finite_min_max(&data), (-100.0, 100.0));
    }

    #[test]
    fn sample_indexes_planes_correctly() {
        let img = FitsImage {
            width: 2,
            height: 2,
            channels: 3,
            data: vec![
                1.0, 2.0, 3.0, 4.0, // red plane
                5.0, 6.0, 7.0, 8.0, // green plane
                9.0, 10.0, 11.0, 12.0, // blue plane
            ],
            header: FitsHeader::default(),
            min: 1.0,
            max: 12.0,
        };
        assert_eq!(img.sample(0, 0, 0), Some(1.0));
        assert_eq!(img.sample(1, 1, 0), Some(4.0));
        assert_eq!(img.sample(0, 0, 1), Some(5.0));
        assert_eq!(img.sample(1, 1, 2), Some(12.0));
        assert_eq!(img.sample(2, 0, 0), None);
        assert_eq!(img.sample(0, 0, 3), None);
    }
}

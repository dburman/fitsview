//! Building valid FITS files in memory, for tests and benchmarks.
//!
//! No binary fixtures are committed to the repository. Every test that needs a
//! file builds one here, which keeps tests deterministic and lets them cover
//! shapes that would be tedious to capture from a real camera.
//!
//! Enabled by the `test-util` feature so it is available to the `fitsview`
//! crate's tests as well as this crate's own.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::FitsError;
use crate::{block_align, CARD_SIZE};

/// Describes a synthetic file to build.
#[derive(Debug, Clone)]
pub struct SyntheticSpec {
    /// `NAXIS1`.
    pub width: usize,
    /// `NAXIS2`.
    pub height: usize,
    /// 1 or 3.
    pub channels: usize,
    /// One of 8, 16, 32, 64, -32, -64.
    pub bitpix: i64,
    /// `BZERO`, written only when it is not the default.
    pub bzero: f64,
    /// `BSCALE`, written only when it is not the default.
    pub bscale: f64,
    /// Extra cards, as `(keyword, formatted value)` pairs. The value is placed
    /// verbatim after the `= ` indicator, so quote strings yourself.
    pub extra_cards: Vec<(String, String)>,
    /// Write an `XTENSION = 'IMAGE'` unit instead of a primary `SIMPLE` one.
    pub extension: bool,
}

impl SyntheticSpec {
    /// A mono image of the given size and `BITPIX`, no scaling.
    #[must_use]
    pub fn new(width: usize, height: usize, bitpix: i64) -> Self {
        Self {
            width,
            height,
            channels: 1,
            bitpix,
            bzero: 0.0,
            bscale: 1.0,
            extra_cards: Vec::new(),
            extension: false,
        }
    }

    /// Sets `BZERO` and `BSCALE`.
    #[must_use]
    pub fn with_scaling(mut self, bzero: f64, bscale: f64) -> Self {
        self.bzero = bzero;
        self.bscale = bscale;
        self
    }

    /// Sets the number of planes, 1 or 3.
    #[must_use]
    pub fn with_channels(mut self, channels: usize) -> Self {
        self.channels = channels;
        self
    }

    /// Adds a header card. The value is written verbatim.
    #[must_use]
    pub fn with_card(mut self, key: &str, value: &str) -> Self {
        self.extra_cards.push((key.to_string(), value.to_string()));
        self
    }

    /// Writes this as an image extension rather than a primary header unit.
    #[must_use]
    pub fn as_extension(mut self) -> Self {
        self.extension = true;
        self
    }

    /// Number of samples the pixel slice must contain.
    #[must_use]
    pub fn pixel_count(&self) -> usize {
        self.width * self.height * self.channels
    }
}

/// Formats one 80-byte header card.
///
/// The value is right-justified into the conventional 20-column field when it
/// fits, which is what real files look like and what makes them readable in a
/// hex dump.
#[must_use]
pub fn card(key: &str, value: &str) -> Vec<u8> {
    let text = if value.is_empty() {
        format!("{key:<8}")
    } else {
        format!("{key:<8}= {value:>20}")
    };
    let mut bytes = text.into_bytes();
    bytes.resize(CARD_SIZE, b' ');
    bytes.truncate(CARD_SIZE);
    bytes
}

/// Builds a complete, block-padded header from `(keyword, value)` pairs,
/// appending the `END` card.
#[must_use]
pub fn header_block(cards: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in cards {
        out.extend_from_slice(&card(k, v));
    }
    out.extend_from_slice(&card("END", ""));
    let padded = block_align(out.len()).unwrap_or(out.len());
    out.resize(padded, b' ');
    out
}

/// Builds a valid FITS file in memory.
///
/// `pixels` holds physical values; they are converted back to raw values using
/// the spec's `BZERO` and `BSCALE`, so that reading the result returns the
/// values passed in.
///
/// # Errors
///
/// Returns [`FitsError::BadHeader`] if `pixels` does not match the spec's
/// dimensions, and [`FitsError::UnsupportedBitpix`] for an illegal `BITPIX`.
pub fn synthetic_fits(spec: &SyntheticSpec, pixels: &[f64]) -> Result<Vec<u8>, FitsError> {
    if pixels.len() != spec.pixel_count() {
        return Err(FitsError::BadHeader(format!(
            "expected {} pixels for {}x{}x{}, got {}",
            spec.pixel_count(),
            spec.width,
            spec.height,
            spec.channels,
            pixels.len()
        )));
    }

    let mut cards: Vec<(String, String)> = Vec::new();
    if spec.extension {
        cards.push(("XTENSION".into(), "'IMAGE   '".into()));
    } else {
        cards.push(("SIMPLE".into(), "T".into()));
    }
    cards.push(("BITPIX".into(), spec.bitpix.to_string()));
    let naxis = if spec.channels > 1 { 3 } else { 2 };
    cards.push(("NAXIS".into(), naxis.to_string()));
    cards.push(("NAXIS1".into(), spec.width.to_string()));
    cards.push(("NAXIS2".into(), spec.height.to_string()));
    if naxis == 3 {
        cards.push(("NAXIS3".into(), spec.channels.to_string()));
    }
    if spec.bzero != 0.0 {
        cards.push(("BZERO".into(), format_f64(spec.bzero)));
    }
    if spec.bscale != 1.0 {
        cards.push(("BSCALE".into(), format_f64(spec.bscale)));
    }
    cards.extend(spec.extra_cards.iter().cloned());

    let refs: Vec<(&str, &str)> = cards
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut out = header_block(&refs);

    encode_pixels(spec, pixels, &mut out)?;

    // The data section is padded to a whole number of blocks too.
    let padded = block_align(out.len()).unwrap_or(out.len());
    out.resize(padded, 0);
    Ok(out)
}

/// Writes physical values back to raw big-endian samples.
fn encode_pixels(spec: &SyntheticSpec, pixels: &[f64], out: &mut Vec<u8>) -> Result<(), FitsError> {
    // Undo the scaling the reader will apply: raw = (physical - bzero) / bscale.
    let raw_of = |p: f64| (p - spec.bzero) / spec.bscale;

    for &p in pixels {
        match spec.bitpix {
            8 => out.push(clamp_int(raw_of(p), 0.0, 255.0) as u8),
            16 => out.extend_from_slice(
                &(clamp_int(raw_of(p), f64::from(i16::MIN), f64::from(i16::MAX)) as i16)
                    .to_be_bytes(),
            ),
            32 => out.extend_from_slice(
                &(clamp_int(raw_of(p), f64::from(i32::MIN), f64::from(i32::MAX)) as i32)
                    .to_be_bytes(),
            ),
            64 => out.extend_from_slice(&(raw_of(p) as i64).to_be_bytes()),
            -32 => out.extend_from_slice(&(raw_of(p) as f32).to_be_bytes()),
            -64 => out.extend_from_slice(&raw_of(p).to_be_bytes()),
            other => return Err(FitsError::UnsupportedBitpix(other)),
        }
    }
    Ok(())
}

/// Rounds and clamps into an integer range, mapping NaN to zero.
///
/// Test data is allowed to be out of range; silently wrapping would make a
/// failing test hard to read, so clamp instead.
fn clamp_int(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.round().clamp(lo, hi)
    }
}

/// Formats a float the way FITS headers do, always with a decimal point so it
/// is unambiguously a float.
fn format_f64(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// Builds a synthetic file and writes it into `dir`, returning its path.
///
/// # Errors
///
/// Returns [`FitsError::Io`] if the file cannot be written, or the errors of
/// [`synthetic_fits`].
pub fn write_synthetic(
    dir: &Path,
    name: &str,
    spec: &SyntheticSpec,
    pixels: &[f64],
) -> Result<PathBuf, FitsError> {
    let bytes = synthetic_fits(spec, pixels)?;
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).map_err(|e| FitsError::io(&path, e))?;
    f.write_all(&bytes).map_err(|e| FitsError::io(&path, e))?;
    Ok(path)
}

/// Converts expected pixel values to `f32` for comparison against decoded data.
///
/// Test data is written as `f64` because that is what the encoder takes, but
/// images decode to `f32`, and the two do not compare directly.
#[must_use]
pub fn as_f32(pixels: &[f64]) -> Vec<f32> {
    #[allow(clippy::cast_possible_truncation)]
    pixels.iter().map(|&v| v as f32).collect()
}

/// A deterministic pseudo-random generator.
///
/// This is `xoshiro`-style rather than anything cryptographic. It exists so
/// tests can produce noisy images without depending on the `rand` crate, and so
/// that a failing test reproduces exactly from its seed.
#[derive(Debug, Clone)]
pub struct Prng {
    state: u64,
}

impl Prng {
    /// Creates a generator. Any seed is acceptable; zero is remapped.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// The next raw 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        // splitmix64: short, well distributed, and easy to verify against the
        // published reference values.
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform value in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        // 53 bits of mantissa is all an f64 can represent exactly.
        #[allow(clippy::cast_precision_loss)]
        {
            (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// A normally distributed value, via the Box-Muller transform.
    pub fn next_gaussian(&mut self) -> f64 {
        // Guard against log(0), which would give an infinity.
        let u1 = self.next_f64().max(f64::MIN_POSITIVE);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Generates a noisy flat background, the shape most astronomical frames have
/// before any stars are added.
///
/// Deterministic for a given seed.
#[must_use]
pub fn gaussian_background(
    width: usize,
    height: usize,
    mean: f64,
    sigma: f64,
    seed: u64,
) -> Vec<f64> {
    let mut rng = Prng::new(seed);
    (0..width * height)
        .map(|_| mean + sigma * rng.next_gaussian())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::read_fits_from_bytes;
    use crate::BLOCK_SIZE;

    #[test]
    fn cards_are_exactly_eighty_bytes() {
        assert_eq!(card("SIMPLE", "T").len(), CARD_SIZE);
        assert_eq!(card("END", "").len(), CARD_SIZE);
        let long = card("VERYLONG", &"x".repeat(200));
        assert_eq!(long.len(), CARD_SIZE);
    }

    #[test]
    fn header_blocks_are_block_aligned_and_end_terminated() {
        let h = header_block(&[("SIMPLE", "T")]);
        assert_eq!(h.len(), BLOCK_SIZE);
        assert!(h.windows(3).any(|w| w == b"END"));
    }

    #[test]
    fn synthetic_files_are_block_aligned() {
        let spec = SyntheticSpec::new(10, 10, 16);
        let bytes = synthetic_fits(&spec, &vec![1.0; 100]).unwrap();
        assert_eq!(bytes.len() % BLOCK_SIZE, 0);
        assert_eq!(bytes.len(), 2 * BLOCK_SIZE);
    }

    #[test]
    fn wrong_pixel_count_is_rejected() {
        let spec = SyntheticSpec::new(4, 4, 16);
        let err = synthetic_fits(&spec, &[1.0, 2.0]).unwrap_err();
        assert!(matches!(err, FitsError::BadHeader(_)), "got {err:?}");
    }

    #[test]
    fn illegal_bitpix_is_rejected_when_encoding() {
        let mut spec = SyntheticSpec::new(1, 1, 16);
        spec.bitpix = 24;
        let err = synthetic_fits(&spec, &[1.0]).unwrap_err();
        assert!(
            matches!(err, FitsError::UnsupportedBitpix(24)),
            "got {err:?}"
        );
    }

    #[test]
    fn values_survive_a_write_then_read_for_every_bitpix() {
        for bitpix in [8, 16, 32, 64, -32, -64] {
            let spec = SyntheticSpec::new(2, 2, bitpix);
            let pixels = [0.0, 1.0, 2.0, 3.0];
            let bytes = synthetic_fits(&spec, &pixels).unwrap();
            let img = read_fits_from_bytes(&bytes).unwrap();
            assert_eq!(img.data, as_f32(&pixels), "bitpix {bitpix}");
        }
    }

    #[test]
    fn scaling_is_undone_so_reads_return_what_was_written() {
        let spec = SyntheticSpec::new(2, 2, 16).with_scaling(32768.0, 1.0);
        let pixels = [0.0, 100.0, 32768.0, 65535.0];
        let bytes = synthetic_fits(&spec, &pixels).unwrap();
        let img = read_fits_from_bytes(&bytes).unwrap();
        assert_eq!(img.data, as_f32(&pixels));
    }

    #[test]
    fn the_generator_is_deterministic() {
        let a = gaussian_background(8, 8, 100.0, 5.0, 42);
        let b = gaussian_background(8, 8, 100.0, 5.0, 42);
        assert_eq!(a, b);
        let c = gaussian_background(8, 8, 100.0, 5.0, 43);
        assert_ne!(a, c, "different seeds must give different noise");
    }

    #[test]
    fn the_generated_background_has_roughly_the_requested_statistics() {
        let n = 20_000;
        let data = gaussian_background(n, 1, 1000.0, 10.0, 7);
        let mean = data.iter().sum::<f64>() / n as f64;
        let var = data.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
        // Generous bounds: this checks the generator is not broken, not that it
        // passes a statistical test.
        assert!((mean - 1000.0).abs() < 1.0, "mean was {mean}");
        assert!((var.sqrt() - 10.0).abs() < 1.0, "sigma was {}", var.sqrt());
    }

    #[test]
    fn the_generator_produces_finite_values_only() {
        let data = gaussian_background(2000, 1, 0.0, 1.0, 99);
        assert!(data.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn uniform_values_stay_in_range() {
        let mut rng = Prng::new(1);
        for _ in 0..10_000 {
            let v = rng.next_f64();
            assert!((0.0..1.0).contains(&v), "{v} out of range");
        }
    }

    #[test]
    fn a_zero_seed_still_produces_varied_output() {
        let mut rng = Prng::new(0);
        let first = rng.next_u64();
        let second = rng.next_u64();
        assert_ne!(first, second);
        assert_ne!(first, 0);
    }
}

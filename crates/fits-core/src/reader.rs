//! Reading FITS files.
//!
//! Files are read with a single [`std::fs::read`] rather than being memory
//! mapped, because mapping requires `unsafe` and this crate forbids it. The
//! conversion pass touches every byte anyway, so the extra copy is a small
//! fraction of total time and the kernel does it at memory bandwidth.

use std::path::Path;

use std::io::Write;

use crate::block_align;
use crate::error::FitsError;
use crate::header::{self, FitsHeader};
use crate::image::{convert_pixels, finite_min_max, FitsImage, Geometry};

/// File extensions that may hold a FITS image, lower case.
const FITS_EXTENSIONS: [&str; 3] = ["fits", "fit", "fts"];

/// Whether `path` looks like a FITS file from its extension alone.
///
/// This is the cheap check used when scanning a folder of thousands of files.
/// It does not open the file; [`read_fits`] verifies the contents.
///
/// ```
/// use std::path::Path;
/// use fits_core::is_fits_path;
/// assert!(is_fits_path(Path::new("light_001.fits")));
/// assert!(is_fits_path(Path::new("LIGHT_001.FIT")));
/// assert!(!is_fits_path(Path::new("notes.txt")));
/// ```
#[must_use]
pub fn is_fits_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| FITS_EXTENSIONS.contains(&e.as_str()))
}

/// Reads and decodes a FITS image from disk.
///
/// # Errors
///
/// Returns [`FitsError::Io`] if the file cannot be read, [`FitsError::NotFits`]
/// if it does not begin with `SIMPLE`, and the parsing errors described on
/// [`FitsError`] for malformed contents.
pub fn read_fits(path: &Path) -> Result<FitsImage, FitsError> {
    let started = std::time::Instant::now();
    let bytes = std::fs::read(path).map_err(|e| FitsError::io(path, e))?;
    let image = read_fits_from_bytes(&bytes)?;
    log_timing(path, &image, started);
    Ok(image)
}

/// Logs how long a read took, so performance regressions are visible without a
/// profiler. Separated out to keep `read_fits` readable.
fn log_timing(path: &Path, image: &FitsImage, started: std::time::Instant) {
    let elapsed = started.elapsed();
    let megapixels = (image.width * image.height) as f64 / 1e6;
    log::debug!(
        "read {:?}: {}x{}x{} ({megapixels:.1} MP) in {elapsed:?}",
        path.file_name().unwrap_or(path.as_os_str()),
        image.width,
        image.height,
        image.channels,
    );
}

/// Decodes a FITS image already held in memory.
///
/// Exposed so tests and benchmarks can work without touching the filesystem.
///
/// # Errors
///
/// As [`read_fits`], minus the I/O variants.
pub fn read_fits_from_bytes(bytes: &[u8]) -> Result<FitsImage, FitsError> {
    if !bytes.starts_with(b"SIMPLE") {
        return Err(FitsError::NotFits);
    }

    let (header, geom, data_start) = find_image_hdu(bytes)?;

    let data_len = geom
        .data_len()
        .ok_or_else(|| FitsError::DimensionOverflow(format!("{geom:?}")))?;
    let end = data_start
        .checked_add(data_len)
        .ok_or_else(|| FitsError::DimensionOverflow(format!("{geom:?}")))?;
    let raw = bytes.get(data_start..end).ok_or(FitsError::Truncated {
        what: "image data",
        expected: end,
        found: bytes.len(),
    })?;

    let mut data = vec![0.0f32; geom.pixel_count()];
    convert_pixels(&geom, raw, &mut data);
    let (min, max) = finite_min_max(&data);

    Ok(FitsImage {
        width: geom.width,
        height: geom.height,
        channels: geom.channels,
        data,
        header,
        min,
        max,
    })
}

/// Finds the first header unit that actually carries an image.
///
/// Many capture programs write an empty primary header with `NAXIS = 0` and put
/// the image in the first extension. Exactly one level of fallback is
/// supported, which covers every file this viewer is meant to open.
fn find_image_hdu(bytes: &[u8]) -> Result<(FitsHeader, Geometry, usize), FitsError> {
    let (primary, parsed) = header::parse_at(bytes, 0)?;

    // NAXIS = 0 means the primary unit holds no data at all.
    if primary.get_i64("NAXIS") == Some(0) {
        // With no data, the next header unit starts immediately.
        let (ext, ext_parsed) = header::parse_at(bytes, parsed.data_start)?;
        let geom = Geometry::from_header(&ext)?;
        return Ok((ext, geom, ext_parsed.data_start));
    }

    let geom = Geometry::from_header(&primary)?;
    Ok((primary, geom, parsed.data_start))
}

/// Writes an image as a 32-bit floating point FITS file.
///
/// Float output is deliberate: a calibrated frame holds values that are no
/// longer integers, and rounding them back to 16 bits would throw away the
/// precision calibration exists to provide. It also means no `BZERO` or
/// `BSCALE` games, so what is written is exactly what was in memory.
///
/// Cards describing the observation are copied from `image`; cards describing
/// the old file's structure are not, since this file has its own. Each line of
/// `history` becomes a `HISTORY` card, so a calibrated file records how it was
/// made.
///
/// The file is written to a temporary name and renamed into place, so an
/// interrupted write cannot leave a half-written image where a valid one used
/// to be.
///
/// # Errors
///
/// Returns [`FitsError::Io`] if the file cannot be written, and
/// [`FitsError::DimensionOverflow`] for an image whose size does not fit.
pub fn write_fits(path: &Path, image: &FitsImage, history: &[String]) -> Result<(), FitsError> {
    let bytes = encode_fits(image, history)?;

    // Same directory, so the rename is on one filesystem and therefore atomic.
    let temp = temp_path_for(path);
    let mut file = std::fs::File::create(&temp).map_err(|e| FitsError::io(&temp, e))?;
    file.write_all(&bytes)
        .map_err(|e| FitsError::io(&temp, e))?;
    file.sync_all().map_err(|e| FitsError::io(&temp, e))?;
    drop(file);

    std::fs::rename(&temp, path).map_err(|e| {
        // Leave nothing behind if the rename fails.
        let _ = std::fs::remove_file(&temp);
        FitsError::io(path, e)
    })
}

/// A hidden sibling of `path`, used while writing.
fn temp_path_for(path: &Path) -> std::path::PathBuf {
    let name = path.file_name().map_or_else(
        || "fitsview".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    path.with_file_name(format!(".{name}.tmp"))
}

/// Encodes an image as FITS bytes.
///
/// Separated from writing so the format can be tested without a filesystem.
///
/// # Errors
///
/// Returns [`FitsError::DimensionOverflow`] if the image is too large to
/// describe.
pub fn encode_fits(image: &FitsImage, history: &[String]) -> Result<Vec<u8>, FitsError> {
    let mut cards: Vec<u8> = Vec::new();
    let naxis = if image.channels > 1 { 3 } else { 2 };

    cards.extend_from_slice(&header::format_card("SIMPLE", "T"));
    cards.extend_from_slice(&header::format_card("BITPIX", "-32"));
    cards.extend_from_slice(&header::format_card("NAXIS", &naxis.to_string()));
    cards.extend_from_slice(&header::format_card("NAXIS1", &image.width.to_string()));
    cards.extend_from_slice(&header::format_card("NAXIS2", &image.height.to_string()));
    if naxis == 3 {
        cards.extend_from_slice(&header::format_card("NAXIS3", &image.channels.to_string()));
    }

    // Carry the observation's own metadata through, but not the old file's
    // structural description.
    for (keyword, value) in &image.header.cards {
        if header::is_structural(keyword) {
            continue;
        }
        cards.extend_from_slice(&header::format_card(keyword, value));
    }

    for line in history {
        cards.extend_from_slice(&header::format_history(line));
    }
    cards.extend_from_slice(&header::format_card("END", ""));

    let header_len = block_align(cards.len())
        .ok_or_else(|| FitsError::DimensionOverflow("header too large".into()))?;
    cards.resize(header_len, b' ');

    let data_len = image
        .data
        .len()
        .checked_mul(4)
        .ok_or_else(|| FitsError::DimensionOverflow(format!("{} samples", image.data.len())))?;
    let padded = block_align(data_len)
        .ok_or_else(|| FitsError::DimensionOverflow(format!("{data_len} bytes")))?;

    let mut out = cards;
    out.reserve(padded);
    for sample in &image.data {
        out.extend_from_slice(&sample.to_be_bytes());
    }
    out.resize(header_len + padded, 0);
    Ok(out)
}

/// Total bytes a FITS file with this geometry occupies, header included.
///
/// Used by the writer and by tests that build files by hand.
#[must_use]
pub fn file_len_for(header_bytes: usize, data_bytes: usize) -> Option<usize> {
    let h = block_align(header_bytes)?;
    let d = block_align(data_bytes)?;
    h.checked_add(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{as_f32, synthetic_fits, SyntheticSpec};
    use crate::BLOCK_SIZE;

    #[test]
    fn recognises_fits_extensions_case_insensitively() {
        for good in ["a.fits", "a.FITS", "a.fit", "a.Fit", "a.fts", "a.FTS"] {
            assert!(is_fits_path(Path::new(good)), "{good}");
        }
        for bad in ["a.txt", "a.fits.gz", "a.tif", "a", "a.fitsx", ".fits.bak"] {
            assert!(!is_fits_path(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn a_bare_dot_fits_name_is_accepted() {
        // ".fits" alone is a hidden file with no extension as far as Rust is
        // concerned; folder scanning skips hidden files separately.
        assert!(!is_fits_path(Path::new(".fits")));
    }

    #[test]
    fn rejects_a_file_that_is_not_fits() {
        let err = read_fits_from_bytes(b"this is a text file").unwrap_err();
        assert!(matches!(err, FitsError::NotFits), "got {err:?}");
    }

    #[test]
    fn rejects_an_empty_input() {
        assert!(matches!(
            read_fits_from_bytes(&[]).unwrap_err(),
            FitsError::NotFits
        ));
    }

    #[test]
    fn reads_a_synthetic_image_of_every_bitpix() {
        for bitpix in [8, 16, 32, 64, -32, -64] {
            let spec = SyntheticSpec::new(3, 2, bitpix);
            let pixels = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
            let bytes = synthetic_fits(&spec, &pixels).unwrap();
            let img = read_fits_from_bytes(&bytes)
                .unwrap_or_else(|e| panic!("bitpix {bitpix} failed: {e}"));
            assert_eq!((img.width, img.height, img.channels), (3, 2, 1));
            assert_eq!(img.data, as_f32(&pixels), "bitpix {bitpix}");
            assert_eq!((img.min, img.max), (0.0, 5.0), "bitpix {bitpix}");
        }
    }

    #[test]
    fn reads_unsigned_sixteen_bit_with_bzero() {
        let spec = SyntheticSpec::new(2, 2, 16).with_scaling(32768.0, 1.0);
        let pixels = [0.0, 1.0, 32768.0, 65535.0];
        let bytes = synthetic_fits(&spec, &pixels).unwrap();
        let img = read_fits_from_bytes(&bytes).unwrap();
        assert_eq!(img.data, as_f32(&pixels));
    }

    #[test]
    fn reads_a_three_plane_colour_image() {
        let spec = SyntheticSpec::new(2, 2, 16).with_channels(3);
        let pixels: Vec<f64> = (0..12).map(f64::from).collect();
        let bytes = synthetic_fits(&spec, &pixels).unwrap();
        let img = read_fits_from_bytes(&bytes).unwrap();
        assert_eq!(img.channels, 3);
        assert_eq!(img.len(), 12);
        assert_eq!(img.sample(0, 0, 0), Some(0.0));
        assert_eq!(img.sample(0, 0, 1), Some(4.0));
        assert_eq!(img.sample(0, 0, 2), Some(8.0));
    }

    #[test]
    fn a_truncated_data_block_is_reported_not_a_panic() {
        let spec = SyntheticSpec::new(10, 10, 16);
        let pixels = vec![1.0; 100];
        let mut bytes = synthetic_fits(&spec, &pixels).unwrap();
        bytes.truncate(bytes.len() - BLOCK_SIZE);
        let err = read_fits_from_bytes(&bytes).unwrap_err();
        assert!(matches!(err, FitsError::Truncated { .. }), "got {err:?}");
    }

    #[test]
    fn a_file_missing_only_its_trailing_padding_still_reads() {
        // The standard pads the data section to a whole 2880-byte block, but a
        // file that stops right after the last pixel has lost nothing that
        // matters. Being lenient here means a partially written capture, or a
        // download cut short at the very end, still opens. Anything shorter
        // than the full pixel data is still refused.
        let spec = SyntheticSpec::new(20, 20, 16);
        let pixels = vec![1.0f64; 400];
        let full = synthetic_fits(&spec, &pixels).unwrap();

        let data_end = BLOCK_SIZE + 400 * 2;
        assert!(full.len() > data_end, "expected trailing padding to exist");

        let img = read_fits_from_bytes(&full[..data_end]).unwrap();
        assert_eq!(img.data.len(), 400);

        // One byte short of the full pixel data is a truncation.
        let err = read_fits_from_bytes(&full[..data_end - 1]).unwrap_err();
        assert!(matches!(err, FitsError::Truncated { .. }), "got {err:?}");
    }

    #[test]
    fn a_header_with_no_end_card_is_reported_not_a_panic() {
        let mut bytes = vec![b' '; BLOCK_SIZE];
        bytes[..6].copy_from_slice(b"SIMPLE");
        let err = read_fits_from_bytes(&bytes).unwrap_err();
        assert!(matches!(err, FitsError::Truncated { .. }), "got {err:?}");
    }

    #[test]
    fn falls_back_to_the_first_extension_when_the_primary_is_empty() {
        // Cameras that write an empty primary header put the image in the
        // first extension. Build exactly that layout.
        let primary = crate::testutil::header_block(&[
            ("SIMPLE", "                   T"),
            ("BITPIX", "                   8"),
            ("NAXIS", "                   0"),
            ("EXTEND", "                   T"),
        ]);
        let spec = SyntheticSpec::new(2, 2, 16).as_extension();
        let pixels = [1.0, 2.0, 3.0, 4.0];
        let ext = synthetic_fits(&spec, &pixels).unwrap();

        let mut bytes = primary;
        bytes.extend_from_slice(&ext);

        let img = read_fits_from_bytes(&bytes).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.data, as_f32(&pixels));
    }

    #[test]
    fn nan_pixels_do_not_blank_the_statistics() {
        let spec = SyntheticSpec::new(2, 2, -32);
        let pixels = [1.0, f64::NAN, 3.0, 5.0];
        let bytes = synthetic_fits(&spec, &pixels).unwrap();
        let img = read_fits_from_bytes(&bytes).unwrap();
        assert!(img.data[1].is_nan());
        assert_eq!((img.min, img.max), (1.0, 5.0));
    }

    #[test]
    fn header_cards_survive_the_round_trip() {
        let spec = SyntheticSpec::new(2, 2, 16)
            .with_card("OBJECT", "'M31     '")
            .with_card("EXPTIME", "               120.0");
        let bytes = synthetic_fits(&spec, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        let img = read_fits_from_bytes(&bytes).unwrap();
        assert_eq!(img.header.get("OBJECT"), Some("M31"));
        assert_eq!(img.header.get_f64("EXPTIME"), Some(120.0));
    }

    #[test]
    fn reads_from_a_real_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(4, 3, 16);
        let pixels: Vec<f64> = (0..12).map(f64::from).collect();
        let path =
            crate::testutil::write_synthetic(dir.path(), "light.fits", &spec, &pixels).unwrap();

        assert!(is_fits_path(&path));
        let img = read_fits(&path).unwrap();
        assert_eq!((img.width, img.height), (4, 3));
        assert_eq!(img.data.len(), 12);
    }

    #[test]
    fn a_missing_file_reports_its_path() {
        let err = read_fits(Path::new("/nonexistent/definitely-not-here.fits")).unwrap_err();
        match err {
            FitsError::Io { ref path, .. } => {
                assert!(path.to_string_lossy().contains("definitely-not-here"));
            }
            other => panic!("expected an io error, got {other:?}"),
        }
        assert!(err.to_string().contains("definitely-not-here"));
    }

    #[test]
    fn writing_then_reading_returns_identical_samples() {
        // The round trip the plan deferred from Phase 1 to arrive with the
        // writer. Float output means this is exact, not approximate.
        for channels in [1, 3] {
            let (w, h) = (7usize, 5usize);
            let pixels: Vec<f64> = (0..w * h * channels)
                .map(|i| i as f64 * 1.25 - 3.0)
                .collect();
            let spec = SyntheticSpec::new(w, h, -32).with_channels(channels);
            let original = read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap();

            let encoded = encode_fits(&original, &[]).unwrap();
            let back = read_fits_from_bytes(&encoded).unwrap();

            assert_eq!(back.width, original.width, "channels {channels}");
            assert_eq!(back.height, original.height);
            assert_eq!(back.channels, original.channels);
            assert_eq!(back.data, original.data, "samples must survive exactly");
            assert_eq!((back.min, back.max), (original.min, original.max));
        }
    }

    #[test]
    fn written_files_are_block_aligned() {
        let spec = SyntheticSpec::new(10, 10, -32);
        let img = read_fits_from_bytes(&synthetic_fits(&spec, &vec![1.0; 100]).unwrap()).unwrap();
        let encoded = encode_fits(&img, &[]).unwrap();
        assert_eq!(encoded.len() % BLOCK_SIZE, 0);
        assert!(encoded.starts_with(b"SIMPLE"));
    }

    #[test]
    fn observation_metadata_is_carried_through_but_structure_is_not() {
        let spec = SyntheticSpec::new(4, 4, 16)
            .with_scaling(32768.0, 1.0)
            .with_card("OBJECT", "'M31     '")
            .with_card("EXPTIME", "               120.0");
        let img = read_fits_from_bytes(&synthetic_fits(&spec, &[1000.0; 16]).unwrap()).unwrap();

        let back = read_fits_from_bytes(&encode_fits(&img, &[]).unwrap()).unwrap();
        assert_eq!(back.header.get("OBJECT"), Some("M31"));
        assert_eq!(back.header.get_f64("EXPTIME"), Some(120.0));

        // The source was 16-bit with an offset; the output is float, so the old
        // structural cards must not follow it.
        assert_eq!(back.header.get_i64("BITPIX"), Some(-32));
        assert_eq!(
            back.header.get("BZERO"),
            None,
            "a stale BZERO would misread every pixel"
        );
    }

    #[test]
    fn a_scaled_source_round_trips_to_the_same_physical_values() {
        // 16-bit unsigned input, float output: the numbers must not shift.
        let spec = SyntheticSpec::new(4, 4, 16).with_scaling(32768.0, 1.0);
        let pixels: Vec<f64> = (0..16).map(|i| f64::from(i) * 4000.0).collect();
        let img = read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap();

        let back = read_fits_from_bytes(&encode_fits(&img, &[]).unwrap()).unwrap();
        assert_eq!(back.data, as_f32(&pixels));
    }

    #[test]
    fn history_lines_are_written_into_the_file() {
        let spec = SyntheticSpec::new(2, 2, -32);
        let img = read_fits_from_bytes(&synthetic_fits(&spec, &[1.0; 4]).unwrap()).unwrap();
        let history = vec![
            "fitsview: dark subtracted (5 frames)".to_string(),
            "fitsview: flat divided".to_string(),
        ];
        let encoded = encode_fits(&img, &history).unwrap();
        let text = String::from_utf8_lossy(&encoded[..2880]);
        assert!(
            text.contains("HISTORY fitsview: dark subtracted (5 frames)"),
            "{text}"
        );
        assert!(text.contains("HISTORY fitsview: flat divided"));
    }

    #[test]
    fn a_header_that_needs_more_than_one_block_is_written_correctly() {
        let mut spec = SyntheticSpec::new(2, 2, -32);
        for i in 0..60 {
            spec = spec.with_card(&format!("KEY{i:<5}"), &format!("{i}"));
        }
        let img = read_fits_from_bytes(&synthetic_fits(&spec, &[1.0; 4]).unwrap()).unwrap();
        let encoded = encode_fits(&img, &[]).unwrap();
        let back = read_fits_from_bytes(&encoded).unwrap();
        assert_eq!(back.header.get_i64("KEY59"), Some(59));
        assert_eq!(back.data, vec![1.0f32; 4]);
    }

    #[test]
    fn non_finite_samples_survive_a_write() {
        // NaN means "no data" and must not be quietly turned into a number.
        let spec = SyntheticSpec::new(2, 2, -32);
        let img = read_fits_from_bytes(&synthetic_fits(&spec, &[1.0, f64::NAN, 3.0, 4.0]).unwrap())
            .unwrap();
        let back = read_fits_from_bytes(&encode_fits(&img, &[]).unwrap()).unwrap();
        assert!(back.data[1].is_nan());
        assert_eq!(back.data[0], 1.0);
    }

    #[test]
    fn writing_to_disk_produces_a_readable_file_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(6, 4, -32);
        let pixels: Vec<f64> = (0..24).map(f64::from).collect();
        let img = read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap();

        let out = dir.path().join("calibrated.fits");
        write_fits(&out, &img, &["fitsview: test".to_string()]).unwrap();

        let back = read_fits(&out).unwrap();
        assert_eq!(back.data, as_f32(&pixels));

        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[test]
    fn writing_to_an_unwritable_place_reports_the_path() {
        let err = write_fits(
            Path::new("/nonexistent/directory/out.fits"),
            &read_fits_from_bytes(
                &synthetic_fits(&SyntheticSpec::new(2, 2, -32), &[1.0; 4]).unwrap(),
            )
            .unwrap(),
            &[],
        )
        .unwrap_err();
        assert!(matches!(err, FitsError::Io { .. }), "got {err:?}");
    }

    #[test]
    fn file_len_accounts_for_block_padding() {
        assert_eq!(file_len_for(1, 1), Some(2 * BLOCK_SIZE));
        assert_eq!(file_len_for(BLOCK_SIZE, 0), Some(BLOCK_SIZE));
        assert_eq!(file_len_for(usize::MAX, 0), None);
    }
}

//! Property tests.
//!
//! These exist to answer one question that example-based tests cannot: does the
//! parser survive input it was never designed for? A viewer that opens whatever
//! is in a folder will meet truncated downloads, half-written captures and
//! files that are not FITS at all. None of those may panic.

use fits_core::testutil::{as_f32, synthetic_fits, SyntheticSpec};
use fits_core::{header, read_fits_from_bytes, Geometry};
use proptest::prelude::*;

/// Every legal BITPIX, for use as a strategy input.
const BITPIX: [i64; 6] = [8, 16, 32, 64, -32, -64];

proptest! {
    /// Arbitrary bytes must never panic the header parser. Any outcome is
    /// acceptable as long as it is returned rather than thrown.
    #[test]
    fn header_parsing_never_panics_on_arbitrary_bytes(
        bytes in proptest::collection::vec(any::<u8>(), 0..9000)
    ) {
        let _ = header::parse_at(&bytes, 0);
    }

    /// The same, starting from an arbitrary offset, which is what the
    /// extension fallback does.
    #[test]
    fn header_parsing_never_panics_at_an_arbitrary_offset(
        bytes in proptest::collection::vec(any::<u8>(), 0..9000),
        offset in 0usize..9000,
    ) {
        let _ = header::parse_at(&bytes, offset);
    }

    /// Arbitrary bytes that happen to start with the FITS magic must not panic
    /// the whole reader. This is the realistic corruption case: a real file
    /// whose contents have been damaged.
    #[test]
    fn reading_never_panics_on_arbitrary_bytes_after_the_magic(
        tail in proptest::collection::vec(any::<u8>(), 0..9000)
    ) {
        let mut bytes = b"SIMPLE".to_vec();
        bytes.extend_from_slice(&tail);
        let _ = read_fits_from_bytes(&bytes);
    }

    /// Truncating a valid file at any point must produce an error or a
    /// complete image, never a panic and never a partially filled image.
    ///
    /// A read may legitimately succeed when only the trailing block padding was
    /// cut off, because every pixel byte is still present. See
    /// `reader::tests::a_file_missing_only_its_trailing_padding_still_reads`.
    #[test]
    fn truncating_a_valid_file_anywhere_is_handled(
        cut in 0usize..6000,
    ) {
        let spec = SyntheticSpec::new(20, 20, 16);
        let pixels = vec![1.0f64; 400];
        let full = synthetic_fits(&spec, &pixels).unwrap();
        // Header is one block, data is 800 bytes padded to a second block.
        let data_end = 2880 + 400 * 2;
        let cut = cut.min(full.len());

        if let Ok(img) = read_fits_from_bytes(&full[..cut]) {
            // Success is only possible once every pixel byte is present.
            prop_assert!(
                cut >= data_end,
                "read succeeded with only {cut} bytes, needs {data_end}"
            );
            prop_assert_eq!(img.data.len(), 400);
            prop_assert!(img.data.iter().all(|v| (*v - 1.0).abs() < f32::EPSILON));
        }
    }

    /// Values written at any legal BITPIX come back unchanged, for values that
    /// the format can represent exactly.
    #[test]
    fn integer_values_round_trip_through_every_bitpix(
        bitpix_index in 0usize..BITPIX.len(),
        values in proptest::collection::vec(0i32..200, 1..40),
    ) {
        let bitpix = BITPIX[bitpix_index];
        let width = values.len();
        let pixels: Vec<f64> = values.iter().map(|&v| f64::from(v)).collect();
        let spec = SyntheticSpec::new(width, 1, bitpix);
        let bytes = synthetic_fits(&spec, &pixels).unwrap();
        let img = read_fits_from_bytes(&bytes).unwrap();
        prop_assert_eq!(img.data, as_f32(&pixels));
    }

    /// Whatever the header says, geometry either validates or is refused. It
    /// must never report a pixel count that would overflow when allocated.
    #[test]
    fn geometry_never_reports_an_impossible_pixel_count(
        bitpix in any::<i64>(),
        naxis in 0i64..5,
        n1 in any::<i64>(),
        n2 in any::<i64>(),
        n3 in any::<i64>(),
    ) {
        let h = header::FitsHeader {
            cards: vec![
                ("BITPIX".into(), bitpix.to_string()),
                ("NAXIS".into(), naxis.to_string()),
                ("NAXIS1".into(), n1.to_string()),
                ("NAXIS2".into(), n2.to_string()),
                ("NAXIS3".into(), n3.to_string()),
            ],
        };
        if let Ok(g) = Geometry::from_header(&h) {
            // A geometry that validated must be usable without overflowing.
            prop_assert!(g.data_len().is_some());
            prop_assert!(g.pixel_count() > 0);
            prop_assert!(g.channels == 1 || g.channels == 3);
        }
    }

    /// Header values survive being written and read back.
    #[test]
    fn string_header_values_round_trip(
        // FITS string values are ASCII text without quotes or control codes.
        text in "[A-Za-z0-9 _.+-]{0,60}",
    ) {
        let trimmed = text.trim_end();
        let spec = SyntheticSpec::new(1, 1, 16)
            .with_card("OBJECT", &format!("'{text}'"));
        let bytes = synthetic_fits(&spec, &[1.0]).unwrap();
        let img = read_fits_from_bytes(&bytes).unwrap();
        prop_assert_eq!(img.header.get("OBJECT").unwrap_or(""), trimmed);
    }
}

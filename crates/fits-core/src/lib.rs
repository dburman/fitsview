//! Core FITS handling for fitsview: parsing, pixel conversion, stretching and
//! calibration.
//!
//! This crate deliberately has no GUI dependencies so that every piece of image
//! logic can be unit tested without opening a window.
//!
//! # Safety
//!
//! This crate contains no `unsafe` code, enforced by the attribute below.

#![forbid(unsafe_code)]

pub mod calib;
pub mod debayer;
pub mod error;
pub mod header;
pub mod image;
pub mod reader;
pub mod stretch;

#[cfg(any(test, feature = "test-util"))]
pub mod testutil;

pub use calib::{build_master_median, calibrate, CalibError, MasterFrame};
pub use debayer::{debayer, BayerPattern};
pub use error::FitsError;
pub use header::FitsHeader;
pub use image::{convert_pixels, finite_min_max, FitsImage, Geometry};
pub use reader::{is_fits_path, read_fits, read_fits_from_bytes, write_fits};
pub use stretch::{build_lut, compute_stretch, Stretch, StretchParams};

/// FITS files are a sequence of 2880-byte blocks. Headers are padded to a whole
/// number of blocks, and so is the data section.
pub const BLOCK_SIZE: usize = 2880;

/// Header cards are fixed-width 80-byte ASCII records, 36 to a block.
pub const CARD_SIZE: usize = 80;

/// Number of header cards in one 2880-byte block.
pub const CARDS_PER_BLOCK: usize = BLOCK_SIZE / CARD_SIZE;

/// Rounds `len` up to a whole number of 2880-byte FITS blocks.
///
/// Returns `None` on overflow rather than wrapping, because the lengths this is
/// called with are derived from values read out of a file and cannot be trusted.
///
/// ```
/// use fits_core::{block_align, BLOCK_SIZE};
/// assert_eq!(block_align(0), Some(0));
/// assert_eq!(block_align(1), Some(BLOCK_SIZE));
/// assert_eq!(block_align(BLOCK_SIZE), Some(BLOCK_SIZE));
/// assert_eq!(block_align(BLOCK_SIZE + 1), Some(BLOCK_SIZE * 2));
/// ```
#[must_use]
pub fn block_align(len: usize) -> Option<usize> {
    let blocks = len.checked_add(BLOCK_SIZE - 1)? / BLOCK_SIZE;
    blocks.checked_mul(BLOCK_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_constants_are_consistent() {
        assert_eq!(CARDS_PER_BLOCK, 36);
        assert_eq!(CARDS_PER_BLOCK * CARD_SIZE, BLOCK_SIZE);
    }

    #[test]
    fn block_align_rounds_up_to_whole_blocks() {
        assert_eq!(block_align(0), Some(0));
        assert_eq!(block_align(1), Some(BLOCK_SIZE));
        assert_eq!(block_align(BLOCK_SIZE - 1), Some(BLOCK_SIZE));
        assert_eq!(block_align(BLOCK_SIZE), Some(BLOCK_SIZE));
        assert_eq!(block_align(BLOCK_SIZE + 1), Some(2 * BLOCK_SIZE));
    }

    #[test]
    fn block_align_returns_none_on_overflow_instead_of_wrapping() {
        assert_eq!(block_align(usize::MAX), None);
    }
}

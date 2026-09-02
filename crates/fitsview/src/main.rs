//! fitsview: a fast cross-platform viewer for FITS astrophotography images.
//!
//! # Safety
//!
//! This crate contains no `unsafe` code, enforced by the attribute below.

#![forbid(unsafe_code)]

/// Human-readable version string shown in the window title and `--version`.
#[must_use]
pub fn version_string() -> String {
    format!("fitsview {}", env!("CARGO_PKG_VERSION"))
}

fn main() {
    println!("{}", version_string());
    println!("No viewer yet. See README.md; the UI arrives in Phase 2.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_names_the_application() {
        let v = version_string();
        assert!(v.starts_with("fitsview "), "unexpected version: {v}");
        assert!(
            v.split_whitespace().nth(1).is_some_and(|n| n.contains('.')),
            "expected a dotted version number: {v}"
        );
    }

    #[test]
    fn core_crate_is_linked_and_usable() {
        assert_eq!(fits_core::block_align(1), Some(fits_core::BLOCK_SIZE));
    }
}

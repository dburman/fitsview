//! fitsview: a fast cross-platform viewer for FITS astrophotography images.
//!
//! The crate is split so that everything except drawing can be tested without
//! opening a window:
//!
//! - [`app`] holds the state and every rule that changes it.
//! - [`actions`] deletes, renames and flags files, behind a trait so tests
//!   never touch the real trash.
//! - [`crash`] turns a panic into a message the user can act on.
//! - [`folder`] is the file list and the selection within it.
//! - [`shortcuts`] is the single list of key bindings, used by both the help
//!   overlay and `--help`.
//! - [`sidecar`] remembers keep flags beside the images.
//! - [`measurements`] remembers what each frame measured, beside it.
//! - [`memory`] says whether a stack fits in memory all at once.
//! - [`jobs`] runs slow work, such as combining darks or exporting a folder,
//!   on a background thread with progress and cancellation.
//! - [`library`] finds the darks and flats that suit a night.
//! - [`loader`] decodes images on a worker thread and caches the results.
//! - [`natsort`] orders file names so `light_2` comes before `light_10`.
//! - [`view`] is the zoom and pan arithmetic.
//! - [`watch`] notices frames added to the open folder while it is open.
//! - [`texture`] converts decoded samples into something the GPU can draw,
//!   including the vertical flip FITS requires.
//! - [`ui`] is the only part that touches `eframe`, and contains no rules of
//!   its own. [`ui::input`] maps raw input onto actions and is tested directly.
//!
//! # Safety
//!
//! This crate contains no `unsafe` code, enforced by the attribute below.

#![forbid(unsafe_code)]

pub mod actions;
pub mod adapter;
pub mod app;
pub mod crash;
pub mod folder;
pub mod icon;
pub mod jobs;
pub mod library;
pub mod loader;
pub mod measurements;
pub mod memory;
pub mod natsort;
pub mod shortcuts;
pub mod sidecar;
pub mod stardetect;
pub mod texture;
pub mod ui;
pub mod view;
pub mod watch;

/// Human-readable version string, shown in the window title and by `--version`.
#[must_use]
pub fn version_string() -> String {
    format!("fitsview {}", env!("CARGO_PKG_VERSION"))
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

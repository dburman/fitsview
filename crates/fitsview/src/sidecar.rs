//! Per-folder settings stored beside the images.
//!
//! Keep flags have to survive restarting the application, and they belong to
//! the folder rather than to the machine: copy a session to another disk and
//! the flags should travel with it. So they live in a small JSON file named
//! `.fitsview.json` in the folder itself.
//!
//! Two properties matter more than the format:
//!
//! 1. **Writes are atomic.** The file is written to a temporary name and then
//!    renamed over the old one, so an interrupted write cannot leave a
//!    half-written file that loses every flag in the folder.
//! 2. **Unknown fields survive.** A file written by a later version, carrying
//!    settings this version knows nothing about, keeps those settings when this
//!    version writes it back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Name of the sidecar file. Hidden, so folder scanning skips it already.
pub const SIDECAR_NAME: &str = ".fitsview.json";

/// The contents of a folder's sidecar file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sidecar {
    /// File names marked to keep. Names rather than paths, so moving the folder
    /// does not invalidate them.
    #[serde(default)]
    pub flagged: Vec<String>,

    /// Master dark last used with this folder, as an absolute path.
    ///
    /// A path rather than a name, because calibration frames usually live
    /// elsewhere. A stale one is ignored on load rather than reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_dark: Option<String>,

    /// Master flat last used with this folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_flat: Option<String>,

    /// Anything this version does not recognise, kept so that settings written
    /// by a newer version are not silently discarded when this one saves.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl Sidecar {
    /// Whether a file name is flagged.
    #[must_use]
    pub fn is_flagged(&self, name: &str) -> bool {
        self.flagged.iter().any(|n| n == name)
    }

    /// Replaces the flag list with `names`, sorted so the file has a stable
    /// order and does not churn in version control.
    pub fn set_flagged(&mut self, mut names: Vec<String>) {
        names.sort();
        names.dedup();
        self.flagged = names;
    }
}

/// Where the sidecar lives for a folder.
#[must_use]
pub fn path_for(dir: &Path) -> PathBuf {
    dir.join(SIDECAR_NAME)
}

/// Reads a folder's sidecar.
///
/// A missing file, an unreadable one, or one containing invalid JSON all yield
/// the default. Losing flags is bad; refusing to open the folder because a
/// settings file is damaged would be worse.
#[must_use]
pub fn load(dir: &Path) -> Sidecar {
    let path = path_for(dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Sidecar::default();
    };
    match serde_json::from_str(&text) {
        Ok(sidecar) => sidecar,
        Err(e) => {
            log::warn!("ignoring damaged {}: {e}", path.display());
            Sidecar::default()
        }
    }
}

/// Writes a folder's sidecar atomically.
///
/// The content goes to a temporary file in the same folder, which is then
/// renamed over the target. Rename within one filesystem is atomic, so a reader
/// sees either the old file or the new one, never a partial write.
///
/// # Errors
///
/// Returns the underlying error if the folder cannot be written to, which
/// happens with read-only media.
pub fn save(dir: &Path, sidecar: &Sidecar) -> std::io::Result<()> {
    let target = path_for(dir);

    // If there is nothing to remember, remove the file rather than leaving an
    // empty one cluttering the user's folder.
    if sidecar.flagged.is_empty()
        && sidecar.unknown.is_empty()
        && sidecar.master_dark.is_none()
        && sidecar.master_flat.is_none()
    {
        return match std::fs::remove_file(&target) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
    }

    let text = serde_json::to_string_pretty(sidecar)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    // A fixed temporary name in the same folder: same filesystem, so the rename
    // is atomic, and it is hidden so a crash cannot leave visible litter.
    let temp = dir.join(".fitsview.json.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, &target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_without_a_sidecar_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let s = load(dir.path());
        assert!(s.flagged.is_empty());
        assert!(s.unknown.is_empty());
    }

    #[test]
    fn flags_survive_a_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Sidecar::default();
        s.set_flagged(vec!["b.fits".into(), "a.fits".into()]);
        save(dir.path(), &s).unwrap();

        let back = load(dir.path());
        assert!(back.is_flagged("a.fits"));
        assert!(back.is_flagged("b.fits"));
        assert!(!back.is_flagged("c.fits"));
    }

    #[test]
    fn the_flag_list_is_sorted_and_deduplicated() {
        let mut s = Sidecar::default();
        s.set_flagged(vec!["b.fits".into(), "a.fits".into(), "b.fits".into()]);
        assert_eq!(s.flagged, vec!["a.fits", "b.fits"]);
    }

    #[test]
    fn the_sidecar_is_hidden_so_folder_scanning_skips_it() {
        assert!(SIDECAR_NAME.starts_with('.'));
    }

    #[test]
    fn a_damaged_sidecar_is_ignored_rather_than_failing_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path_for(dir.path()), b"{not valid json at all").unwrap();
        let s = load(dir.path());
        assert!(s.flagged.is_empty(), "should fall back to defaults");
    }

    #[test]
    fn a_sidecar_of_the_wrong_shape_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path_for(dir.path()), b"[1, 2, 3]").unwrap();
        assert!(load(dir.path()).flagged.is_empty());
    }

    #[test]
    fn settings_from_a_newer_version_are_preserved_across_a_save() {
        // Otherwise opening a folder in an older build would silently discard
        // the calibration paths a later phase stores here.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path_for(dir.path()),
            br#"{"flagged":["a.fits"],"master_dark":"/darks/master.fits","future":{"x":1}}"#,
        )
        .unwrap();

        let mut s = load(dir.path());
        assert!(s.is_flagged("a.fits"));
        assert_eq!(s.unknown.len(), 2, "unknown keys should be retained");

        s.set_flagged(vec!["b.fits".into()]);
        save(dir.path(), &s).unwrap();

        let text = std::fs::read_to_string(path_for(dir.path())).unwrap();
        assert!(
            text.contains("master_dark"),
            "lost a newer version's setting"
        );
        assert!(text.contains("/darks/master.fits"));
        assert!(text.contains("b.fits"));
    }

    #[test]
    fn calibration_paths_survive_a_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let s = Sidecar {
            master_dark: Some("/library/master_dark.fits".into()),
            master_flat: Some("/library/master_flat.fits".into()),
            ..Sidecar::default()
        };
        save(dir.path(), &s).unwrap();

        let back = load(dir.path());
        assert_eq!(
            back.master_dark.as_deref(),
            Some("/library/master_dark.fits")
        );
        assert_eq!(
            back.master_flat.as_deref(),
            Some("/library/master_flat.fits")
        );
    }

    #[test]
    fn a_sidecar_holding_only_calibration_paths_is_kept() {
        // It is not empty just because nothing is flagged.
        let dir = tempfile::tempdir().unwrap();
        let s = Sidecar {
            master_dark: Some("/library/d.fits".into()),
            ..Sidecar::default()
        };
        save(dir.path(), &s).unwrap();
        assert!(path_for(dir.path()).exists());
    }

    #[test]
    fn saving_nothing_removes_the_file_rather_than_leaving_it_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Sidecar::default();
        s.set_flagged(vec!["a.fits".into()]);
        save(dir.path(), &s).unwrap();
        assert!(path_for(dir.path()).exists());

        s.set_flagged(Vec::new());
        save(dir.path(), &s).unwrap();
        assert!(
            !path_for(dir.path()).exists(),
            "an empty sidecar should be removed"
        );
    }

    #[test]
    fn removing_an_absent_sidecar_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(save(dir.path(), &Sidecar::default()).is_ok());
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Sidecar::default();
        s.set_flagged(vec!["a.fits".into()]);
        save(dir.path(), &s).unwrap();

        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[test]
    fn saving_twice_replaces_rather_than_appends() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Sidecar::default();
        s.set_flagged(vec!["a.fits".into()]);
        save(dir.path(), &s).unwrap();
        s.set_flagged(vec!["b.fits".into()]);
        save(dir.path(), &s).unwrap();

        let back = load(dir.path());
        assert_eq!(back.flagged, vec!["b.fits"]);
    }
}

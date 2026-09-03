//! Deleting, renaming and flagging files.
//!
//! This is the only module that changes the user's files, so it is written to
//! be tested exhaustively without ever touching a real trash can. Every
//! filesystem effect goes through [`FileOps`]; tests substitute a recording
//! implementation, and only [`RealFileOps`] talks to the operating system.
//!
//! Deleting always means moving to the operating system's trash. There is no
//! code path in this application that permanently removes a file.

use std::path::Path;

use crate::folder::Folder;
use crate::sidecar::{self, Sidecar};

/// The filesystem effects this module needs.
///
/// Existing behind a trait is not architecture for its own sake: it is what
/// lets the delete tests run in continuous integration without filling a
/// build agent's trash, and what lets failure paths be tested at all.
pub trait FileOps: std::fmt::Debug {
    /// Moves a file to the operating system's trash. Never a permanent delete.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be moved, for example on read-only
    /// media or where no trash is available.
    fn trash(&self, path: &Path) -> Result<(), String>;

    /// Renames a file within its folder.
    ///
    /// # Errors
    ///
    /// Returns an error if the rename fails.
    fn rename(&self, from: &Path, to: &Path) -> Result<(), String>;

    /// Whether a path already exists.
    fn exists(&self, path: &Path) -> bool;
}

/// The real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealFileOps;

/// Moves a path to the operating system's trash.
///
/// On macOS the trash crate defaults to driving Finder through AppleScript.
/// That approach needs automation permission, and when it does not have it the
/// call blocks for two minutes before failing with an Apple Event timeout,
/// which would freeze the window. `NSFileManager` does the same job through a
/// direct API call: no extra permission, no Finder dependency, and fast.
///
/// The trade-off is that files trashed this way may not offer "Put Back" in
/// Finder. That costs nothing here, because programmatic restore is unavailable
/// on macOS regardless, which is why [`undo_supported`] reports false there.
#[cfg(target_os = "macos")]
fn move_to_trash(path: &Path) -> Result<(), String> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};

    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path).map_err(|e| e.to_string())
}

/// Moves a path to the operating system's trash.
///
/// Windows and Freedesktop systems have a real trash API, so the crate's
/// default is already the right thing.
#[cfg(not(target_os = "macos"))]
fn move_to_trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| e.to_string())
}

impl FileOps for RealFileOps {
    fn trash(&self, path: &Path) -> Result<(), String> {
        // This is deliberately the only deletion call in the whole crate, and
        // it moves the file rather than destroying it.
        move_to_trash(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), String> {
        std::fs::rename(from, to).map_err(|e| e.to_string())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

/// Whether this platform can restore files from the trash.
///
/// Available on Windows and on Freedesktop systems, not on macOS. The interface
/// hides the undo hint where it would not work rather than offering something
/// that silently fails.
#[must_use]
pub const fn undo_supported() -> bool {
    cfg!(any(
        target_os = "windows",
        all(
            unix,
            not(target_os = "macos"),
            not(target_os = "ios"),
            not(target_os = "android")
        )
    ))
}

/// What an action did, for the status line and toasts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A file was moved to the trash.
    Deleted {
        /// The file's name.
        name: String,
    },
    /// A file was renamed.
    Renamed {
        /// The name before.
        from: String,
        /// The name after.
        to: String,
    },
    /// A keep flag was turned on or off.
    Flagged {
        /// The file's name.
        name: String,
        /// Whether it is now flagged.
        flagged: bool,
    },
    /// The request had no effect, such as renaming a file to its own name.
    NoChange,
}

/// Why a rename was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenameError {
    /// The proposed name was blank.
    #[error("the name cannot be empty")]
    Empty,
    /// The proposed name contained a path separator.
    #[error("the name cannot contain a path separator")]
    HasSeparator,
    /// The proposed name was `.` or `..`.
    #[error("that name is reserved")]
    Reserved,
    /// A file of that name already exists in the folder.
    #[error("{0} already exists")]
    AlreadyExists(String),
}

/// Why an action could not be carried out.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionError {
    /// Nothing was selected.
    #[error("no file is selected")]
    NoSelection,
    /// The proposed name was not usable.
    #[error("{0}")]
    Rename(#[from] RenameError),
    /// The filesystem refused.
    #[error("{0}")]
    Failed(String),
}

/// Whether deleting the current selection should ask first.
///
/// A flagged file always asks, because flagging it is the user saying they mean
/// to keep it. `confirm_all` additionally asks for unflagged files, for people
/// who would rather be careful than fast.
#[must_use]
pub fn needs_delete_confirmation(folder: &Folder, confirm_all: bool) -> bool {
    match folder.selected_entry() {
        Some(entry) => confirm_all || entry.flagged,
        None => false,
    }
}

/// Moves the selected file to the trash and removes it from the list.
///
/// The selection moves to the file that took its place, which is the next file,
/// or the last file if the deleted one was at the end. That keeps a culling run
/// moving forwards without a keystroke.
///
/// This does not check flags: [`needs_delete_confirmation`] decides whether to
/// ask, and the interface asks before calling this.
///
/// # Errors
///
/// Returns [`ActionError::NoSelection`] with nothing selected, or
/// [`ActionError::Failed`] if the file could not be moved to the trash.
pub fn delete_selected(folder: &mut Folder, ops: &dyn FileOps) -> Result<Outcome, ActionError> {
    let index = folder.selected.ok_or(ActionError::NoSelection)?;
    let entry = folder
        .files
        .get(index)
        .ok_or(ActionError::NoSelection)?
        .clone();

    ops.trash(&entry.path).map_err(ActionError::Failed)?;

    folder.files.remove(index);
    folder.selected = if folder.files.is_empty() {
        None
    } else {
        Some(index.min(folder.files.len() - 1))
    };

    Ok(Outcome::Deleted { name: entry.name })
}

/// Checks a proposed new name and returns the name that would be used.
///
/// The extension is preserved when the user removes it, because renaming
/// `light_1.fits` to `m31` should not produce a file the viewer can no longer
/// open.
///
/// # Errors
///
/// See [`RenameError`].
pub fn validate_new_name(
    folder: &Folder,
    ops: &dyn FileOps,
    proposed: &str,
) -> Result<String, RenameError> {
    let Some(entry) = folder.selected_entry() else {
        return Err(RenameError::Empty);
    };

    let trimmed = proposed.trim();
    if trimmed.is_empty() {
        return Err(RenameError::Empty);
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(RenameError::HasSeparator);
    }
    if trimmed == "." || trimmed == ".." {
        return Err(RenameError::Reserved);
    }

    // Restore the original extension if the user dropped it.
    let original_ext = Path::new(&entry.name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let has_ext = Path::new(trimmed)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| !e.is_empty());
    let final_name = if has_ext || original_ext.is_empty() {
        trimmed.to_string()
    } else {
        format!("{trimmed}.{original_ext}")
    };

    if final_name == entry.name {
        // Renaming a file to its own name is a no-op, not a clash.
        return Ok(final_name);
    }

    // Compare case-insensitively as well, because Windows and macOS filesystems
    // usually are, and a rename that only changes case would clobber the file.
    let clashes = folder
        .files
        .iter()
        .any(|e| e.name.eq_ignore_ascii_case(&final_name))
        || ops.exists(&folder.dir.join(&final_name));
    if clashes {
        return Err(RenameError::AlreadyExists(final_name));
    }

    Ok(final_name)
}

/// Renames the selected file.
///
/// The selection and the keep flag both follow the file. The list is re-sorted,
/// because the new name may belong elsewhere in the order.
///
/// # Errors
///
/// See [`ActionError`].
pub fn rename_selected(
    folder: &mut Folder,
    ops: &dyn FileOps,
    proposed: &str,
) -> Result<Outcome, ActionError> {
    if folder.selected_entry().is_none() {
        return Err(ActionError::NoSelection);
    }
    let final_name = validate_new_name(folder, ops, proposed)?;

    let index = folder.selected.ok_or(ActionError::NoSelection)?;
    let old_name = folder.files[index].name.clone();
    if final_name == old_name {
        return Ok(Outcome::NoChange);
    }

    let from = folder.files[index].path.clone();
    let to = folder.dir.join(&final_name);
    ops.rename(&from, &to).map_err(ActionError::Failed)?;

    folder.files[index].name.clone_from(&final_name);
    folder.files[index].path = to;

    // The new name may sort elsewhere, so re-order and follow the file.
    let moved = folder.files[index].path.clone();
    folder.sort();
    folder.select_path(&moved);

    Ok(Outcome::Renamed {
        from: old_name,
        to: final_name,
    })
}

/// Turns the selected file's keep flag on or off.
///
/// Returns `None` when nothing is selected.
pub fn toggle_flag(folder: &mut Folder) -> Option<Outcome> {
    let index = folder.selected?;
    let entry = folder.files.get_mut(index)?;
    entry.flagged = !entry.flagged;
    Some(Outcome::Flagged {
        name: entry.name.clone(),
        flagged: entry.flagged,
    })
}

/// Writes the folder's flags to its sidecar, preserving settings this version
/// does not recognise.
///
/// # Errors
///
/// Returns the underlying error if the sidecar cannot be written, which happens
/// on read-only media.
pub fn save_flags(folder: &Folder) -> std::io::Result<()> {
    let mut existing: Sidecar = sidecar::load(&folder.dir);
    existing.set_flagged(
        folder
            .files
            .iter()
            .filter(|e| e.flagged)
            .map(|e| e.name.clone())
            .collect(),
    );
    sidecar::save(&folder.dir, &existing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder::FileEntry;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::path::PathBuf;

    /// A stand-in filesystem that records what it was asked to do.
    ///
    /// Tests use this so that deleting never reaches the real trash, and so
    /// that failures can be provoked on demand.
    #[derive(Debug, Default)]
    struct RecordingOps {
        trashed: RefCell<Vec<PathBuf>>,
        renamed: RefCell<Vec<(PathBuf, PathBuf)>>,
        existing: RefCell<HashSet<PathBuf>>,
        fail_trash: bool,
        fail_rename: bool,
    }

    impl RecordingOps {
        fn with_existing(paths: &[&str]) -> Self {
            Self {
                existing: RefCell::new(paths.iter().map(PathBuf::from).collect()),
                ..Self::default()
            }
        }
        fn failing_trash() -> Self {
            Self {
                fail_trash: true,
                ..Self::default()
            }
        }
        fn failing_rename() -> Self {
            Self {
                fail_rename: true,
                ..Self::default()
            }
        }
    }

    impl FileOps for RecordingOps {
        fn trash(&self, path: &Path) -> Result<(), String> {
            if self.fail_trash {
                return Err("permission denied".into());
            }
            self.trashed.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
        fn rename(&self, from: &Path, to: &Path) -> Result<(), String> {
            if self.fail_rename {
                return Err("disk full".into());
            }
            self.renamed
                .borrow_mut()
                .push((from.to_path_buf(), to.to_path_buf()));
            Ok(())
        }
        fn exists(&self, path: &Path) -> bool {
            self.existing.borrow().contains(path)
        }
    }

    /// A folder of numbered files, built without touching the filesystem.
    fn folder(names: &[&str]) -> Folder {
        Folder {
            dir: PathBuf::from("/session"),
            files: names
                .iter()
                .map(|n| FileEntry {
                    path: PathBuf::from("/session").join(n),
                    name: (*n).to_string(),
                    size: 100,
                    flagged: false,
                    quality: None,
                })
                .collect(),
            selected: if names.is_empty() { None } else { Some(0) },
        }
    }

    #[test]
    fn deleting_moves_to_the_trash_and_never_removes_permanently() {
        // The whole safety story of this module in one assertion.
        let mut f = folder(&["a.fits", "b.fits"]);
        let ops = RecordingOps::default();

        let outcome = delete_selected(&mut f, &ops).unwrap();
        assert_eq!(
            outcome,
            Outcome::Deleted {
                name: "a.fits".into()
            }
        );
        assert_eq!(
            *ops.trashed.borrow(),
            vec![PathBuf::from("/session/a.fits")],
            "the file must go to the trash"
        );
    }

    #[test]
    fn deleting_advances_the_selection_to_the_next_file() {
        let mut f = folder(&["a.fits", "b.fits", "c.fits"]);
        f.select(1);
        delete_selected(&mut f, &RecordingOps::default()).unwrap();

        assert_eq!(f.len(), 2);
        assert_eq!(
            f.selected_entry().unwrap().name,
            "c.fits",
            "selection should land on the file that took its place"
        );
    }

    #[test]
    fn deleting_the_last_file_falls_back_to_the_previous_one() {
        let mut f = folder(&["a.fits", "b.fits"]);
        f.select_last();
        delete_selected(&mut f, &RecordingOps::default()).unwrap();
        assert_eq!(f.selected_entry().unwrap().name, "a.fits");
    }

    #[test]
    fn deleting_the_only_file_leaves_an_empty_folder() {
        let mut f = folder(&["only.fits"]);
        delete_selected(&mut f, &RecordingOps::default()).unwrap();
        assert!(f.is_empty());
        assert_eq!(f.selected, None);
    }

    #[test]
    fn deleting_with_nothing_selected_is_refused() {
        let mut f = folder(&[]);
        assert_eq!(
            delete_selected(&mut f, &RecordingOps::default()),
            Err(ActionError::NoSelection)
        );
    }

    #[test]
    fn a_failed_delete_leaves_the_list_untouched() {
        // If the file is still on disk, it must still be in the list, or the
        // interface would be lying about what is in the folder.
        let mut f = folder(&["a.fits", "b.fits"]);
        let result = delete_selected(&mut f, &RecordingOps::failing_trash());

        assert!(matches!(result, Err(ActionError::Failed(_))));
        assert_eq!(f.len(), 2, "nothing should have been removed");
        assert_eq!(f.selected, Some(0));
    }

    #[test]
    fn an_unflagged_file_deletes_without_asking() {
        let f = folder(&["a.fits"]);
        assert!(!needs_delete_confirmation(&f, false));
    }

    #[test]
    fn a_flagged_file_always_asks_first() {
        let mut f = folder(&["a.fits"]);
        f.files[0].flagged = true;
        assert!(
            needs_delete_confirmation(&f, false),
            "a file marked to keep must not delete silently"
        );
        assert!(needs_delete_confirmation(&f, true));
    }

    #[test]
    fn confirm_every_delete_asks_for_unflagged_files_too() {
        let f = folder(&["a.fits"]);
        assert!(needs_delete_confirmation(&f, true));
    }

    #[test]
    fn nothing_selected_needs_no_confirmation() {
        assert!(!needs_delete_confirmation(&folder(&[]), true));
    }

    #[test]
    fn renaming_moves_the_file_and_updates_the_entry() {
        let mut f = folder(&["light_1.fits", "light_2.fits"]);
        let ops = RecordingOps::default();

        let outcome = rename_selected(&mut f, &ops, "m31_first.fits").unwrap();
        assert_eq!(
            outcome,
            Outcome::Renamed {
                from: "light_1.fits".into(),
                to: "m31_first.fits".into()
            }
        );
        assert_eq!(
            *ops.renamed.borrow(),
            vec![(
                PathBuf::from("/session/light_1.fits"),
                PathBuf::from("/session/m31_first.fits")
            )]
        );
        assert_eq!(f.selected_entry().unwrap().name, "m31_first.fits");
    }

    #[test]
    fn renaming_keeps_the_extension_when_the_user_drops_it() {
        let mut f = folder(&["light_1.fits"]);
        let outcome = rename_selected(&mut f, &RecordingOps::default(), "m31").unwrap();
        assert_eq!(
            outcome,
            Outcome::Renamed {
                from: "light_1.fits".into(),
                to: "m31.fits".into()
            }
        );
    }

    #[test]
    fn renaming_can_change_the_extension_deliberately() {
        let mut f = folder(&["light_1.fits"]);
        rename_selected(&mut f, &RecordingOps::default(), "light_1.fit").unwrap();
        assert_eq!(f.selected_entry().unwrap().name, "light_1.fit");
    }

    #[test]
    fn renaming_keeps_the_selection_and_the_flag() {
        let mut f = folder(&["a.fits", "b.fits", "c.fits"]);
        f.select(1);
        f.files[1].flagged = true;

        rename_selected(&mut f, &RecordingOps::default(), "zzz.fits").unwrap();

        let entry = f.selected_entry().expect("selection should survive");
        assert_eq!(entry.name, "zzz.fits");
        assert!(entry.flagged, "the keep flag must follow the file");
    }

    #[test]
    fn renaming_reorders_the_list_and_follows_the_file() {
        let mut f = folder(&["b.fits", "c.fits"]);
        f.select(0);
        rename_selected(&mut f, &RecordingOps::default(), "z.fits").unwrap();

        let names: Vec<&str> = f.files.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["c.fits", "z.fits"], "list should be re-sorted");
        assert_eq!(f.selected_entry().unwrap().name, "z.fits");
    }

    #[test]
    fn renaming_to_the_same_name_does_nothing() {
        let mut f = folder(&["a.fits"]);
        let ops = RecordingOps::default();
        assert_eq!(
            rename_selected(&mut f, &ops, "a.fits").unwrap(),
            Outcome::NoChange
        );
        assert!(ops.renamed.borrow().is_empty(), "no rename should happen");
    }

    #[test]
    fn an_empty_or_blank_name_is_refused() {
        let mut f = folder(&["a.fits"]);
        for bad in ["", "   ", "\t"] {
            assert_eq!(
                rename_selected(&mut f, &RecordingOps::default(), bad),
                Err(ActionError::Rename(RenameError::Empty)),
                "input {bad:?}"
            );
        }
    }

    #[test]
    fn a_name_containing_a_path_separator_is_refused() {
        // Otherwise a rename could move a file out of the folder, or overwrite
        // something elsewhere on the disk.
        let mut f = folder(&["a.fits"]);
        for bad in [
            "../escape.fits",
            "sub/dir.fits",
            "back\\slash.fits",
            "/abs.fits",
        ] {
            assert_eq!(
                rename_selected(&mut f, &RecordingOps::default(), bad),
                Err(ActionError::Rename(RenameError::HasSeparator)),
                "input {bad:?}"
            );
        }
    }

    #[test]
    fn the_reserved_names_are_refused() {
        let mut f = folder(&["a.fits"]);
        for bad in [".", ".."] {
            assert_eq!(
                rename_selected(&mut f, &RecordingOps::default(), bad),
                Err(ActionError::Rename(RenameError::Reserved)),
                "input {bad:?}"
            );
        }
    }

    #[test]
    fn renaming_onto_another_file_in_the_list_is_refused() {
        let mut f = folder(&["a.fits", "b.fits"]);
        assert_eq!(
            rename_selected(&mut f, &RecordingOps::default(), "b.fits"),
            Err(ActionError::Rename(RenameError::AlreadyExists(
                "b.fits".into()
            )))
        );
    }

    #[test]
    fn renaming_onto_a_file_that_is_on_disk_but_not_listed_is_refused() {
        // A non-FITS file in the folder is not in the list, but renaming over
        // it would still destroy it.
        let mut f = folder(&["a.fits"]);
        let ops = RecordingOps::with_existing(&["/session/notes.txt"]);
        assert_eq!(
            rename_selected(&mut f, &ops, "notes.txt"),
            Err(ActionError::Rename(RenameError::AlreadyExists(
                "notes.txt".into()
            )))
        );
    }

    #[test]
    fn a_clash_differing_only_in_case_is_refused() {
        // Most desktop filesystems are case-insensitive, so this would clobber.
        let mut f = folder(&["a.fits", "b.fits"]);
        assert!(matches!(
            rename_selected(&mut f, &RecordingOps::default(), "B.FITS"),
            Err(ActionError::Rename(RenameError::AlreadyExists(_)))
        ));
    }

    #[test]
    fn a_failed_rename_leaves_the_entry_untouched() {
        let mut f = folder(&["a.fits"]);
        let result = rename_selected(&mut f, &RecordingOps::failing_rename(), "b.fits");
        assert!(matches!(result, Err(ActionError::Failed(_))));
        assert_eq!(f.files[0].name, "a.fits", "the list must match the disk");
    }

    #[test]
    fn renaming_with_nothing_selected_is_refused() {
        let mut f = folder(&[]);
        assert_eq!(
            rename_selected(&mut f, &RecordingOps::default(), "x.fits"),
            Err(ActionError::NoSelection)
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_from_a_new_name() {
        let mut f = folder(&["a.fits"]);
        rename_selected(&mut f, &RecordingOps::default(), "  spaced.fits  ").unwrap();
        assert_eq!(f.selected_entry().unwrap().name, "spaced.fits");
    }

    #[test]
    fn flagging_toggles_and_reports_the_new_state() {
        let mut f = folder(&["a.fits"]);
        assert_eq!(
            toggle_flag(&mut f),
            Some(Outcome::Flagged {
                name: "a.fits".into(),
                flagged: true
            })
        );
        assert!(f.files[0].flagged);

        assert_eq!(
            toggle_flag(&mut f),
            Some(Outcome::Flagged {
                name: "a.fits".into(),
                flagged: false
            })
        );
        assert!(!f.files[0].flagged);
    }

    #[test]
    fn flagging_with_nothing_selected_does_nothing() {
        assert_eq!(toggle_flag(&mut folder(&[])), None);
    }

    #[test]
    fn flags_are_written_to_the_sidecar_and_read_back_by_a_scan() {
        use fits_core::testutil::{write_synthetic, SyntheticSpec};

        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(2, 2, 16);
        for n in ["a.fits", "b.fits"] {
            write_synthetic(dir.path(), n, &spec, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        }

        let mut f = crate::folder::scan_folder(dir.path()).unwrap();
        f.select(1);
        toggle_flag(&mut f);
        save_flags(&f).unwrap();

        // A fresh scan, as though the application had been restarted.
        let reopened = crate::folder::scan_folder(dir.path()).unwrap();
        assert!(!reopened.files[0].flagged, "a.fits should not be flagged");
        assert!(reopened.files[1].flagged, "b.fits should still be flagged");
    }

    #[test]
    fn clearing_a_flag_is_persisted_too() {
        use fits_core::testutil::{write_synthetic, SyntheticSpec};

        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(2, 2, 16);
        write_synthetic(dir.path(), "a.fits", &spec, &[1.0, 2.0, 3.0, 4.0]).unwrap();

        let mut f = crate::folder::scan_folder(dir.path()).unwrap();
        toggle_flag(&mut f);
        save_flags(&f).unwrap();
        assert!(crate::folder::scan_folder(dir.path()).unwrap().files[0].flagged);

        toggle_flag(&mut f);
        save_flags(&f).unwrap();
        assert!(!crate::folder::scan_folder(dir.path()).unwrap().files[0].flagged);
    }

    #[test]
    fn undo_support_is_reported_honestly_for_this_platform() {
        // macOS has no programmatic restore from the trash; the interface must
        // not offer undo there.
        assert_eq!(undo_supported(), !cfg!(target_os = "macos"));
    }
}

#[cfg(test)]
mod real_filesystem_tests {
    use super::*;

    /// Checks that [`RealFileOps`] really does move a file to the trash.
    ///
    /// Marked `#[ignore]` because it is the one test with a side effect
    /// outside the temporary directory: it genuinely puts a file in the trash
    /// can of whoever runs it. Every other test uses a recording stand-in, so
    /// this is the only thing standing between us and shipping a delete button
    /// wired to a function that does not work on some platform.
    ///
    /// Run it deliberately, on each platform, when touching this code:
    ///
    /// ```text
    /// cargo test -p fitsview --all-features -- --ignored real_delete
    /// ```
    #[test]
    #[ignore = "moves a real file to the trash; run deliberately"]
    fn real_delete_moves_the_file_to_the_trash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fitsview-trash-check.fits");
        std::fs::write(&path, b"placeholder").unwrap();
        assert!(path.exists());

        RealFileOps
            .trash(&path)
            .expect("moving to the trash should succeed");

        assert!(!path.exists(), "the file should no longer be in the folder");
    }

    /// Checks that [`RealFileOps`] renames within a folder. No side effects
    /// outside the temporary directory, so this one runs normally.
    #[test]
    fn real_rename_moves_the_file_within_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("before.fits");
        let to = dir.path().join("after.fits");
        std::fs::write(&from, b"placeholder").unwrap();

        assert!(RealFileOps.exists(&from));
        assert!(!RealFileOps.exists(&to));

        RealFileOps
            .rename(&from, &to)
            .expect("rename should succeed");

        assert!(!from.exists());
        assert!(to.exists());
    }

    #[test]
    fn real_rename_reports_failure_rather_than_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-here.fits");
        let target = dir.path().join("target.fits");
        assert!(RealFileOps.rename(&missing, &target).is_err());
    }
}

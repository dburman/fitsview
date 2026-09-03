//! Application state and the rules that change it.
//!
//! [`Model`] is deliberately free of anything that needs a window: no context,
//! no texture handle, no painter. Every state transition goes through
//! [`Model::handle`], so the behaviour a user actually notices, such as what
//! happens when they press a key, is testable without opening a window.
//!
//! The thin `eframe` wrapper that turns input into [`Action`]s and draws the
//! result lives in [`crate::ui`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Pos2, Rect, Vec2};
use fits_core::FitsImage;

use crate::actions::{self, ActionError, FileOps, Outcome, RealFileOps};
use crate::folder::{scan_folder, Folder};
use crate::loader::Loader;
use crate::view::ViewState;

/// An image that has been loaded and is being displayed.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// Where it came from.
    pub path: PathBuf,
    /// The decoded samples.
    pub image: Arc<FitsImage>,
    /// How long the read took, in milliseconds, for the status bar.
    pub load_ms: f64,
}

impl Loaded {
    /// The image's dimensions as a vector, for view arithmetic.
    #[must_use]
    pub fn size(&self) -> Vec2 {
        #[allow(clippy::cast_precision_loss)]
        Vec2::new(self.image.width as f32, self.image.height as f32)
    }

    /// A one-line description for the status bar.
    #[must_use]
    pub fn summary(&self) -> String {
        let name = self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let bitpix = self.image.header.get("BITPIX").unwrap_or("?");
        let colour = if self.image.channels == 3 { " RGB" } else { "" };
        format!(
            "{name}  {}x{}{colour}  BITPIX {bitpix}  {:.0} ms",
            self.image.width, self.image.height, self.load_ms
        )
    }
}

/// Everything the user can ask the application to do.
///
/// Input handling turns key presses, clicks and dialogs into these; nothing
/// else mutates the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Open a path. A folder is browsed; a file is displayed with its folder
    /// loaded around it, so the arrow keys work straight away.
    Open(PathBuf),
    /// Show the file at this index in the current folder.
    Select(usize),
    /// Move to the next file. Stops at the end.
    NextFile,
    /// Move to the previous file. Stops at the start.
    PreviousFile,
    /// Jump to the first file.
    FirstFile,
    /// Jump to the last file.
    LastFile,
    /// Re-read the folder from disk, keeping the selection where possible.
    Rescan,
    /// Scale the image to fit the viewport and centre it.
    FitToWindow,
    /// Show the image at one screen pixel per image pixel.
    ActualSize,
    /// Move the image by a screen-space delta.
    Pan(Vec2),
    /// Zoom by `factor` while keeping `anchor` fixed.
    ZoomAt {
        /// Screen position to keep fixed.
        anchor: Pos2,
        /// Multiplier applied to the current zoom.
        factor: f32,
    },
    /// Turn the selected file's keep flag on or off.
    ToggleFlag,
    /// Ask to delete the selected file. Opens a confirmation if one is needed.
    RequestDelete,
    /// Delete without asking. Only reached from the confirmation dialog.
    ConfirmDelete,
    /// Begin renaming the selected file.
    BeginRename,
    /// The rename editor's text changed, so revalidate it.
    RenameTextChanged(String),
    /// Finish renaming, using the name currently typed.
    CommitRename(String),
    /// Abandon a confirmation or a rename.
    Cancel,
    /// Turn the "confirm every delete" setting on or off.
    ToggleConfirmEveryDelete,
    /// Show or hide the keyboard shortcut overlay.
    ToggleHelp,
    /// Dismiss the current error message.
    ClearError,
}

/// A modal question or edit that is waiting on the user.
///
/// Only one can be active at a time, which is why this is an enum rather than
/// a set of booleans that could contradict each other.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Pending {
    /// Nothing is waiting.
    #[default]
    None,
    /// Waiting for the user to confirm deleting this file.
    DeleteConfirm {
        /// Name of the file in question.
        name: String,
        /// Whether it carries a keep flag, which changes the wording.
        flagged: bool,
    },
    /// Renaming, with the text typed so far.
    Rename {
        /// Current contents of the edit box.
        text: String,
        /// Why the current text is unusable, if it is.
        problem: Option<String>,
    },
}

/// A short-lived message shown after an action.
#[derive(Debug, Clone)]
pub struct Toast {
    /// What to show.
    pub text: String,
    /// When it stops being shown.
    pub until: std::time::Instant,
}

impl Toast {
    /// How long a message stays on screen.
    pub const LIFETIME: std::time::Duration = std::time::Duration::from_secs(3);

    /// A message shown from now.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            until: std::time::Instant::now() + Self::LIFETIME,
        }
    }

    /// Whether it should still be shown.
    #[must_use]
    pub fn is_live(&self) -> bool {
        std::time::Instant::now() < self.until
    }
}

/// The application's state.
#[derive(Debug)]
pub struct Model {
    /// The folder being browsed, if any.
    pub folder: Option<Folder>,
    /// The image on screen, if any.
    pub loaded: Option<Loaded>,
    /// Where that image is placed.
    pub view: ViewState,
    /// The most recent failure, shown until dismissed or superseded.
    pub error: Option<String>,
    /// Set when a new image arrives, so the next frame can fit it to a viewport
    /// whose size is only known during drawing.
    pub needs_fit: bool,
    /// Bumped whenever the displayed image changes, so the drawing layer knows
    /// its cached texture is stale.
    pub generation: u64,
    /// The viewport used by the last frame, so keyboard actions that need to
    /// know the window size can work before the next draw.
    pub viewport: Rect,
    /// True while the selected file is still being decoded.
    pub loading: bool,
    /// A confirmation or rename waiting on the user.
    pub pending: Pending,
    /// When on, even unflagged files ask before being deleted.
    pub confirm_every_delete: bool,
    /// Whether the shortcut overlay is showing.
    pub show_help: bool,
    /// The most recent transient message.
    pub toast: Option<Toast>,
    /// Decodes images off the UI thread and caches the results.
    loader: Loader,
    /// How files are deleted and renamed. Swapped in tests so that nothing
    /// reaches the real trash.
    ops: Box<dyn FileOps + Send>,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Model {
    /// A model with nothing loaded.
    ///
    /// The viewport starts at the window's default size so that actions
    /// arriving before the first frame, such as opening a file named on the
    /// command line, have something sensible to fit against.
    #[must_use]
    pub fn new() -> Self {
        Self {
            folder: None,
            loaded: None,
            view: ViewState::default(),
            error: None,
            needs_fit: false,
            generation: 0,
            viewport: Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0)),
            loading: false,
            pending: Pending::None,
            confirm_every_delete: false,
            show_help: false,
            toast: None,
            loader: Loader::default(),
            ops: Box::new(RealFileOps),
        }
    }

    /// A model that performs file operations through `ops`.
    ///
    /// Tests use this so that deleting never reaches the real trash.
    #[must_use]
    pub fn with_file_ops(ops: Box<dyn FileOps + Send>) -> Self {
        Self { ops, ..Self::new() }
    }

    /// Applies an action.
    ///
    /// Actions that cannot be satisfied, such as zooming with no image loaded,
    /// are ignored rather than treated as errors.
    pub fn handle(&mut self, action: Action) {
        match action {
            Action::Open(path) => self.open(&path),
            Action::Select(index) => self.move_selection(|f| f.select(index)),
            Action::NextFile => self.move_selection(Folder::select_next),
            Action::PreviousFile => self.move_selection(Folder::select_previous),
            Action::FirstFile => self.move_selection(Folder::select_first),
            Action::LastFile => self.move_selection(Folder::select_last),
            Action::Rescan => self.rescan(),
            Action::FitToWindow => {
                if let Some(l) = &self.loaded {
                    self.view = ViewState::fit(l.size(), self.viewport);
                }
            }
            Action::ActualSize => {
                if self.loaded.is_some() {
                    let viewport = self.viewport;
                    self.view.set_zoom_about_centre(viewport, 1.0);
                }
            }
            Action::Pan(delta) => {
                if self.loaded.is_some() {
                    self.view.pan(delta);
                }
            }
            Action::ZoomAt { anchor, factor } => {
                if self.loaded.is_some() {
                    self.view.zoom_about(anchor, factor);
                }
            }
            Action::ToggleFlag => self.toggle_flag(),
            Action::RequestDelete => self.request_delete(),
            Action::ConfirmDelete => self.delete_now(),
            Action::BeginRename => self.begin_rename(),
            Action::RenameTextChanged(text) => self.check_rename(&text),
            Action::CommitRename(text) => self.commit_rename(&text),
            Action::Cancel => self.pending = Pending::None,
            Action::ToggleConfirmEveryDelete => {
                self.confirm_every_delete = !self.confirm_every_delete;
            }
            Action::ToggleHelp => self.show_help = !self.show_help,
            Action::ClearError => self.error = None,
        }
    }

    /// Turns the selected file's keep flag on or off and records it.
    fn toggle_flag(&mut self) {
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        let Some(Outcome::Flagged { name, flagged }) = actions::toggle_flag(folder) else {
            return;
        };
        if let Err(e) = actions::save_flags(folder) {
            // The flag is set in memory either way; say so rather than
            // pretending it will survive a restart.
            self.error = Some(format!("Could not save flags: {e}"));
        }
        self.toast = Some(Toast::new(if flagged {
            format!("Keeping {name}")
        } else {
            format!("No longer keeping {name}")
        }));
    }

    /// Deletes, or asks first when the file is flagged or the user has asked to
    /// be asked every time.
    fn request_delete(&mut self) {
        let Some(folder) = self.folder.as_ref() else {
            return;
        };
        let Some(entry) = folder.selected_entry() else {
            return;
        };
        if actions::needs_delete_confirmation(folder, self.confirm_every_delete) {
            self.pending = Pending::DeleteConfirm {
                name: entry.name.clone(),
                flagged: entry.flagged,
            };
        } else {
            self.delete_now();
        }
    }

    /// Performs the delete. The confirmation, if any, has already happened.
    fn delete_now(&mut self) {
        self.pending = Pending::None;
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        match actions::delete_selected(folder, self.ops.as_ref()) {
            Ok(Outcome::Deleted { name }) => {
                let hint = if actions::undo_supported() {
                    " — restore it from the trash"
                } else {
                    ""
                };
                self.toast = Some(Toast::new(format!("Deleted {name}{hint}")));
                self.error = None;
                self.after_list_changed();
            }
            Ok(_) => {}
            Err(e) => self.error = Some(format!("Could not delete: {e}")),
        }
    }

    /// Opens the rename editor with the current name in it.
    fn begin_rename(&mut self) {
        let Some(name) = self
            .folder
            .as_ref()
            .and_then(Folder::selected_entry)
            .map(|e| e.name.clone())
        else {
            return;
        };
        self.pending = Pending::Rename {
            text: name,
            problem: None,
        };
    }

    /// Checks the typed name and reports what is wrong with it, if anything.
    ///
    /// Called as the user types so the problem appears before they commit.
    pub fn check_rename(&mut self, text: &str) {
        let problem = self.folder.as_ref().and_then(|f| {
            actions::validate_new_name(f, self.ops.as_ref(), text)
                .err()
                .map(|e| e.to_string())
        });
        self.pending = Pending::Rename {
            text: text.to_string(),
            problem,
        };
    }

    /// Applies a rename, keeping the editor open if the name is unusable.
    fn commit_rename(&mut self, text: &str) {
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        match actions::rename_selected(folder, self.ops.as_ref(), text) {
            Ok(Outcome::Renamed { from, to }) => {
                self.pending = Pending::None;
                if let Err(e) = actions::save_flags(folder) {
                    self.error = Some(format!("Could not save flags: {e}"));
                }
                self.toast = Some(Toast::new(format!("Renamed {from} to {to}")));
                self.after_list_changed();
            }
            Ok(_) => self.pending = Pending::None,
            Err(ActionError::Rename(e)) => {
                // Keep the editor open so the user can correct the name.
                self.pending = Pending::Rename {
                    text: text.to_string(),
                    problem: Some(e.to_string()),
                };
            }
            Err(e) => {
                self.pending = Pending::None;
                self.error = Some(format!("Could not rename: {e}"));
            }
        }
    }

    /// Brings the display back in step after the file list changes.
    fn after_list_changed(&mut self) {
        if self
            .folder
            .as_ref()
            .is_some_and(|f| f.selected_path().is_none())
        {
            self.loaded = None;
            self.loading = false;
            return;
        }
        self.show_selection();
    }

    /// Drops the toast once its time is up. Called each frame.
    pub fn expire_toast(&mut self) {
        if self.toast.as_ref().is_some_and(|t| !t.is_live()) {
            self.toast = None;
        }
    }

    /// Opens a folder, or a file together with the folder containing it.
    ///
    /// Opening a single file still loads its folder, so the arrow keys work
    /// immediately without the user having to open the folder separately.
    fn open(&mut self, path: &Path) {
        let (dir, wanted) = if path.is_dir() {
            (path.to_path_buf(), None)
        } else {
            match path.parent() {
                Some(parent) => (parent.to_path_buf(), Some(path.to_path_buf())),
                None => {
                    self.error = Some(format!("{} has no parent folder", path.display()));
                    return;
                }
            }
        };

        match scan_folder(&dir) {
            Ok(mut folder) => {
                if let Some(w) = &wanted {
                    if !folder.select_path(w) {
                        // The file exists but the scan filtered it out, most
                        // likely a non-FITS file dropped on the window.
                        self.error = Some(format!(
                            "{} is not a FITS file",
                            w.file_name().unwrap_or(w.as_os_str()).to_string_lossy()
                        ));
                    }
                }
                if folder.is_empty() {
                    self.error = Some(format!("No FITS files in {}", dir.display()));
                }
                self.loader.reset();
                self.folder = Some(folder);
                self.show_selection();
            }
            Err(e) => {
                log::warn!("could not scan {}: {e}", dir.display());
                self.error = Some(format!("{}: {e}", dir.display()));
            }
        }
    }

    /// Re-reads the folder, keeping the selection on the same file if it is
    /// still there.
    fn rescan(&mut self) {
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        match folder.rescan() {
            Ok(()) => {
                // Cached images may be stale, and files may have gone.
                self.loader.reset();
                self.show_selection();
            }
            Err(e) => self.error = Some(format!("Rescan failed: {e}")),
        }
    }

    /// Applies a selection change and displays whatever it lands on.
    fn move_selection(&mut self, change: impl FnOnce(&mut Folder)) {
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        let before = folder.selected;
        change(folder);
        if folder.selected != before {
            self.show_selection();
        }
    }

    /// Displays the selected file, from cache if possible, and asks the loader
    /// for it and its neighbours otherwise.
    fn show_selection(&mut self) {
        // Take what is needed from the folder up front, so the borrow ends
        // before anything mutates the model.
        let Some((path, wanted)) = self
            .folder
            .as_ref()
            .map(|f| (f.selected_path().map(Path::to_path_buf), f.prefetch_paths()))
        else {
            return;
        };
        let Some(path) = path else {
            self.loaded = None;
            self.loading = false;
            return;
        };

        // A cached image is shown at once, so stepping back and forth through
        // a folder never flickers.
        if let Some(image) = self.loader.cache_mut().get(&path) {
            self.display(path, image, None);
            self.loading = false;
        } else {
            self.loading = true;
        }

        self.loader.request(&wanted);
    }

    /// Collects finished loads. Call once per frame.
    ///
    /// Returns true if anything changed, so the caller knows to repaint.
    pub fn poll(&mut self) -> bool {
        let arrivals = self.loader.poll();
        if arrivals.is_empty() {
            return false;
        }

        let selected = self
            .folder
            .as_ref()
            .and_then(|f| f.selected_path())
            .map(Path::to_path_buf);

        let mut changed = false;
        for arrival in arrivals {
            let is_selected = selected.as_deref() == Some(arrival.path.as_path());
            match arrival.result {
                Ok(image) => {
                    if is_selected {
                        self.display(arrival.path, image, Some(arrival.millis));
                        self.loading = false;
                        changed = true;
                    }
                }
                Err(message) => {
                    // Only complain about the file the user is looking at. A
                    // prefetch that fails will be reported when they reach it.
                    if is_selected {
                        log::warn!("could not load {}: {message}", arrival.path.display());
                        self.error = Some(format!("{}: {message}", arrival.path.display()));
                        self.loading = false;
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// Puts an image on screen.
    fn display(&mut self, path: PathBuf, image: Arc<FitsImage>, millis: Option<f64>) {
        let load_ms = millis.unwrap_or(0.0);
        self.loaded = Some(Loaded {
            path,
            image,
            load_ms,
        });
        self.error = None;
        self.needs_fit = true;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Records the viewport and, if an image has just arrived, fits it.
    ///
    /// Called once per frame by the drawing layer, which is the only place the
    /// real viewport size is known.
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
        if self.needs_fit {
            if let Some(l) = &self.loaded {
                self.view = ViewState::fit(l.size(), viewport);
                self.needs_fit = false;
            }
        }
    }

    /// Text for the status bar.
    #[must_use]
    pub fn status_text(&self) -> String {
        if let Some(e) = &self.error {
            return format!("Error: {e}");
        }
        if let Some(l) = &self.loaded {
            return format!("{}  {:.0}%", l.summary(), self.view.zoom * 100.0);
        }
        if self.loading {
            return "Loading…".to_string();
        }
        "No image. Use Open File or Open Folder, or drop a FITS file on the window.".to_string()
    }

    /// Cached images and the bytes they occupy.
    ///
    /// Exposed so that the memory bound can be asserted in tests and shown in
    /// a diagnostic panel later, rather than being taken on trust.
    #[must_use]
    pub fn cache_stats(&self) -> (usize, usize) {
        (self.loader.cache().len(), self.loader.cache().bytes())
    }

    /// The `3 / 142` position label, empty when no folder is open.
    #[must_use]
    pub fn position_label(&self) -> String {
        self.folder
            .as_ref()
            .map(Folder::position_label)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::testutil::{write_synthetic, SyntheticSpec};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// Writes `count` numbered files of the given size into a fresh folder.
    fn folder_of(count: usize, width: usize, height: usize) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(width, height, 16);
        let pixels: Vec<f64> = (0..width * height).map(|i| (i % 1000) as f64).collect();
        for i in 1..=count {
            write_synthetic(dir.path(), &format!("light_{i}.fits"), &spec, &pixels).unwrap();
        }
        dir
    }

    /// A single file in its own folder.
    fn sample(width: usize, height: usize) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(width, height, 16);
        let pixels: Vec<f64> = (0..width * height).map(|i| (i % 1000) as f64).collect();
        let path = write_synthetic(dir.path(), "light.fits", &spec, &pixels).unwrap();
        (dir, path)
    }

    /// Pumps the model until it settles: an image is shown, or an error is
    /// recorded, or nothing is outstanding. Loading is asynchronous now, so
    /// tests must wait rather than assume.
    fn settle(model: &mut Model) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            model.poll();
            if !model.loading && (model.loaded.is_some() || model.error.is_some()) {
                return;
            }
            if !model.loading && model.folder.as_ref().is_some_and(Folder::is_empty) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!(
            "model never settled: loading={}, error={:?}",
            model.loading, model.error
        );
    }

    fn viewport() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))
    }

    #[test]
    fn a_new_model_has_nothing_loaded() {
        let m = Model::new();
        assert!(m.loaded.is_none());
        assert!(m.folder.is_none());
        assert!(m.error.is_none());
        assert!(m.status_text().contains("No image"));
        assert_eq!(m.position_label(), "");
    }

    #[test]
    fn opening_a_file_also_opens_its_folder() {
        // So that the arrow keys work without opening the folder separately.
        let dir = folder_of(3, 20, 15);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().join("light_2.fits")));
        settle(&mut m);

        let folder = m.folder.as_ref().expect("folder should be open");
        assert_eq!(folder.len(), 3);
        assert_eq!(folder.selected_entry().unwrap().name, "light_2.fits");
        assert_eq!(m.position_label(), "2 / 3");
        assert_eq!(m.loaded.as_ref().unwrap().image.width, 20);
    }

    #[test]
    fn opening_a_folder_selects_its_first_file() {
        let dir = folder_of(4, 10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        assert_eq!(m.position_label(), "1 / 4");
        assert_eq!(
            m.folder.as_ref().unwrap().selected_entry().unwrap().name,
            "light_1.fits"
        );
        assert!(m.loaded.is_some());
    }

    #[test]
    fn a_folder_with_no_fits_files_reports_that_rather_than_failing_silently() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"nothing here").unwrap();

        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        assert!(m.error.is_some(), "expected an explanatory error");
        assert!(m.error.as_ref().unwrap().contains("No FITS files"));
        assert!(m.loaded.is_none());
    }

    #[test]
    fn opening_a_missing_path_does_not_panic() {
        let mut m = Model::new();
        m.handle(Action::Open(PathBuf::from("/nowhere/at/all.fits")));
        assert!(m.error.is_some());
        assert!(m.loaded.is_none());
    }

    #[test]
    fn opening_a_corrupt_file_reports_it() {
        let dir = folder_of(1, 10, 10);
        let bad = dir.path().join("broken.fits");
        std::fs::write(&bad, b"SIMPLE but not really a fits file").unwrap();

        let mut m = Model::new();
        m.handle(Action::Open(bad));
        settle(&mut m);
        assert!(m.error.is_some(), "a corrupt file should be reported");
        assert!(m.loaded.is_none());
    }

    #[test]
    fn arrow_navigation_moves_through_the_folder_and_stops_at_the_ends() {
        let dir = folder_of(3, 10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        m.handle(Action::PreviousFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 3", "must not wrap backwards");

        m.handle(Action::NextFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "2 / 3");

        m.handle(Action::LastFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "3 / 3");

        m.handle(Action::NextFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "3 / 3", "must not wrap forwards");

        m.handle(Action::FirstFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 3");
    }

    #[test]
    fn every_file_in_a_folder_can_be_stepped_through() {
        let dir = folder_of(6, 12, 9);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        let mut seen = Vec::new();
        for _ in 0..6 {
            seen.push(
                m.loaded
                    .as_ref()
                    .unwrap()
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
            m.handle(Action::NextFile);
            settle(&mut m);
        }
        assert_eq!(
            seen,
            vec![
                "light_1.fits",
                "light_2.fits",
                "light_3.fits",
                "light_4.fits",
                "light_5.fits",
                "light_6.fits"
            ]
        );
    }

    #[test]
    fn selecting_by_index_shows_that_file() {
        let dir = folder_of(5, 10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        m.handle(Action::Select(3));
        settle(&mut m);
        assert_eq!(m.position_label(), "4 / 5");

        // Out of range is ignored rather than clearing the view.
        m.handle(Action::Select(99));
        settle(&mut m);
        assert_eq!(m.position_label(), "4 / 5");
    }

    #[test]
    fn stepping_back_to_a_visited_file_is_served_from_cache_without_reloading() {
        // The prefetch and cache exist so that going back is instant. If the
        // image were reloaded, `loading` would be set on the way back.
        let dir = folder_of(3, 40, 30);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        m.handle(Action::NextFile);
        settle(&mut m);
        m.handle(Action::PreviousFile);
        assert!(
            !m.loading,
            "returning to a cached image should not need loading"
        );
        assert_eq!(m.position_label(), "1 / 3");
        assert!(m.loaded.is_some());
    }

    #[test]
    fn navigation_does_nothing_when_no_folder_is_open() {
        let mut m = Model::new();
        for a in [
            Action::NextFile,
            Action::PreviousFile,
            Action::FirstFile,
            Action::LastFile,
            Action::Select(0),
            Action::Rescan,
        ] {
            m.handle(a);
        }
        assert!(m.folder.is_none());
        assert!(m.loaded.is_none());
    }

    #[test]
    fn rescan_picks_up_a_new_file() {
        let dir = folder_of(2, 10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 2");

        let spec = SyntheticSpec::new(10, 10, 16);
        write_synthetic(dir.path(), "light_3.fits", &spec, &vec![1.0; 100]).unwrap();

        m.handle(Action::Rescan);
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 3");
    }

    #[test]
    fn rescan_keeps_the_selection_on_the_same_file() {
        let dir = folder_of(3, 10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);
        m.handle(Action::LastFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "3 / 3");

        m.handle(Action::Rescan);
        settle(&mut m);
        assert_eq!(
            m.folder.as_ref().unwrap().selected_entry().unwrap().name,
            "light_3.fits"
        );
    }

    #[test]
    fn opening_a_different_folder_replaces_the_first() {
        let a = folder_of(2, 10, 10);
        let b = folder_of(5, 10, 10);
        let mut m = Model::new();

        m.handle(Action::Open(a.path().to_path_buf()));
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 2");

        m.handle(Action::Open(b.path().to_path_buf()));
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 5");
        assert_eq!(m.folder.as_ref().unwrap().dir, b.path());
    }

    #[test]
    fn the_first_frame_fits_the_image_to_the_viewport() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(2000, 1000, 16);
        write_synthetic(dir.path(), "big.fits", &spec, &vec![1.0; 2_000_000]).unwrap();

        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);
        m.set_viewport(viewport());

        assert!(!m.needs_fit, "fit should be consumed");
        assert!((m.view.zoom - 0.5).abs() < 0.001, "zoom {}", m.view.zoom);
    }

    #[test]
    fn later_frames_do_not_refit_and_undo_the_users_zoom() {
        let (_dir, path) = sample(2000, 1000);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        m.set_viewport(viewport());

        m.handle(Action::ZoomAt {
            anchor: Pos2::new(500.0, 400.0),
            factor: 2.0,
        });
        let zoomed = m.view.zoom;

        m.set_viewport(viewport());
        assert!(
            (m.view.zoom - zoomed).abs() < f32::EPSILON,
            "a redraw must not reset the view"
        );
    }

    #[test]
    fn moving_to_another_file_refits_it() {
        let dir = tempfile::tempdir().unwrap();
        write_synthetic(
            dir.path(),
            "a_small.fits",
            &SyntheticSpec::new(100, 100, 16),
            &vec![1.0; 10_000],
        )
        .unwrap();
        write_synthetic(
            dir.path(),
            "b_large.fits",
            &SyntheticSpec::new(4000, 2000, 16),
            &vec![1.0; 8_000_000],
        )
        .unwrap();

        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);
        m.set_viewport(viewport());
        assert!((m.view.zoom - 1.0).abs() < 0.001, "small image at 1:1");

        m.handle(Action::NextFile);
        settle(&mut m);
        m.set_viewport(viewport());
        assert!((m.view.zoom - 0.25).abs() < 0.001, "zoom {}", m.view.zoom);
    }

    #[test]
    fn actions_are_ignored_when_no_image_is_loaded() {
        let mut m = Model::new();
        let before = m.view;
        m.handle(Action::FitToWindow);
        m.handle(Action::ActualSize);
        m.handle(Action::Pan(Vec2::new(10.0, 10.0)));
        m.handle(Action::ZoomAt {
            anchor: Pos2::ZERO,
            factor: 2.0,
        });
        assert_eq!(m.view, before, "view must not move with nothing loaded");
    }

    #[test]
    fn actual_size_sets_one_to_one_zoom() {
        let (_dir, path) = sample(2000, 1000);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        m.set_viewport(viewport());
        assert!(m.view.zoom < 1.0);

        m.handle(Action::ActualSize);
        assert!((m.view.zoom - 1.0).abs() < 0.001, "zoom {}", m.view.zoom);
    }

    #[test]
    fn fit_after_zooming_returns_to_the_fitted_view() {
        let (_dir, path) = sample(2000, 1000);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        m.set_viewport(viewport());
        let fitted = m.view;

        m.handle(Action::ZoomAt {
            anchor: Pos2::new(100.0, 100.0),
            factor: 4.0,
        });
        m.handle(Action::Pan(Vec2::new(50.0, -30.0)));
        assert_ne!(m.view, fitted);

        m.handle(Action::FitToWindow);
        assert!((m.view.zoom - fitted.zoom).abs() < 0.001);
        assert!((m.view.origin.x - fitted.origin.x).abs() < 0.001);
        assert!((m.view.origin.y - fitted.origin.y).abs() < 0.001);
    }

    #[test]
    fn clearing_the_error_leaves_the_image_alone() {
        let (_dir, path) = sample(10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        m.error = Some("something".into());
        m.handle(Action::ClearError);
        assert!(m.error.is_none());
        assert!(m.loaded.is_some());
    }

    #[test]
    fn the_status_line_reports_size_bit_depth_and_load_time() {
        let (_dir, path) = sample(40, 30);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        let s = m.status_text();
        assert!(s.contains("light.fits"), "{s}");
        assert!(s.contains("40x30"), "{s}");
        assert!(s.contains("BITPIX 16"), "{s}");
        assert!(s.contains("ms"), "{s}");
    }

    #[test]
    fn the_status_line_reports_the_zoom_level() {
        let (_dir, path) = sample(100, 100);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        m.set_viewport(viewport());
        m.handle(Action::ActualSize);
        assert!(m.status_text().contains("100%"), "{}", m.status_text());
    }

    #[test]
    fn a_colour_image_is_labelled_as_rgb() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(4, 4, 16).with_channels(3);
        let path = write_synthetic(dir.path(), "rgb.fits", &spec, &vec![1.0; 48]).unwrap();
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        assert!(m.status_text().contains("RGB"), "{}", m.status_text());
    }

    /// A model whose file operations are recorded rather than performed, so
    /// that tests never reach the real trash.
    #[derive(Debug, Default)]
    struct SpyOps {
        trashed: std::sync::Mutex<Vec<PathBuf>>,
    }

    impl crate::actions::FileOps for std::sync::Arc<SpyOps> {
        fn trash(&self, path: &Path) -> Result<(), String> {
            self.trashed.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
        fn rename(&self, from: &Path, to: &Path) -> Result<(), String> {
            std::fs::rename(from, to).map_err(|e| e.to_string())
        }
        fn exists(&self, path: &Path) -> bool {
            path.exists()
        }
    }

    /// A model over `dir` that records deletes instead of performing them.
    fn model_over(dir: &Path) -> (Model, std::sync::Arc<SpyOps>) {
        let spy = std::sync::Arc::new(SpyOps::default());
        let mut m = Model::with_file_ops(Box::new(std::sync::Arc::clone(&spy)));
        m.handle(Action::Open(dir.to_path_buf()));
        settle(&mut m);
        (m, spy)
    }

    #[test]
    fn flagging_marks_the_file_and_survives_reopening_the_folder() {
        let dir = folder_of(3, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::NextFile);
        settle(&mut m);
        m.handle(Action::ToggleFlag);

        assert!(m.folder.as_ref().unwrap().files[1].flagged);
        assert!(m.toast.is_some(), "the user should be told");

        // Reopen, as though the application had been restarted.
        let (m2, _spy2) = model_over(dir.path());
        assert!(
            m2.folder.as_ref().unwrap().files[1].flagged,
            "the flag must survive a restart"
        );
        assert!(!m2.folder.as_ref().unwrap().files[0].flagged);
    }

    #[test]
    fn unflagging_is_persisted_too() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::ToggleFlag);
        m.handle(Action::ToggleFlag);

        let (m2, _spy2) = model_over(dir.path());
        assert!(!m2.folder.as_ref().unwrap().files[0].flagged);
    }

    #[test]
    fn deleting_an_unflagged_file_happens_at_once_with_no_dialog() {
        let dir = folder_of(3, 10, 10);
        let (mut m, spy) = model_over(dir.path());

        m.handle(Action::RequestDelete);
        assert_eq!(m.pending, Pending::None, "no dialog for an unflagged file");
        assert_eq!(spy.trashed.lock().unwrap().len(), 1);
        assert_eq!(m.position_label(), "1 / 2", "selection should advance");
        settle(&mut m);
        assert!(m.loaded.is_some(), "the next image should be shown");
    }

    #[test]
    fn deleting_a_flagged_file_always_asks_first() {
        let dir = folder_of(2, 10, 10);
        let (mut m, spy) = model_over(dir.path());
        m.handle(Action::ToggleFlag);

        m.handle(Action::RequestDelete);
        match &m.pending {
            Pending::DeleteConfirm { name, flagged } => {
                assert_eq!(name, "light_1.fits");
                assert!(flagged);
            }
            other => panic!("expected a confirmation, got {other:?}"),
        }
        assert!(
            spy.trashed.lock().unwrap().is_empty(),
            "nothing may be deleted before the user confirms"
        );
    }

    #[test]
    fn cancelling_the_confirmation_keeps_the_file() {
        let dir = folder_of(2, 10, 10);
        let (mut m, spy) = model_over(dir.path());
        m.handle(Action::ToggleFlag);
        m.handle(Action::RequestDelete);

        m.handle(Action::Cancel);
        assert_eq!(m.pending, Pending::None);
        assert!(spy.trashed.lock().unwrap().is_empty());
        assert_eq!(m.position_label(), "1 / 2", "the file is still there");
    }

    #[test]
    fn confirming_deletes_the_flagged_file() {
        let dir = folder_of(2, 10, 10);
        let (mut m, spy) = model_over(dir.path());
        m.handle(Action::ToggleFlag);
        m.handle(Action::RequestDelete);

        m.handle(Action::ConfirmDelete);
        assert_eq!(m.pending, Pending::None);
        assert_eq!(spy.trashed.lock().unwrap().len(), 1);
        assert_eq!(m.position_label(), "1 / 1");
    }

    #[test]
    fn confirm_every_delete_asks_even_for_unflagged_files() {
        let dir = folder_of(2, 10, 10);
        let (mut m, spy) = model_over(dir.path());
        m.handle(Action::ToggleConfirmEveryDelete);
        assert!(m.confirm_every_delete);

        m.handle(Action::RequestDelete);
        assert!(matches!(m.pending, Pending::DeleteConfirm { .. }));
        assert!(spy.trashed.lock().unwrap().is_empty());
    }

    #[test]
    fn deleting_the_last_remaining_file_clears_the_view() {
        let dir = folder_of(1, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::RequestDelete);
        assert_eq!(m.position_label(), "0 / 0");
        assert!(m.loaded.is_none(), "nothing left to show");
        assert!(!m.loading);
    }

    #[test]
    fn a_delete_that_fails_is_reported_and_changes_nothing() {
        #[derive(Debug)]
        struct AlwaysFails;
        impl crate::actions::FileOps for AlwaysFails {
            fn trash(&self, _: &Path) -> Result<(), String> {
                Err("permission denied".into())
            }
            fn rename(&self, _: &Path, _: &Path) -> Result<(), String> {
                Err("permission denied".into())
            }
            fn exists(&self, _: &Path) -> bool {
                false
            }
        }

        let dir = folder_of(2, 10, 10);
        let mut m = Model::with_file_ops(Box::new(AlwaysFails));
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        m.handle(Action::RequestDelete);
        assert!(m.error.is_some(), "the failure must be surfaced");
        assert_eq!(m.position_label(), "1 / 2", "the file is still listed");
    }

    #[test]
    fn renaming_moves_the_file_on_disk_and_keeps_it_selected() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::BeginRename);
        match &m.pending {
            Pending::Rename { text, problem } => {
                assert_eq!(text, "light_1.fits", "editor starts with the current name");
                assert!(problem.is_none());
            }
            other => panic!("expected the rename editor, got {other:?}"),
        }

        m.handle(Action::CommitRename("m31_first.fits".into()));
        assert_eq!(m.pending, Pending::None);
        assert!(dir.path().join("m31_first.fits").exists());
        assert!(!dir.path().join("light_1.fits").exists());
        assert_eq!(
            m.folder.as_ref().unwrap().selected_entry().unwrap().name,
            "m31_first.fits"
        );
    }

    #[test]
    fn a_rename_that_clashes_keeps_the_editor_open_with_a_reason() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::BeginRename);
        m.handle(Action::CommitRename("light_2.fits".into()));

        match &m.pending {
            Pending::Rename { problem, .. } => {
                let problem = problem.as_ref().expect("should explain the clash");
                assert!(problem.contains("already exists"), "{problem}");
            }
            other => panic!("the editor should stay open, got {other:?}"),
        }
        assert!(dir.path().join("light_1.fits").exists(), "nothing moved");
    }

    #[test]
    fn typing_an_invalid_name_reports_it_before_committing() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::BeginRename);

        m.handle(Action::RenameTextChanged("sub/dir.fits".into()));
        match &m.pending {
            Pending::Rename { problem, .. } => {
                assert!(problem.is_some(), "should complain about the separator");
            }
            other => panic!("expected the editor, got {other:?}"),
        }

        m.handle(Action::RenameTextChanged("fine.fits".into()));
        match &m.pending {
            Pending::Rename { problem, .. } => assert!(problem.is_none()),
            other => panic!("expected the editor, got {other:?}"),
        }
    }

    #[test]
    fn cancelling_a_rename_leaves_the_file_alone() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::BeginRename);
        m.handle(Action::Cancel);

        assert_eq!(m.pending, Pending::None);
        assert!(dir.path().join("light_1.fits").exists());
    }

    #[test]
    fn a_renamed_file_keeps_its_flag_across_a_restart() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::ToggleFlag);
        m.handle(Action::BeginRename);
        m.handle(Action::CommitRename("keeper.fits".into()));

        let (m2, _spy2) = model_over(dir.path());
        let entry = m2
            .folder
            .as_ref()
            .unwrap()
            .files
            .iter()
            .find(|e| e.name == "keeper.fits")
            .expect("renamed file should be listed");
        assert!(entry.flagged, "the flag must follow the new name");
    }

    #[test]
    fn the_help_overlay_toggles() {
        let mut m = Model::new();
        assert!(!m.show_help);
        m.handle(Action::ToggleHelp);
        assert!(m.show_help);
        m.handle(Action::ToggleHelp);
        assert!(!m.show_help);
    }

    #[test]
    fn file_actions_do_nothing_without_a_folder() {
        let mut m = Model::new();
        for a in [
            Action::ToggleFlag,
            Action::RequestDelete,
            Action::ConfirmDelete,
            Action::BeginRename,
            Action::CommitRename("x.fits".into()),
        ] {
            m.handle(a);
        }
        assert!(m.folder.is_none());
        assert_eq!(m.pending, Pending::None);
    }

    #[test]
    fn a_toast_expires_on_its_own() {
        let mut m = Model::new();
        m.toast = Some(Toast {
            text: "gone".into(),
            until: std::time::Instant::now() - Duration::from_secs(1),
        });
        m.expire_toast();
        assert!(m.toast.is_none());

        m.toast = Some(Toast::new("still here"));
        m.expire_toast();
        assert!(m.toast.is_some());
    }

    #[test]
    fn polling_with_nothing_outstanding_reports_no_change() {
        let mut m = Model::new();
        assert!(!m.poll());
    }
}

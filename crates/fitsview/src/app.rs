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
    /// Dismiss the current error message.
    ClearError,
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
    /// Decodes images off the UI thread and caches the results.
    loader: Loader,
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
            loader: Loader::default(),
        }
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
            Action::ClearError => self.error = None,
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

    #[test]
    fn polling_with_nothing_outstanding_reports_no_change() {
        let mut m = Model::new();
        assert!(!m.poll());
    }
}

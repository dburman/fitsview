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
use std::time::Instant;

use egui::{Pos2, Rect, Vec2};
use fits_core::{read_fits, FitsImage};

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
    /// Load and display a file.
    Open(PathBuf),
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
            loaded: None,
            view: ViewState::default(),
            error: None,
            needs_fit: false,
            generation: 0,
            viewport: Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0)),
        }
    }

    /// Applies an action.
    ///
    /// Actions that cannot be satisfied, such as zooming with no image loaded,
    /// are ignored rather than treated as errors.
    pub fn handle(&mut self, action: Action) {
        match action {
            Action::Open(path) => self.open(&path),
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

    /// Loads a file, replacing whatever is displayed.
    ///
    /// A failure leaves the previous image on screen and records the error, so
    /// that opening a corrupt file does not also lose the user's place.
    fn open(&mut self, path: &Path) {
        let started = Instant::now();
        match read_fits(path) {
            Ok(image) => {
                #[allow(clippy::cast_precision_loss)]
                let load_ms = started.elapsed().as_secs_f64() * 1000.0;
                self.loaded = Some(Loaded {
                    path: path.to_path_buf(),
                    image: Arc::new(image),
                    load_ms,
                });
                self.error = None;
                self.needs_fit = true;
                self.generation = self.generation.wrapping_add(1);
            }
            Err(e) => {
                log::warn!("could not open {}: {e}", path.display());
                self.error = Some(format!("{}: {e}", path.display()));
            }
        }
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
        match &self.loaded {
            Some(l) => format!("{}  {:.0}%", l.summary(), self.view.zoom * 100.0),
            None => "No image. Use Open File, or drop a FITS file on the window.".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::testutil::{write_synthetic, SyntheticSpec};
    use tempfile::TempDir;

    /// Writes a synthetic file and returns the directory holding it, which must
    /// stay alive for the path to remain valid.
    fn sample(width: usize, height: usize) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(width, height, 16);
        let pixels: Vec<f64> = (0..width * height).map(|i| (i % 1000) as f64).collect();
        let path = write_synthetic(dir.path(), "light.fits", &spec, &pixels).unwrap();
        (dir, path)
    }

    fn viewport() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))
    }

    #[test]
    fn a_new_model_has_nothing_loaded() {
        let m = Model::new();
        assert!(m.loaded.is_none());
        assert!(m.error.is_none());
        assert!(m.status_text().contains("No image"));
    }

    #[test]
    fn opening_a_file_loads_and_requests_a_fit() {
        let (_dir, path) = sample(40, 30);
        let mut m = Model::new();
        m.handle(Action::Open(path.clone()));

        let loaded = m.loaded.as_ref().expect("image should be loaded");
        assert_eq!(loaded.path, path);
        assert_eq!((loaded.image.width, loaded.image.height), (40, 30));
        assert!(m.needs_fit, "a new image must be fitted on the next frame");
        assert_eq!(m.generation, 1);
        assert!(m.error.is_none());
    }

    #[test]
    fn opening_a_bad_file_records_an_error_and_keeps_the_previous_image() {
        let (dir, good) = sample(10, 10);
        let bad = dir.path().join("broken.fits");
        std::fs::write(&bad, b"SIMPLE but not really a fits file").unwrap();

        let mut m = Model::new();
        m.handle(Action::Open(good.clone()));
        let generation = m.generation;

        m.handle(Action::Open(bad));
        assert!(m.error.is_some(), "expected an error to be recorded");
        assert_eq!(
            m.loaded.as_ref().unwrap().path,
            good,
            "the previous image must stay on screen"
        );
        assert_eq!(m.generation, generation, "a failed open is not a new image");
        assert!(m.status_text().starts_with("Error:"));
    }

    #[test]
    fn opening_a_missing_file_does_not_panic() {
        let mut m = Model::new();
        m.handle(Action::Open(PathBuf::from("/nowhere/at/all.fits")));
        assert!(m.error.is_some());
        assert!(m.loaded.is_none());
    }

    #[test]
    fn clearing_the_error_leaves_the_image_alone() {
        let (_dir, path) = sample(10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        m.error = Some("something".into());
        m.handle(Action::ClearError);
        assert!(m.error.is_none());
        assert!(m.loaded.is_some());
    }

    #[test]
    fn the_first_frame_fits_the_image_to_the_viewport() {
        let (_dir, path) = sample(2000, 1000);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        m.set_viewport(viewport());

        assert!(!m.needs_fit, "fit should be consumed");
        // 2000 wide into 1000 wide is a factor of two.
        assert!((m.view.zoom - 0.5).abs() < 0.001, "zoom {}", m.view.zoom);
    }

    #[test]
    fn later_frames_do_not_refit_and_undo_the_users_zoom() {
        let (_dir, path) = sample(2000, 1000);
        let mut m = Model::new();
        m.handle(Action::Open(path));
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
    fn opening_a_second_image_replaces_the_first_and_refits() {
        let dir = tempfile::tempdir().unwrap();
        let small = write_synthetic(
            dir.path(),
            "small.fits",
            &SyntheticSpec::new(10, 10, 16),
            &vec![1.0; 100],
        )
        .unwrap();
        let large = write_synthetic(
            dir.path(),
            "large.fits",
            &SyntheticSpec::new(4000, 2000, 16),
            &vec![1.0; 8_000_000],
        )
        .unwrap();

        let mut m = Model::new();
        m.handle(Action::Open(small));
        m.set_viewport(viewport());
        assert_eq!(m.generation, 1);

        m.handle(Action::Open(large));
        assert!(m.needs_fit);
        assert_eq!(m.generation, 2);
        m.set_viewport(viewport());
        assert_eq!(m.loaded.as_ref().unwrap().image.width, 4000);
        assert!((m.view.zoom - 0.25).abs() < 0.001, "zoom {}", m.view.zoom);
    }

    #[test]
    fn the_status_line_reports_size_bit_depth_and_load_time() {
        let (_dir, path) = sample(40, 30);
        let mut m = Model::new();
        m.handle(Action::Open(path));
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
        assert!(m.status_text().contains("RGB"), "{}", m.status_text());
    }
}

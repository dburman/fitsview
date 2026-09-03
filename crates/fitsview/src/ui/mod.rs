//! The drawing layer.
//!
//! Everything here reads [`crate::app::Model`] and produces
//! [`crate::app::Action`]s. No rules live in this module tree, which is why it
//! carries few tests of its own: the behaviour worth testing lives in
//! [`crate::app`], [`crate::view`], [`crate::texture`] and [`input`].

mod calibration;
mod dialogs;
mod filelist;
mod header;
mod histogram;
pub mod input;
mod toolbar;
mod viewer;

use std::sync::Arc;

use egui::{TextureHandle, TextureOptions, Ui};

/// What the histogram strip needs, computed once per image.
pub struct HistogramView {
    /// The model generation it was built from.
    generation: u64,
    /// The distribution of the samples.
    pub histogram: fits_core::Histogram,
    /// The stretch in force, for marking the black point and midtone.
    pub stretch: Option<fits_core::Stretch>,
}
use fits_core::stretch::StretchParams;

use crate::app::{Action, Model};
use crate::texture::{self, DetailRegion, Mapping};

/// Storage keys for settings that outlive a session.
const KEY_STRETCH_ENABLED: &str = "stretch_enabled";
const KEY_STRETCH_SHADOWS: &str = "stretch_shadows_clip";
const KEY_STRETCH_TARGET: &str = "stretch_target_bg";
const KEY_CONFIRM_EVERY_DELETE: &str = "confirm_every_delete";
const KEY_LAST_FOLDER: &str = "last_folder";
const KEY_SHOW_FILELIST: &str = "show_filelist";
const KEY_SHOW_HEADER: &str = "show_header";
const KEY_SHOW_HISTOGRAM: &str = "show_histogram";
const KEY_STARS: &str = "stars_enabled";

/// The `eframe` application: a model, a cached texture, and the glue between
/// them.
pub struct FitsViewApp {
    model: Model,
    /// The uploaded overview texture, tagged with the model generation it was
    /// built from, so it is rebuilt only when the displayed image changes.
    texture: Option<(u64, TextureHandle)>,
    /// How samples are turned into display bytes, built once per image.
    ///
    /// Shared by the overview texture, the detail texture and the histogram's
    /// marks. Each used to build its own, which measured the image's background
    /// three times over, and again on every pan once zoomed in.
    mapping: Option<(u64, Mapping)>,
    /// The distribution of the displayed image's samples, and where the
    /// stretch puts its black point and midtone.
    ///
    /// Both are rebuilt only when the image changes. Measuring the stretch
    /// costs 12 ms on a full frame, so doing it while drawing would spend most
    /// of a frame's budget on a decoration, every frame.
    histogram: Option<HistogramView>,
    /// A full-resolution texture for the visible part of the image, used once
    /// the zoom passes the point where the overview is being magnified.
    ///
    /// Tagged with the generation and the region, so panning within a tile
    /// reuses it and only a real change rebuilds it.
    detail: Option<(u64, DetailRegion, TextureHandle)>,
}

impl FitsViewApp {
    /// Creates the application, optionally opening a file at startup.
    #[must_use]
    pub fn new(initial: Option<std::path::PathBuf>) -> Self {
        let mut model = Model::new();
        if let Some(path) = initial {
            model.handle(Action::Open(path));
        }
        Self {
            model,
            texture: None,
            detail: None,
            mapping: None,
            histogram: None,
        }
    }

    /// Creates the application, restoring settings saved by a previous session.
    ///
    /// A missing or unreadable setting falls back to its default rather than
    /// failing to start.
    #[must_use]
    pub fn with_storage(
        initial: Option<std::path::PathBuf>,
        storage: Option<&dyn eframe::Storage>,
    ) -> Self {
        let mut app = Self::new(initial);
        let Some(storage) = storage else {
            return app;
        };

        let defaults = StretchParams::default();
        app.model.stretch_enabled =
            eframe::get_value(storage, KEY_STRETCH_ENABLED).unwrap_or(false);
        app.model.stretch_params = StretchParams {
            shadows_clip: eframe::get_value(storage, KEY_STRETCH_SHADOWS)
                .unwrap_or(defaults.shadows_clip),
            target_bg: eframe::get_value(storage, KEY_STRETCH_TARGET).unwrap_or(defaults.target_bg),
        };
        app.model.confirm_every_delete =
            eframe::get_value(storage, KEY_CONFIRM_EVERY_DELETE).unwrap_or(false);
        app.model.show_filelist = eframe::get_value(storage, KEY_SHOW_FILELIST).unwrap_or(true);
        app.model.show_header = eframe::get_value(storage, KEY_SHOW_HEADER).unwrap_or(true);
        app.model.show_histogram = eframe::get_value(storage, KEY_SHOW_HISTOGRAM).unwrap_or(false);

        // Reopen the folder from last time, but only when the command line did
        // not name something, and only if it is still there.
        if app.model.folder.is_none() {
            if let Some(last) = eframe::get_value::<String>(storage, KEY_LAST_FOLDER) {
                let path = std::path::PathBuf::from(last);
                if path.is_dir() {
                    app.model.handle(Action::Open(path));
                    // Reopening is not news, and a folder that has since been
                    // emptied should not greet the user with an error.
                    app.model.error = None;
                    app.model.toast = None;
                }
            }
        }
        app
    }

    /// The current model. Useful to tests and to startup checks.
    #[must_use]
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Rebuilds the texture when the displayed image has changed.
    ///
    /// Uploading is the expensive part of showing a large image, so it happens
    /// once per image rather than once per frame.
    fn sync_texture(&mut self, ui: &Ui) {
        let Some(loaded) = &self.model.loaded else {
            self.texture = None;
            self.detail = None;
            return;
        };
        if self
            .texture
            .as_ref()
            .is_some_and(|(generation, _)| *generation == self.model.generation)
        {
            return;
        }

        let image = &loaded.image;
        let factor =
            texture::downsample_factor(image.width, image.height, texture::MAX_TEXTURE_EDGE);
        if factor > 1 {
            log::debug!(
                "downsampling {}x{} by {factor} for display",
                image.width,
                image.height
            );
        }
        let image = Arc::clone(image);
        let mapping = self.mapping_for(&image);
        let colour = texture::to_color_image(&image, &mapping, factor);
        let handle = ui
            .ctx()
            .load_texture("fits-image", colour, TextureOptions::LINEAR);
        self.texture = Some((self.model.generation, handle));
        // The overview changed, so any detail built from the old one is stale.
        self.detail = None;
    }

    /// The tone mapping for the displayed image, built once and reused.
    fn mapping_for(&mut self, image: &fits_core::FitsImage) -> Mapping {
        if let Some((generation, mapping)) = &self.mapping {
            if *generation == self.model.generation {
                return mapping.clone();
            }
        }
        let mapping = if self.model.stretch_enabled {
            Mapping::stretched(image, &self.model.stretch_params)
        } else {
            Mapping::linear(image)
        };
        self.mapping = Some((self.model.generation, mapping.clone()));
        mapping
    }

    /// Recomputes the histogram when the displayed image changes.
    ///
    /// Counting samples is cheap but not free, and the distribution changes
    /// only when the image does.
    fn sync_histogram(&mut self) {
        if !self.model.show_histogram {
            return;
        }
        let Some(loaded) = &self.model.loaded else {
            self.histogram = None;
            return;
        };
        if self
            .histogram
            .as_ref()
            .is_some_and(|view| view.generation == self.model.generation)
        {
            return;
        }

        // The stretch comes from the mapping the texture already built.
        let image = Arc::clone(&loaded.image);
        let stretch = self.mapping_for(&image).stretch();

        self.histogram = Some(HistogramView {
            generation: self.model.generation,
            histogram: fits_core::histogram::compute(&image),
            stretch,
        });
    }

    /// Uploads the visible part of the image at full resolution, when the
    /// overview is being magnified.
    ///
    /// Without this, "100%" on a full-frame image shows a texture that was
    /// shrunk to fit the size limit and then stretched back out, which is a
    /// blur rather than the image, and defeats the purpose of looking closely
    /// at a frame at all.
    fn sync_detail(&mut self, ui: &Ui) {
        let Some(loaded) = &self.model.loaded else {
            self.detail = None;
            return;
        };
        // A newly opened image has not been fitted yet, so the zoom still holds
        // whatever the previous view had. Asking now would upload a detail
        // texture for a view about to be replaced.
        if self.model.needs_fit {
            self.detail = None;
            return;
        }
        let image = &loaded.image;
        let factor =
            texture::downsample_factor(image.width, image.height, texture::MAX_TEXTURE_EDGE);

        let Some(region) = texture::detail_region_for(
            (image.width, image.height),
            factor,
            &self.model.view,
            self.model.viewport,
        ) else {
            self.detail = None;
            return;
        };

        // Nothing to do while the same region is already uploaded.
        if self
            .detail
            .as_ref()
            .is_some_and(|(g, r, _)| *g == self.model.generation && *r == region)
        {
            return;
        }

        let image = Arc::clone(image);
        let mapping = self.mapping_for(&image);
        let colour = texture::to_color_image_region(&image, &mapping, &region);
        log::debug!(
            "detail texture {}x{} at ({}, {})",
            colour.size[0],
            colour.size[1],
            region.x,
            region.y
        );
        // Nearest, not linear: the point of this texture is to show the pixels
        // as they are, and smoothing them would undo that.
        let handle = ui
            .ctx()
            .load_texture("fits-detail", colour, TextureOptions::NEAREST);
        self.detail = Some((self.model.generation, region, handle));
    }
}

impl eframe::App for FitsViewApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, KEY_STRETCH_ENABLED, &self.model.stretch_enabled);
        eframe::set_value(
            storage,
            KEY_STRETCH_SHADOWS,
            &self.model.stretch_params.shadows_clip,
        );
        eframe::set_value(
            storage,
            KEY_STRETCH_TARGET,
            &self.model.stretch_params.target_bg,
        );
        eframe::set_value(
            storage,
            KEY_CONFIRM_EVERY_DELETE,
            &self.model.confirm_every_delete,
        );
        eframe::set_value(storage, KEY_SHOW_FILELIST, &self.model.show_filelist);
        eframe::set_value(storage, KEY_SHOW_HEADER, &self.model.show_header);
        eframe::set_value(storage, KEY_SHOW_HISTOGRAM, &self.model.show_histogram);
        eframe::set_value(storage, KEY_STARS, &self.model.stars_enabled);
        if let Some(folder) = self.model.folder.as_ref() {
            eframe::set_value(storage, KEY_LAST_FOLDER, &folder.dir.display().to_string());
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        // Collect anything the worker finished since the last frame. Painting
        // continues either way; this never blocks.
        if self.model.poll() {
            ui.ctx().request_repaint();
        }
        // While a decode is outstanding, keep asking for frames so the result
        // appears as soon as it lands rather than on the next input event.
        if self.model.loading {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        }

        for action in toolbar::show(ui, &self.model) {
            self.model.handle(action);
        }
        for action in filelist::show(ui, &self.model) {
            self.model.handle(action);
        }
        for action in calibration::show(ui, &self.model) {
            self.model.handle(action);
        }

        self.sync_histogram();
        let histogram = self.histogram.take();
        for action in histogram::show(ui, &self.model, histogram.as_ref()) {
            self.model.handle(action);
        }
        self.histogram = histogram;

        self.sync_texture(ui);
        self.sync_detail(ui);
        let texture = self.texture.as_ref().map(|(_, t)| t.clone());
        let detail = self
            .detail
            .as_ref()
            .map(|(_, region, handle)| (*region, handle.clone()));

        for action in viewer::show(ui, &mut self.model, texture.as_ref(), detail.as_ref()) {
            self.model.handle(action);
        }
        for action in dialogs::show(ui, &self.model) {
            self.model.handle(action);
        }

        // The scroll request is for the frame that has just been drawn.
        self.model.scroll_to_selection = false;

        // Toasts disappear on their own, so keep painting while one is up.
        self.model.expire_toast();
        if self.model.toast.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A storage back end held in memory, standing in for the file eframe
    /// writes. Lets the save and restore round-trip be tested without a window
    /// or a clean shutdown.
    #[derive(Default)]
    struct MemoryStorage {
        values: HashMap<String, String>,
    }

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.values.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.values.insert(key.to_string(), value);
        }
        fn remove_string(&mut self, key: &str) {
            self.values.remove(key);
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn settings_survive_a_save_and_restore() {
        let mut app = FitsViewApp::new(None);
        app.model.handle(Action::ToggleStretch);
        app.model.handle(Action::SetStretchParams(StretchParams {
            shadows_clip: -1.5,
            target_bg: 0.4,
        }));
        app.model.handle(Action::ToggleConfirmEveryDelete);

        let mut storage = MemoryStorage::default();
        eframe::App::save(&mut app, &mut storage);

        let restored = FitsViewApp::with_storage(None, Some(&storage));
        assert!(
            restored.model.stretch_enabled,
            "the stretch should come back"
        );
        assert!(
            (restored.model.stretch_params.shadows_clip - (-1.5)).abs() < f32::EPSILON,
            "got {}",
            restored.model.stretch_params.shadows_clip
        );
        assert!(
            (restored.model.stretch_params.target_bg - 0.4).abs() < f32::EPSILON,
            "got {}",
            restored.model.stretch_params.target_bg
        );
        assert!(restored.model.confirm_every_delete);
    }

    /// An application showing one small frame, settled and ready to draw.
    fn app_showing_a_frame() -> (tempfile::TempDir, FitsViewApp) {
        use fits_core::testutil::{write_synthetic, SyntheticSpec};
        let dir = tempfile::tempdir().unwrap();
        write_synthetic(
            dir.path(),
            "light.fits",
            &SyntheticSpec::new(32, 32, 16),
            &(0..1024).map(f64::from).collect::<Vec<_>>(),
        )
        .unwrap();
        let mut app = FitsViewApp::new(None);
        app.model.handle(Action::Open(dir.path().to_path_buf()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.model.loaded.is_none() && std::time::Instant::now() < deadline {
            app.model.poll();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        (dir, app)
    }

    #[test]
    fn the_tone_mapping_is_built_once_and_shared() {
        // The overview texture, the detail texture and the histogram all need
        // it. Each building its own measured the image's background three times
        // over, and again on every pan once zoomed in.
        let (_dir, mut app) = app_showing_a_frame();
        app.model.handle(Action::ToggleStretch);

        let image = Arc::clone(&app.model.loaded.as_ref().unwrap().image);
        let first = app.mapping_for(&image);
        assert!(first.stretch().is_some(), "the stretch should be recorded");

        // A second request for the same image must not measure it again.
        let cached = app
            .mapping
            .as_ref()
            .map(|(generation, _)| *generation)
            .expect("it should have been cached");
        assert_eq!(cached, app.model.generation);

        let again = app.mapping_for(&image);
        assert_eq!(
            again.stretch(),
            first.stretch(),
            "the same mapping should come back"
        );
    }

    #[test]
    fn a_changed_image_gets_a_fresh_mapping() {
        let (_dir, mut app) = app_showing_a_frame();
        let image = Arc::clone(&app.model.loaded.as_ref().unwrap().image);
        let _ = app.mapping_for(&image);
        let before = app.mapping.as_ref().unwrap().0;

        app.model.generation += 1;
        let _ = app.mapping_for(&image);
        assert_ne!(
            app.mapping.as_ref().unwrap().0,
            before,
            "a new image must not keep the old mapping"
        );
    }

    #[test]
    fn a_linear_mapping_records_no_stretch_to_mark() {
        let (_dir, mut app) = app_showing_a_frame();
        assert!(!app.model.stretch_enabled);
        let image = Arc::clone(&app.model.loaded.as_ref().unwrap().image);
        assert_eq!(app.mapping_for(&image).stretch(), None);
    }

    #[test]
    fn the_histogram_is_not_rebuilt_while_the_image_is_unchanged() {
        // Measuring the stretch for the marks costs 12 ms on a full frame.
        // Doing it while drawing would spend most of a frame's budget on a
        // decoration, sixty times a second.
        use fits_core::testutil::{write_synthetic, SyntheticSpec};

        let dir = tempfile::tempdir().unwrap();
        write_synthetic(
            dir.path(),
            "light.fits",
            &SyntheticSpec::new(32, 32, 16),
            &(0..1024).map(f64::from).collect::<Vec<_>>(),
        )
        .unwrap();

        let mut app = FitsViewApp::new(None);
        app.model.handle(Action::Open(dir.path().to_path_buf()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.model.loaded.is_none() && std::time::Instant::now() < deadline {
            app.model.poll();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        app.model.handle(Action::ToggleHistogram);
        app.model.handle(Action::ToggleStretch);

        app.sync_histogram();
        assert!(app.histogram.is_some(), "it should have been built once");

        // Mark the cached copy. A rebuild would discard the mark.
        app.histogram.as_mut().unwrap().histogram.counted = 12_345;
        app.sync_histogram();
        assert_eq!(
            app.histogram.as_ref().unwrap().histogram.counted,
            12_345,
            "an unchanged image must not be measured again"
        );

        // A new image is a different matter.
        app.model.generation += 1;
        app.sync_histogram();
        assert_ne!(
            app.histogram.as_ref().unwrap().histogram.counted,
            12_345,
            "a changed image should be measured afresh"
        );
    }

    #[test]
    fn the_collapsed_file_list_stays_collapsed_next_time() {
        let mut app = FitsViewApp::new(None);
        app.model.handle(Action::ToggleFileList);
        assert!(!app.model.show_filelist);

        let mut storage = MemoryStorage::default();
        eframe::App::save(&mut app, &mut storage);

        let restored = FitsViewApp::with_storage(None, Some(&storage));
        assert!(!restored.model.show_filelist);
    }

    #[test]
    fn the_metadata_section_state_is_remembered() {
        let mut app = FitsViewApp::new(None);
        app.model.handle(Action::ToggleHeader);
        assert!(!app.model.show_header);

        let mut storage = MemoryStorage::default();
        eframe::App::save(&mut app, &mut storage);

        let restored = FitsViewApp::with_storage(None, Some(&storage));
        assert!(!restored.model.show_header);
    }

    #[test]
    fn the_file_list_is_visible_on_a_first_run() {
        let app = FitsViewApp::with_storage(None, Some(&MemoryStorage::default()));
        assert!(app.model.show_filelist, "it should not start hidden");
    }

    #[test]
    fn the_last_folder_is_reopened_on_a_later_run() {
        use fits_core::testutil::{write_synthetic, SyntheticSpec};

        let dir = tempfile::tempdir().unwrap();
        write_synthetic(
            dir.path(),
            "light.fits",
            &SyntheticSpec::new(4, 4, 16),
            &[1.0; 16],
        )
        .unwrap();

        let mut app = FitsViewApp::new(None);
        app.model.handle(Action::Open(dir.path().to_path_buf()));

        let mut storage = MemoryStorage::default();
        eframe::App::save(&mut app, &mut storage);

        let restored = FitsViewApp::with_storage(None, Some(&storage));
        assert_eq!(
            restored.model.folder.as_ref().map(|f| f.dir.clone()),
            Some(dir.path().to_path_buf())
        );
    }

    #[test]
    fn a_folder_named_on_the_command_line_wins_over_the_saved_one() {
        use fits_core::testutil::{write_synthetic, SyntheticSpec};

        let saved_dir = tempfile::tempdir().unwrap();
        let asked_dir = tempfile::tempdir().unwrap();
        for dir in [saved_dir.path(), asked_dir.path()] {
            write_synthetic(dir, "light.fits", &SyntheticSpec::new(4, 4, 16), &[1.0; 16]).unwrap();
        }

        let mut storage = MemoryStorage::default();
        eframe::set_value(
            &mut storage,
            KEY_LAST_FOLDER,
            &saved_dir.path().display().to_string(),
        );

        let app = FitsViewApp::with_storage(Some(asked_dir.path().to_path_buf()), Some(&storage));
        assert_eq!(
            app.model.folder.as_ref().map(|f| f.dir.clone()),
            Some(asked_dir.path().to_path_buf()),
            "the command line should take precedence"
        );
    }

    #[test]
    fn a_saved_folder_that_no_longer_exists_is_skipped_quietly() {
        let mut storage = MemoryStorage::default();
        eframe::set_value(
            &mut storage,
            KEY_LAST_FOLDER,
            &"/definitely/not/here".to_string(),
        );

        let app = FitsViewApp::with_storage(None, Some(&storage));
        assert!(app.model.folder.is_none());
        assert!(
            app.model.error.is_none(),
            "a missing folder is not an error"
        );
    }

    #[test]
    fn a_first_run_with_no_saved_settings_uses_the_defaults() {
        let storage = MemoryStorage::default();
        let app = FitsViewApp::with_storage(None, Some(&storage));
        assert!(!app.model.stretch_enabled);
        assert_eq!(app.model.stretch_params, StretchParams::default());
        assert!(!app.model.confirm_every_delete);
    }

    #[test]
    fn damaged_settings_fall_back_to_the_defaults_rather_than_failing_to_start() {
        let mut storage = MemoryStorage::default();
        eframe::Storage::set_string(
            &mut storage,
            KEY_STRETCH_ENABLED,
            "not a boolean".to_string(),
        );
        eframe::Storage::set_string(&mut storage, KEY_STRETCH_TARGET, "{{{".to_string());

        let app = FitsViewApp::with_storage(None, Some(&storage));
        assert!(!app.model.stretch_enabled);
        assert_eq!(app.model.stretch_params, StretchParams::default());
    }

    #[test]
    fn starting_without_any_storage_is_fine() {
        let app = FitsViewApp::with_storage(None, None);
        assert_eq!(app.model.stretch_params, StretchParams::default());
    }
}

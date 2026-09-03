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
pub mod input;
mod toolbar;
mod viewer;

use egui::{TextureHandle, TextureOptions, Ui};
use fits_core::stretch::StretchParams;

use crate::app::{Action, Model};
use crate::texture::{self, Mapping};

/// Storage keys for settings that outlive a session.
const KEY_STRETCH_ENABLED: &str = "stretch_enabled";
const KEY_STRETCH_SHADOWS: &str = "stretch_shadows_clip";
const KEY_STRETCH_TARGET: &str = "stretch_target_bg";
const KEY_CONFIRM_EVERY_DELETE: &str = "confirm_every_delete";
const KEY_LAST_FOLDER: &str = "last_folder";
const KEY_SHOW_FILELIST: &str = "show_filelist";
const KEY_SHOW_HEADER: &str = "show_header";

/// The `eframe` application: a model, a cached texture, and the glue between
/// them.
pub struct FitsViewApp {
    model: Model,
    /// The uploaded texture, tagged with the model generation it was built
    /// from, so it is rebuilt only when the displayed image really changes.
    texture: Option<(u64, TextureHandle)>,
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
        let mapping = if self.model.stretch_enabled {
            Mapping::stretched(image, &self.model.stretch_params)
        } else {
            Mapping::linear(image)
        };
        let colour = texture::to_color_image(image, &mapping, factor);
        let handle = ui
            .ctx()
            .load_texture("fits-image", colour, TextureOptions::LINEAR);
        self.texture = Some((self.model.generation, handle));
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

        self.sync_texture(ui);
        let texture = self.texture.as_ref().map(|(_, t)| t.clone());

        for action in viewer::show(ui, &mut self.model, texture.as_ref()) {
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

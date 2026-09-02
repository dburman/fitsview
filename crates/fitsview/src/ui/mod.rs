//! The drawing layer.
//!
//! Everything here reads [`crate::app::Model`] and produces
//! [`crate::app::Action`]s. No rules live in this module tree, which is why it
//! carries few tests of its own: the behaviour worth testing lives in
//! [`crate::app`], [`crate::view`], [`crate::texture`] and [`input`].

pub mod input;
mod toolbar;
mod viewer;

use egui::{TextureHandle, TextureOptions, Ui};

use crate::app::{Action, Model};
use crate::texture::{self, Mapping};

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
        let colour = texture::to_color_image(image, &Mapping::linear(image), factor);
        let handle = ui
            .ctx()
            .load_texture("fits-image", colour, TextureOptions::LINEAR);
        self.texture = Some((self.model.generation, handle));
    }
}

impl eframe::App for FitsViewApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        for action in toolbar::show(ui, &self.model) {
            self.model.handle(action);
        }

        self.sync_texture(ui);
        let texture = self.texture.as_ref().map(|(_, t)| t.clone());

        for action in viewer::show(ui, &mut self.model, texture.as_ref()) {
            self.model.handle(action);
        }
    }
}

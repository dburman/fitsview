//! The top bar of file actions and the bottom status line.

use egui::{Button, Panel, Ui};

use crate::app::{Action, Model};

/// Draws the toolbar and status line, returning whatever the user asked for.
pub fn show(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();

    Panel::top("toolbar").show(ui, |ui| {
        ui.horizontal(|ui| {
            if ui
                .button("Open File…")
                .on_hover_text("Open a single FITS image")
                .clicked()
            {
                if let Some(path) = pick_file() {
                    actions.push(Action::Open(path));
                }
            }

            // Folder browsing arrives in Phase 3. Showing the control disabled
            // keeps the shape of the interface stable between phases.
            ui.add_enabled(false, Button::new("Open Folder…"))
                .on_disabled_hover_text("Folder browsing arrives in a later phase");

            ui.separator();

            let has_image = model.loaded.is_some();
            if ui
                .add_enabled(has_image, Button::new("Fit"))
                .on_hover_text("Fit the image to the window (F)")
                .clicked()
            {
                actions.push(Action::FitToWindow);
            }
            if ui
                .add_enabled(has_image, Button::new("100%"))
                .on_hover_text("One image pixel per screen pixel (1)")
                .clicked()
            {
                actions.push(Action::ActualSize);
            }
        });
    });

    Panel::bottom("status").show(ui, |ui| {
        ui.horizontal(|ui| {
            if model.error.is_some() {
                ui.colored_label(ui.visuals().error_fg_color, model.status_text());
                if ui.small_button("Dismiss").clicked() {
                    actions.push(Action::ClearError);
                }
            } else {
                ui.label(model.status_text());
            }
        });
    });

    actions
}

/// Asks the operating system for a file to open.
fn pick_file() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter("FITS images", &["fits", "fit", "fts"])
        .add_filter("All files", &["*"])
        .set_title("Open a FITS image")
        .pick_file()
}

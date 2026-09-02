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

            if ui
                .button("Open Folder…")
                .on_hover_text("Browse every FITS image in a folder")
                .clicked()
            {
                if let Some(dir) = pick_folder() {
                    actions.push(Action::Open(dir));
                }
            }

            ui.separator();

            let has_folder = model.folder.is_some();
            if ui
                .add_enabled(has_folder, Button::new("◀"))
                .on_hover_text("Previous file (Left arrow)")
                .clicked()
            {
                actions.push(Action::PreviousFile);
            }
            if ui
                .add_enabled(has_folder, Button::new("▶"))
                .on_hover_text("Next file (Right arrow, or Space)")
                .clicked()
            {
                actions.push(Action::NextFile);
            }
            if has_folder {
                ui.label(model.position_label());
            }
            if ui
                .add_enabled(has_folder, Button::new("Rescan"))
                .on_hover_text("Re-read the folder from disk (F5)")
                .clicked()
            {
                actions.push(Action::Rescan);
            }

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

/// Asks the operating system for a folder to browse.
fn pick_folder() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open a folder of FITS images")
        .pick_folder()
}

/// Asks the operating system for a file to open.
fn pick_file() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter("FITS images", &["fits", "fit", "fts"])
        .add_filter("All files", &["*"])
        .set_title("Open a FITS image")
        .pick_file()
}

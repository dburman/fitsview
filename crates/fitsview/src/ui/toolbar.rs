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

            let has_selection = model
                .folder
                .as_ref()
                .is_some_and(|f| f.selected_entry().is_some());
            let flagged = model
                .folder
                .as_ref()
                .and_then(crate::folder::Folder::selected_entry)
                .is_some_and(|e| e.flagged);

            if ui
                .add_enabled(
                    has_selection,
                    Button::new(if flagged { "★ Keep" } else { "☆ Keep" }),
                )
                .on_hover_text("Mark this file to keep (K). Flagged files ask before deleting.")
                .clicked()
            {
                actions.push(Action::ToggleFlag);
            }
            if ui
                .add_enabled(has_selection, Button::new("Rename"))
                .on_hover_text("Rename this file (F2)")
                .clicked()
            {
                actions.push(Action::BeginRename);
            }
            if ui
                .add_enabled(has_selection, Button::new("🗑 Delete"))
                .on_hover_text("Move this file to the trash (Delete)")
                .clicked()
            {
                actions.push(Action::RequestDelete);
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

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("?")
                    .on_hover_text("Keyboard shortcuts (? or H)")
                    .clicked()
                {
                    actions.push(Action::ToggleHelp);
                }
                let mut confirm = model.confirm_every_delete;
                if ui
                    .checkbox(&mut confirm, "Confirm every delete")
                    .on_hover_text("Ask before deleting any file, not only flagged ones")
                    .changed()
                {
                    actions.push(Action::ToggleConfirmEveryDelete);
                }
            });
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

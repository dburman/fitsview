//! The calibration panel, on the right.

use egui::{Button, Color32, Panel, ProgressBar, RichText, Ui};

use crate::app::{Action, Model};

/// Width of the panel.
const PANEL_WIDTH: f32 = 280.0;

/// Draws the calibration panel and returns whatever the user asked for.
pub fn show(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();

    Panel::right("calibration")
        .default_size(PANEL_WIDTH)
        .resizable(true)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.heading("Calibration");
            ui.separator();

            actions.extend(darks_section(ui, model));

            if let Some(job) = &model.job {
                ui.separator();
                ui.label(RichText::new(&job.label).strong());
                ui.add(ProgressBar::new(job.fraction()).show_percentage());
                if !job.item.is_empty() {
                    ui.label(RichText::new(&job.item).weak().small());
                }
                if ui.button("Stop").clicked() {
                    actions.push(Action::CancelJob);
                }
            }

            ui.separator();
            actions.extend(export_section(ui, model));
        });

    actions
}

/// The dark frames section.
fn darks_section(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();
    let calibration = &model.calibration;
    let busy = model.job.is_some();

    ui.add_space(4.0);
    ui.label(RichText::new("Darks").strong());
    ui.label(RichText::new(calibration.summary()).weak().small());
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        if ui
            .add_enabled(!busy, Button::new("Add darks…"))
            .on_hover_text("Choose dark frames to combine into a master")
            .clicked()
        {
            if let Some(paths) = pick_files("Choose dark frames") {
                actions.push(Action::AddDarks(paths));
            }
        }
        if ui
            .add_enabled(
                !busy && !calibration.dark_sources.is_empty(),
                Button::new("Build master"),
            )
            .on_hover_text("Combine the chosen darks, rejecting outliers")
            .clicked()
        {
            actions.push(Action::BuildMasterDark);
        }
    });

    ui.horizontal(|ui| {
        if ui
            .add_enabled(!busy, Button::new("Load master…"))
            .on_hover_text("Use a master saved earlier")
            .clicked()
        {
            if let Some(path) = pick_file("Open a master dark") {
                actions.push(Action::LoadMasterDark(path));
            }
        }
        if ui
            .add_enabled(!busy && calibration.has_dark(), Button::new("Save master…"))
            .on_hover_text("Write the master so it can be reused")
            .clicked()
        {
            if let Some(path) = save_file("Save the master dark", "master_dark.fits") {
                actions.push(Action::SaveMasterDark(path));
            }
        }
        if ui
            .add_enabled(
                !busy && (calibration.has_dark() || !calibration.dark_sources.is_empty()),
                Button::new("Clear"),
            )
            .clicked()
        {
            actions.push(Action::ClearDarks);
        }
    });

    ui.add_space(6.0);

    // The toggle is disabled, with the reason shown, when the master cannot be
    // applied to what is on screen.
    let blocked = calibration.blocked.as_deref();
    let mut apply = calibration.apply_dark;
    let response = ui.add_enabled(
        calibration.has_dark() && blocked.is_none(),
        egui::Checkbox::new(&mut apply, "Apply dark (D)"),
    );
    if response.changed() {
        actions.push(Action::ToggleApplyDark);
    }

    if let Some(reason) = blocked {
        ui.label(
            RichText::new(reason)
                .color(ui.visuals().error_fg_color)
                .small(),
        );
    }
    for warning in &calibration.warnings {
        ui.label(
            RichText::new(format!("⚠ {warning}"))
                .color(Color32::from_rgb(240, 200, 80))
                .small(),
        );
    }

    actions
}

/// The export section.
fn export_section(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();
    let busy = model.job.is_some();
    let count = model.folder.as_ref().map_or(0, crate::folder::Folder::len);

    ui.add_space(4.0);
    ui.label(RichText::new("Export").strong());
    ui.label(
        RichText::new(format!(
            "Writes a calibrated copy of all {count} file{} as _cal.fits. Originals are never changed.",
            if count == 1 { "" } else { "s" }
        ))
        .weak()
        .small(),
    );
    ui.add_space(4.0);

    if ui
        .add_enabled(!busy && count > 0, Button::new("Export calibrated…"))
        .clicked()
    {
        if let Some(directory) = pick_folder("Choose where to write calibrated files") {
            actions.push(Action::StartExport(directory));
        }
    }

    actions
}

/// Asks for several FITS files.
fn pick_files(title: &str) -> Option<Vec<std::path::PathBuf>> {
    rfd::FileDialog::new()
        .add_filter("FITS images", &["fits", "fit", "fts"])
        .set_title(title)
        .pick_files()
}

/// Asks for a single FITS file.
fn pick_file(title: &str) -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter("FITS images", &["fits", "fit", "fts"])
        .set_title(title)
        .pick_file()
}

/// Asks where to save a file.
fn save_file(title: &str, name: &str) -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter("FITS images", &["fits", "fit", "fts"])
        .set_title(title)
        .set_file_name(name)
        .save_file()
}

/// Asks for a folder.
fn pick_folder(title: &str) -> Option<std::path::PathBuf> {
    rfd::FileDialog::new().set_title(title).pick_folder()
}

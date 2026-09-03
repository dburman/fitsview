//! The calibration panel, on the right.

use egui::{Button, CollapsingHeader, Color32, Panel, ProgressBar, RichText, ScrollArea, Ui};

use crate::app::{Action, Model};
use crate::ui::header;

/// Width of the panel.
const PANEL_WIDTH: f32 = 280.0;

/// Draws the right-hand panel: what is in the current image, and what is being
/// applied to it.
///
/// Both live here because they answer the same question. Deciding whether a
/// dark suits a light means comparing exposure and temperature, and those are
/// header values; having them in a different panel meant looking away from the
/// controls to check.
pub fn show(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();

    Panel::right("inspector")
        .default_size(PANEL_WIDTH)
        .resizable(true)
        .show(ui, |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("inspector-scroll")
                .show(ui, |ui| {
                    ui.add_space(4.0);

                    // The metadata section is opened and closed by the model, so
                    // that the I key and a click on the header agree.
                    let metadata = CollapsingHeader::new("Image metadata")
                        .id_salt("metadata")
                        .open(Some(model.show_header))
                        .show(ui, |ui| actions.extend(header::section(ui, model)));
                    if metadata.header_response.clicked() {
                        actions.push(Action::ToggleHeader);
                    }

                    ui.add_space(4.0);
                    CollapsingHeader::new("Calibration")
                        .id_salt("calibration")
                        .default_open(true)
                        .show(ui, |ui| {
                            actions.extend(darks_section(ui, model));
                            ui.separator();
                            actions.extend(flats_section(ui, model));
                            ui.separator();
                            actions.extend(colour_section(ui, model));

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
                });
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

/// The flat frames section.
fn flats_section(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();
    let calibration = &model.calibration;
    let busy = model.job.is_some();

    ui.add_space(4.0);
    ui.label(RichText::new("Flats").strong());
    ui.label(RichText::new(calibration.flat_summary()).weak().small());
    if !calibration.flat_dark_sources.is_empty() {
        ui.label(
            RichText::new(format!(
                "{} flat darks will be subtracted first",
                calibration.flat_dark_sources.len()
            ))
            .weak()
            .small(),
        );
    }
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        if ui
            .add_enabled(!busy, Button::new("Add flats…"))
            .on_hover_text("Evenly lit frames through the same optics as the lights")
            .clicked()
        {
            if let Some(paths) = pick_files("Choose flat frames") {
                actions.push(Action::AddFlats(paths));
            }
        }
        if ui
            .add_enabled(!busy, Button::new("Add flat darks…"))
            .on_hover_text(
                "Darks of the same exposure as the flats, or bias frames. \
                 Flats are short exposures and still carry the read offset.",
            )
            .clicked()
        {
            if let Some(paths) = pick_files("Choose flat darks or bias frames") {
                actions.push(Action::AddFlatDarks(paths));
            }
        }
    });

    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy && !calibration.flat_sources.is_empty(),
                Button::new("Build master"),
            )
            .on_hover_text("Combine the flats into a gain map centred on 1.0")
            .clicked()
        {
            actions.push(Action::BuildMasterFlat);
        }
        if ui
            .add_enabled(!busy, Button::new("Load…"))
            .on_hover_text("Use a gain map saved earlier")
            .clicked()
        {
            if let Some(path) = pick_file("Open a master flat") {
                actions.push(Action::LoadMasterFlat(path));
            }
        }
        if ui
            .add_enabled(!busy && calibration.has_flat(), Button::new("Save…"))
            .clicked()
        {
            if let Some(path) = save_file("Save the master flat", "master_flat.fits") {
                actions.push(Action::SaveMasterFlat(path));
            }
        }
        if ui
            .add_enabled(
                !busy && (calibration.has_flat() || !calibration.flat_sources.is_empty()),
                Button::new("Clear"),
            )
            .clicked()
        {
            actions.push(Action::ClearFlats);
        }
    });

    ui.add_space(6.0);
    let mut apply = calibration.apply_flat;
    if ui
        .add_enabled(
            calibration.has_flat() && calibration.blocked.is_none(),
            egui::Checkbox::new(&mut apply, "Apply flat (Shift+F)"),
        )
        .on_hover_text("Divided out after the dark is subtracted, never before")
        .changed()
    {
        actions.push(Action::ToggleApplyFlat);
    }

    actions
}

/// The one-shot colour section.
fn colour_section(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();
    let bayer = &model.bayer;

    // Only a single-channel image can be a mosaic.
    let is_mosaic = model.loaded.as_ref().is_some_and(|l| l.image.channels == 1);

    ui.add_space(4.0);
    ui.label(RichText::new("One-shot colour").strong());
    ui.label(RichText::new(bayer.summary()).weak().small());
    ui.add_space(4.0);

    let mut enabled = bayer.enabled;
    if ui
        .add_enabled(is_mosaic, egui::Checkbox::new(&mut enabled, "Debayer (B)"))
        .on_hover_text(
            "Reconstruct colour from the sensor's filter grid.\n\
             Applied after the dark and flat, never before, and only to the \
             display: exported files stay as mosaics.",
        )
        .on_disabled_hover_text("This image already has colour channels")
        .changed()
    {
        actions.push(Action::ToggleDebayer);
    }

    if !bayer.enabled {
        return actions;
    }

    ui.horizontal(|ui| {
        ui.label("Pattern");
        let current = bayer.pattern.unwrap_or(fits_core::BayerPattern::Rggb);
        egui::ComboBox::from_id_salt("bayer-pattern")
            .selected_text(current.name())
            .show_ui(ui, |ui| {
                for pattern in fits_core::BayerPattern::ALL {
                    if ui
                        .selectable_label(current == pattern, pattern.name())
                        .clicked()
                    {
                        actions.push(Action::SetBayerPattern(pattern));
                    }
                }
            });
    });

    let mut flip = bayer.flip_rows;
    if ui
        .checkbox(&mut flip, "Flip pattern rows")
        .on_hover_text(
            "FITS stores the bottom row of an image first, and capture programs \
             disagree about which end the pattern describes.\n\
             Turn this on if the colours look wrong, for instance red and blue \
             swapped or a magenta cast. The image itself is fine either way.",
        )
        .changed()
    {
        actions.push(Action::ToggleBayerFlip);
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
            "Writes a calibrated copy of all {count} file{} as _cal.fits. \
             Originals are never changed.",
            if count == 1 { "" } else { "s" }
        ))
        .weak()
        .small(),
    );
    if model.bayer.enabled {
        ui.label(
            RichText::new(
                "Exports stay as mosaics, undebayered, which is what a stacker \
                 wants and what it does better.",
            )
            .weak()
            .small(),
        );
    }
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

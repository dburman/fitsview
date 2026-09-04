//! The top bar of file actions and the bottom status line.

use egui::{Button, DragValue, Panel, RichText, Ui};
use fits_core::stars::DetectionParams;
use fits_core::stretch::StretchParams;

use crate::app::{Action, Model};

/// Draws the toolbar and status line, returning whatever the user asked for.
pub fn show(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();

    Panel::top("toolbar").show(ui, |ui| {
        ui.horizontal(|ui| {
            // The way back when the file list is collapsed to the edge.
            let arrow = if model.show_filelist { "◀" } else { "▶" };
            if ui
                .button(arrow)
                .on_hover_text(if model.show_filelist {
                    "Hide the file list, so the image fills the window (L)"
                } else {
                    "Show the file list (L)"
                })
                .clicked()
            {
                actions.push(Action::ToggleFileList);
            }
            ui.separator();

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
            // A read-only volume — a Windows-formatted drive on macOS is the
            // common one — cannot have its files changed at all. Greying the
            // buttons says so before they are pressed.
            let can_change = has_selection && !model.read_only;
            let refusal = "The volume this folder is on is read-only, so its files \
                           cannot be changed";
            if ui
                .add_enabled(can_change, Button::new("Rename"))
                .on_hover_text(if model.read_only {
                    refusal
                } else {
                    "Rename this file (F2)"
                })
                .clicked()
            {
                actions.push(Action::BeginRename);
            }
            if ui
                .add_enabled(can_change, Button::new("🗑 Delete"))
                .on_hover_text(if model.read_only {
                    refusal
                } else {
                    "Move this file to the trash (Delete)"
                })
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

            ui.separator();

            // The stretch is a display setting, so it stays available even
            // before an image has finished loading.
            let mut stretch = model.stretch_enabled;
            if ui
                .checkbox(&mut stretch, "Stretch")
                .on_hover_text(
                    "Automatic screen stretch, so the faint signal is visible (S).\n\
                     Affects the display only; the pixel data is untouched.",
                )
                .changed()
            {
                actions.push(Action::ToggleStretch);
            }
            actions.extend(stretch_settings(ui, model));

            // Beside the stretch, because both are decisions about what the
            // display shows rather than about the file.
            let mut stars = model.stars_enabled;
            if ui
                .checkbox(&mut stars, "Stars")
                .on_hover_text(
                    "Find the stars and measure them: how many, how wide, how round (Shift+S).\n\
                     Runs in the background and draws a circle on each.\n\
                     Costs about 60 ms a frame, so it is off unless asked for.",
                )
                .changed()
            {
                actions.push(Action::ToggleStars);
            }
            actions.extend(star_settings(ui, model));
            if model.stars_enabled {
                let summary = match &model.stars {
                    None => "finding…".to_string(),
                    Some(field) => match field.fwhm {
                        Some(fwhm) => format!("{} stars, {fwhm:.1} px", field.count()),
                        None => format!("{} stars", field.count()),
                    },
                };
                ui.label(RichText::new(summary).weak().small());
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
                if let Some(readout) = model.pixel_readout() {
                    ui.separator();
                    let explanation = if readout.reconstructed {
                        "The values in the file, before calibration or stretching.\n\
                         A tilde marks colour interpolated from the filter grid: \
                         each pixel measured only one of the three."
                    } else {
                        "The value in the file, before calibration or stretching"
                    };
                    ui.label(RichText::new(readout.describe()).monospace())
                        .on_hover_text(explanation);
                }
            }
        });
    });

    actions
}

/// The stretch settings, tucked behind a menu so they do not clutter the bar.
fn stretch_settings(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();
    let defaults = StretchParams::default();
    let mut params = model.stretch_params;

    ui.menu_button("⚙", |ui| {
        ui.set_min_width(260.0);
        ui.label(RichText::new("Stretch settings").strong());
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("Background")
                .on_hover_text("Where the sky background ends up, from black at 0 to white at 1");
            ui.add(
                DragValue::new(&mut params.target_bg)
                    .speed(0.005)
                    .range(0.05..=0.5),
            );
        });

        ui.horizontal(|ui| {
            ui.label("Black point").on_hover_text(
                "How far below the background, in noise deviations, the black point sits. \
                 More negative keeps more of the faint signal.",
            );
            ui.add(
                DragValue::new(&mut params.shadows_clip)
                    .speed(0.05)
                    .range(-5.0..=0.0),
            );
        });

        ui.add_space(6.0);
        if ui
            .add_enabled(params != defaults, Button::new("Reset"))
            .clicked()
        {
            actions.push(Action::ResetStretchParams);
            ui.close();
        }
    });

    if params != model.stretch_params {
        actions.push(Action::SetStretchParams(params));
    }
    actions
}

/// The gear menu beside the Stars toggle. A rich field and a sparse one want
/// different thresholds, but the defaults should mean this is rarely opened.
fn star_settings(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();
    let defaults = DetectionParams::default();
    let mut params = model.star_params;

    ui.add_enabled_ui(model.stars_enabled, |ui| {
        ui.menu_button("⚙", |ui| {
            ui.set_min_width(280.0);
            ui.label(RichText::new("Star detection").strong());
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.label("Threshold").on_hover_text(
                    "How far above the sky background, in noise deviations, a pixel \
                     must be to belong to a star. Lower finds fainter stars and more \
                     noise.",
                );
                ui.add(
                    DragValue::new(&mut params.threshold)
                        .speed(0.1)
                        .range(2.0..=20.0),
                );
            });

            ui.horizontal(|ui| {
                ui.label("Smallest").on_hover_text(
                    "Fewest pixels a star may cover. Raising it discards hot pixels \
                     and cosmic rays; lowering it keeps the faintest stars.",
                );
                ui.add(
                    DragValue::new(&mut params.minimum_area)
                        .speed(0.2)
                        .range(1..=64),
                );
            });

            ui.horizontal(|ui| {
                ui.label("Most stars").on_hover_text(
                    "The brightest this many are kept, so a dense field stays quick.",
                );
                ui.add(
                    DragValue::new(&mut params.limit)
                        .speed(20.0)
                        .range(100..=50_000),
                );
            });

            ui.add_space(6.0);
            if ui
                .add_enabled(params != defaults, Button::new("Reset"))
                .clicked()
            {
                actions.push(Action::ResetStarParams);
                ui.close();
            }
        })
        .response
        .on_hover_text("Star detection settings");
    });

    if params != model.star_params {
        actions.push(Action::SetStarParams(params));
    }
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

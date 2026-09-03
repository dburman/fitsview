//! The left panel listing the files in the folder.

use egui::{Color32, Label, Panel, RichText, ScrollArea, Sense, Ui};

use crate::app::{Action, Model};
use crate::folder::FileEntry;

/// Width of the panel. Wide enough for a typical capture file name.
const PANEL_WIDTH: f32 = 260.0;

/// Draws the file list, returning whatever the user asked for.
///
/// The panel can be collapsed to give the image the whole window. `egui` also
/// collapses it when the resize edge is dragged past the minimum width, or when
/// that edge is double-clicked, so the state has to be read back afterwards
/// rather than only written.
pub fn show(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut expanded = model.show_filelist;

    Panel::left("filelist")
        .default_size(PANEL_WIDTH)
        .min_size(140.0)
        .resizable(true)
        .show_collapsible(ui, &mut expanded, |ui| {
            let Some(folder) = &model.folder else {
                ui.add_space(8.0);
                ui.label(RichText::new("No folder open").weak());
                return;
            };

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(folder.position_label()).strong());
                if model.loading {
                    ui.label(RichText::new("loading…").weak());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("◀")
                        .on_hover_text("Hide the file list (L)")
                        .clicked()
                    {
                        actions.push(Action::ToggleFileList);
                    }
                });
            });
            ui.separator();

            let selected = folder.selected;
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (index, entry) in folder.files.iter().enumerate() {
                        let response = row(ui, entry, Some(index) == selected);
                        if response.clicked {
                            actions.push(Action::Select(index));
                        }
                        // Keep the selection in view when the keyboard moves it.
                        if Some(index) == selected && model.scroll_to_selection {
                            if let Some(rect) = response.rect {
                                ui.scroll_to_rect(rect, None);
                            }
                        }
                    }
                });
        });

    if expanded != model.show_filelist {
        // The panel collapsed itself, from a drag or a double-click.
        actions.push(Action::SetFileListVisible(expanded));
    }

    actions
}

/// What drawing one row produced.
pub struct RowResponse {
    /// Whether the row was clicked.
    pub clicked: bool,
    /// Where it was drawn, so the list can scroll to it.
    pub rect: Option<egui::Rect>,
}

/// Draws one row.
fn row(ui: &mut Ui, entry: &FileEntry, selected: bool) -> RowResponse {
    let response = ui
        .scope(|ui| {
            ui.horizontal(|ui| {
                // The flag column is present from Phase 3 so that turning it on
                // in Phase 4 does not shift the layout.
                let flag = if entry.flagged { "★" } else { " " };
                ui.label(RichText::new(flag).color(Color32::from_rgb(240, 200, 80)));

                let text = RichText::new(&entry.name);
                let text = if selected { text.strong() } else { text };
                ui.add(Label::new(text).truncate());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(human_size(entry.size)).weak().small());
                });
            });
        })
        .response
        .interact(Sense::click());

    if selected {
        ui.painter().rect_filled(
            response.rect,
            2.0,
            Color32::from_rgba_unmultiplied(90, 130, 200, 40),
        );
    }
    RowResponse {
        clicked: response.clicked(),
        rect: Some(response.rect),
    }
}

/// Formats a byte count for the list, in the units an astrophotographer thinks
/// in. Exact figures do not matter here; recognising a file that is the wrong
/// size does.
#[must_use]
pub fn human_size(bytes: u64) -> String {
    const UNIT: f64 = 1024.0;
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    if b < UNIT {
        format!("{bytes} B")
    } else if b < UNIT * UNIT {
        format!("{:.0} KB", b / UNIT)
    } else if b < UNIT * UNIT * UNIT {
        format!("{:.1} MB", b / (UNIT * UNIT))
    } else {
        format!("{:.2} GB", b / (UNIT * UNIT * UNIT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_shown_in_readable_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_size(48 * 1024 * 1024), "48.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    #[test]
    fn very_large_sizes_do_not_overflow_or_panic() {
        let s = human_size(u64::MAX);
        assert!(s.ends_with("GB"), "{s}");
    }
}

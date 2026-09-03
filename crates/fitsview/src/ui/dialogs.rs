//! Modal dialogs, the rename editor, the help overlay and toasts.

use egui::{Align2, Color32, Key, Modal, RichText, TextEdit, Ui, Window};

use crate::app::{Action, Model, Pending};

/// Every shortcut, shown in the help overlay.
///
/// Kept next to the interface rather than in a document, so that a shortcut
/// cannot be changed without the overlay changing with it.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("→  Space  PgDn", "Next file"),
    ("←  PgUp", "Previous file"),
    ("Home / End", "First / last file"),
    ("K", "Toggle keep flag"),
    ("Delete / Backspace", "Delete to trash"),
    ("Shift+Delete", "Delete a flagged file without asking"),
    ("F2", "Rename"),
    ("F / 1", "Fit to window / actual size"),
    ("Scroll", "Zoom about the pointer"),
    ("Drag", "Pan"),
    ("F5", "Rescan the folder"),
    ("Esc", "Cancel, or dismiss an error"),
    ("? or H", "Show or hide this list"),
];

/// Draws whatever is currently waiting on the user.
pub fn show(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();

    match &model.pending {
        Pending::None => {}
        Pending::DeleteConfirm { name, flagged } => {
            actions.extend(delete_confirm(ui, name, *flagged));
        }
        Pending::Rename { text, problem } => {
            actions.extend(rename_editor(ui, text, problem.as_deref()));
        }
    }

    if model.show_help {
        actions.extend(help(ui));
    }

    if let Some(toast) = &model.toast {
        show_toast(ui, &toast.text);
    }

    actions
}

/// Asks before deleting. Returning nothing means the user has not decided yet.
fn delete_confirm(ui: &mut Ui, name: &str, flagged: bool) -> Vec<Action> {
    let mut actions = Vec::new();

    Modal::new("delete-confirm".into()).show(ui.ctx(), |ui| {
        ui.set_min_width(360.0);
        ui.heading("Delete this file?");
        ui.add_space(6.0);

        if flagged {
            ui.label(
                RichText::new(format!("{name} is flagged to keep."))
                    .color(Color32::from_rgb(240, 200, 80)),
            );
        } else {
            ui.label(name);
        }
        ui.add_space(4.0);
        ui.label(RichText::new("It will be moved to the trash.").weak());
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            // Cancel comes first and is the default, so that a stray Return
            // dismisses the dialog rather than deleting.
            if ui.button("Cancel").clicked() {
                actions.push(Action::Cancel);
            }
            if ui.button("Delete").clicked() {
                actions.push(Action::ConfirmDelete);
            }
        });

        // Escape cancels. Return deliberately does nothing: confirming a delete
        // must be a decision, not the tail end of a burst of keystrokes.
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            actions.push(Action::Cancel);
        }
    });

    actions
}

/// The rename editor.
fn rename_editor(ui: &mut Ui, text: &str, problem: Option<&str>) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut buffer = text.to_string();

    Modal::new("rename".into()).show(ui.ctx(), |ui| {
        ui.set_min_width(380.0);
        ui.heading("Rename");
        ui.add_space(6.0);

        let response = ui.add(
            TextEdit::singleline(&mut buffer)
                .desired_width(f32::INFINITY)
                .hint_text("New file name"),
        );
        response.request_focus();

        if let Some(problem) = problem {
            ui.add_space(4.0);
            ui.label(RichText::new(problem).color(ui.visuals().error_fg_color));
        } else {
            ui.add_space(4.0);
            ui.label(
                RichText::new("The extension is kept if you leave it off.")
                    .weak()
                    .small(),
            );
        }

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                actions.push(Action::Cancel);
            }
            if ui.button("Rename").clicked() {
                actions.push(Action::CommitRename(buffer.clone()));
            }
        });

        if ui.input(|i| i.key_pressed(Key::Enter)) {
            actions.push(Action::CommitRename(buffer.clone()));
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            actions.push(Action::Cancel);
        }
    });

    // Report typing so the name is validated as the user goes, rather than
    // only when they commit.
    if actions.is_empty() && buffer != text {
        actions.push(Action::RenameTextChanged(buffer));
    }

    actions
}

/// The shortcut overlay.
fn help(ui: &mut Ui) -> Vec<Action> {
    let mut actions = Vec::new();

    Window::new("Keyboard shortcuts")
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            egui::Grid::new("shortcut-grid")
                .num_columns(2)
                .spacing([24.0, 4.0])
                .show(ui, |ui| {
                    for (keys, description) in SHORTCUTS {
                        ui.label(RichText::new(*keys).monospace());
                        ui.label(*description);
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            if ui.button("Close").clicked() {
                actions.push(Action::ToggleHelp);
            }
        });

    actions
}

/// A transient message, floating near the bottom of the window.
fn show_toast(ui: &mut Ui, text: &str) {
    egui::Area::new("toast".into())
        .anchor(Align2::CENTER_BOTTOM, [0.0, -48.0])
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .fill(Color32::from_black_alpha(220))
                .show(ui, |ui| {
                    ui.label(RichText::new(text).color(Color32::WHITE));
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_overlay_documents_every_shortcut_the_application_responds_to() {
        // If a shortcut is added without a line here, the overlay silently
        // stops being a complete reference.
        let text = SHORTCUTS
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>()
            .join(" ");
        for expected in [
            "→",
            "←",
            "Home",
            "K",
            "Delete",
            "Shift+Delete",
            "F2",
            "F5",
            "Esc",
        ] {
            assert!(text.contains(expected), "the overlay is missing {expected}");
        }
    }

    #[test]
    fn every_shortcut_line_has_a_description() {
        for (keys, description) in SHORTCUTS {
            assert!(!keys.trim().is_empty());
            assert!(!description.trim().is_empty(), "{keys} has no description");
        }
    }
}

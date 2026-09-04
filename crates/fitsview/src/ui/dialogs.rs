//! Modal dialogs, the rename editor, the help overlay and toasts.

use egui::{Align2, Color32, Key, Modal, RichText, TextEdit, Ui, Window};

use crate::app::{Action, Model, Pending};

use crate::shortcuts::SHORTCUTS;

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

/// How wide a floating message may be, in a window of this width.
///
/// A floating area has no width of its own, so without a limit the text is
/// measured against whatever the area happens to offer. When that turns out to
/// be narrower than a single word, the word itself is broken: "Finding stars…"
/// came out split across two lines. Naming a width means any wrapping happens
/// between words, and short messages still shrink to fit their text.
fn toast_width(window_width: f32) -> f32 {
    /// Kept clear of the window edges.
    const MARGIN: f32 = 64.0;
    /// Long messages read badly in one line across a wide screen.
    const WIDEST: f32 = 560.0;
    /// Narrower than this and words break again, so overflow a small window
    /// instead.
    const NARROWEST: f32 = 220.0;

    (window_width - MARGIN).clamp(NARROWEST, WIDEST)
}

/// A transient message, floating near the bottom of the window.
fn show_toast(ui: &mut Ui, text: &str) {
    let width = toast_width(ui.ctx().content_rect().width());
    egui::Area::new("toast".into())
        .anchor(Align2::CENTER_BOTTOM, [0.0, -48.0])
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .fill(Color32::from_black_alpha(220))
                .show(ui, |ui| {
                    ui.set_max_width(width);
                    ui.label(RichText::new(text).color(Color32::WHITE));
                });
        });
}

#[cfg(test)]
mod tests {
    use super::toast_width;

    /// The shortest word egui must never break, measured generously: the
    /// widest message the application produces is the read-only explanation,
    /// whose longest word is "read-only," at ten characters.
    const LONGEST_WORD_PX: f32 = 140.0;

    #[test]
    fn a_message_always_has_room_for_a_whole_word() {
        // The bug this replaces: a window narrow enough that the area offered
        // less width than one word, so the word was broken mid-way.
        for window in [0.0, 120.0, 240.0, 400.0, 800.0, 3840.0] {
            assert!(
                toast_width(window) >= LONGEST_WORD_PX,
                "a {window} px window left only {} px for the text",
                toast_width(window)
            );
        }
    }

    #[test]
    fn a_message_stays_clear_of_the_edges_of_an_ordinary_window() {
        // Wide enough to matter, not so wide that a line becomes hard to read.
        assert!(toast_width(1200.0) < 1200.0);
        assert!(toast_width(1200.0) >= 400.0);
    }

    #[test]
    fn a_wider_window_never_gives_a_narrower_message() {
        let mut previous = 0.0;
        for window in [200.0, 400.0, 600.0, 1000.0, 2000.0] {
            let width = toast_width(window);
            assert!(width >= previous, "{window} px window narrowed the message");
            previous = width;
        }
    }
}

//! The keyboard shortcuts, in one place.
//!
//! Both the help overlay and the `--help` output are generated from this list,
//! so a shortcut cannot be added to one and forgotten in the other. The mapping
//! from keys to behaviour lives in [`crate::ui::input`]; this is only how it is
//! described.

/// Every shortcut, as `(keys, what it does)`.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("→  ↓  Space  PgDn", "Next file"),
    ("←  ↑  PgUp", "Previous file"),
    ("Home / End", "First / last file"),
    ("K", "Toggle keep flag"),
    ("Delete / Backspace", "Delete to trash"),
    ("Shift+Delete", "Delete a flagged file without asking"),
    ("F2", "Rename"),
    ("S", "Toggle the automatic stretch"),
    ("D", "Toggle dark calibration"),
    ("Shift+F", "Toggle flat calibration"),
    ("B", "Toggle colour reconstruction"),
    ("I", "Show or hide the image metadata"),
    ("G", "Show or hide the histogram"),
    ("L", "Hide or show the file list"),
    ("F / 1", "Fit to window / actual size"),
    ("Scroll", "Zoom about the pointer"),
    ("Drag", "Pan"),
    ("F5", "Rescan the folder"),
    ("Esc", "Cancel, or dismiss an error"),
    ("? or H", "Show or hide the shortcut list"),
];

/// The shortcuts formatted for a terminal, two columns.
#[must_use]
pub fn as_text() -> String {
    let width = SHORTCUTS
        .iter()
        .map(|(keys, _)| keys.chars().count())
        .max()
        .unwrap_or(0);
    SHORTCUTS
        .iter()
        .map(|(keys, description)| {
            let padding = " ".repeat(width - keys.chars().count());
            format!("  {keys}{padding}  {description}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_has_keys_and_a_description() {
        for (keys, description) in SHORTCUTS {
            assert!(!keys.trim().is_empty());
            assert!(!description.trim().is_empty(), "{keys} has no description");
        }
    }

    #[test]
    fn the_list_covers_every_key_the_application_acts_on() {
        // If a shortcut is added to `ui::input` without a line here, both the
        // help overlay and `--help` silently stop being complete.
        let text = SHORTCUTS
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>()
            .join(" ");
        for expected in [
            "→",
            "←",
            "Home",
            "End",
            "K",
            "Delete",
            "Shift+Delete",
            "F2",
            "S",
            "D",
            "Shift+F",
            "G",
            "I",
            "F5",
            "Esc",
        ] {
            assert!(text.contains(expected), "the list is missing {expected}");
        }
    }

    #[test]
    fn the_text_form_lines_up_in_columns() {
        let text = as_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), SHORTCUTS.len());
        // Every description starts at the same column.
        let columns: std::collections::HashSet<usize> = lines
            .iter()
            .map(|l| l.len() - l.trim_start_matches(' ').len())
            .collect();
        assert_eq!(columns.len(), 1, "the key column should be a fixed width");
        assert!(text.contains("Next file"));
    }
}

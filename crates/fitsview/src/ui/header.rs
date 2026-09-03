//! The FITS header viewer.
//!
//! Every question about a frame that the image itself cannot answer, such as
//! what exposure it was, which filter, what the sensor temperature was, is in
//! the header. Showing it is a few lines and saves reaching for another tool.

use egui::{Panel, RichText, ScrollArea, TextEdit, Ui};

use crate::app::{Action, Model};

/// Width of the panel.
const PANEL_WIDTH: f32 = 320.0;

/// Draws the header panel when it is showing.
pub fn show(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();
    if !model.show_header {
        return actions;
    }

    Panel::right("header")
        .default_size(PANEL_WIDTH)
        .resizable(true)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("Header");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("✕").on_hover_text("Close (I)").clicked() {
                        actions.push(Action::ToggleHeader);
                    }
                });
            });

            let Some(loaded) = model.loaded.as_ref() else {
                ui.label(RichText::new("No image").weak());
                return;
            };

            let mut filter = model.header_filter.clone();
            if ui
                .add(
                    TextEdit::singleline(&mut filter)
                        .hint_text("Filter by keyword or value")
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                actions.push(Action::SetHeaderFilter(filter.clone()));
            }
            ui.separator();

            let cards = matching_cards(&loaded.image.header, &filter);
            if cards.is_empty() {
                ui.label(RichText::new("Nothing matches").weak());
                return;
            }

            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("header-grid")
                        .num_columns(2)
                        .spacing([12.0, 2.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for (keyword, value) in cards {
                                ui.label(RichText::new(keyword).monospace().strong());
                                ui.label(RichText::new(value).monospace());
                                ui.end_row();
                            }
                        });
                });
        });

    actions
}

/// The header cards matching a filter.
///
/// Matching is case-insensitive across both keyword and value, so looking for
/// "temp" finds `CCD-TEMP` and looking for "M31" finds the object.
#[must_use]
pub fn matching_cards<'a>(
    header: &'a fits_core::FitsHeader,
    filter: &str,
) -> Vec<(&'a str, &'a str)> {
    let needle = filter.trim().to_ascii_lowercase();
    header
        .cards
        .iter()
        .filter(|(keyword, value)| {
            needle.is_empty()
                || keyword.to_ascii_lowercase().contains(&needle)
                || value.to_ascii_lowercase().contains(&needle)
        })
        .map(|(keyword, value)| (keyword.as_str(), value.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::FitsHeader;

    fn header() -> FitsHeader {
        FitsHeader {
            cards: vec![
                ("BITPIX".into(), "16".into()),
                ("EXPTIME".into(), "300.0".into()),
                ("CCD-TEMP".into(), "-10.5".into()),
                ("OBJECT".into(), "M31".into()),
                ("FILTER".into(), "Ha".into()),
            ],
        }
    }

    #[test]
    fn an_empty_filter_shows_every_card() {
        assert_eq!(matching_cards(&header(), "").len(), 5);
        assert_eq!(matching_cards(&header(), "   ").len(), 5);
    }

    #[test]
    fn filtering_matches_keywords_case_insensitively() {
        let h = header();
        let found = matching_cards(&h, "temp");
        assert_eq!(found, vec![("CCD-TEMP", "-10.5")]);
        assert_eq!(matching_cards(&header(), "CCD").len(), 1);
    }

    #[test]
    fn filtering_matches_values_too() {
        // So that looking for a target finds the card naming it.
        let h = header();
        let found = matching_cards(&h, "m31");
        assert_eq!(found, vec![("OBJECT", "M31")]);
    }

    #[test]
    fn a_filter_matching_nothing_returns_nothing() {
        assert!(matching_cards(&header(), "zzz").is_empty());
    }

    #[test]
    fn cards_keep_the_order_they_appeared_in() {
        // Header order is meaningful: it is how the capture program wrote it.
        let h = header();
        let found = matching_cards(&h, "");
        assert_eq!(found[0].0, "BITPIX");
        assert_eq!(found[4].0, "FILTER");
    }
}

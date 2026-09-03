//! The metadata section of the right-hand panel.
//!
//! Every question about a frame that the image itself cannot answer, such as
//! what exposure it was, which filter, or what the sensor temperature was, is
//! in the FITS header. It sits beside the calibration controls because those
//! are the questions calibration raises: whether this dark matches this light
//! is a question about exposure and temperature, and the answer is here.

use egui::{Grid, RichText, ScrollArea, TextEdit, Ui};

use crate::app::{Action, Model};

/// Height beyond which the card list scrolls rather than growing.
///
/// A capture program can write a hundred cards, which would push the
/// calibration controls off the bottom of the panel.
const MAX_HEIGHT: f32 = 320.0;

/// Draws the metadata section into an existing panel.
pub fn section(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();

    let Some(loaded) = model.loaded.as_ref() else {
        ui.label(RichText::new("No image").weak());
        return actions;
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

    let cards = matching_cards(&loaded.image.header, &filter);
    ui.label(
        RichText::new(if filter.trim().is_empty() {
            format!("{} cards", cards.len())
        } else {
            format!(
                "{} of {} cards",
                cards.len(),
                loaded.image.header.cards.len()
            )
        })
        .weak()
        .small(),
    );

    if cards.is_empty() {
        ui.label(RichText::new("Nothing matches").weak());
        return actions;
    }

    ScrollArea::vertical()
        .max_height(MAX_HEIGHT)
        .auto_shrink([false, true])
        .id_salt("header-cards")
        .show(ui, |ui| {
            Grid::new("header-grid")
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

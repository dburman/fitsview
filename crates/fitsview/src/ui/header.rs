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

    // The measurements first: they are about this frame rather than in it, and
    // they are what a decision to keep or discard actually rests on.
    if let Some(quality) = model
        .folder
        .as_ref()
        .and_then(crate::folder::Folder::selected_entry)
        .and_then(|e| e.quality)
    {
        Grid::new("quality-grid")
            .num_columns(2)
            .spacing([12.0, 2.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Background").strong());
                ui.label(format!("{:.0}", quality.background));
                ui.end_row();
                ui.label(RichText::new("Noise").strong());
                ui.label(format!("{:.1}", quality.noise));
                ui.end_row();
                ui.label(RichText::new("Sharpness").strong());
                ui.label(format!("{:.2}", quality.sharpness));
                ui.end_row();
            });
        ui.label(
            RichText::new(
                "Compares within this folder only. Sharpness near 1 is mostly \
                 noise; higher means more structure.",
            )
            .weak()
            .small(),
        );
        ui.separator();
    }

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
            format!("{} cards", cards.rows.len())
        } else {
            format!(
                "{} of {} cards",
                cards.rows.len(),
                loaded.image.header.cards.len()
            )
        })
        .weak()
        .small(),
    );

    if cards.rows.is_empty() {
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
                    for (index, (keyword, value)) in cards.rows.iter().enumerate() {
                        // A rule between the keywords worth seeing first and
                        // everything else the file happens to carry.
                        if index == cards.pinned && cards.pinned > 0 {
                            ui.separator();
                            ui.separator();
                            ui.end_row();
                        }
                        ui.label(RichText::new(*keyword).monospace().strong());
                        ui.label(RichText::new(*value).monospace());
                        ui.end_row();
                    }
                });
        });

    actions
}

/// The keywords worth seeing first, in the order they are shown.
///
/// A capture program writes dozens of cards, most of them uninteresting. These
/// are the ones that answer "what is this frame", and they are pinned above the
/// rest rather than left to be hunted for in whatever order the file happened
/// to store them.
pub const PINNED: &[&str] = &[
    "OBJECT", "TELESCOP", "CAMERAID", "IMAGETYP", "FILTER", "EXPOSURE", "GAIN", "CCD_TEMP",
    "BAYERPAT", "DATE-OBS",
];

/// The header cards to show, in the order to show them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cards<'a> {
    /// Keyword and value pairs, pinned ones first.
    pub rows: Vec<(&'a str, &'a str)>,
    /// How many of the leading rows came from [`PINNED`], so the interface can
    /// separate them from the rest.
    pub pinned: usize,
}

/// Compares keywords the way headers are written rather than byte for byte.
///
/// Case varies, and so does the separator: the same value appears as
/// `CCD-TEMP` in one program's files and `CCD_TEMP` in another's. Treating the
/// two as the same keyword means a pinned entry works whichever a camera wrote.
fn same_keyword(a: &str, b: &str) -> bool {
    let normalise = |k: &str| {
        k.trim()
            .chars()
            .map(|c| match c.to_ascii_uppercase() {
                '_' => '-',
                other => other,
            })
            .collect::<String>()
    };
    normalise(a) == normalise(b)
}

/// The header cards matching a filter, with the interesting ones first.
///
/// Pinned keywords come first in the order [`PINNED`] lists them, skipping any
/// the file does not carry. Everything else follows in the order the file wrote
/// it, which is meaningful: it is how the capture program grouped them.
#[must_use]
pub fn matching_cards<'a>(header: &'a fits_core::FitsHeader, filter: &str) -> Cards<'a> {
    let needle = filter.trim().to_ascii_lowercase();
    let matches = |keyword: &str, value: &str| {
        needle.is_empty()
            || keyword.to_ascii_lowercase().contains(&needle)
            || value.to_ascii_lowercase().contains(&needle)
    };

    let mut rows: Vec<(&str, &str)> = Vec::with_capacity(header.cards.len());

    for wanted in PINNED {
        for (keyword, value) in &header.cards {
            if same_keyword(keyword, wanted) && matches(keyword, value) {
                rows.push((keyword.as_str(), value.as_str()));
            }
        }
    }
    let pinned = rows.len();

    for (keyword, value) in &header.cards {
        if PINNED.iter().any(|p| same_keyword(keyword, p)) {
            continue;
        }
        if matches(keyword, value) {
            rows.push((keyword.as_str(), value.as_str()));
        }
    }

    Cards { rows, pinned }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::FitsHeader;

    fn header_of(cards: &[(&str, &str)]) -> FitsHeader {
        FitsHeader {
            cards: cards
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    /// A header in the order a capture program might have written it, which is
    /// nothing like the order worth reading it in.
    fn header() -> FitsHeader {
        header_of(&[
            ("BITPIX", "16"),
            ("NAXIS", "2"),
            ("DATE-OBS", "2026-09-03T21:14:02"),
            ("GAIN", "120"),
            ("SWCREATE", "'Capture 1.0'"),
            ("EXPOSURE", "300.0"),
            ("CCD-TEMP", "-10.5"),
            ("OBJECT", "M31"),
            ("FILTER", "Ha"),
            ("TELESCOP", "'RC8'"),
            ("BAYERPAT", "RGGB"),
            ("IMAGETYP", "LIGHT"),
            ("CAMERAID", "'ASI2600'"),
        ])
    }

    fn keywords<'a>(cards: &Cards<'a>) -> Vec<&'a str> {
        cards.rows.iter().map(|(k, _)| *k).collect()
    }

    #[test]
    fn the_pinned_keywords_come_first_in_the_order_asked_for() {
        let h = header();
        let cards = matching_cards(&h, "");
        assert_eq!(
            &keywords(&cards)[..cards.pinned],
            &[
                "OBJECT", "TELESCOP", "CAMERAID", "IMAGETYP", "FILTER", "EXPOSURE", "GAIN",
                "CCD-TEMP", "BAYERPAT", "DATE-OBS",
            ]
        );
        assert_eq!(cards.pinned, 10);
    }

    #[test]
    fn an_underscore_and_a_hyphen_name_the_same_keyword() {
        // One program writes CCD-TEMP, another CCD_TEMP, and both should pin.
        for spelling in ["CCD-TEMP", "CCD_TEMP", "ccd_temp"] {
            let h = header_of(&[("BITPIX", "16"), (spelling, "-10.5")]);
            let cards = matching_cards(&h, "");
            assert_eq!(cards.pinned, 1, "spelling {spelling}");
            assert_eq!(cards.rows[0].0, spelling, "the file's own spelling is kept");
        }
    }

    #[test]
    fn pinned_keywords_the_file_lacks_are_skipped_without_leaving_a_gap() {
        let h = header_of(&[("OBJECT", "M31"), ("GAIN", "120"), ("BITPIX", "16")]);
        let cards = matching_cards(&h, "");
        assert_eq!(cards.pinned, 2);
        assert_eq!(keywords(&cards), vec!["OBJECT", "GAIN", "BITPIX"]);
    }

    #[test]
    fn everything_else_keeps_the_order_the_file_wrote_it_in() {
        // Header order is meaningful: it is how the capture program grouped it.
        let h = header();
        let cards = matching_cards(&h, "");
        assert_eq!(
            &keywords(&cards)[cards.pinned..],
            &["BITPIX", "NAXIS", "SWCREATE"]
        );
    }

    #[test]
    fn a_header_with_nothing_pinned_is_left_in_its_own_order() {
        let h = header_of(&[("BITPIX", "16"), ("NAXIS", "2"), ("SWCREATE", "x")]);
        let cards = matching_cards(&h, "");
        assert_eq!(cards.pinned, 0);
        assert_eq!(keywords(&cards), vec!["BITPIX", "NAXIS", "SWCREATE"]);
    }

    #[test]
    fn every_card_appears_exactly_once() {
        let h = header();
        let cards = matching_cards(&h, "");
        assert_eq!(cards.rows.len(), h.cards.len(), "none lost or duplicated");

        let mut seen: Vec<&str> = keywords(&cards);
        seen.sort_unstable();
        let mut expected: Vec<&str> = h.cards.iter().map(|(k, _)| k.as_str()).collect();
        expected.sort_unstable();
        assert_eq!(seen, expected);
    }

    #[test]
    fn an_empty_filter_shows_every_card() {
        let h = header();
        assert_eq!(matching_cards(&h, "").rows.len(), h.cards.len());
        assert_eq!(matching_cards(&h, "   ").rows.len(), h.cards.len());
    }

    #[test]
    fn filtering_matches_keywords_case_insensitively() {
        let h = header();
        let cards = matching_cards(&h, "temp");
        assert_eq!(keywords(&cards), vec!["CCD-TEMP"]);
    }

    #[test]
    fn filtering_matches_values_too() {
        // So that looking for a target finds the card naming it.
        let h = header();
        let cards = matching_cards(&h, "m31");
        assert_eq!(keywords(&cards), vec!["OBJECT"]);
    }

    #[test]
    fn filtering_keeps_the_pinned_ones_first() {
        let h = header();
        // Matches several pinned keywords and two unpinned ones.
        let cards = matching_cards(&h, "a");
        let names = keywords(&cards);
        assert!(cards.pinned > 0, "{names:?}");

        // The matching pinned keywords lead, still in the order PINNED gives.
        assert_eq!(
            &names[..cards.pinned],
            &["CAMERAID", "IMAGETYP", "FILTER", "GAIN", "BAYERPAT", "DATE-OBS"],
            "pinned order should survive filtering"
        );
        // Everything after is unpinned, in the file's own order.
        assert_eq!(&names[cards.pinned..], &["NAXIS", "SWCREATE"]);
    }

    #[test]
    fn a_filter_matching_nothing_returns_nothing() {
        let h = header();
        let cards = matching_cards(&h, "zzz");
        assert!(cards.rows.is_empty());
        assert_eq!(cards.pinned, 0);
    }

    #[test]
    fn the_pinned_list_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for keyword in PINNED {
            assert!(seen.insert(*keyword), "{keyword} is listed twice");
        }
    }
}

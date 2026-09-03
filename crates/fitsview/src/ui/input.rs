//! Turning raw input into [`Action`]s.
//!
//! Kept apart from drawing so the mapping from a key or a gesture to a
//! behaviour can be tested directly, without an event loop.

use egui::{Key, Modifiers, Pos2, Vec2};

use crate::app::Action;

/// How much one wheel notch changes the zoom.
///
/// Chosen so a normal scroll feels responsive without overshooting; a full
/// wheel turn moves roughly one power of two.
pub const ZOOM_PER_SCROLL_UNIT: f32 = 0.0015;

/// A summary of the input for one frame, in a form that can be built by hand
/// in a test.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    /// Keys pressed this frame, each with the modifiers held at the time.
    pub keys: Vec<(Key, Modifiers)>,
    /// Accumulated wheel movement.
    pub scroll_delta: f32,
    /// Where the pointer is, if it is over the image area.
    pub pointer: Option<Pos2>,
    /// Drag movement in screen pixels, when a drag is in progress.
    pub drag_delta: Vec2,
    /// Files dropped on the window this frame.
    pub dropped: Vec<std::path::PathBuf>,
    /// True while a dialog or the rename editor owns the keyboard.
    ///
    /// Navigation and destructive shortcuts are suppressed then, so that typing
    /// a new file name cannot delete files or jump through the folder.
    pub modal: bool,
}

/// Converts a frame of input into the actions it implies.
///
/// The order matters: a dropped file is handled before view changes, so that
/// dropping and scrolling in the same frame does not zoom the outgoing image.
///
/// While a dialog or the rename editor is open, only Escape is honoured. Every
/// other shortcut is suppressed, because the keystrokes belong to the editor
/// and because `d` in a file name must never mean delete.
#[must_use]
pub fn actions_for(frame: &Frame) -> Vec<Action> {
    let mut out = Vec::new();

    if frame.modal {
        if frame.keys.iter().any(|(k, _)| *k == Key::Escape) {
            out.push(Action::Cancel);
        }
        return out;
    }

    // Only the last dropped file is opened. Opening several at once would need
    // multi-selection, which this application does not have.
    if let Some(path) = frame.dropped.last() {
        out.push(Action::Open(path.clone()));
    }

    for (key, modifiers) in &frame.keys {
        match key {
            // Navigation. Space steps forward because culling is a
            // one-hand-on-the-keyboard job. Down and up move through the file
            // list the way its own order reads, which is what anyone who has
            // just clicked a row expects next.
            Key::ArrowRight | Key::ArrowDown | Key::Space | Key::PageDown => {
                out.push(Action::NextFile);
            }
            Key::ArrowLeft | Key::ArrowUp | Key::PageUp => out.push(Action::PreviousFile),
            Key::Home => out.push(Action::FirstFile),
            Key::End => out.push(Action::LastFile),
            Key::F5 => out.push(Action::Rescan),
            // File management. Backspace is included because that is what
            // deletes a file on macOS.
            Key::Delete | Key::Backspace => {
                // Shift is the deliberate second gesture that gets past the
                // confirmation for a file marked to keep.
                if modifiers.shift {
                    out.push(Action::ConfirmDelete);
                } else {
                    out.push(Action::RequestDelete);
                }
            }
            Key::K => out.push(Action::ToggleFlag),
            Key::F2 => out.push(Action::BeginRename),
            // View.
            Key::S if modifiers.shift => out.push(Action::ToggleStars),
            Key::S => out.push(Action::ToggleStretch),
            Key::D => out.push(Action::ToggleApplyDark),
            Key::B => out.push(Action::ToggleDebayer),
            Key::I => out.push(Action::ToggleHeader),
            Key::G => out.push(Action::ToggleHistogram),
            Key::L => out.push(Action::ToggleFileList),
            // Plain F fits the image to the window, so the flat takes Shift+F
            // rather than stealing a key people use constantly.
            Key::F if modifiers.shift => out.push(Action::ToggleApplyFlat),
            Key::F => out.push(Action::FitToWindow),
            Key::Num1 => out.push(Action::ActualSize),
            Key::Questionmark | Key::H => out.push(Action::ToggleHelp),
            Key::Escape => out.push(Action::ClearError),
            _ => {}
        }
    }

    if frame.drag_delta != Vec2::ZERO {
        out.push(Action::Pan(frame.drag_delta));
    }

    if frame.scroll_delta != 0.0 {
        if let Some(anchor) = frame.pointer {
            // Exponential so that zooming in and back out by the same amount
            // returns to where it started.
            let factor = (frame.scroll_delta * ZOOM_PER_SCROLL_UNIT).exp();
            if factor.is_finite() && factor > 0.0 {
                out.push(Action::ZoomAt { anchor, factor });
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn frame() -> Frame {
        Frame::default()
    }

    /// One key press with no modifiers.
    fn key(k: Key) -> Vec<(Key, Modifiers)> {
        vec![(k, Modifiers::NONE)]
    }

    /// Several key presses with no modifiers.
    fn keys(list: &[Key]) -> Vec<(Key, Modifiers)> {
        list.iter().map(|k| (*k, Modifiers::NONE)).collect()
    }

    #[test]
    fn an_empty_frame_produces_no_actions() {
        assert!(actions_for(&frame()).is_empty());
    }

    #[test]
    fn f_fits_and_one_shows_actual_size() {
        let f = Frame {
            keys: key(Key::F),
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::FitToWindow]);

        let f = Frame {
            keys: key(Key::Num1),
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::ActualSize]);
    }

    #[test]
    fn escape_clears_the_error() {
        let f = Frame {
            keys: key(Key::Escape),
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::ClearError]);
    }

    #[test]
    fn unmapped_keys_do_nothing() {
        let f = Frame {
            keys: keys(&[Key::Q, Key::Z, Key::W]),
            ..frame()
        };
        assert!(actions_for(&f).is_empty());
    }

    #[test]
    fn every_navigation_key_maps_to_its_action() {
        let cases = [
            (Key::ArrowRight, Action::NextFile),
            (Key::ArrowDown, Action::NextFile),
            (Key::Space, Action::NextFile),
            (Key::PageDown, Action::NextFile),
            (Key::ArrowLeft, Action::PreviousFile),
            (Key::ArrowUp, Action::PreviousFile),
            (Key::PageUp, Action::PreviousFile),
            (Key::Home, Action::FirstFile),
            (Key::End, Action::LastFile),
            (Key::F5, Action::Rescan),
        ];
        for (k, expected) in cases {
            let f = Frame {
                keys: key(k),
                ..frame()
            };
            assert_eq!(actions_for(&f), vec![expected], "key {k:?}");
        }
    }

    #[test]
    fn holding_a_navigation_key_produces_one_action_per_reported_press() {
        // The event collector filters out auto-repeat, so several presses in a
        // frame means the user really pressed it several times.
        let f = Frame {
            keys: keys(&[Key::ArrowRight, Key::ArrowRight, Key::ArrowRight]),
            ..frame()
        };
        assert_eq!(actions_for(&f).len(), 3);
    }

    #[test]
    fn file_management_keys_map_to_their_actions() {
        for (k, expected) in [
            (Key::K, Action::ToggleFlag),
            (Key::Delete, Action::RequestDelete),
            (Key::Backspace, Action::RequestDelete),
            (Key::F2, Action::BeginRename),
        ] {
            let f = Frame {
                keys: key(k),
                ..frame()
            };
            assert_eq!(actions_for(&f), vec![expected], "key {k:?}");
        }
    }

    #[test]
    fn shift_delete_skips_the_confirmation() {
        // The deliberate second gesture for a file marked to keep.
        for k in [Key::Delete, Key::Backspace] {
            let f = Frame {
                keys: vec![(k, Modifiers::SHIFT)],
                ..frame()
            };
            assert_eq!(actions_for(&f), vec![Action::ConfirmDelete], "key {k:?}");
        }
    }

    #[test]
    fn plain_delete_asks_rather_than_deleting_outright() {
        let f = Frame {
            keys: key(Key::Delete),
            ..frame()
        };
        assert_eq!(
            actions_for(&f),
            vec![Action::RequestDelete],
            "an unmodified Delete must go through the confirmation path"
        );
    }

    #[test]
    fn shift_f_toggles_the_flat_while_plain_f_still_fits() {
        let plain = Frame {
            keys: key(Key::F),
            ..frame()
        };
        assert_eq!(actions_for(&plain), vec![Action::FitToWindow]);

        let shifted = Frame {
            keys: vec![(Key::F, Modifiers::SHIFT)],
            ..frame()
        };
        assert_eq!(actions_for(&shifted), vec![Action::ToggleApplyFlat]);
    }

    #[test]
    fn shift_s_toggles_the_stars_while_plain_s_still_stretches() {
        let plain = Frame {
            keys: key(Key::S),
            ..frame()
        };
        assert_eq!(actions_for(&plain), vec![Action::ToggleStretch]);

        let shifted = Frame {
            keys: vec![(Key::S, Modifiers::SHIFT)],
            ..frame()
        };
        assert_eq!(actions_for(&shifted), vec![Action::ToggleStars]);
    }

    #[test]
    fn the_vertical_arrows_move_through_the_file_list() {
        // Down moves down the list, which is the next file, and up the reverse.
        let down = Frame {
            keys: key(Key::ArrowDown),
            ..frame()
        };
        assert_eq!(actions_for(&down), vec![Action::NextFile]);

        let up = Frame {
            keys: key(Key::ArrowUp),
            ..frame()
        };
        assert_eq!(actions_for(&up), vec![Action::PreviousFile]);
    }

    #[test]
    fn l_collapses_the_file_list() {
        let f = Frame {
            keys: key(Key::L),
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::ToggleFileList]);
    }

    #[test]
    fn b_toggles_colour_reconstruction() {
        let f = Frame {
            keys: key(Key::B),
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::ToggleDebayer]);
    }

    #[test]
    fn g_toggles_the_histogram() {
        let f = Frame {
            keys: key(Key::G),
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::ToggleHistogram]);
    }

    #[test]
    fn i_toggles_the_header_panel() {
        let f = Frame {
            keys: key(Key::I),
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::ToggleHeader]);
    }

    #[test]
    fn d_toggles_dark_calibration() {
        let f = Frame {
            keys: key(Key::D),
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::ToggleApplyDark]);
    }

    #[test]
    fn s_toggles_the_stretch() {
        let f = Frame {
            keys: key(Key::S),
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::ToggleStretch]);
    }

    #[test]
    fn either_help_key_toggles_the_overlay() {
        for k in [Key::Questionmark, Key::H] {
            let f = Frame {
                keys: key(k),
                ..frame()
            };
            assert_eq!(actions_for(&f), vec![Action::ToggleHelp], "key {k:?}");
        }
    }

    #[test]
    fn a_modal_swallows_every_shortcut_except_escape() {
        // Typing a file name must never delete files or jump through the
        // folder, which is what would happen if these leaked through.
        let dangerous = keys(&[
            Key::Delete,
            Key::Backspace,
            Key::K,
            Key::D,
            Key::S,
            Key::F,
            Key::L,
            Key::B,
            Key::G,
            Key::ArrowRight,
            Key::ArrowDown,
            Key::ArrowUp,
            Key::Home,
            Key::F5,
            Key::F2,
        ]);
        let f = Frame {
            keys: dangerous,
            scroll_delta: 40.0,
            pointer: Some(Pos2::ZERO),
            drag_delta: Vec2::new(3.0, 3.0),
            dropped: vec![PathBuf::from("/tmp/a.fits")],
            modal: true,
        };
        assert!(
            actions_for(&f).is_empty(),
            "no shortcut may act while a dialog is open: {:?}",
            actions_for(&f)
        );
    }

    #[test]
    fn escape_still_cancels_while_a_modal_is_open() {
        let f = Frame {
            keys: key(Key::Escape),
            modal: true,
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::Cancel]);
    }

    #[test]
    fn escape_outside_a_modal_dismisses_the_error_instead() {
        let f = Frame {
            keys: key(Key::Escape),
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::ClearError]);
    }

    #[test]
    fn dragging_pans_by_the_drag_delta() {
        let f = Frame {
            drag_delta: Vec2::new(12.0, -4.0),
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::Pan(Vec2::new(12.0, -4.0))]);
    }

    #[test]
    fn scrolling_without_a_pointer_position_does_nothing() {
        // The pointer can be outside the image area, where a zoom has no anchor.
        let f = Frame {
            scroll_delta: 50.0,
            pointer: None,
            ..frame()
        };
        assert!(actions_for(&f).is_empty());
    }

    #[test]
    fn scrolling_up_zooms_in_and_down_zooms_out() {
        let anchor = Pos2::new(100.0, 100.0);
        let up = actions_for(&Frame {
            scroll_delta: 100.0,
            pointer: Some(anchor),
            ..frame()
        });
        let down = actions_for(&Frame {
            scroll_delta: -100.0,
            pointer: Some(anchor),
            ..frame()
        });

        match (up.first(), down.first()) {
            (
                Some(Action::ZoomAt { factor: fin, .. }),
                Some(Action::ZoomAt { factor: fout, .. }),
            ) => {
                assert!(*fin > 1.0, "scrolling up should magnify, got {fin}");
                assert!(*fout < 1.0, "scrolling down should shrink, got {fout}");
            }
            other => panic!("expected two zoom actions, got {other:?}"),
        }
    }

    #[test]
    fn equal_and_opposite_scrolls_cancel_out() {
        // Exponential scaling makes zoom reversible; a linear factor would not.
        let anchor = Pos2::new(0.0, 0.0);
        let get = |d: f32| match actions_for(&Frame {
            scroll_delta: d,
            pointer: Some(anchor),
            ..frame()
        })
        .first()
        {
            Some(Action::ZoomAt { factor, .. }) => *factor,
            other => panic!("expected a zoom, got {other:?}"),
        };
        let product = get(75.0) * get(-75.0);
        assert!((product - 1.0).abs() < 1e-5, "product was {product}");
    }

    #[test]
    fn a_dropped_file_is_opened() {
        let f = Frame {
            dropped: vec![PathBuf::from("/tmp/a.fits")],
            ..frame()
        };
        assert_eq!(
            actions_for(&f),
            vec![Action::Open(PathBuf::from("/tmp/a.fits"))]
        );
    }

    #[test]
    fn dropping_several_files_opens_the_last_one() {
        let f = Frame {
            dropped: vec![PathBuf::from("/tmp/a.fits"), PathBuf::from("/tmp/b.fits")],
            ..frame()
        };
        assert_eq!(
            actions_for(&f),
            vec![Action::Open(PathBuf::from("/tmp/b.fits"))]
        );
    }

    #[test]
    fn a_drop_is_handled_before_view_changes_in_the_same_frame() {
        let f = Frame {
            dropped: vec![PathBuf::from("/tmp/a.fits")],
            scroll_delta: 50.0,
            pointer: Some(Pos2::new(1.0, 1.0)),
            drag_delta: Vec2::new(5.0, 5.0),
            keys: key(Key::F),
            modal: false,
        };
        let actions = actions_for(&f);
        assert!(
            matches!(actions.first(), Some(Action::Open(_))),
            "open must come first, got {actions:?}"
        );
        assert_eq!(actions.len(), 4);
    }

    #[test]
    fn a_nonsense_scroll_value_does_not_produce_a_broken_zoom() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let f = Frame {
                scroll_delta: bad,
                pointer: Some(Pos2::ZERO),
                ..frame()
            };
            for a in actions_for(&f) {
                if let Action::ZoomAt { factor, .. } = a {
                    assert!(factor.is_finite() && factor > 0.0, "bad factor {factor}");
                }
            }
        }
    }
}

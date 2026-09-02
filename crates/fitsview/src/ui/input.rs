//! Turning raw input into [`Action`]s.
//!
//! Kept apart from drawing so the mapping from a key or a gesture to a
//! behaviour can be tested directly, without an event loop.

use egui::{Key, Pos2, Vec2};

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
    /// Keys pressed this frame.
    pub keys: Vec<Key>,
    /// Accumulated wheel movement.
    pub scroll_delta: f32,
    /// Where the pointer is, if it is over the image area.
    pub pointer: Option<Pos2>,
    /// Drag movement in screen pixels, when a drag is in progress.
    pub drag_delta: Vec2,
    /// Files dropped on the window this frame.
    pub dropped: Vec<std::path::PathBuf>,
}

/// Converts a frame of input into the actions it implies.
///
/// The order matters: a dropped file is handled before view changes, so that
/// dropping and scrolling in the same frame does not zoom the outgoing image.
#[must_use]
pub fn actions_for(frame: &Frame) -> Vec<Action> {
    let mut out = Vec::new();

    // Only the last dropped file is opened. Opening several at once would need
    // the folder model, which arrives in Phase 3.
    if let Some(path) = frame.dropped.last() {
        out.push(Action::Open(path.clone()));
    }

    for key in &frame.keys {
        match key {
            Key::F => out.push(Action::FitToWindow),
            Key::Num1 => out.push(Action::ActualSize),
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

    #[test]
    fn an_empty_frame_produces_no_actions() {
        assert!(actions_for(&frame()).is_empty());
    }

    #[test]
    fn f_fits_and_one_shows_actual_size() {
        let f = Frame {
            keys: vec![Key::F],
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::FitToWindow]);

        let f = Frame {
            keys: vec![Key::Num1],
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::ActualSize]);
    }

    #[test]
    fn escape_clears_the_error() {
        let f = Frame {
            keys: vec![Key::Escape],
            ..frame()
        };
        assert_eq!(actions_for(&f), vec![Action::ClearError]);
    }

    #[test]
    fn unmapped_keys_do_nothing() {
        let f = Frame {
            keys: vec![Key::Q, Key::Z, Key::Space],
            ..frame()
        };
        assert!(actions_for(&f).is_empty());
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
            keys: vec![Key::F],
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

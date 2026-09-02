//! Zoom and pan state.
//!
//! This is deliberately plain arithmetic over `egui`'s vector types, with no
//! reference to a context, a window or a texture. That keeps every rule about
//! how the image moves under the cursor testable without opening a window,
//! which is where the fiddly bugs in an image viewer actually live.

use egui::{Pos2, Rect, Vec2};

/// Smallest and largest zoom, in screen pixels per image pixel.
///
/// The lower bound stops a huge image from collapsing to nothing; the upper
/// bound stops a scroll gesture from running away into meaningless
/// magnification.
pub const MIN_ZOOM: f32 = 0.01;
/// See [`MIN_ZOOM`].
pub const MAX_ZOOM: f32 = 64.0;

/// How the image is currently placed on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewState {
    /// Screen pixels per image pixel. 1.0 means one image pixel per screen pixel.
    pub zoom: f32,
    /// Where the top-left corner of the image sits, in screen coordinates.
    pub origin: Pos2,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            origin: Pos2::ZERO,
        }
    }
}

impl ViewState {
    /// Places `image` centred in `viewport` at the largest zoom that fits it
    /// entirely.
    ///
    /// Never zooms past 1:1, because blowing a small image up to fill the
    /// window on open is more surprising than useful.
    #[must_use]
    pub fn fit(image: Vec2, viewport: Rect) -> Self {
        if image.x <= 0.0 || image.y <= 0.0 {
            return Self::default();
        }
        let scale = (viewport.width() / image.x)
            .min(viewport.height() / image.y)
            .min(1.0)
            .clamp(MIN_ZOOM, MAX_ZOOM);
        Self::centred(image, viewport, scale)
    }

    /// Centres `image` in `viewport` at exactly `zoom`.
    #[must_use]
    pub fn centred(image: Vec2, viewport: Rect, zoom: f32) -> Self {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let scaled = image * zoom;
        Self {
            zoom,
            origin: viewport.center() - scaled / 2.0,
        }
    }

    /// The rectangle the image occupies on screen.
    #[must_use]
    pub fn image_rect(&self, image: Vec2) -> Rect {
        Rect::from_min_size(self.origin, image * self.zoom)
    }

    /// Converts a screen position to image coordinates.
    ///
    /// The result is in image pixels and may fall outside the image.
    #[must_use]
    pub fn screen_to_image(&self, screen: Pos2) -> Vec2 {
        (screen - self.origin) / self.zoom
    }

    /// Converts image coordinates to a screen position.
    #[must_use]
    pub fn image_to_screen(&self, image: Vec2) -> Pos2 {
        self.origin + image * self.zoom
    }

    /// Moves the image by a screen-space delta.
    pub fn pan(&mut self, delta: Vec2) {
        self.origin += delta;
    }

    /// Changes zoom while keeping whatever is under `anchor` under `anchor`.
    ///
    /// This is the behaviour that makes wheel zoom feel right: the pixel you
    /// point at stays put. Getting it wrong is subtle enough to be worth its
    /// own test.
    pub fn zoom_about(&mut self, anchor: Pos2, factor: f32) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        // The image point under the anchor, before the change.
        let image_point = self.screen_to_image(anchor);
        self.zoom = new_zoom;
        // Put that same image point back under the anchor.
        self.origin = anchor - image_point * new_zoom;
    }

    /// Sets zoom to exactly `zoom`, keeping the centre of `viewport` fixed.
    pub fn set_zoom_about_centre(&mut self, viewport: Rect, zoom: f32) {
        let factor = zoom / self.zoom;
        self.zoom_about(viewport.center(), factor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.001
    }

    #[test]
    fn fit_scales_a_large_image_down_to_the_viewport() {
        // 2000x1000 into 1000x800: width is the binding constraint, so 0.5.
        let v = ViewState::fit(Vec2::new(2000.0, 1000.0), viewport());
        assert!(approx(v.zoom, 0.5), "zoom was {}", v.zoom);
        let r = v.image_rect(Vec2::new(2000.0, 1000.0));
        assert!(r.width() <= viewport().width() + 0.001);
        assert!(r.height() <= viewport().height() + 0.001);
    }

    #[test]
    fn fit_uses_the_binding_dimension() {
        // A tall image is limited by height, not width.
        let v = ViewState::fit(Vec2::new(1000.0, 4000.0), viewport());
        assert!(approx(v.zoom, 0.2), "zoom was {}", v.zoom);
    }

    #[test]
    fn fit_does_not_enlarge_a_small_image() {
        let v = ViewState::fit(Vec2::new(100.0, 100.0), viewport());
        assert!(approx(v.zoom, 1.0), "zoom was {}", v.zoom);
    }

    #[test]
    fn fit_centres_the_image() {
        let image = Vec2::new(2000.0, 1000.0);
        let v = ViewState::fit(image, viewport());
        let r = v.image_rect(image);
        assert!(approx(r.center().x, viewport().center().x));
        assert!(approx(r.center().y, viewport().center().y));
    }

    #[test]
    fn fit_handles_a_degenerate_image_without_dividing_by_zero() {
        let v = ViewState::fit(Vec2::ZERO, viewport());
        assert!(v.zoom.is_finite() && v.zoom > 0.0);
        let v = ViewState::fit(Vec2::new(-5.0, 10.0), viewport());
        assert!(v.zoom.is_finite() && v.zoom > 0.0);
    }

    #[test]
    fn screen_and_image_coordinates_round_trip() {
        let v = ViewState {
            zoom: 2.5,
            origin: Pos2::new(37.0, -11.0),
        };
        for p in [
            Pos2::new(0.0, 0.0),
            Pos2::new(500.0, 400.0),
            Pos2::new(-20.0, 900.0),
        ] {
            let back = v.image_to_screen(v.screen_to_image(p));
            assert!(
                approx(back.x, p.x) && approx(back.y, p.y),
                "{p:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn panning_moves_the_image_by_exactly_the_delta() {
        let mut v = ViewState {
            zoom: 1.5,
            origin: Pos2::new(10.0, 20.0),
        };
        v.pan(Vec2::new(5.0, -7.0));
        assert_eq!(v.origin, Pos2::new(15.0, 13.0));
        // Panning must not change zoom.
        assert!(approx(v.zoom, 1.5));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor_fixed() {
        // This is the property that makes wheel zoom feel correct.
        let mut v = ViewState {
            zoom: 1.0,
            origin: Pos2::new(0.0, 0.0),
        };
        let anchor = Pos2::new(300.0, 200.0);
        let before = v.screen_to_image(anchor);

        v.zoom_about(anchor, 1.25);
        let after = v.screen_to_image(anchor);

        assert!(
            approx(before.x, after.x) && approx(before.y, after.y),
            "image point moved: {before:?} -> {after:?}"
        );
        assert!(approx(v.zoom, 1.25));
    }

    #[test]
    fn zooming_keeps_the_anchor_fixed_across_repeated_gestures() {
        // A real scroll is many small steps; drift would accumulate.
        let mut v = ViewState {
            zoom: 0.3,
            origin: Pos2::new(-120.0, 45.0),
        };
        let anchor = Pos2::new(640.0, 360.0);
        let before = v.screen_to_image(anchor);
        for _ in 0..40 {
            v.zoom_about(anchor, 1.1);
        }
        let after = v.screen_to_image(anchor);
        assert!(
            (before.x - after.x).abs() < 0.5 && (before.y - after.y).abs() < 0.5,
            "drifted: {before:?} -> {after:?}"
        );
    }

    #[test]
    fn zoom_is_clamped_at_both_ends() {
        let mut v = ViewState::default();
        for _ in 0..200 {
            v.zoom_about(Pos2::new(10.0, 10.0), 2.0);
        }
        assert!(approx(v.zoom, MAX_ZOOM), "zoom was {}", v.zoom);

        for _ in 0..400 {
            v.zoom_about(Pos2::new(10.0, 10.0), 0.5);
        }
        assert!(approx(v.zoom, MIN_ZOOM), "zoom was {}", v.zoom);
    }

    #[test]
    fn a_nonsense_zoom_factor_is_ignored_rather_than_poisoning_the_state() {
        let mut v = ViewState::default();
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            v.zoom_about(Pos2::new(10.0, 10.0), bad);
            assert!(
                v.zoom.is_finite() && v.zoom > 0.0,
                "factor {bad} broke zoom"
            );
            assert!(v.origin.x.is_finite() && v.origin.y.is_finite());
        }
    }

    #[test]
    fn setting_zoom_about_the_centre_keeps_the_centre_fixed() {
        let image = Vec2::new(2000.0, 1000.0);
        let mut v = ViewState::fit(image, viewport());
        let centre_before = v.screen_to_image(viewport().center());

        v.set_zoom_about_centre(viewport(), 1.0);

        assert!(approx(v.zoom, 1.0));
        let centre_after = v.screen_to_image(viewport().center());
        assert!(
            approx(centre_before.x, centre_after.x) && approx(centre_before.y, centre_after.y),
            "{centre_before:?} -> {centre_after:?}"
        );
    }

    #[test]
    fn image_rect_reflects_zoom() {
        let image = Vec2::new(100.0, 50.0);
        let v = ViewState {
            zoom: 3.0,
            origin: Pos2::new(7.0, 9.0),
        };
        let r = v.image_rect(image);
        assert_eq!(r.min, Pos2::new(7.0, 9.0));
        assert!(approx(r.width(), 300.0) && approx(r.height(), 150.0));
    }
}

//! The central image area: draws the texture and collects gestures.

use egui::{
    pos2, Align2, CentralPanel, Color32, FontId, Frame as PanelFrame, Rect, Sense, Stroke,
    TextureHandle, Ui, Vec2,
};

use crate::app::{Action, Model};
use crate::texture::DetailRegion;
use crate::ui::input::{actions_for, Frame};

/// Background behind the image. Dark, because astronomical images are.
const BACKDROP: Color32 = Color32::from_gray(16);

/// Draws the image and returns whatever this frame's input implies.
pub fn show(
    ui: &mut Ui,
    model: &mut Model,
    texture: Option<&TextureHandle>,
    detail: Option<&(DetailRegion, TextureHandle)>,
) -> Vec<Action> {
    let mut actions = Vec::new();

    CentralPanel::default()
        .frame(PanelFrame::NONE.fill(BACKDROP))
        .show(ui, |ui| {
            let viewport = ui.max_rect();
            model.set_viewport(viewport);

            let response = ui.allocate_rect(viewport, Sense::click_and_drag());

            match (texture, model.loaded.as_ref()) {
                (Some(texture), Some(loaded)) => {
                    let rect = model.view.image_rect(loaded.size());
                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    // The texture already holds the flipped, downsampled image,
                    // so it is drawn with the full UV range.
                    ui.painter().image(texture.id(), rect, uv, Color32::WHITE);

                    // Over the top, where one exists, the same region at full
                    // resolution. Drawing the overview underneath means a pan
                    // shows something immediately rather than a gap while the
                    // detail texture is rebuilt.
                    if let Some((region, handle)) = detail {
                        ui.painter().image(
                            handle.id(),
                            region.screen_rect(&model.view),
                            uv,
                            Color32::WHITE,
                        );
                    }

                    draw_stars(ui, model, loaded.image.height);
                }
                _ => {
                    let message = if model.arriving.is_some() {
                        "Still being written…"
                    } else if model.loading {
                        "Loading…"
                    } else {
                        "Drop a FITS file or folder here, or use Open File…"
                    };
                    ui.painter().text(
                        viewport.center(),
                        Align2::CENTER_CENTER,
                        message,
                        FontId::proportional(16.0),
                        Color32::from_gray(140),
                    );
                }
            }

            // Recorded for the status bar, which needs it after this panel has
            // finished drawing.
            model.pointer = ui
                .ctx()
                .input(|i| i.pointer.hover_pos())
                .filter(|p| viewport.contains(*p));

            let modal = model.pending != crate::app::Pending::None;
            actions.extend(actions_for(&collect_input(ui, &response, viewport, modal)));
        });

    actions
}

/// Marks each detected star, sized to its measured width.
///
/// Drawing them is what makes a bad detection obvious rather than hidden inside
/// a number: a circle around a hot pixel or a galaxy is visible at a glance.
fn draw_stars(ui: &Ui, model: &Model, image_height: usize) {
    let Some(field) = model.stars.as_ref() else {
        return;
    };
    if !model.stars_enabled {
        return;
    }

    let painter = ui.painter();
    let ordinary = Color32::from_rgb(120, 220, 140);
    let saturated = Color32::from_rgb(240, 170, 90);

    for star in &field.stars {
        // Detection works in the file's row order; the picture is flipped.
        #[allow(clippy::cast_precision_loss)]
        let display_y = image_height as f64 - 1.0 - star.y;
        #[allow(clippy::cast_possible_truncation)]
        let centre = model
            .view
            .image_to_screen(Vec2::new(star.x as f32, display_y as f32));

        // A circle a little wider than the star, so it frames rather than hides.
        #[allow(clippy::cast_possible_truncation)]
        let radius = ((star.fwhm as f32) * model.view.zoom).clamp(3.0, 40.0);
        painter.circle_stroke(
            centre,
            radius,
            Stroke::new(1.0, if star.saturated { saturated } else { ordinary }),
        );
    }
}

/// Gathers this frame's input into the plain structure [`actions_for`]
/// understands. Keeping the two apart is what makes the mapping testable.
fn collect_input(ui: &Ui, response: &egui::Response, viewport: Rect, modal: bool) -> Frame {
    let ctx = ui.ctx();

    let dropped = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .filter_map(|f| f.path.clone())
            .collect::<Vec<_>>()
    });

    let keys = ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Key {
                    key,
                    modifiers,
                    pressed: true,
                    repeat: false,
                    ..
                } => Some((*key, *modifiers)),
                _ => None,
            })
            .collect::<Vec<_>>()
    });

    // Only zoom when the pointer is over the image, so that scrolling a side
    // panel added in a later phase will not move the image.
    let pointer = ctx
        .input(|i| i.pointer.hover_pos())
        .filter(|p| viewport.contains(*p));
    let scroll_delta = if pointer.is_some() {
        ctx.input(|i| i.smooth_scroll_delta.y)
    } else {
        0.0
    };

    let drag_delta = if response.dragged() {
        response.drag_delta()
    } else {
        Vec2::ZERO
    };

    Frame {
        keys,
        scroll_delta,
        pointer,
        drag_delta,
        dropped,
        modal,
    }
}

//! The histogram strip beneath the image.
//!
//! Shows where a frame's samples sit, with the stretch's black point and
//! midtone drawn on it, so the effect of the stretch settings can be seen
//! rather than guessed at.

use egui::{Color32, Panel, Rect, RichText, Sense, Stroke, Ui, Vec2};
use fits_core::histogram::BINS;

use crate::app::{Action, Model};
use crate::ui::HistogramView;

/// Height of the strip. Enough to read a shape, small enough not to compete
/// with the image.
const HEIGHT: f32 = 72.0;

/// Draws the histogram, when it is showing.
pub fn show(ui: &mut Ui, model: &Model, view: Option<&HistogramView>) -> Vec<Action> {
    let mut actions = Vec::new();
    if !model.show_histogram {
        return actions;
    }

    Panel::bottom("histogram").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Histogram").small().strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text("Hide (G)").clicked() {
                    actions.push(Action::ToggleHistogram);
                }
                if model.stretch_enabled {
                    ui.label(
                        RichText::new("black point and midtone marked")
                            .weak()
                            .small(),
                    );
                }
            });
        });

        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), HEIGHT), Sense::hover());
        match view {
            Some(view) if !view.histogram.is_empty() => draw(ui, rect, view),
            _ => {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Nothing to show",
                    egui::FontId::proportional(12.0),
                    ui.visuals().weak_text_color(),
                );
            }
        }
    });

    actions
}

/// Paints the bars, and the stretch points over them.
fn draw(ui: &Ui, rect: Rect, view: &HistogramView) {
    let histogram = &view.histogram;
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, Color32::from_gray(24));

    #[allow(clippy::cast_precision_loss)]
    let bar = rect.width() / BINS as f32;
    let colour = Color32::from_rgb(150, 180, 220);

    for bin in 0..BINS {
        let height = histogram.height(bin) * rect.height();
        if height <= 0.0 {
            continue;
        }
        #[allow(clippy::cast_precision_loss)]
        let x = rect.left() + bin as f32 * bar;
        painter.rect_filled(
            Rect::from_min_max(
                egui::pos2(x, rect.bottom() - height),
                egui::pos2(x + bar.max(1.0), rect.bottom()),
            ),
            0.0,
            colour,
        );
    }

    // Where the stretch is putting things, so its settings are visible rather
    // than a matter of trial and error. Measured once per image, not here.
    let Some(stretch) = view.stretch else {
        return;
    };
    let mark = |position: f32, colour: Color32, label: &str| {
        let x = rect.left() + position.clamp(0.0, 1.0) * rect.width();
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.0, colour),
        );
        painter.text(
            egui::pos2(x + 3.0, rect.top() + 1.0),
            egui::Align2::LEFT_TOP,
            label,
            egui::FontId::proportional(10.0),
            colour,
        );
    };

    mark(stretch.shadows, Color32::from_rgb(230, 120, 120), "black");
    // The midtone is a curve parameter, not a level; it sits where the input
    // that becomes mid grey lies.
    let midtone_input = stretch.shadows + stretch.midtones * (stretch.highlights - stretch.shadows);
    mark(midtone_input, Color32::from_rgb(230, 200, 120), "mid");
}

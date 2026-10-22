//! The "nil" wordmark, drawn from the logo's rounded strokes with a zero ring for the i's
//! dot. Coordinates are in a 122 × 100 box, scaled to the height asked for.

use egui::{
    Color32, Painter, Pos2, Rect, Response, Role, Sense, Shape, Stroke, Ui, WidgetInfo, vec2,
};

use crate::theme::TEXT;

/// Width over height of the wordmark's box.
const ASPECT: f32 = 1.22;
const STROKE: f32 = 12.0;
/// The ring is a little larger than a dot would be, so the zero reads at title-bar size.
const RING_RADIUS: f32 = 9.0;
const RING_STROKE: f32 = 5.0;

/// The wordmark `height` px tall, labelled "nil" for screen readers.
pub fn show(ui: &mut Ui, height: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(height * ASPECT, height), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(Role::Image, true, "nil"));
    if ui.is_rect_visible(rect) {
        paint(ui.painter(), rect, TEXT);
    }
    resp
}

pub fn paint(p: &Painter, rect: Rect, color: Color32) {
    let s = rect.height() / 100.0;
    let at = |x: f32, y: f32| rect.min + vec2(x * s, y * s);
    let width = STROKE * s;
    let stroke = Stroke::new(width, color);
    // egui strokes end square: a dot on each end rounds them like the logo's.
    let line = |points: Vec<Pos2>| {
        for end in [points[0], points[points.len() - 1]] {
            p.circle_filled(end, width / 2.0, color);
        }
        p.add(Shape::line(points, stroke));
    };
    // n: the stem, then the arch from the stem over to the right leg.
    line(vec![at(14.0, 44.0), at(14.0, 86.0)]);
    let mut arch = vec![at(14.0, 62.0)];
    arch.extend((1..24).map(|i| {
        let a = std::f32::consts::PI * (1.0 - i as f32 / 24.0);
        at(33.0 + 19.0 * a.cos(), 62.0 - 19.0 * a.sin())
    }));
    arch.extend([at(52.0, 62.0), at(52.0, 86.0)]);
    line(arch);
    // i, with a zero for its dot; l.
    line(vec![at(80.0, 48.0), at(80.0, 86.0)]);
    p.circle_stroke(
        at(80.0, 23.0),
        RING_RADIUS * s,
        Stroke::new((RING_STROKE * s).max(1.2), color),
    );
    line(vec![at(108.0, 14.0), at(108.0, 86.0)]);
}

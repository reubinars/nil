//! Spectrum bars: one per band, rising from the bottom of their rect.

use egui::{Color32, CornerRadius, Pos2, Rect, Role, Sense, Ui, WidgetInfo};

use crate::views::gradient_rect;

#[derive(Clone, Copy)]
pub enum Style {
    /// The now-playing view: wider gaps, white fading downward.
    Large,
    /// The bar: thin bars in translucent white.
    Mini,
}

/// Paints `levels` (0…1) as bars filling `rect`; one accessibility node, "Spectrum".
pub fn show(ui: &mut Ui, rect: Rect, levels: &[f32], style: Style) {
    let resp = ui.allocate_rect(rect, Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(Role::Image, true, "Spectrum"));
    if levels.is_empty() || !ui.is_rect_visible(rect) {
        return;
    }
    let n = levels.len() as f32;
    let gap = match style {
        Style::Large => 3.0,
        Style::Mini => 2.0,
    };
    let w = ((rect.width() - gap * (n - 1.0)) / n).max(1.0);
    let p = ui.painter();
    for (i, &level) in levels.iter().enumerate() {
        let h = level.clamp(0.0, 1.0) * rect.height();
        if h < 0.5 {
            continue;
        }
        let x = rect.left() + i as f32 * (w + gap);
        let bar = Rect::from_min_max(
            Pos2::new(x, rect.bottom() - h),
            Pos2::new(x + w, rect.bottom()),
        );
        match style {
            Style::Large => gradient_rect(
                p,
                bar,
                Color32::from_white_alpha(242),
                Color32::from_white_alpha(89),
            ),
            Style::Mini => {
                p.rect_filled(bar, CornerRadius::same(1), Color32::from_white_alpha(191));
            }
        }
    }
}

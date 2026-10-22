//! egui views. They read state and return actions; `app` applies them.

pub mod album_page;
pub mod albums;
pub mod artists;
pub mod bar;
pub mod device_list;
pub mod now_playing;
pub mod search;
pub mod settings;
pub mod spectrum;
pub mod top_bar;
pub mod window_controls;
pub mod wordmark;

use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontId, Galley, Painter, Pos2, Rect, Response, Role, Sense, Ui,
    WidgetInfo,
};

use crate::art::ArtState;
use crate::fmt::initials;
use crate::theme::TEXT_2;

const ART_PLACEHOLDER: Color32 = Color32::from_rgb(0x1D, 0x1D, 0x1D);

/// Single-line text cut with an ellipsis at `width`.
pub(crate) fn truncated(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width(width);
    ui.painter().layout_job(job)
}

/// The text as shown, ellipsis included.
pub(crate) fn shown_text(galley: &Galley) -> String {
    galley.rows.iter().map(|r| r.text()).collect()
}

/// Paints `galley` at `pos` as a label whose accessible text is what is shown.
pub(crate) fn text_at(ui: &mut Ui, pos: Pos2, galley: Arc<Galley>) -> Response {
    let rect = Rect::from_min_size(pos, galley.size());
    let resp = ui.allocate_rect(rect, Sense::hover());
    let text = shown_text(&galley);
    resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, &text));
    ui.painter().galley(pos, galley, Color32::PLACEHOLDER);
    resp
}

/// A page heading: the title in 40 px bold, cut with an ellipsis so the count beside it
/// always shows.
pub(crate) fn heading(ui: &mut Ui, title: &str, count: Option<&str>) {
    let count_g = count.map(|c| {
        ui.painter().layout_no_wrap(
            c.to_owned(),
            FontId::proportional(14.0),
            crate::theme::TEXT_3,
        )
    });
    let count_w = count_g.as_ref().map_or(0.0, |g| g.size().x + 14.0);
    let width = ui.available_width();
    let title_g = truncated(
        ui,
        title,
        FontId::new(40.0, crate::theme::bold()),
        crate::theme::TEXT,
        (width - count_w).max(0.0),
    );
    let height = title_g.size().y;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    let shown = shown_text(&title_g);
    let label = match count {
        Some(c) => format!("{shown} {c}"),
        None => shown,
    };
    resp.widget_info(|| WidgetInfo::labeled(Role::Heading, true, &label));
    let p = ui.painter();
    let title_w = title_g.size().x;
    p.galley(rect.left_top(), title_g, Color32::PLACEHOLDER);
    if let Some(g) = count_g {
        // Baselines roughly aligned: the count sits on the title's lower third.
        let y = rect.top() + height * 0.72 - g.size().y / 2.0;
        p.galley(
            Pos2::new(rect.left() + title_w + 14.0, y),
            g,
            Color32::PLACEHOLDER,
        );
    }
}

/// "1 album", "3 albums".
pub(crate) fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `rect` filled with a vertical gradient from `top` to `bottom`.
pub(crate) fn gradient_rect(p: &Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    p.add(mesh);
}

/// A soft drop shadow under `rect`.
pub(crate) fn shadow(p: &Painter, rect: Rect, radius: f32) {
    for (grow, alpha) in [(6.0, 14u8), (4.0, 24), (2.0, 36)] {
        let r = rect.translate(egui::vec2(0.0, 4.0)).expand(grow);
        p.rect_filled(
            r,
            CornerRadius::same((radius + grow).min(255.0) as u8),
            Color32::from_black_alpha(alpha),
        );
    }
}

/// Album art cropped to fill `rect` with rounded corners, or a grey tile with the
/// album's initials.
pub(crate) fn paint_art_rounded(
    p: &Painter,
    rect: Rect,
    state: &ArtState,
    title: &str,
    radius: f32,
) {
    let corner = CornerRadius::same(radius.min(255.0) as u8);
    match state {
        ArtState::Ready(tex) => {
            let s = tex.size_vec2();
            // Cover-crop: the shorter side fills the rect.
            let (w, h) = (s.x.max(1.0), s.y.max(1.0));
            let (rw, rh) = (rect.width().max(1.0), rect.height().max(1.0));
            let uv = if w / h > rw / rh {
                let u = (rw / rh) * h / w;
                Rect::from_min_max(Pos2::new(0.5 - u / 2.0, 0.0), Pos2::new(0.5 + u / 2.0, 1.0))
            } else {
                let v = (rh / rw) * w / h;
                Rect::from_min_max(Pos2::new(0.0, 0.5 - v / 2.0), Pos2::new(1.0, 0.5 + v / 2.0))
            };
            p.add(
                egui::epaint::RectShape::filled(rect, corner, Color32::WHITE)
                    .with_texture(tex.id(), uv),
            );
        }
        ArtState::Loading | ArtState::Missing => {
            p.rect_filled(rect, corner, ART_PLACEHOLDER);
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                initials(title),
                FontId::new(rect.width() * 0.28, crate::theme::bold()),
                TEXT_2,
            );
        }
    }
}

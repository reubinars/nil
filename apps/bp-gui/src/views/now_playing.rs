//! The full-window now-playing view on the bar's neutral black: the cover, the transport,
//! and the queue.

use std::time::Duration;

use egui::{
    Color32, CornerRadius, FontId, Pos2, Rect, Role, Sense, Stroke, Ui, UiBuilder, WidgetInfo, vec2,
};

use crate::art::{ArtKind, ArtLoader};
use crate::backend::Album;
use crate::fmt::duration;
use crate::state::now_playing::{Badge, NowPlaying};
use crate::theme::{BAR, bold, semibold};
use crate::views::bar::{Icon, fatal_line, format_line, icon_button, seek_line};
use crate::views::{paint_art_rounded, shadow, text_at, truncated, window_controls};

const COLUMN: f32 = 440.0;
const MAX_COVER: f32 = 380.0;
const MIN_COVER: f32 = 160.0;
/// Height the column needs under the cover at its smallest.
const BELOW_COVER: f32 = 340.0;
const SPECTRUM_MAX: f32 = 72.0;
const QUEUE_ROW: f32 = 50.0;
/// Height of the window strip at the top: drag area and window buttons.
const WINDOW_STRIP: f32 = 40.0;
const DIM: Color32 = Color32::from_rgba_premultiplied(190, 190, 190, 190);
const TRACK: Color32 = Color32::from_rgba_premultiplied(56, 56, 56, 56);

pub struct NowPlayingData<'a> {
    pub np: &'a NowPlaying,
    /// The album the queue was started from.
    pub album: Option<&'a Album>,
    pub device: Option<&'a str>,
    /// Large spectrum levels while playing.
    pub spectrum: Option<&'a [f32]>,
}

pub enum NowPlayingAction {
    Close,
    PlayPause,
    Previous,
    Next,
    Seek(Duration),
    /// Play the queue from this index.
    PlayIndex(usize),
    Retry,
}

/// The cover's side for a window of `available`: as large as 380 px while the controls
/// below it still fit.
pub fn cover_side(available: egui::Vec2) -> f32 {
    (available.y - BELOW_COVER).clamp(MIN_COVER, MAX_COVER)
}

pub fn show(ui: &mut Ui, data: &NowPlayingData, art: &mut ArtLoader) -> Option<NowPlayingAction> {
    let mut action = None;
    let full = ui.max_rect();
    ui.painter().rect_filled(full, CornerRadius::ZERO, BAR);
    // No title bar: the strip above the content moves the window, the buttons sit on it.
    let strip = Rect::from_min_size(full.min, vec2(full.width(), WINDOW_STRIP));
    window_controls::drag_area(ui, strip);
    let buttons = Rect::from_min_max(
        Pos2::new(full.right() - window_controls::WIDTH, full.top()),
        Pos2::new(full.right(), full.top() + WINDOW_STRIP),
    );
    window_controls::buttons(ui, buttons);

    let close = Rect::from_min_size(full.min + vec2(24.0, 10.0), vec2(36.0, 36.0));
    if close_button(ui, close).clicked() {
        action = Some(NowPlayingAction::Close);
    }

    let np = data.np;
    let item = np.index.and_then(|i| np.queue.get(i));
    let content = Rect::from_min_max(full.min + vec2(64.0, 56.0), full.max - vec2(64.0, 28.0));
    let col_w = COLUMN.min(content.width() * 0.5);
    let col = Rect::from_min_size(content.min, vec2(col_w, content.height()));
    let side = cover_side(full.size()).min(col_w);
    let cover = Rect::from_min_size(
        Pos2::new(col.center().x - side / 2.0, col.top()),
        vec2(side, side),
    );
    if let Some(album) = data.album {
        let state = art.texture(ui.ctx(), album.id, ArtKind::Page);
        shadow(ui.painter(), cover, 8.0);
        paint_art_rounded(ui.painter(), cover, &state, &album.title, 8.0);
    }
    let mut y = cover.bottom() + 16.0;
    if let Some(item) = item {
        let title = truncated(
            ui,
            &item.title,
            FontId::new(24.0, bold()),
            Color32::WHITE,
            col_w,
        );
        let h = title.size().y;
        text_at(ui, Pos2::new(col.left(), y), title);
        y += h + 2.0;
    }
    if let Some(album) = data.album {
        let sub = truncated(
            ui,
            &format!("{} · {}", album.artist, album.title),
            FontId::proportional(15.0),
            DIM,
            col_w,
        );
        let h = sub.size().y;
        text_at(ui, Pos2::new(col.left(), y), sub);
        y += h;
    }
    // Whatever height is left between the text and the controls goes to the spectrum.
    let controls_h = 14.0 + 24.0 + 12.0 + 56.0 + 14.0 + 28.0;
    let spectrum_h = (col.bottom() - controls_h - y - 14.0).clamp(0.0, SPECTRUM_MAX);
    y += 14.0;
    if let Some(levels) = data.spectrum
        && spectrum_h >= 16.0
    {
        let rect = Rect::from_min_size(Pos2::new(col.left(), y), vec2(col_w, spectrum_h));
        crate::views::spectrum::show(ui, rect, levels, crate::views::spectrum::Style::Large);
    }
    y += spectrum_h + 14.0;

    // Seek line with the times under it.
    if let Some(item) = item {
        let total = np.duration.unwrap_or(item.duration);
        let seek = Rect::from_min_size(Pos2::new(col.left(), y - 4.0), vec2(col_w, 12.0));
        if let Some(to) = seek_line(ui, seek, np.position, total, TRACK) {
            action = Some(NowPlayingAction::Seek(to));
        }
        let mono = FontId::monospace(11.0);
        let pos = truncated(ui, &duration(np.position), mono.clone(), DIM, 60.0);
        let end = truncated(ui, &duration(total), mono, DIM, 60.0);
        let end_w = end.size().x;
        text_at(ui, Pos2::new(col.left(), y + 8.0), pos);
        text_at(ui, Pos2::new(col.right() - end_w, y + 8.0), end);
    }
    y += 24.0 + 12.0;

    // Transport.
    let cx = col.center().x;
    let row = y + 28.0;
    let (play_icon, play_label) = if np.is_playing() {
        (Icon::Pause, "Pause")
    } else {
        (Icon::Play, "Play")
    };
    for (x, icon, label, a, disc) in [
        (
            cx - 72.0,
            Icon::Previous,
            "Previous",
            NowPlayingAction::Previous,
            false,
        ),
        (cx, play_icon, play_label, NowPlayingAction::PlayPause, true),
        (cx + 72.0, Icon::Next, "Next", NowPlayingAction::Next, false),
    ] {
        let side = if disc { 56.0 } else { 36.0 };
        let rect = Rect::from_center_size(Pos2::new(x, row), vec2(side, side));
        if icon_button(ui, rect, icon, label, disc).clicked() {
            action = Some(a);
        }
    }
    y += 56.0 + 14.0;

    // The badge and the device, or the fatal error.
    let line = Rect::from_min_size(Pos2::new(col.left(), y), vec2(col_w, 28.0));
    if let Some(message) = &np.fatal {
        let rect = Rect::from_min_size(line.min, vec2(col_w, 34.0));
        if fatal_line(ui, rect, message) {
            action = Some(NowPlayingAction::Retry);
        }
    } else if let Some(item) = item {
        badge_pill(ui, line, &np.badge, &format_line(item));
        if let Some(device) = data.device {
            let g = truncated(ui, device, FontId::proportional(12.0), DIM, col_w / 2.0);
            let w = g.size().x;
            let h = g.size().y;
            text_at(
                ui,
                Pos2::new(line.right() - w, line.center().y - h / 2.0),
                g,
            );
        }
    }

    // The queue.
    let queue = Rect::from_min_max(Pos2::new(col.right() + 64.0, content.top()), content.max);
    if queue.width() > 120.0
        && let Some(i) = queue_list(ui, queue, data, art)
    {
        action = Some(NowPlayingAction::PlayIndex(i));
    }
    action
}

fn close_button(ui: &mut Ui, rect: Rect) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with("np_close"), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Close now playing"));
    let p = ui.painter();
    let fill = if resp.hovered() {
        Color32::from_black_alpha(120)
    } else {
        Color32::from_black_alpha(76)
    };
    p.circle_filled(rect.center(), rect.width() / 2.0, fill);
    let c = rect.center();
    let s = Stroke::new(2.0, Color32::WHITE);
    p.line_segment([c + vec2(-6.0, -3.0), c + vec2(0.0, 3.0)], s);
    p.line_segment([c + vec2(0.0, 3.0), c + vec2(6.0, -3.0)], s);
    resp
}

/// "Bit-perfect  FLAC 24/48" as plain text, like the bar's badge.
fn badge_pill(ui: &mut Ui, line: Rect, badge: &Badge, format: &str) {
    let text = match badge {
        Badge::BitPerfect => "Bit-perfect",
        Badge::NotBitPerfect { .. } => "Not bit-perfect",
        Badge::Opening => "—",
        Badge::Idle => return,
    };
    let label = ui.painter().layout_no_wrap(
        text.to_owned(),
        FontId::new(12.0, semibold()),
        Color32::WHITE,
    );
    let fmt = ui
        .painter()
        .layout_no_wrap(format.to_owned(), FontId::monospace(12.0), DIM);
    let cy = line.center().y;
    let lw = label.size().x;
    let lh = label.size().y;
    text_at(ui, Pos2::new(line.left(), cy - lh / 2.0), label);
    let fh = fmt.size().y;
    text_at(ui, Pos2::new(line.left() + lw + 10.0, cy - fh / 2.0), fmt);
}

/// "QUEUE · album" and one row per queued track; returns the row clicked.
fn queue_list(
    ui: &mut Ui,
    rect: Rect,
    data: &NowPlayingData,
    art: &mut ArtLoader,
) -> Option<usize> {
    let mut clicked = None;
    let np = data.np;
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        let header = match data.album {
            Some(a) => format!("QUEUE · {}", a.title.to_uppercase()),
            None => "QUEUE".to_owned(),
        };
        let g = truncated(
            ui,
            &header,
            FontId::new(12.0, semibold()),
            DIM,
            rect.width(),
        );
        let (r, _) = ui.allocate_exact_size(vec2(rect.width(), 28.0), Sense::hover());
        text_at(ui, Pos2::new(r.left() + 12.0, r.top()), g);
        let thumb = data
            .album
            .map(|a| (a, art.texture(ui.ctx(), a.id, ArtKind::Thumb)));
        egui::ScrollArea::vertical()
            .id_salt("np_queue")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (i, item) in np.queue.iter().enumerate() {
                    let (row, resp) = ui
                        .allocate_exact_size(vec2(ui.available_width(), QUEUE_ROW), Sense::click());
                    resp.widget_info(|| WidgetInfo::labeled(Role::ListItem, true, &item.title));
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                    if !ui.is_rect_visible(row) {
                        continue;
                    }
                    let current = np.index == Some(i);
                    let played = np.index.is_some_and(|c| i < c);
                    let p = ui.painter();
                    if current {
                        p.rect_filled(row, CornerRadius::same(6), Color32::from_white_alpha(40));
                    } else if resp.hovered() {
                        p.rect_filled(row, CornerRadius::same(6), Color32::from_white_alpha(18));
                    }
                    let alpha = if played { 0.55 } else { 1.0 };
                    let th = Rect::from_min_size(row.min + vec2(12.0, 7.0), vec2(36.0, 36.0));
                    if let Some((album, state)) = &thumb {
                        paint_art_rounded(p, th, state, &album.title, 3.0);
                        if played {
                            p.rect_filled(
                                th,
                                CornerRadius::same(3),
                                Color32::from_black_alpha(110),
                            );
                        }
                    }
                    let x = th.right() + 14.0;
                    let time_w = 56.0;
                    let w = (row.right() - time_w - 12.0 - x).max(0.0);
                    let font = if current {
                        FontId::new(14.0, semibold())
                    } else {
                        FontId::proportional(14.0)
                    };
                    let white = Color32::WHITE.gamma_multiply(alpha);
                    let title = truncated(ui, &item.title, font, white, w);
                    let artist = data.album.map_or("", |a| a.artist.as_str());
                    let sub = truncated(
                        ui,
                        artist,
                        FontId::proportional(12.0),
                        DIM.gamma_multiply(alpha),
                        w,
                    );
                    let p = ui.painter();
                    let cy = row.center().y;
                    p.galley(Pos2::new(x, cy - title.size().y), title, white);
                    p.galley(Pos2::new(x, cy + 1.0), sub, DIM);
                    p.text(
                        Pos2::new(row.right() - 12.0, cy),
                        egui::Align2::RIGHT_CENTER,
                        duration(item.duration),
                        FontId::monospace(12.0),
                        DIM.gamma_multiply(alpha),
                    );
                }
            });
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    #[test]
    fn cover_shrinks_to_fit() {
        assert_eq!(cover_side(vec2(1280.0, 800.0)), 380.0);
        assert_eq!(cover_side(vec2(900.0, 600.0)), 260.0);
        assert_eq!(cover_side(vec2(500.0, 300.0)), 160.0);
    }
}

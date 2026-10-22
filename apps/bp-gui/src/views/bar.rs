//! The now-playing bar, floating over the bottom of the page on a neutral wash: cover,
//! title and artist (click for the now-playing view); the transport and seek line; the
//! badge, the source format and the device button.

use std::time::Duration;

use egui::{
    Color32, CornerRadius, FontId, Id, Pos2, Rect, Response, Role, Sense, Shape, Stroke,
    StrokeKind, Ui, WidgetInfo, vec2,
};

use crate::art::{ArtKind, ArtLoader};
use crate::backend::{Album, QueueItem};
use crate::fmt::{device_label, duration, long_source, short_format};
use crate::state::now_playing::{Badge, NowPlaying};
use crate::theme::{TEXT, TEXT_2, TEXT_3, semibold};
use crate::views::{paint_art_rounded, spectrum, text_at, truncated};

/// Height of the controls row, at the window bottom.
pub const HEIGHT: f32 = 76.0;
/// Paints the wash: clear at the top of `rect`, near-black at the window bottom.
pub fn wash(p: &egui::Painter, rect: Rect) {
    crate::views::gradient_rect(
        p,
        rect,
        Color32::TRANSPARENT,
        Color32::from_black_alpha(235),
    );
}

/// Height of the wash that fades the page out under the controls.
pub const WASH: f32 = 132.0;
/// Space pages leave at their end so their last row scrolls clear of the controls.
pub const CLEARANCE: f32 = HEIGHT + 24.0;
const SIDE_PAD: f32 = 20.0;
const LEFT_W: f32 = 300.0;
const RIGHT_W: f32 = 400.0;
const ART: f32 = 52.0;
const BUTTON: f32 = 30.0;
const DISC: f32 = 38.0;
const SEEK_MAX: f32 = 520.0;
const TRACK: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
const ON_WHITE: Color32 = Color32::from_rgb(0x11, 0x11, 0x11);
/// The mini spectrum, left of the badge.
const SPECTRUM_W: f32 = 120.0;
const SPECTRUM_H: f32 = 30.0;
/// Narrowest centre (transport and seek line) the mini spectrum may leave.
const MIN_CENTRE: f32 = 240.0;

pub struct BarData<'a> {
    pub np: &'a NowPlaying,
    /// Player-clock time, for the notice expiry.
    pub now: Duration,
    /// The album the queue was started from.
    pub album: Option<&'a Album>,
    /// The selected device's name.
    pub device: Option<&'a str>,
    /// Mini spectrum levels while playing.
    pub spectrum: Option<&'a [f32]>,
}

pub enum BarAction {
    PlayPause,
    Previous,
    Next,
    Seek(Duration),
    Retry,
    OpenNowPlaying,
}

pub struct BarOutput {
    pub action: Option<BarAction>,
    /// The device button; the device list anchors to it.
    pub device_button: Response,
    /// Ids of the focusable bar controls; arrow keys seek while one of them has focus.
    pub controls: Vec<Id>,
}

/// The badge's text and fill colours: plain white text whether or not the output is
/// bit-perfect; the words tell them apart.
pub(crate) fn badge_colors(b: &Badge) -> (Color32, Color32) {
    match b {
        Badge::BitPerfect | Badge::NotBitPerfect { .. } => (TEXT, Color32::TRANSPARENT),
        Badge::Opening | Badge::Idle => (TEXT_3, Color32::TRANSPARENT),
    }
}

/// `FLAC 24/192`, `DSF DSD64`, `AAC/44.1`.
pub(crate) fn format_line(item: &QueueItem) -> String {
    if item.source.codec.is_lossy() {
        short_format(&item.source)
    } else {
        format!("{} {}", item.source.codec, short_format(&item.source))
    }
}

/// `FLAC 24bit 192.0kHz ▸ DAC A 32bit ▸ Exclusive ▸ Bit-perfect`.
pub(crate) fn signal_path(np: &NowPlaying, item: &QueueItem, device: &str) -> String {
    // Until `TrackStarted`, the path may still be the previous track's.
    let head = match np.path.as_ref().filter(|p| p.source == item.source) {
        Some(path) => format!(
            "{} ▸ {} ▸ Exclusive",
            long_source(&path.source),
            device_label(device, path)
        ),
        None => format!("{} ▸ {device} ▸ Exclusive", long_source(&item.source)),
    };
    match &np.badge {
        Badge::BitPerfect => format!("{head} ▸ Bit-perfect"),
        Badge::NotBitPerfect { cause } => format!("{head} ▸ Not bit-perfect: {cause}"),
        Badge::Opening | Badge::Idle => head,
    }
}

pub fn show(ui: &mut Ui, data: &BarData, art: &mut ArtLoader) -> BarOutput {
    let (full, group) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
    group.widget_info(|| WidgetInfo::labeled(Role::Group, true, "Now playing"));
    let np = data.np;
    let mut action = None;
    let item = np.index.and_then(|i| np.queue.get(i));
    let device_name = data.device.unwrap_or("No device");
    let cy = full.center().y;

    // Left: cover, title and artist; opens the now-playing view.
    let left = Rect::from_min_size(
        Pos2::new(full.left() + SIDE_PAD, full.top()),
        vec2(LEFT_W, full.height()),
    );
    let text_x = left.left() + ART + 12.0;
    let text_w = left.right() - text_x;
    if let Some(item) = item {
        let open = ui.interact(left, ui.scope_id().with("open_now_playing"), Sense::click());
        open.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Open now playing"));
        if open.clicked() {
            action = Some(BarAction::OpenNowPlaying);
        }
        let art_rect = Rect::from_min_size(Pos2::new(left.left(), cy - ART / 2.0), vec2(ART, ART));
        if let Some(album) = data.album {
            let state = art.texture(ui.ctx(), album.id, ArtKind::Thumb);
            paint_art_rounded(ui.painter(), art_rect, &state, &album.title, 4.0);
        }
        let title = truncated(ui, &item.title, FontId::new(14.0, semibold()), TEXT, text_w);
        let title_h = title.size().y;
        text_at(ui, Pos2::new(text_x, cy - title_h - 1.0), title);
        if let Some(album) = data.album {
            let sub = truncated(
                ui,
                &format!("{} · {}", album.artist, album.title),
                FontId::proportional(12.0),
                TEXT_2,
                text_w,
            );
            text_at(ui, Pos2::new(text_x, cy + 2.0), sub);
        }
    } else {
        let idle = truncated(
            ui,
            &format!("{device_name} · Idle"),
            FontId::monospace(12.0),
            TEXT_2,
            text_w,
        );
        let h = idle.size().y;
        text_at(ui, Pos2::new(text_x, cy - h / 2.0), idle);
    }

    // Right: mini spectrum, badge and format, device button.
    let right = Rect::from_min_max(
        Pos2::new(full.right() - SIDE_PAD - RIGHT_W, full.top()),
        Pos2::new(full.right() - SIDE_PAD, full.bottom()),
    );
    let name = truncated(ui, device_name, FontId::proportional(13.0), TEXT, 140.0);
    let dev_w = name.size().x + 58.0;
    let dev_rect = Rect::from_min_size(
        Pos2::new(right.right() - dev_w, cy - 18.0),
        vec2(dev_w, 36.0),
    );
    let device_button = device_button(ui, dev_rect, device_name, name);
    let mut controls = vec![device_button.id];
    let badge_right = dev_rect.left() - 14.0;
    // Left edge of what the right side draws; the centre stays clear of it.
    let mut occupied = dev_rect.left();
    if let Some(item) = item {
        let badge_left = badge_block(ui, badge_right, cy, np, item, device_name);
        occupied = occupied.min(badge_left);
        let spectrum_left = badge_left - 14.0 - SPECTRUM_W;
        // Shown only where the transport and seek line keep their room.
        if let Some(levels) = data.spectrum
            && spectrum_left - 24.0 >= left.right() + 24.0 + MIN_CENTRE
        {
            let rect = Rect::from_min_size(
                Pos2::new(spectrum_left, cy - SPECTRUM_H / 2.0),
                vec2(SPECTRUM_W, SPECTRUM_H),
            );
            spectrum::show(ui, rect, levels, spectrum::Style::Mini);
            occupied = spectrum_left;
        }
    }

    // Centre: transport over the seek line, or the fatal error line.
    let centre = Rect::from_min_max(
        Pos2::new(left.right() + 24.0, full.top()),
        Pos2::new(right.left().min(occupied) - 24.0, full.bottom()),
    );
    if let Some(message) = &np.fatal {
        let w = centre.width().min(SEEK_MAX + 80.0);
        let rect = Rect::from_center_size(centre.center(), vec2(w, 34.0));
        if fatal_line(ui, rect, message) {
            action = Some(BarAction::Retry);
        }
        return BarOutput {
            action,
            device_button,
            controls,
        };
    }
    let row_y = full.top() + 26.0;
    let mid = centre.center().x;
    let playing = np.is_playing();
    let (play_icon, play_label) = if playing {
        (Icon::Pause, "Pause")
    } else {
        (Icon::Play, "Play")
    };
    let buttons = [
        (
            mid - DISC / 2.0 - 16.0 - BUTTON / 2.0,
            Icon::Previous,
            "Previous",
            BarAction::Previous,
            false,
        ),
        (mid, play_icon, play_label, BarAction::PlayPause, true),
        (
            mid + DISC / 2.0 + 16.0 + BUTTON / 2.0,
            Icon::Next,
            "Next",
            BarAction::Next,
            false,
        ),
    ];
    for (x, icon, label, a, disc) in buttons {
        let side = if disc { DISC } else { BUTTON };
        let rect = Rect::from_center_size(Pos2::new(x, row_y), vec2(side, side));
        let resp = icon_button(ui, rect, icon, label, disc);
        controls.push(resp.id);
        if resp.clicked() {
            action = Some(a);
        }
    }

    let line_y = full.top() + 58.0;
    let mono = FontId::monospace(11.0);
    if let Some(notice) = np.notice_text(data.now) {
        let g = truncated(ui, notice, FontId::monospace(12.0), TEXT_2, centre.width());
        let pos = Pos2::new(mid - g.size().x / 2.0, line_y - g.size().y / 2.0);
        text_at(ui, pos, g);
    } else if let Some(item) = item {
        let total = np.duration.unwrap_or(item.duration);
        let w = centre.width().min(SEEK_MAX);
        let row = Rect::from_center_size(Pos2::new(mid, line_y), vec2(w, 14.0));
        let pos = truncated(ui, &duration(np.position), mono.clone(), TEXT_2, 60.0);
        let end = truncated(ui, &duration(total), mono, TEXT_2, 60.0);
        let pos_w = pos.size().x;
        let end_w = end.size().x;
        let h = pos.size().y;
        text_at(ui, Pos2::new(row.left(), line_y - h / 2.0), pos);
        text_at(ui, Pos2::new(row.right() - end_w, line_y - h / 2.0), end);
        let seek_rect = Rect::from_min_max(
            Pos2::new(row.left() + pos_w + 10.0, line_y - 6.0),
            Pos2::new(row.right() - end_w - 10.0, line_y + 6.0),
        );
        controls.push(seek_id(ui));
        if let Some(to) = seek_line(ui, seek_rect, np.position, total, TRACK) {
            action = Some(BarAction::Seek(to));
        }
    }
    BarOutput {
        action,
        device_button,
        controls,
    }
}

/// The badge over the format line, right-aligned at `right`; returns its left edge. The
/// badge's tooltip is the full signal path.
fn badge_block(
    ui: &mut Ui,
    right: f32,
    cy: f32,
    np: &NowPlaying,
    item: &QueueItem,
    device: &str,
) -> f32 {
    let (fg, bg) = badge_colors(&np.badge);
    let mono = FontId::monospace(11.0);
    let second = if np.underruns > 0 {
        format!("underruns: {}", np.underruns)
    } else {
        format_line(item)
    };
    let second = truncated(ui, &second, mono, TEXT_2, 200.0);
    let second_w = second.size().x;
    text_at(ui, Pos2::new(right - second_w, cy + 3.0), second);
    let text = match &np.badge {
        Badge::Idle => return right - second_w,
        Badge::Opening => "—",
        Badge::BitPerfect => "Bit-perfect",
        Badge::NotBitPerfect { .. } => "Not bit-perfect",
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::new(11.0, semibold()), fg);
    let pad = if bg == Color32::TRANSPARENT { 0.0 } else { 6.0 };
    let w = galley.size().x + pad * 2.0;
    let rect = Rect::from_min_size(
        Pos2::new(right - w, cy - 3.0 - galley.size().y - 2.0),
        vec2(w, galley.size().y + 2.0),
    );
    let resp = ui.allocate_rect(rect, Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    let p = ui.painter();
    if bg != Color32::TRANSPARENT {
        p.rect_filled(rect, CornerRadius::same(3), bg);
    }
    p.galley(Pos2::new(rect.left() + pad, rect.top() + 1.0), galley, fg);
    // Monospace: Inter has no "▸".
    let path = signal_path(np, item, device);
    resp.on_hover_ui(|ui| {
        ui.label(egui::RichText::new(path).monospace().size(12.0));
    });
    rect.left().min(right - second_w)
}

fn seek_id(ui: &Ui) -> Id {
    ui.scope_id().with("seek")
}

/// The 4 px progress line; click or drag to seek. Returns the seek target.
pub(crate) fn seek_line(
    ui: &mut Ui,
    rect: Rect,
    position: Duration,
    total: Duration,
    track: Color32,
) -> Option<Duration> {
    let resp = ui.interact(rect, seek_id(ui), Sense::click_and_drag());
    resp.widget_info(|| WidgetInfo::labeled(Role::Slider, true, "Seek"));
    let frac_at = |pos: Pos2| ((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    let pointer = resp.interact_pointer_pos();
    let frac = match pointer {
        Some(pos) if resp.dragged() => frac_at(pos),
        _ if total.is_zero() => 0.0,
        _ => (position.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0),
    };
    let y = rect.center().y;
    let p = ui.painter();
    let line =
        |from: f32, to: f32| Rect::from_min_max(Pos2::new(from, y - 2.0), Pos2::new(to, y + 2.0));
    p.rect_filled(
        line(rect.left(), rect.right()),
        CornerRadius::same(2),
        track,
    );
    let fill_to = rect.left() + rect.width() * frac;
    p.rect_filled(line(rect.left(), fill_to), CornerRadius::same(2), TEXT);
    if resp.hovered() || resp.dragged() {
        p.circle_filled(Pos2::new(fill_to, y), 6.0, TEXT);
    }
    if resp.clicked() || resp.drag_stopped() {
        return pointer.map(|pos| total.mul_f32(frac_at(pos)));
    }
    None
}

/// The inverted fatal-error line; returns whether Retry was clicked.
pub(crate) fn fatal_line(ui: &mut Ui, rect: Rect, message: &str) -> bool {
    ui.painter()
        .rect_filled(rect, CornerRadius::same(6), Color32::WHITE);
    let retry_w = 64.0;
    let retry = Rect::from_center_size(
        Pos2::new(rect.right() - 6.0 - retry_w / 2.0, rect.center().y),
        vec2(retry_w, 22.0),
    );
    let text_w = (retry.left() - rect.left() - 16.0).max(0.0);
    let msg = truncated(ui, message, FontId::monospace(12.0), Color32::BLACK, text_w);
    let pos = Pos2::new(rect.left() + 8.0, rect.center().y - msg.size().y / 2.0);
    text_at(ui, pos, msg);
    let button = egui::Button::new(egui::RichText::new("Retry").color(Color32::WHITE))
        .fill(Color32::BLACK)
        .stroke(Stroke::new(1.0, Color32::BLACK));
    ui.put(retry, button).clicked()
}

fn device_button(
    ui: &mut Ui,
    rect: Rect,
    label: &str,
    name: std::sync::Arc<egui::Galley>,
) -> Response {
    let resp = ui.interact(rect, ui.scope_id().with("device_button"), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let p = ui.painter();
    // No outline at rest: the button floats on the wash like the rest of the bar.
    if resp.hovered() {
        p.rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.0, TEXT_3),
            StrokeKind::Inside,
        );
    }
    // A small DAC: a box with a dial.
    let icon = Rect::from_center_size(
        Pos2::new(rect.left() + 18.0, rect.center().y),
        vec2(11.0, 15.0),
    );
    let s = Stroke::new(1.3, TEXT_2);
    p.rect_stroke(icon, CornerRadius::same(2), s, StrokeKind::Inside);
    p.circle_stroke(icon.center() + vec2(0.0, 2.0), 2.6, s);
    let text_pos = Pos2::new(rect.left() + 32.0, rect.center().y - name.size().y / 2.0);
    p.galley(text_pos, name, TEXT);
    let c = Pos2::new(rect.right() - 13.0, rect.center().y);
    let chevron = Stroke::new(1.5, TEXT_2);
    p.line_segment([c + vec2(-4.0, -2.0), c + vec2(0.0, 2.0)], chevron);
    p.line_segment([c + vec2(0.0, 2.0), c + vec2(4.0, -2.0)], chevron);
    resp
}

#[derive(Clone, Copy)]
pub(crate) enum Icon {
    Previous,
    Play,
    Pause,
    Next,
}

/// A painted transport button; the bundled fonts have no transport glyphs. A `disc`
/// button is a white circle with a dark glyph.
pub(crate) fn icon_button(
    ui: &mut Ui,
    rect: Rect,
    icon: Icon,
    label: &str,
    disc: bool,
) -> Response {
    let resp = ui.interact(rect, ui.scope_id().with(label), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let p = ui.painter();
    let color = if disc {
        let fill = if resp.hovered() { Color32::WHITE } else { TEXT };
        p.circle_filled(rect.center(), rect.width() / 2.0, fill);
        ON_WHITE
    } else if resp.hovered() {
        Color32::WHITE
    } else {
        TEXT_2
    };
    let c = rect.center();
    let tri = |tip: f32, base: f32| {
        Shape::convex_polygon(
            vec![
                Pos2::new(c.x + base, c.y - 6.0),
                Pos2::new(c.x + tip, c.y),
                Pos2::new(c.x + base, c.y + 6.0),
            ],
            color,
            Stroke::NONE,
        )
    };
    let bar = |x: f32| {
        Rect::from_min_max(
            Pos2::new(c.x + x, c.y - 6.0),
            Pos2::new(c.x + x + 2.0, c.y + 6.0),
        )
    };
    match icon {
        Icon::Play => {
            p.add(tri(6.0, -4.0));
        }
        Icon::Pause => {
            p.rect_filled(bar(-4.0), CornerRadius::ZERO, color);
            p.rect_filled(bar(2.0), CornerRadius::ZERO, color);
        }
        Icon::Previous => {
            p.rect_filled(bar(-6.0), CornerRadius::ZERO, color);
            p.add(tri(-4.0, 5.0));
        }
        Icon::Next => {
            p.add(tri(4.0, -5.0));
            p.rect_filled(bar(4.0), CornerRadius::ZERO, color);
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_badge_is_white() {
        assert_eq!(
            badge_colors(&Badge::BitPerfect),
            (TEXT, Color32::TRANSPARENT)
        );
        assert_eq!(
            badge_colors(&Badge::NotBitPerfect {
                cause: "lossy".into()
            }),
            (TEXT, Color32::TRANSPARENT),
            "same look as bit-perfect: plain white text"
        );
    }
}

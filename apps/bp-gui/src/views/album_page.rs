//! The album page: a header over the blurred cover (art, title, artist, facts, Play), then
//! the tracklist. The whole page scrolls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Id, Key, Layout, Pos2, Rect, Response, Role,
    Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo, vec2,
};

use crate::art::{ArtKind, ArtLoader, ArtState};
use crate::backend::{Album, ArtistId, DeviceCaps, Track, TrackId};
use crate::fmt::{duration, short_format};
use crate::state::support::track_support;
use crate::theme::{BORDER, HOVER, TEXT, TEXT_2, TEXT_3, WINDOW, bold, semibold};
use crate::views::{gradient_rect, paint_art_rounded, shadow, text_at, truncated};

const ART: f32 = 260.0;
const BACKDROP_HEIGHT: f32 = 520.0;
const ROW_HEIGHT: f32 = 40.0;
const NUMBER_W: f32 = 44.0;
const ICON_W: f32 = 18.0;
const FORMAT_W: f32 = 110.0;
const DURATION_W: f32 = 60.0;
const PLAY_HEIGHT: f32 = 44.0;
/// Text on a white Play button.
const ON_WHITE: Color32 = Color32::from_rgb(0x11, 0x11, 0x11);

pub struct PageData<'a> {
    pub album: &'a Album,
    /// Disc, then track order.
    pub tracks: &'a [Track],
    pub highlight: Option<TrackId>,
    /// The track now playing.
    pub playing: Option<&'a Path>,
    /// The selected device's name and capabilities; with none, no track is marked
    /// unsupported.
    pub device: Option<(&'a str, &'a DeviceCaps)>,
    pub track_errors: &'a HashMap<PathBuf, String>,
}

pub enum PageAction {
    Select(TrackId),
    /// Play the album from this track index.
    Play(usize),
    OpenArtist(ArtistId),
}

pub fn show(ui: &mut Ui, data: &PageData, art: &mut ArtLoader) -> Option<PageAction> {
    let mut action = None;
    // The page margin is outside this ui; the backdrop reaches the panel's edges.
    let panel = ui.clip_rect();
    egui::ScrollArea::vertical()
        .id_salt("album_page")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let top = ui.min_rect().top();
            let backdrop = Rect::from_min_max(
                Pos2::new(panel.left(), top - (top - panel.top()).max(0.0)),
                Pos2::new(panel.right(), top + BACKDROP_HEIGHT),
            );
            if let ArtState::Ready(tex) = art.texture(ui.ctx(), data.album.id, ArtKind::Backdrop) {
                paint_cover(ui, &tex, backdrop);
                let fade = Rect::from_min_max(
                    Pos2::new(backdrop.left(), backdrop.bottom() - BACKDROP_HEIGHT / 2.0),
                    backdrop.right_bottom(),
                );
                gradient_rect(ui.painter(), fade, Color32::TRANSPARENT, WINDOW);
            }
            if let Some(a) = header(ui, data, art) {
                action = Some(a);
            }
            ui.add_space(32.0);
            if let Some(a) = tracklist(ui, data) {
                action = Some(a);
            }
            ui.add_space(crate::views::bar::CLEARANCE);
        });
    action
}

/// The backdrop covers `rect`, cropped to fill it.
fn paint_cover(ui: &Ui, tex: &egui::TextureHandle, rect: Rect) {
    let s = tex.size_vec2();
    let scale = (rect.width() / s.x.max(1.0)).max(rect.height() / s.y.max(1.0));
    let img = Rect::from_center_size(rect.center(), s * scale);
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .image(tex.id(), img, uv, Color32::WHITE);
}

/// Cover, then the album's title, artist, facts and the Play button.
fn header(ui: &mut Ui, data: &PageData, art: &mut ArtLoader) -> Option<PageAction> {
    let mut action = None;
    let album = data.album;
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(ART, ART), Sense::hover());
        let state = art.texture(ui.ctx(), album.id, ArtKind::Page);
        shadow(ui.painter(), rect, 6.0);
        paint_art_rounded(ui.painter(), rect, &state, &album.title, 6.0);
        ui.add_space(32.0);
        let width = ui.available_width();
        ui.allocate_ui_with_layout(vec2(width, ART), Layout::bottom_up(Align::LEFT), |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            if play_button(ui).clicked() {
                action = Some(PageAction::Play(0));
            }
            ui.add_space(8.0);
            facts(ui, data);
            if link(ui, &album.artist, width).clicked() {
                action = Some(PageAction::OpenArtist(album.artist_id));
            }
            line(
                ui,
                &album.title,
                FontId::new(52.0, bold()),
                TEXT,
                width,
                Role::Heading,
            );
            line(
                ui,
                "ALBUM",
                FontId::new(12.0, semibold()),
                TEXT_2,
                width,
                Role::Label,
            );
        });
    });
    action
}

/// One line of text cut to `width`, exposed with `role`.
fn line(ui: &mut Ui, text: &str, font: FontId, color: Color32, width: f32, role: Role) {
    let galley = truncated(ui, text, font, color, width);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::hover());
    let shown = crate::views::shown_text(&galley);
    resp.widget_info(|| WidgetInfo::labeled(role, true, &shown));
    ui.painter().galley(rect.min, galley, color);
}

/// The artist name, underlined on hover.
fn link(ui: &mut Ui, text: &str, width: f32) -> Response {
    let galley = truncated(ui, text, FontId::proportional(18.0), TEXT, width);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Link, true, text));
    if resp.hovered() {
        ui.painter()
            .hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, TEXT));
    }
    ui.painter().galley(rect.min, galley, TEXT);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// "2002 · 13 tracks · 48:46" and the format chip.
fn facts(ui: &mut Ui, data: &PageData) {
    let album = data.album;
    let n = data.tracks.len();
    let mut parts = Vec::new();
    if let Some(y) = album.year {
        parts.push(y.to_string());
    }
    parts.push(crate::views::count(n, "track", "tracks"));
    parts.push(duration(album.duration));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let font = FontId::proportional(13.0);
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                ui.label(egui::RichText::new("·").font(font.clone()).color(TEXT_2));
            }
            ui.label(egui::RichText::new(part).font(font.clone()).color(TEXT_2));
        }
        ui.label(egui::RichText::new("·").font(font).color(TEXT_2));
        let chip = ui.painter().layout_no_wrap(
            album.format_summary.clone(),
            FontId::monospace(12.0),
            TEXT_2,
        );
        let size = chip.size() + vec2(10.0, 4.0);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(1.0, Color32::from_white_alpha(80)),
            StrokeKind::Inside,
        );
        text_at(ui, rect.min + vec2(5.0, 2.0), chip);
    });
}

/// The white pill that plays the album from its first track.
fn play_button(ui: &mut Ui) -> Response {
    let (fill, fg) = (TEXT, ON_WHITE);
    let label = ui
        .painter()
        .layout_no_wrap("Play".into(), FontId::new(14.0, semibold()), fg);
    let width = 24.0 + 14.0 + 8.0 + label.size().x + 24.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, PLAY_HEIGHT), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Play album"));
    let p = ui.painter();
    let fill = if resp.hovered() {
        fill.gamma_multiply(0.9)
    } else {
        fill
    };
    p.rect_filled(rect, CornerRadius::same((PLAY_HEIGHT / 2.0) as u8), fill);
    let cy = rect.center().y;
    let x = rect.left() + 24.0;
    p.add(Shape::convex_polygon(
        vec![
            Pos2::new(x, cy - 7.0),
            Pos2::new(x + 12.0, cy),
            Pos2::new(x, cy + 7.0),
        ],
        fg,
        Stroke::NONE,
    ));
    p.galley(Pos2::new(x + 22.0, cy - label.size().y / 2.0), label, fg);
    resp
}

fn tracklist(ui: &mut Ui, data: &PageData) -> Option<PageAction> {
    let mut action = None;
    let multi_disc = data
        .tracks
        .first()
        .is_some_and(|first| data.tracks.iter().any(|t| t.disc != first.disc));
    column_header(ui);
    ui.spacing_mut().item_spacing.y = 0.0;
    let mut disc = None;
    for (i, t) in data.tracks.iter().enumerate() {
        if multi_disc && disc != Some(t.disc) {
            disc = Some(t.disc);
            ui.add_space(if i == 0 { 8.0 } else { 18.0 });
            ui.label(
                egui::RichText::new(format!("Disc {}", t.disc))
                    .family(semibold())
                    .color(TEXT_2),
            );
            ui.add_space(6.0);
        }
        let resp = track_row(ui, data, t);
        let enter = resp.has_focus() && ui.input(|inp| inp.key_pressed(Key::Enter));
        if resp.double_clicked() || enter {
            action = Some(PageAction::Play(i));
        } else if resp.clicked() {
            action = Some(PageAction::Select(t.id));
        }
    }
    action
}

/// "#  TITLE  FORMAT  TIME" over a divider.
fn column_header(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let p = ui.painter();
    let font = FontId::new(11.0, semibold());
    let cy = rect.center().y;
    let dur_right = rect.right() - 12.0;
    let fmt_left = dur_right - DURATION_W - FORMAT_W;
    for (x, align, text) in [
        (rect.left() + NUMBER_W - 6.0, Align2::RIGHT_CENTER, "#"),
        (rect.left() + NUMBER_W + 6.0, Align2::LEFT_CENTER, "TITLE"),
        (fmt_left, Align2::LEFT_CENTER, "FORMAT"),
        (dur_right, Align2::RIGHT_CENTER, "TIME"),
    ] {
        p.text(Pos2::new(x, cy), align, text, font.clone(), TEXT_3);
    }
    p.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, BORDER),
    );
    ui.add_space(4.0);
}

fn track_row(ui: &mut Ui, data: &PageData, t: &Track) -> Response {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::ListItem, true, &t.title));
    let unsupported = data
        .device
        .and_then(|(name, caps)| Some((name, track_support(&t.source, caps).err()?)));
    let error = data.track_errors.get(&t.path);
    let hover: Vec<String> = error
        .cloned()
        .into_iter()
        .chain(
            unsupported
                .as_ref()
                .map(|(name, reason)| format!("{name}: {reason}")),
        )
        .collect();
    let resp = if hover.is_empty() {
        resp
    } else {
        resp.on_hover_text(hover.join("\n"))
    };
    if !ui.is_rect_visible(rect) {
        return resp;
    }

    if data.highlight == Some(t.id) {
        // Scroll the highlighted track into view once, when it first shows.
        let key = Id::unique("album_page_highlight_shown");
        let shown = ui.data(|d| d.get_temp::<TrackId>(key));
        if shown != Some(t.id) {
            resp.scroll_to_me(Some(Align::Center));
            ui.data_mut(|d| d.insert_temp(key, t.id));
        }
    }
    let p = ui.painter();
    let playing = data.playing == Some(t.path.as_path());
    if resp.hovered() || playing {
        p.rect_filled(rect, CornerRadius::same(4), HOVER);
    }
    if data.highlight == Some(t.id) {
        p.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, Color32::WHITE),
            StrokeKind::Inside,
        );
    }
    let cy = rect.center().y;
    let mono = FontId::monospace(12.0);
    let number = if playing {
        "▶".to_owned()
    } else if t.number > 0 {
        t.number.to_string()
    } else {
        String::new()
    };
    let num_x = rect.left() + NUMBER_W - 6.0;
    p.text(
        Pos2::new(num_x, cy),
        Align2::RIGHT_CENTER,
        number,
        mono.clone(),
        if playing { TEXT } else { TEXT_3 },
    );

    let (title_color, format_color) = if unsupported.is_some() {
        (TEXT_3, TEXT_3)
    } else {
        (TEXT, TEXT_2)
    };
    let mut x = rect.left() + NUMBER_W + 6.0;
    if error.is_some() {
        paint_warning(p, Pos2::new(x + 6.0, cy));
        x += ICON_W;
    }
    let dur_right = rect.right() - 12.0;
    let fmt_left = dur_right - DURATION_W - FORMAT_W;
    let title_w = (fmt_left - x - 12.0).max(0.0);
    let font = if playing {
        FontId::new(14.0, semibold())
    } else {
        FontId::proportional(14.0)
    };
    let title = truncated(ui, &t.title, font, title_color, title_w);
    p.galley(Pos2::new(x, cy - title.size().y / 2.0), title, title_color);

    let format = truncated(
        ui,
        &short_format(&t.source),
        mono.clone(),
        format_color,
        FORMAT_W,
    );
    let fpos = Pos2::new(fmt_left, cy - format.size().y / 2.0);
    let format_w = format.size().x;
    p.galley(fpos, format, format_color);
    if unsupported.is_some() {
        p.line_segment(
            [Pos2::new(fpos.x, cy), Pos2::new(fpos.x + format_w, cy)],
            Stroke::new(1.0, TEXT_3),
        );
    }
    p.text(
        Pos2::new(dur_right, cy),
        Align2::RIGHT_CENTER,
        duration(t.duration),
        mono,
        TEXT_2,
    );
    resp
}

/// A small outlined warning triangle with an exclamation mark, centered on `c`.
fn paint_warning(p: &egui::Painter, c: Pos2) {
    let stroke = Stroke::new(1.2, TEXT);
    let pts = vec![
        Pos2::new(c.x, c.y - 6.0),
        Pos2::new(c.x + 6.5, c.y + 5.0),
        Pos2::new(c.x - 6.5, c.y + 5.0),
    ];
    p.add(Shape::closed_line(pts, stroke));
    p.line_segment(
        [Pos2::new(c.x, c.y - 2.5), Pos2::new(c.x, c.y + 1.0)],
        stroke,
    );
    p.circle_filled(Pos2::new(c.x, c.y + 3.0), 0.8, TEXT);
}

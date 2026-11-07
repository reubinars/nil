//! The full-window now-playing view on the bar's neutral black: the cover, the transport,
//! and the queue.

use std::time::Duration;

use egui::{
    Color32, CornerRadius, FontId, Popup, Pos2, Rect, Role, Sense, Stroke, Ui, UiBuilder,
    WidgetInfo, vec2,
};

use crate::art::{ArtKind, ArtLoader};
use crate::backend::Album;
use crate::fmt::duration;
use crate::queue_edit;
use crate::state::now_playing::{Badge, NowPlaying};
use crate::state::selection::{ListId, Selection};
use crate::theme::{BAR, TEXT, TEXT_2, bold, semibold};
use crate::views::art_row::{self, ArtRow, DIM};
use crate::views::bar::{Icon, fatal_line, format_line, icon_button, seek_line};
use crate::views::context_menu::{self, MenuPlace, MenuSpec, RowAction};
use crate::views::track_list::{RowDrag, pill};
use crate::views::{paint_art_rounded, shadow, shown_text, text_at, truncated, window_controls};

const COLUMN: f32 = 440.0;
const MAX_COVER: f32 = 380.0;
const MIN_COVER: f32 = 160.0;
/// Height the column needs under the cover at its smallest.
const BELOW_COVER: f32 = 340.0;
const SPECTRUM_MAX: f32 = 72.0;
const QUEUE_ROW: f32 = art_row::ROW_H;
/// Space between queue rows.
const QUEUE_SPACING: f32 = 2.0;
/// Height under the queue list for the Save button.
const SAVE_STRIP: f32 = 48.0;
/// Height of the window strip at the top: drag area and window buttons.
const WINDOW_STRIP: f32 = 40.0;
const TRACK: Color32 = Color32::from_rgba_premultiplied(56, 56, 56, 56);

pub struct NowPlayingData<'a> {
    pub np: &'a NowPlaying,
    /// The playing track's album.
    pub album: Option<&'a Album>,
    /// The album the queue was started from.
    pub queue_album: Option<&'a Album>,
    /// Each queued track's album, by queue index.
    pub row_albums: &'a [Option<Album>],
    pub device: Option<&'a str>,
    /// Large spectrum levels while playing.
    pub spectrum: Option<&'a [f32]>,
    pub selection: &'a Selection,
    pub playlists: &'a [String],
    pub device_ready: bool,
    /// The app's notice and whether it offers Undo (the bar is hidden here).
    pub notice: Option<(&'a str, bool)>,
}

pub enum NowPlayingAction {
    Close,
    PlayPause,
    Previous,
    Next,
    Seek(Duration),
    /// A click, double-click or menu choice on a queue row.
    Row(RowAction),
    Retry,
    /// Save the queue as a playlist.
    SaveQueue,
    /// Remove every upcoming track.
    ClearUpcoming,
    /// The notice's Undo.
    Undo,
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
    if queue.width() > 120.0 {
        match queue_list(ui, queue, data, art) {
            Some(QueueClick::Row(a)) => action = Some(NowPlayingAction::Row(a)),
            Some(QueueClick::Save) => action = Some(NowPlayingAction::SaveQueue),
            Some(QueueClick::Clear) => action = Some(NowPlayingAction::ClearUpcoming),
            Some(QueueClick::Undo) => action = Some(NowPlayingAction::Undo),
            None => {}
        }
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

enum QueueClick {
    Row(RowAction),
    Save,
    Clear,
    Undo,
}

/// "QUEUE · album" with the notice on its right, one row per queued track and the Save
/// and Clear upcoming buttons under them; returns what was clicked.
fn queue_list(
    ui: &mut Ui,
    rect: Rect,
    data: &NowPlayingData,
    art: &mut ArtLoader,
) -> Option<QueueClick> {
    let mut clicked = None;
    let np = data.np;
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        let header = match data.queue_album {
            Some(a) => format!("QUEUE · {}", a.title.to_uppercase()),
            None => "QUEUE".to_owned(),
        };
        let (r, _) = ui.allocate_exact_size(vec2(rect.width(), 28.0), Sense::hover());
        let notice_w = match data.notice {
            Some((text, undo)) => {
                let (w, undo_clicked) = notice(ui, r, text, undo);
                if undo_clicked {
                    clicked = Some(QueueClick::Undo);
                }
                w + 24.0
            }
            None => 0.0,
        };
        let g = truncated(
            ui,
            &header,
            FontId::new(12.0, semibold()),
            DIM,
            (rect.width() - notice_w).max(0.0),
        );
        text_at(ui, Pos2::new(r.left() + 12.0, r.top()), g);
        egui::ScrollArea::vertical()
            .id_salt("np_queue")
            .max_height((ui.available_height() - SAVE_STRIP).max(0.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = QUEUE_SPACING;
                let shown: Vec<usize> = (0..np.queue.len()).collect();
                let tail = if np.fatal.is_none() {
                    queue_edit::tail(&np.queue, np.index)
                } else {
                    None
                };
                // Only upcoming rows drag; the drop's gap is a queue row (0..=len).
                let mut drag = RowDrag::begin(ui.ctx(), tail.is_some());
                let mut list_rect = Rect::NOTHING;
                for (i, item) in np.queue.iter().enumerate() {
                    let drags = tail.as_ref().is_some_and(|t| i >= t.from);
                    let sense = if drags {
                        Sense::click_and_drag()
                    } else {
                        Sense::click()
                    };
                    let (row, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), QUEUE_ROW), sense);
                    list_rect = list_rect.union(row);
                    resp.widget_info(|| WidgetInfo::labeled(Role::ListItem, true, &item.title));
                    // The counts cost a pass over the queue, so only a row whose menu
                    // can show this frame works them out.
                    let menu_may_show = resp.secondary_clicked()
                        || resp.has_focus()
                        || Popup::is_id_open(ui.ctx(), Popup::default_response_id(&resp));
                    let (upcoming, play_next) = if menu_may_show {
                        let target = data.selection.target(&ListId::Queue, &shown, i);
                        queue_edit::counts(tail.as_ref(), &target)
                    } else {
                        (0, false)
                    };
                    let spec = MenuSpec {
                        place: MenuPlace::QueueRow {
                            upcoming,
                            play_next,
                        },
                        count: 0,
                        playlists: data.playlists,
                        exclude: None,
                        device_ready: data.device_ready,
                    };
                    if let Some(a) =
                        context_menu::row(&resp, &ListId::Queue, &shown, i, data.selection, spec)
                    {
                        clicked = Some(QueueClick::Row(a));
                    }
                    let selected = data.selection.is_selected(&ListId::Queue, i);
                    if drags {
                        let target = || data.selection.target(&ListId::Queue, &shown, i);
                        if let Some(a) = drag.row(&resp, i, selected, target)
                            && clicked.is_none()
                        {
                            clicked = Some(QueueClick::Row(a));
                        }
                    }
                    if !ui.is_rect_visible(row) {
                        continue;
                    }
                    let album = data.row_albums.get(i).and_then(Option::as_ref);
                    let time = duration(item.duration);
                    let r = ArtRow {
                        title: &item.title,
                        artist: album.map_or("", |a| a.artist.as_str()),
                        time: &time,
                        album,
                        current: np.index == Some(i),
                        selected,
                        dim: np.index.is_some_and(|c| i < c),
                    };
                    art_row::paint(ui, row, resp.hovered(), &r, art);
                }
                // No bar here: the scroll area's own clip is what can be seen.
                let pitch = QUEUE_ROW + QUEUE_SPACING;
                if let Some(a) = drag.finish(ui, list_rect, np.queue.len(), pitch, 0.0) {
                    clicked = Some(QueueClick::Row(a));
                }
            });
        ui.add_space(12.0);
        let upcoming = np.fatal.is_none()
            && queue_edit::tail(&np.queue, np.index).is_some_and(|t| !t.rows.is_empty());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if pill(
                ui,
                "Save queue as playlist…",
                "Save queue as playlist…",
                false,
            )
            .clicked()
            {
                clicked = Some(QueueClick::Save);
            }
            if upcoming && pill(ui, "Clear upcoming", "Clear upcoming", false).clicked() {
                clicked = Some(QueueClick::Clear);
            }
        });
    });
    clicked
}

/// The notice right-aligned in the header row `r`, in the bar's style, with Undo after
/// it when `undo`; its width, and whether Undo was clicked. It takes no space.
fn notice(ui: &mut Ui, r: Rect, text: &str, undo: bool) -> (f32, bool) {
    let undo_w = if undo { 48.0 } else { 0.0 };
    let g = truncated(
        ui,
        text,
        FontId::monospace(12.0),
        TEXT_2,
        r.width() * 0.6 - undo_w,
    );
    let total = g.size().x + undo_w;
    let left = r.right() - 12.0 - total;
    // Centred on the header text's line, which sits at the top of the row.
    let header_h = ui
        .painter()
        .layout_no_wrap("Q".to_owned(), FontId::new(12.0, semibold()), DIM)
        .size()
        .y;
    let line_y = r.top() + header_h / 2.0;
    let pos = Pos2::new(left, line_y - g.size().y / 2.0);
    let text_rect = Rect::from_min_size(pos, g.size());
    let shown = shown_text(&g);
    let resp = ui.interact(text_rect, ui.scope_id().with("np_notice"), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(Role::Status, true, &shown));
    let text_w = g.size().x;
    ui.painter().galley(pos, g, Color32::PLACEHOLDER);
    let mut clicked = false;
    if undo {
        let rect = Rect::from_min_size(
            Pos2::new(left + text_w + 12.0, line_y - 9.0),
            vec2(undo_w - 12.0, 18.0),
        );
        let resp = ui.interact(rect, ui.scope_id().with("np_undo"), Sense::click());
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Undo"));
        let color = if resp.hovered() { TEXT } else { TEXT_2 };
        let u = truncated(ui, "Undo", FontId::monospace(12.0), color, rect.width());
        let h = u.size().y;
        ui.painter().galley(
            Pos2::new(rect.left(), line_y - h / 2.0),
            u,
            Color32::PLACEHOLDER,
        );
        clicked = resp.clicked();
    }
    (total, clicked)
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

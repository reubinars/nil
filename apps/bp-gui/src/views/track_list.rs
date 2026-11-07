//! A flat list of tracks identified by path, for playlists. Rows look like the album page's.

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::{
    Align2, Color32, Context, CornerRadius, FontId, Id, Key, PointerButton, Pos2, Rect, Response,
    Role, Sense, Stroke, StrokeKind, Ui, WidgetInfo, vec2,
};

use crate::fmt::duration;
use crate::state::selection::{Click, ListId, Selection};
use crate::theme::{DISABLED, HOVER, SELECTED, TEXT, TEXT_2, TEXT_3, semibold};
use crate::views::bar;
use crate::views::context_menu::{self, MenuSpec, RowAction};
use crate::views::truncated;

const ROW_HEIGHT: f32 = 40.0;
const NUMBER_W: f32 = 44.0;
const DURATION_W: f32 = 60.0;
const PILL_HEIGHT: f32 = 36.0;
/// How far the list scrolls per frame while a drag holds the pointer near its edge.
const DRAG_SCROLL: f32 = 8.0;
/// Text on a white pill.
pub(crate) const ON_WHITE: Color32 = Color32::from_rgb(0x11, 0x11, 0x11);

#[derive(Debug, Clone, PartialEq)]
pub struct TrackRow {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub duration_s: Option<f64>,
    /// The file is not on disk: the row is greyed and cannot be played.
    pub missing: bool,
}

pub enum TrackListAction {
    /// Play from this row (the first playable one at or after it): an index into `rows`.
    PlayFrom(usize),
    PlayAll,
    QueueAll,
    SaveAs,
    /// A click or menu choice on a selectable row; rows are the ids in `RowSelect::shown`.
    Row(RowAction),
}

/// Makes the rows selectable, with the right-click menu.
pub struct RowSelect<'a> {
    pub list: &'a ListId,
    /// The id of each row, in display order: `shown[k]` is `rows[k]`.
    pub shown: &'a [usize],
    pub selection: &'a Selection,
    pub menu: MenuSpec<'a>,
    /// Rows can be dragged to a new place (an open playlist); a drop is reported as
    /// `RowAction::Drop`.
    pub drag: bool,
}

/// A drag in progress: the row widget being dragged and the rows (ids) it moves.
#[derive(Clone)]
struct Drag {
    widget: Id,
    rows: Vec<usize>,
}

fn drag_id() -> Id {
    Id::unique("track_list_drag")
}

/// Whether rows are being dragged in some list, or their drag ends this frame (egui
/// itself ends a drag on Esc, before the list sees it).
pub fn dragging(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<Drag>(drag_id()))
        .is_some_and(|d| ctx.is_being_dragged(d.widget) || ctx.drag_stopped_id() == Some(d.widget))
}

/// Cancels a drag in progress, so its release drops nothing (Esc).
pub fn cancel_drag(ctx: &Context) {
    ctx.data_mut(|d| d.remove::<Drag>(drag_id()));
}

/// Where dragging shown rows `rows` (file indices, from `shown`) to `gap` (a shown position,
/// `0..=shown.len()`) moves them in a file of `file_len` rows: `(indices, to)` for
/// `playlists::reorder`. Each dragged row takes the hidden rows directly after it along,
/// up to the next shown row; `to` is the file row at the gap, or `file_len` at the end.
pub fn drop_indices(
    file_len: usize,
    shown: &[usize],
    rows: &[usize],
    gap: usize,
) -> (Vec<usize>, usize) {
    let mut indices = Vec::new();
    for &r in rows {
        let Some(k) = shown.iter().position(|&s| s == r) else {
            continue;
        };
        let end = shown.get(k + 1).copied().unwrap_or(file_len).max(r + 1);
        indices.extend(r..end);
    }
    let to = shown.get(gap).copied().unwrap_or(file_len);
    (indices, to)
}

/// The Play all / Queue all / Save as playlist pills, in a row.
pub fn action_pills(ui: &mut Ui) -> Option<TrackListAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if pill(ui, "Play all", "Play all", true).clicked() {
            action = Some(TrackListAction::PlayAll);
        }
        if pill(ui, "Queue all", "Queue all", false).clicked() {
            action = Some(TrackListAction::QueueAll);
        }
        if pill(ui, "Save as playlist", "Save as playlist", false).clicked() {
            action = Some(TrackListAction::SaveAs);
        }
    });
    action
}

/// The rows, preceded by the Play all / Queue all / Save as playlist buttons when
/// `buttons` is set. `playing` marks the row of the track now playing. With `select`,
/// rows can be selected and right-clicked; without, only double-click and Enter play.
pub fn show(
    ui: &mut Ui,
    id_salt: &str,
    rows: &[TrackRow],
    playing: Option<&Path>,
    buttons: bool,
    select: Option<RowSelect<'_>>,
) -> Option<TrackListAction> {
    let mut action = None;
    ui.push_id(id_salt, |ui| {
        if buttons {
            action = action_pills(ui);
            ui.add_space(16.0);
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        let can_drag = select.as_ref().is_some_and(|s| s.drag);
        let mut drag = RowDrag::begin(ui.ctx(), can_drag);
        let mut list_rect = Rect::NOTHING;
        for (i, row) in rows.iter().enumerate() {
            let id = select.as_ref().and_then(|s| s.shown.get(i).copied());
            let selected = match (&select, id) {
                (Some(s), Some(id)) => s.selection.is_selected(s.list, id),
                _ => false,
            };
            let draggable = can_drag && !row.missing;
            let resp = track_row(ui, i, row, playing, selected, draggable);
            list_rect = list_rect.union(resp.rect);
            if let (Some(s), Some(id)) = (&select, id) {
                match context_menu::row(&resp, s.list, s.shown, id, s.selection, s.menu) {
                    Some(RowAction::Activate(_)) => action = Some(TrackListAction::PlayFrom(i)),
                    Some(a) => action = Some(TrackListAction::Row(a)),
                    None => {}
                }
                if draggable {
                    let target = || s.selection.target(s.list, s.shown, id);
                    if let Some(a) = drag.row(&resp, id, selected, target)
                        && action.is_none()
                    {
                        action = Some(TrackListAction::Row(a));
                    }
                }
                continue;
            }
            let enter = resp.has_focus() && ui.input(|inp| inp.key_pressed(Key::Enter));
            if !row.missing && (resp.double_clicked() || enter) {
                action = Some(TrackListAction::PlayFrom(i));
            }
        }
        // The floating bar covers the page's bottom.
        if let Some(a) = drag.finish(ui, list_rect, rows.len(), ROW_HEIGHT, bar::HEIGHT) {
            action = Some(TrackListAction::Row(a));
        }
    });
    action
}

/// Dragging rows of a list to a new place, over one frame: [`RowDrag::begin`] before the
/// rows, [`RowDrag::row`] for each row that can be dragged, [`RowDrag::finish`] after them.
pub(crate) struct RowDrag {
    ctx: Context,
    /// The drag in progress when the frame began.
    drag: Option<Drag>,
    /// The drag's row: still held, or released this frame.
    held: bool,
    released: bool,
    /// A drag started this frame.
    started: bool,
}

impl RowDrag {
    /// Starts a frame of a list whose rows can be dragged when `can_drag`.
    pub(crate) fn begin(ctx: &Context, can_drag: bool) -> Self {
        let drag = if can_drag {
            ctx.data(|d| d.get_temp::<Drag>(drag_id()))
        } else {
            None
        };
        Self {
            ctx: ctx.clone(),
            drag,
            held: false,
            released: false,
            started: false,
        }
    }

    /// A row that can be dragged, `id` in its list: starts a drag of the rows `target`
    /// gives, or follows the drag in progress on it. Dragging an unselected row makes it
    /// the selection: that `Select` is returned.
    pub(crate) fn row(
        &mut self,
        resp: &Response,
        id: usize,
        selected: bool,
        target: impl FnOnce() -> Vec<usize>,
    ) -> Option<RowAction> {
        if resp.drag_started_by(PointerButton::Primary) {
            self.started = true;
            let drag = Drag {
                widget: resp.id,
                rows: target(),
            };
            self.ctx.data_mut(|d| d.insert_temp(drag_id(), drag));
            return (!selected).then_some(RowAction::Select(id, Click::Plain));
        }
        if self.drag.as_ref().is_some_and(|d| d.widget == resp.id) {
            self.held |= resp.dragged_by(PointerButton::Primary);
            self.released |= resp.drag_stopped_by(PointerButton::Primary);
        }
        None
    }

    /// After the rows in `list_rect` (`n` rows, `row_h` apart): draws the gap while the
    /// drag is held, and returns the `Drop` of a drag released over the list. `covered`
    /// is how much of the window's bottom floats over the list and hides its rows (the
    /// bar), or 0.
    pub(crate) fn finish(
        self,
        ui: &Ui,
        list_rect: Rect,
        n: usize,
        row_h: f32,
        covered: f32,
    ) -> Option<RowAction> {
        let d = self.drag.filter(|_| !self.started)?;
        let gap = (self.held || self.released)
            .then(|| drag_frame(ui, list_rect, n, row_h, covered, self.held))
            .flatten();
        // Released, or its row is gone: the drag is over.
        if !self.held {
            cancel_drag(&self.ctx);
        }
        match (self.released, gap) {
            (true, Some(gap)) => Some(RowAction::Drop { rows: d.rows, gap }),
            _ => None,
        }
    }
}

/// One frame of a drag over the rows in `list_rect` (`n` rows, `row_h` apart, the window's
/// bottom `covered` px hidden): the gap under the pointer, or `None` when it is outside the
/// list. While `held`, draws the gap line and scrolls near the edges.
fn drag_frame(
    ui: &Ui,
    list_rect: Rect,
    n: usize,
    row_h: f32,
    covered: f32,
    held: bool,
) -> Option<usize> {
    let pos = ui.ctx().pointer_latest_pos()?;
    // What of the list can be seen: whatever floats over the window's bottom hides it.
    let mut clip = ui.clip_rect();
    clip.max.y = clip.max.y.min(ui.ctx().content_rect().bottom() - covered);
    if held {
        // Only while there are rows out of view that way.
        let dy = if pos.y < clip.top() + row_h && list_rect.top() < clip.top() {
            DRAG_SCROLL
        } else if pos.y > clip.bottom() - row_h && list_rect.bottom() > clip.bottom() {
            -DRAG_SCROLL
        } else {
            0.0
        };
        if dy != 0.0 {
            ui.scroll_with_delta(vec2(0.0, dy));
            ui.ctx().request_repaint();
        }
    }
    if n == 0 {
        return None;
    }
    let area = list_rect.expand2(vec2(0.0, row_h / 2.0)).intersect(clip);
    if !area.contains(pos) {
        return None;
    }
    let gap = ((pos.y - list_rect.top()) / row_h)
        .round()
        .clamp(0.0, n as f32) as usize;
    if held {
        let y = list_rect.top() + gap as f32 * row_h;
        ui.painter()
            .hline(list_rect.x_range(), y, Stroke::new(2.0, TEXT));
    }
    Some(gap)
}

/// The fill of a white pill, a little dimmer under the pointer.
pub(crate) fn white_fill(hovered: bool) -> Color32 {
    if hovered {
        TEXT.gamma_multiply(0.9)
    } else {
        TEXT
    }
}

/// A round-ended button; `primary` is the white one. `label` is its accessible name.
pub(crate) fn pill(ui: &mut Ui, text: &str, label: &str, primary: bool) -> Response {
    let fg = if primary { ON_WHITE } else { TEXT };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::new(14.0, semibold()), fg);
    let width = galley.size().x + 40.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, PILL_HEIGHT), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let p = ui.painter();
    let round = CornerRadius::same((PILL_HEIGHT / 2.0) as u8);
    if primary {
        p.rect_filled(rect, round, white_fill(resp.hovered()));
    } else {
        if resp.hovered() {
            p.rect_filled(rect, round, HOVER);
        }
        p.rect_stroke(
            rect,
            round,
            Stroke::new(1.0, Color32::from_white_alpha(80)),
            StrokeKind::Inside,
        );
    }
    p.galley(
        Pos2::new(rect.left() + 20.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn track_row(
    ui: &mut Ui,
    index: usize,
    t: &TrackRow,
    playing: Option<&Path>,
    selected: bool,
    draggable: bool,
) -> Response {
    let sense = if t.missing {
        Sense::hover()
    } else if draggable {
        Sense::click_and_drag()
    } else {
        Sense::click()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), sense);
    resp.widget_info(|| WidgetInfo::labeled(Role::ListItem, !t.missing, &t.title));
    let resp = if t.missing {
        resp.on_hover_text("File not found")
    } else {
        resp
    };
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let now = !t.missing && playing == Some(t.path.as_path());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, CornerRadius::same(4), SELECTED);
    } else if !t.missing && (resp.hovered() || now) {
        p.rect_filled(rect, CornerRadius::same(4), HOVER);
    }
    let cy = rect.center().y;
    let mono = FontId::monospace(12.0);
    let number = if now {
        "▶".to_owned()
    } else {
        (index + 1).to_string()
    };
    p.text(
        Pos2::new(rect.left() + NUMBER_W - 6.0, cy),
        Align2::RIGHT_CENTER,
        number,
        mono.clone(),
        if now { TEXT } else { TEXT_3 },
    );
    let (title_color, artist_color) = if t.missing {
        (DISABLED, DISABLED)
    } else {
        (TEXT, TEXT_2)
    };
    let x = rect.left() + NUMBER_W + 6.0;
    let dur_right = rect.right() - 12.0;
    let inner = (dur_right - DURATION_W - x - 24.0).max(0.0);
    let title_w = inner * 0.6;
    let font = if now {
        FontId::new(14.0, semibold())
    } else {
        FontId::proportional(14.0)
    };
    let title = truncated(ui, &t.title, font, title_color, title_w);
    let artist = truncated(
        ui,
        &t.artist,
        FontId::proportional(13.0),
        artist_color,
        inner - title_w,
    );
    let p = ui.painter();
    p.galley(Pos2::new(x, cy - title.size().y / 2.0), title, title_color);
    p.galley(
        Pos2::new(x + title_w + 12.0, cy - artist.size().y / 2.0),
        artist,
        artist_color,
    );
    if let Some(s) = t.duration_s {
        p.text(
            Pos2::new(dur_right, cy),
            Align2::RIGHT_CENTER,
            duration(Duration::from_secs_f64(s)),
            mono,
            if t.missing { DISABLED } else { TEXT_2 },
        );
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_indices_carries_hidden_followers() {
        // A, (B), C: B is hidden.
        assert_eq!(drop_indices(3, &[0, 2], &[0], 2), (vec![0, 1], 3));
    }

    #[test]
    fn drop_indices_moves_c_above_a() {
        assert_eq!(drop_indices(3, &[0, 2], &[2], 0), (vec![2], 0));
    }

    #[test]
    fn drop_indices_carries_hidden_rows_at_the_end() {
        // A, B, (C), (D): dragging B takes C and D with it.
        assert_eq!(drop_indices(4, &[0, 1], &[1], 0), (vec![1, 2, 3], 0));
    }

    #[test]
    fn drop_indices_gap_between_rows_is_the_next_shown_row() {
        // A, (B), C, D: the gap above D is file row 3.
        assert_eq!(drop_indices(4, &[0, 2, 3], &[0], 2), (vec![0, 1], 3));
    }
}

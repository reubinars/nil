//! The Playlists tab: the list of saved playlists, and one playlist's page.

use std::path::Path;

use egui::{
    Align, CornerRadius, FontId, Id, Layout, Modal, Pos2, Role, Sense, Ui, WidgetInfo, vec2,
};

use crate::theme::{BORDER, HOVER, TEXT, TEXT_2, TEXT_3};
use crate::views::context_menu::RowAction;
use crate::views::track_list::{self, RowSelect, TrackListAction, TrackRow, pill};
use crate::views::{count, heading, truncated};

const ROW_HEIGHT: f32 = 52.0;

pub enum PlaylistsAction {
    Open(String),
    New,
}

/// The playlist names, with a New playlist button beside the heading; returns what
/// was clicked.
pub fn list_page(ui: &mut Ui, names: &[String]) -> Option<PlaylistsAction> {
    let mut open = None;
    let top = ui.cursor().min;
    heading(
        ui,
        "Playlists",
        Some(&count(names.len(), "playlist", "playlists")),
    );
    let right = ui.max_rect().right();
    let button_area = egui::Rect::from_min_max(
        Pos2::new(right - 220.0, top.y),
        Pos2::new(right, ui.cursor().top()),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(button_area)
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            if pill(ui, "New playlist", "New playlist", false).clicked() {
                open = Some(PlaylistsAction::New);
            }
        },
    );
    ui.add_space(24.0);
    egui::ScrollArea::vertical()
        .id_salt("playlists_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if names.is_empty() {
                ui.label(
                    egui::RichText::new("No playlists yet. Save the queue from Now playing.")
                        .color(TEXT_2),
                );
            }
            ui.spacing_mut().item_spacing.y = 0.0;
            for name in names {
                if name_row(ui, name).clicked() {
                    open = Some(PlaylistsAction::Open(name.clone()));
                }
            }
            ui.add_space(crate::views::bar::CLEARANCE);
        });
    open
}

fn name_row(ui: &mut Ui, name: &str) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, name));
    if ui.is_rect_visible(rect) {
        let galley = truncated(
            ui,
            name,
            FontId::proportional(16.0),
            TEXT,
            (rect.width() - 24.0).max(0.0),
        );
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, CornerRadius::same(4), HOVER);
        }
        p.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            egui::Stroke::new(1.0, BORDER),
        );
        p.galley(
            Pos2::new(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0),
            galley,
            TEXT,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub enum PlaylistAction {
    Play,
    Queue,
    Rename,
    Delete,
    /// Play from this row: an index into the shown rows.
    PlayFrom(usize),
    /// A click or menu choice on a row; rows are file indices.
    Row(RowAction),
}

/// A readable playlist: the rows shown (those whose file exists) and how they are
/// selected.
pub struct PageRows<'a> {
    pub rows: &'a [TrackRow],
    /// Rows in the file, hidden ones included.
    pub file_len: usize,
    pub select: RowSelect<'a>,
}

/// One playlist: its name, the Play / Queue / Rename / Delete buttons and its tracks.
/// `rows` is an error message when the file could not be read.
pub fn playlist_page(
    ui: &mut Ui,
    name: &str,
    rows: Result<PageRows<'_>, &str>,
    playing: Option<&Path>,
    playing_now: bool,
) -> Option<PlaylistAction> {
    let mut action = None;
    let (n, hidden) = rows
        .as_ref()
        .map_or((0, 0), |r| (r.file_len, r.file_len - r.rows.len()));
    let tracks = count(n, "track", "tracks");
    if hidden > 0 {
        heading(ui, name, None);
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("{tracks} · {hidden} not found"))
                .size(14.0)
                .color(TEXT_3),
        );
    } else {
        heading(ui, name, Some(&tracks));
    }
    if playing_now {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Playing now · changes apply next time you play it.")
                .size(14.0)
                .color(TEXT_3),
        );
    }
    ui.add_space(16.0);
    let ok = rows.is_ok();
    egui::ScrollArea::vertical()
        .id_salt("playlist_page")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if ok {
                    if pill(ui, "Play", "Play playlist", true).clicked() {
                        action = Some(PlaylistAction::Play);
                    }
                    if pill(ui, "Queue", "Queue playlist", false).clicked() {
                        action = Some(PlaylistAction::Queue);
                    }
                    if pill(ui, "Rename", "Rename playlist", false).clicked() {
                        action = Some(PlaylistAction::Rename);
                    }
                }
                if pill(ui, "Delete", "Delete playlist", false).clicked() {
                    action = Some(PlaylistAction::Delete);
                }
            });
            ui.add_space(16.0);
            match rows {
                Ok(page) => {
                    if page.file_len == 0 {
                        ui.label(
                            egui::RichText::new(
                                "No tracks yet. Right-click a track and choose Add to playlist.",
                            )
                            .color(TEXT_2),
                        );
                    }
                    match track_list::show(
                        ui,
                        "playlist_tracks",
                        page.rows,
                        playing,
                        false,
                        Some(page.select),
                    ) {
                        Some(TrackListAction::PlayFrom(i)) => {
                            action = Some(PlaylistAction::PlayFrom(i));
                        }
                        Some(TrackListAction::Row(a)) => action = Some(PlaylistAction::Row(a)),
                        _ => {}
                    }
                }
                Err(e) => {
                    ui.label(
                        egui::RichText::new(format!("Could not open this playlist: {e}"))
                            .color(TEXT_3),
                    );
                }
            }
            ui.add_space(crate::views::bar::CLEARANCE);
        });
    action
}

/// The delete confirmation; `Some(true)` = delete, `Some(false)` = cancelled.
pub fn delete_dialog(ctx: &egui::Context, name: &str, error: Option<&str>) -> Option<bool> {
    let mut answer = None;
    let modal = Modal::new(Id::unique("delete_playlist")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.label(format!("Delete playlist \"{name}\"?"));
        if let Some(e) = error {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(e).color(egui::Color32::from_rgb(0xE5, 0x73, 0x73)));
        }
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Delete").clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }
    answer
}

//! The search page: matching artists, albums and tracks, each section shown when it has
//! hits.

use egui::{Align2, CornerRadius, FontId, Key, Pos2, Response, Role, Sense, Ui, WidgetInfo, vec2};

use crate::art::ArtLoader;
use crate::backend::{AlbumId, ArtistId, Track, TrackId};
use crate::fmt::{duration, short_format};
use crate::state::search::SearchResults;
use crate::theme::{HOVER, TEXT, TEXT_2, TEXT_3, semibold};
use crate::views::albums::{GAP, layout, tile_widget};
use crate::views::{artists, heading, truncated};

const ROW_HEIGHT: f32 = 48.0;
const FORMAT_W: f32 = 110.0;
const DURATION_W: f32 = 60.0;

pub enum SearchAction {
    Artist(ArtistId),
    Album(AlbumId),
    OpenTrack { album: AlbumId, track: TrackId },
    PlayTrack { album: AlbumId, track: TrackId },
}

pub fn show(ui: &mut Ui, r: &SearchResults, art: &mut ArtLoader) -> Option<SearchAction> {
    let mut action = None;
    if r.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() / 3.0).max(0.0));
            ui.label(
                egui::RichText::new(format!("No results for “{}”", r.query))
                    .size(16.0)
                    .color(TEXT_2),
            );
        });
        return None;
    }
    egui::ScrollArea::vertical()
        .id_salt("search")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            heading(ui, &format!("“{}”", r.query), None);
            if !r.artists.is_empty() {
                section(ui, "Artists");
                if let Some(id) = artists::row(ui, &r.artists, art) {
                    action = Some(SearchAction::Artist(id));
                }
            }
            if !r.albums.is_empty() {
                section(ui, "Albums");
                let (cols, side) = layout(ui.available_width());
                for row in r.albums.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = GAP;
                        for album in row {
                            if tile_widget(ui, album, art, side) {
                                action = Some(SearchAction::Album(album.id));
                            }
                        }
                    });
                    ui.add_space(16.0);
                }
            }
            if !r.tracks.is_empty() {
                section(ui, "Tracks");
                ui.spacing_mut().item_spacing.y = 0.0;
                for (t, album) in &r.tracks {
                    let resp = track_row(ui, t, &album.artist, &album.title);
                    let enter = resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if resp.double_clicked() || enter {
                        action = Some(SearchAction::PlayTrack {
                            album: album.id,
                            track: t.id,
                        });
                    } else if resp.clicked() {
                        action = Some(SearchAction::OpenTrack {
                            album: album.id,
                            track: t.id,
                        });
                    }
                }
            }
            ui.add_space(crate::views::bar::CLEARANCE);
        });
    action
}

/// A section title, 22 px semibold.
fn section(ui: &mut Ui, title: &str) {
    ui.add_space(28.0);
    let galley = ui
        .painter()
        .layout_no_wrap(title.to_owned(), FontId::new(22.0, semibold()), TEXT);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(Role::Heading, true, title));
    ui.painter().galley(rect.min, galley, TEXT);
    ui.add_space(14.0);
}

/// Title over "artist · album", then format and length.
fn track_row(ui: &mut Ui, t: &Track, artist: &str, album: &str) -> Response {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::ListItem, true, &t.title));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(4), HOVER);
    }
    let dur_right = rect.right() - 12.0;
    let fmt_left = dur_right - DURATION_W - FORMAT_W;
    let x = rect.left() + 12.0;
    let width = (fmt_left - x - 12.0).max(0.0);
    let title = truncated(ui, &t.title, FontId::proportional(14.0), TEXT, width);
    let sub = truncated(
        ui,
        &format!("{artist} · {album}"),
        FontId::proportional(12.0),
        TEXT_2,
        width,
    );
    let cy = rect.center().y;
    let p = ui.painter();
    p.galley(Pos2::new(x, cy - title.size().y), title, TEXT);
    p.galley(Pos2::new(x, cy + 1.0), sub, TEXT_2);
    let mono = FontId::monospace(12.0);
    p.text(
        Pos2::new(fmt_left, cy),
        Align2::LEFT_CENTER,
        short_format(&t.source),
        mono.clone(),
        TEXT_3,
    );
    p.text(
        Pos2::new(dur_right, cy),
        Align2::RIGHT_CENTER,
        duration(t.duration),
        mono,
        TEXT_2,
    );
    resp
}

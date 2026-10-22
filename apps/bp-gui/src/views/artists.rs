//! Artist tiles: the artist's first cover cropped to a circle, the name and an album count.

use egui::{FontId, Pos2, Rect, Role, Sense, Stroke, Ui, WidgetInfo, vec2};

use crate::art::{ArtKind, ArtLoader, ArtState};
use crate::backend::ArtistId;
use crate::state::artists::ArtistTile;
use crate::theme::{TEXT, TEXT_2, semibold};
use crate::views::albums::grid;
use crate::views::{count, paint_art_rounded, shadow, truncated};

/// Name and count lines under the circle.
const CAPTION: f32 = 48.0;
/// Tile side in the one-row strip on the search page.
const ROW_TILE: f32 = 140.0;

/// The artists grid; returns the artist clicked and the scroll offset.
pub fn show(
    ui: &mut Ui,
    tiles: &[ArtistTile],
    art: &mut ArtLoader,
    restore: Option<f32>,
) -> (Option<ArtistId>, f32) {
    let (clicked, scroll) = grid(
        ui,
        "artists",
        tiles.len(),
        CAPTION,
        restore,
        |ui, i, side| tile_widget(ui, &tiles[i], art, side),
    );
    (clicked.map(|i| tiles[i].id), scroll)
}

/// One row of tiles that scrolls sideways, for the search page.
pub fn row(ui: &mut Ui, tiles: &[ArtistTile], art: &mut ArtLoader) -> Option<ArtistId> {
    let mut clicked = None;
    egui::ScrollArea::horizontal()
        .id_salt("artist_row")
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 20.0;
                for t in tiles {
                    if tile_widget(ui, t, art, ROW_TILE) {
                        clicked = Some(t.id);
                    }
                }
            });
        });
    clicked
}

fn tile_widget(ui: &mut Ui, t: &ArtistTile, art: &mut ArtLoader, side: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side + CAPTION), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &t.name));
    if !ui.is_rect_visible(rect) {
        return resp.clicked();
    }
    let circle = Rect::from_min_size(rect.min, vec2(side, side));
    let state = match t.cover {
        Some(album) => art.texture(ui.ctx(), album, ArtKind::Thumb),
        None => ArtState::Missing,
    };
    let radius = side / 2.0;
    let p = ui.painter();
    shadow(p, circle, radius);
    paint_art_rounded(p, circle, &state, &t.name, radius);
    if resp.hovered() {
        p.circle_stroke(circle.center(), radius - 0.5, Stroke::new(1.0, TEXT));
    }
    let name = truncated(ui, &t.name, FontId::new(14.0, semibold()), TEXT, side);
    let albums = truncated(
        ui,
        &count(t.albums, "album", "albums"),
        FontId::proportional(12.0),
        TEXT_2,
        side,
    );
    let y = circle.bottom() + 10.0;
    let p = ui.painter();
    let name_h = name.size().y;
    p.galley(
        Pos2::new(rect.center().x - name.size().x / 2.0, y),
        name,
        TEXT,
    );
    p.galley(
        Pos2::new(rect.center().x - albums.size().x / 2.0, y + name_h + 2.0),
        albums,
        TEXT_2,
    );
    resp.clicked()
}

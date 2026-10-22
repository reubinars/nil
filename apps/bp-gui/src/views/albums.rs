//! The album grid: virtualized rows of square art tiles that fill the width, and the
//! empty state shown before any music folder is set.

use egui::{
    Color32, CornerRadius, FontId, Pos2, Rect, Role, Sense, Stroke, StrokeKind, Ui, WidgetInfo,
    vec2,
};

use crate::art::{ArtKind, ArtLoader};
use crate::backend::{Album, AlbumId};
use crate::fmt::format_tag;
use crate::theme::{TEXT, TEXT_2, semibold};
use crate::views::{paint_art_rounded, shadow, text_at, truncated};

/// Space between tiles across.
pub const GAP: f32 = 20.0;
/// Space between rows.
const ROW_GAP: f32 = 24.0;
/// Title and artist lines under the art.
const CAPTION: f32 = 44.0;
/// Smallest tile side; tiles grow from it to fill the row.
pub const TILE_TARGET: f32 = 190.0;
const RADIUS: f32 = 6.0;
const TAG_OUTLINE: Color32 = Color32::from_rgb(0x3A, 0x3A, 0x3A);

/// Tiles per row for an available width.
pub fn columns(width: f32, tile: f32) -> usize {
    ((width / (tile + GAP)).floor() as usize).max(1)
}

/// Columns and tile side for `width`: as many `TILE_TARGET` tiles as fit, widened so the
/// row spans the whole width.
pub fn layout(width: f32) -> (usize, f32) {
    let cols = columns(width + GAP, TILE_TARGET);
    let tile = ((width - GAP * (cols - 1) as f32) / cols as f32).floor();
    (cols, tile.max(1.0))
}

/// A virtualized grid of `count` tiles `caption` px taller than wide, scrolled to
/// `restore` if given. `tile` paints item `i` at side `side` and returns whether it was
/// clicked. Returns the clicked index and the scroll offset.
pub(crate) fn grid(
    ui: &mut Ui,
    id: &str,
    count: usize,
    caption: f32,
    restore: Option<f32>,
    mut tile: impl FnMut(&mut Ui, usize, f32) -> bool,
) -> (Option<usize>, f32) {
    let mut clicked = None;
    let (cols, side) = layout(ui.available_width());
    let rows = count.div_ceil(cols);
    // One empty row at the end lets the last real row scroll clear of the floating bar.
    let total_rows = if count == 0 { 0 } else { rows + 1 };
    ui.spacing_mut().item_spacing = vec2(GAP, ROW_GAP);
    let mut area = egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false]);
    if let Some(y) = restore {
        area = area.vertical_scroll_offset(y);
    }
    let out = area.show_rows(ui, side + caption, total_rows, |ui, range| {
        for r in range.filter(|&r| r < rows) {
            ui.horizontal(|ui| {
                for i in (r * cols)..((r + 1) * cols).min(count) {
                    if tile(ui, i, side) {
                        clicked = Some(i);
                    }
                }
            });
        }
    });
    (clicked, out.state.clamped_offset().y)
}

/// Shows the album grid; returns the album clicked and the scroll offset.
pub fn show(
    ui: &mut Ui,
    albums: &[Album],
    art: &mut ArtLoader,
    restore: Option<f32>,
) -> (Option<AlbumId>, f32) {
    let (clicked, scroll) = grid(ui, "grid", albums.len(), CAPTION, restore, |ui, i, side| {
        tile_widget(ui, &albums[i], art, side)
    });
    (clicked.map(|i| albums[i].id), scroll)
}

/// One album tile; returns whether it was clicked.
pub(crate) fn tile_widget(ui: &mut Ui, album: &Album, art: &mut ArtLoader, tile: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(tile, tile + CAPTION), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &album.title));
    if !ui.is_rect_visible(rect) {
        return resp.clicked();
    }
    let art_rect = Rect::from_min_size(rect.min, vec2(tile, tile));
    let state = art.texture(ui.ctx(), album.id, ArtKind::Thumb);
    {
        let p = ui.painter();
        shadow(p, art_rect, RADIUS);
        paint_art_rounded(p, art_rect, &state, &album.title, RADIUS);
        if resp.hovered() {
            p.rect_stroke(
                art_rect,
                CornerRadius::same(RADIUS as u8),
                Stroke::new(1.0, Color32::WHITE),
                StrokeKind::Inside,
            );
        }
    }
    // The format tag sits at the caption's right; title and artist fill the rest.
    let tag = ui.painter().layout_no_wrap(
        format_tag(&album.format_summary),
        FontId::monospace(10.0),
        TEXT_2,
    );
    let tag_w = tag.size().x + 8.0;
    let y = art_rect.bottom() + 8.0;
    let tag_pos = Pos2::new(rect.right() - tag_w + 4.0, y + 2.0);
    let outline = Rect::from_min_size(tag_pos - vec2(4.0, 1.0), vec2(tag_w, tag.size().y + 2.0));
    ui.painter().rect_stroke(
        outline,
        CornerRadius::same(3),
        Stroke::new(1.0, TAG_OUTLINE),
        StrokeKind::Inside,
    );
    text_at(ui, tag_pos, tag);
    let text_w = (tile - tag_w - 8.0).max(0.0);
    let title = truncated(
        ui,
        &album.title,
        FontId::new(13.0, semibold()),
        TEXT,
        text_w,
    );
    let artist = truncated(
        ui,
        &album.artist,
        FontId::proportional(12.0),
        TEXT_2,
        text_w,
    );
    let title_h = title.size().y;
    let p = ui.painter();
    p.galley(Pos2::new(rect.left(), y), title, TEXT);
    p.galley(Pos2::new(rect.left(), y + title_h + 2.0), artist, TEXT_2);
    resp.clicked()
}

/// The page shown before any music folder is set; returns whether "Add music folder"
/// was clicked.
pub fn empty(ui: &mut Ui) -> bool {
    let mut clicked = false;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() / 2.0 - 60.0).max(0.0));
        ui.label(
            egui::RichText::new("No music folders yet")
                .size(18.0)
                .family(semibold())
                .color(TEXT),
        );
        ui.add_space(16.0);
        let button = egui::Button::new(
            egui::RichText::new("Add music folder")
                .size(14.0)
                .family(semibold())
                .color(Color32::from_rgb(0x11, 0x11, 0x11)),
        )
        .fill(TEXT)
        .corner_radius(CornerRadius::same(22))
        .min_size(vec2(180.0, 44.0));
        clicked = ui.add(button).clicked();
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tile_layout_fills_the_row() {
        for width in [150.0, 500.0, 1000.0, 1050.0, 1063.0] {
            let (cols, tile) = layout(width);
            let used = cols as f32 * tile + (cols - 1) as f32 * GAP;
            assert!(used <= width, "{width}: {used}");
            assert!(used > width - cols as f32, "{width}: {used}");
            if width >= TILE_TARGET {
                assert!(
                    (TILE_TARGET..TILE_TARGET * 2.0).contains(&tile),
                    "{width}: {tile}"
                );
            }
        }
        // 5 × 190 + 4 × 20 = 1030 fits in 1030.
        assert_eq!(layout(1030.0).0, 5);
    }

    #[test]
    fn columns_fill_width_with_at_least_one() {
        assert_eq!(columns(1000.0, 160.0), 5);
        assert_eq!(columns(100.0, 160.0), 1);
        assert_eq!(columns(180.0 * 3.0, 160.0), 3);
    }
}

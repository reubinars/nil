//! A track row with its album's thumbnail, as the Now Playing queue draws it. Plugin
//! track lists use it too, so the two cannot drift apart.

use egui::{Color32, CornerRadius, FontId, Pos2, Rect, Ui, vec2};

use crate::art::{ArtKind, ArtLoader, ArtState};
use crate::backend::Album;
use crate::theme::{SELECTED, semibold};
use crate::views::{paint_art_rounded, truncated};

pub const ROW_H: f32 = 50.0;
/// The queue's secondary text: artists, times, its header.
pub const DIM: Color32 = Color32::from_rgba_premultiplied(190, 190, 190, 190);

pub struct ArtRow<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    /// Drawn right-aligned in mono; empty draws nothing.
    pub time: &'a str,
    /// The thumbnail's album; without one the row gets the grey placeholder.
    pub album: Option<&'a Album>,
    /// The playing track: filled, with a semibold title.
    pub current: bool,
    pub selected: bool,
    /// Faded, with a darkened thumbnail (played queue rows, missing files).
    pub dim: bool,
}

/// Paints `r` into `row`. Art loads in the background: the placeholder shows until then.
pub fn paint(ui: &Ui, row: Rect, hovered: bool, r: &ArtRow, art: &mut ArtLoader) {
    let p = ui.painter();
    if r.selected {
        p.rect_filled(row, CornerRadius::same(6), SELECTED);
    } else if r.current {
        p.rect_filled(row, CornerRadius::same(6), Color32::from_white_alpha(40));
    } else if hovered {
        p.rect_filled(row, CornerRadius::same(6), Color32::from_white_alpha(18));
    }
    let alpha = if r.dim { 0.55 } else { 1.0 };
    let th = Rect::from_min_size(row.min + vec2(12.0, 7.0), vec2(36.0, 36.0));
    match r.album {
        Some(album) => {
            let state = art.texture(ui.ctx(), album.id, ArtKind::Thumb);
            paint_art_rounded(ui.painter(), th, &state, &album.title, 3.0);
        }
        None => paint_art_rounded(ui.painter(), th, &ArtState::Missing, r.title, 3.0),
    }
    if r.dim {
        ui.painter()
            .rect_filled(th, CornerRadius::same(3), Color32::from_black_alpha(110));
    }
    let x = th.right() + 14.0;
    let time_w = 56.0;
    let w = (row.right() - time_w - 12.0 - x).max(0.0);
    let font = if r.current {
        FontId::new(14.0, semibold())
    } else {
        FontId::proportional(14.0)
    };
    let white = Color32::WHITE.gamma_multiply(alpha);
    let title = truncated(ui, r.title, font, white, w);
    let sub = truncated(
        ui,
        r.artist,
        FontId::proportional(12.0),
        DIM.gamma_multiply(alpha),
        w,
    );
    let p = ui.painter();
    let cy = row.center().y;
    p.galley(Pos2::new(x, cy - title.size().y), title, white);
    p.galley(Pos2::new(x, cy + 1.0), sub, DIM);
    if !r.time.is_empty() {
        p.text(
            Pos2::new(row.right() - 12.0, cy),
            egui::Align2::RIGHT_CENTER,
            r.time,
            FontId::monospace(12.0),
            DIM.gamma_multiply(alpha),
        );
    }
}

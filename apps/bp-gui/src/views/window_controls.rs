//! The window has no system title bar: the minimize, maximize and close buttons, the
//! drag area that moves the window, and the edges that resize it.

use egui::{
    Color32, Context, CursorIcon, Id, Order, Pos2, Rect, ResizeDirection, Response, Role, Sense,
    Stroke, Ui, ViewportCommand, WidgetInfo, vec2,
};

use crate::theme::{HOVER, TEXT, TEXT_2};

/// Width of one window button; the three sit flush in the top-right corner.
pub const BUTTON_W: f32 = 46.0;
/// Width of the three buttons together.
pub const WIDTH: f32 = BUTTON_W * 3.0;
/// The close button's hover fill, as on Windows.
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
/// Thickness of the resize edges; corners are twice as long.
const EDGE: f32 = 5.0;

#[derive(Clone, Copy)]
enum Kind {
    Minimize,
    Maximize,
    Restore,
    Close,
}

fn maximized(ctx: &Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

/// Dragging `rect` moves the window; a double click maximizes or restores it. Add it
/// before the widgets over `rect`, so they take the clicks.
pub fn drag_area(ui: &mut Ui, rect: Rect) {
    let resp = ui.interact(
        rect,
        ui.scope_id().with("window_drag"),
        Sense::click_and_drag(),
    );
    let ctx = ui.ctx();
    if resp.double_clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized(ctx)));
    } else if resp.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// Minimize, maximize (restore while maximized) and close, left to right in `rect`.
pub fn buttons(ui: &mut Ui, rect: Rect) {
    let ctx = ui.ctx().clone();
    let max = if maximized(&ctx) {
        Kind::Restore
    } else {
        Kind::Maximize
    };
    for (i, kind) in [Kind::Minimize, max, Kind::Close].into_iter().enumerate() {
        let r = Rect::from_min_size(
            rect.min + vec2(BUTTON_W * i as f32, 0.0),
            vec2(BUTTON_W, rect.height()),
        );
        if button(ui, r, kind).clicked() {
            ctx.send_viewport_cmd(match kind {
                Kind::Minimize => ViewportCommand::Minimized(true),
                Kind::Maximize => ViewportCommand::Maximized(true),
                Kind::Restore => ViewportCommand::Maximized(false),
                Kind::Close => ViewportCommand::Close,
            });
        }
    }
}

fn button(ui: &mut Ui, rect: Rect, kind: Kind) -> Response {
    let label = match kind {
        Kind::Minimize => "Minimize",
        Kind::Maximize => "Maximize",
        Kind::Restore => "Restore",
        Kind::Close => "Close",
    };
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("window_button", label)),
        Sense::click(),
    );
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let p = ui.painter();
    let hovered = resp.hovered();
    if hovered {
        let fill = if matches!(kind, Kind::Close) {
            CLOSE_HOVER
        } else {
            HOVER
        };
        p.rect_filled(rect, 0.0, fill);
    }
    let stroke = Stroke::new(1.0, if hovered { TEXT } else { TEXT_2 });
    let c = rect.center();
    match kind {
        Kind::Minimize => {
            p.line_segment([c + vec2(-5.0, 0.0), c + vec2(5.0, 0.0)], stroke);
        }
        Kind::Maximize => {
            p.rect_stroke(
                Rect::from_center_size(c, vec2(10.0, 10.0)),
                1.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        Kind::Restore => {
            // A front square with a second one peeking out behind, up and right.
            let front = Rect::from_min_size(c + vec2(-5.0, -3.0), vec2(8.0, 8.0));
            p.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Inside);
            let (l, t, r, b) = (c.x - 3.0, c.y - 5.0, c.x + 5.0, c.y + 3.0);
            p.line_segment([Pos2::new(l, front.top()), Pos2::new(l, t)], stroke);
            p.line_segment([Pos2::new(l, t), Pos2::new(r, t)], stroke);
            p.line_segment([Pos2::new(r, t), Pos2::new(r, b)], stroke);
            p.line_segment([Pos2::new(r, b), Pos2::new(front.right(), b)], stroke);
        }
        Kind::Close => {
            p.line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], stroke);
            p.line_segment([c + vec2(5.0, -5.0), c + vec2(-5.0, 5.0)], stroke);
        }
    }
    resp
}

/// Grab zones along the window's edges and corners that resize it. Not while maximized.
pub fn resize_edges(ctx: &Context, screen: Rect) {
    if maximized(ctx) {
        return;
    }
    use ResizeDirection::*;
    let (l, t, r, b) = (screen.left(), screen.top(), screen.right(), screen.bottom());
    let corner = EDGE * 2.0;
    let zones = [
        (
            North,
            Rect::from_min_max(Pos2::new(l + corner, t), Pos2::new(r - corner, t + EDGE)),
        ),
        (
            South,
            Rect::from_min_max(Pos2::new(l + corner, b - EDGE), Pos2::new(r - corner, b)),
        ),
        (
            West,
            Rect::from_min_max(Pos2::new(l, t + corner), Pos2::new(l + EDGE, b - corner)),
        ),
        (
            East,
            Rect::from_min_max(Pos2::new(r - EDGE, t + corner), Pos2::new(r, b - corner)),
        ),
        (
            NorthWest,
            Rect::from_min_size(Pos2::new(l, t), vec2(corner, corner)),
        ),
        (
            NorthEast,
            Rect::from_min_size(Pos2::new(r - corner, t), vec2(corner, corner)),
        ),
        (
            SouthWest,
            Rect::from_min_size(Pos2::new(l, b - corner), vec2(corner, corner)),
        ),
        (
            SouthEast,
            Rect::from_min_size(Pos2::new(r - corner, b - corner), vec2(corner, corner)),
        ),
    ];
    for (dir, rect) in zones {
        let cursor = match dir {
            North => CursorIcon::ResizeNorth,
            South => CursorIcon::ResizeSouth,
            West => CursorIcon::ResizeWest,
            East => CursorIcon::ResizeEast,
            NorthWest => CursorIcon::ResizeNorthWest,
            NorthEast => CursorIcon::ResizeNorthEast,
            SouthWest => CursorIcon::ResizeSouthWest,
            SouthEast => CursorIcon::ResizeSouthEast,
        };
        // Foreground, so the edges win over the pages and the floating bar under them.
        egui::Area::new(Id::unique(("window_resize", format!("{dir:?}"))))
            .order(Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                let (_, resp) = ui.allocate_exact_size(rect.size(), Sense::drag());
                if resp.hovered() || resp.dragged() {
                    ui.ctx().set_cursor_icon(cursor);
                }
                if resp.drag_started() {
                    ui.ctx()
                        .send_viewport_cmd(ViewportCommand::BeginResize(dir));
                }
            });
    }
}

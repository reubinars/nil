//! The device list that opens from the bar's device button: pick an output, or go to
//! its capabilities in Settings.

use bp_core::{DeviceId, DeviceInfo};
use egui::{
    Align2, CornerRadius, FontId, Pos2, Response, Role, Sense, Stroke, Ui, WidgetInfo, vec2,
};

use crate::theme::{BORDER, HOVER, RAISED, TEXT, TEXT_2, TEXT_3, semibold};
use crate::views::truncated;

/// Inner width; the popup frame adds its margin.
const WIDTH: f32 = 268.0;
const ROW_HEIGHT: f32 = 40.0;

pub enum DeviceListAction {
    Select(DeviceId),
    Capabilities,
}

pub fn show(
    ui: &mut Ui,
    devices: &[DeviceInfo],
    selected: Option<&DeviceId>,
) -> Option<DeviceListAction> {
    let mut action = None;
    ui.set_width(WIDTH);
    ui.spacing_mut().item_spacing.y = 2.0;
    let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 26.0), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        "OUTPUT",
        FontId::new(11.0, semibold()),
        TEXT_3,
    );
    for d in devices {
        let is_selected = selected == Some(&d.id);
        if device_row(ui, &d.name, is_selected).clicked() && !is_selected {
            action = Some(DeviceListAction::Select(d.id.clone()));
        }
    }
    let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 9.0), Sense::hover());
    ui.painter().hline(
        rect.x_range().shrink(4.0),
        rect.center().y,
        Stroke::new(1.0, BORDER),
    );
    if link_row(ui, "Device capabilities…").clicked() {
        action = Some(DeviceListAction::Capabilities);
    }
    action
}

/// A device; the selected one has a dot and a check.
fn device_row(ui: &mut Ui, name: &str, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(WIDTH, ROW_HEIGHT), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, name));
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, CornerRadius::same(5), RAISED);
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(5), HOVER);
    }
    let cy = rect.center().y;
    if selected {
        p.circle_filled(Pos2::new(rect.left() + 14.0, cy), 4.0, TEXT);
        let c = Pos2::new(rect.right() - 16.0, cy);
        let stroke = Stroke::new(1.8, TEXT);
        p.line_segment(
            [Pos2::new(c.x - 5.0, c.y), Pos2::new(c.x - 1.5, c.y + 3.5)],
            stroke,
        );
        p.line_segment(
            [
                Pos2::new(c.x - 1.5, c.y + 3.5),
                Pos2::new(c.x + 5.0, c.y - 4.0),
            ],
            stroke,
        );
    }
    let color = if selected { TEXT } else { TEXT_2 };
    let text_x = rect.left() + 28.0;
    let galley = truncated(
        ui,
        name,
        FontId::proportional(14.0),
        color,
        rect.right() - text_x - 30.0,
    );
    let pos = Pos2::new(text_x, cy - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, color);
    resp
}

fn link_row(ui: &mut Ui, text: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(WIDTH, 36.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, text));
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(5), HOVER);
    }
    let color = if resp.hovered() { TEXT } else { TEXT_2 };
    p.text(
        Pos2::new(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(13.0),
        color,
    );
    resp
}

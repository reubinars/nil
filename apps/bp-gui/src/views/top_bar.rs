//! The top bar, which is also the window's title bar: the nil wordmark, back/forward, the Home / Albums /
//! Artists tabs, search, the settings gear and the window buttons. Its empty space moves
//! the window.

use egui::{
    Align, Color32, CornerRadius, FontId, Layout, Pos2, Response, Role, Sense, Stroke, Ui,
    WidgetInfo, vec2,
};

use crate::state::nav::Tab;
use crate::theme::{DISABLED, RAISED, TEXT, TEXT_2, TEXT_3, semibold};
use crate::views::{window_controls, wordmark};

const ROUND: f32 = 32.0;
const SEARCH_WIDTH: f32 = 300.0;
const SEARCH_HEIGHT: f32 = 34.0;
const WORDMARK_HEIGHT: f32 = 26.0;

pub struct TopBarData {
    pub tab: Option<Tab>,
    pub can_back: bool,
    pub can_forward: bool,
    pub settings_open: bool,
}

pub enum TopBarAction {
    Back,
    Forward,
    Tab(Tab),
    QueryChanged,
    Settings,
}

/// The search field's id, for `Ctrl+F`.
pub fn search_id() -> egui::Id {
    egui::Id::unique("top_bar_search")
}

pub fn show(ui: &mut Ui, data: &TopBarData, query: &mut String) -> Option<TopBarAction> {
    let mut action = None;
    window_controls::drag_area(ui, ui.max_rect());
    ui.horizontal_centered(|ui| {
        ui.add_space(24.0);
        wordmark::show(ui, WORDMARK_HEIGHT);
        ui.add_space(24.0);
        ui.spacing_mut().item_spacing.x = 4.0;
        if arrow_button(ui, "Back", false, data.can_back).clicked() {
            action = Some(TopBarAction::Back);
        }
        if arrow_button(ui, "Forward", true, data.can_forward).clicked() {
            action = Some(TopBarAction::Forward);
        }
        ui.add_space(24.0);
        ui.spacing_mut().item_spacing.x = 24.0;
        for (tab, label) in [
            (Tab::Home, "Home"),
            (Tab::Albums, "Albums"),
            (Tab::Artists, "Artists"),
        ] {
            if tab_button(ui, label, data.tab == Some(tab)).clicked() {
                action = Some(TopBarAction::Tab(tab));
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let height = ui.available_height();
            let (rect, _) =
                ui.allocate_exact_size(vec2(window_controls::WIDTH, height), Sense::hover());
            window_controls::buttons(ui, rect);
            ui.add_space(16.0);
            ui.spacing_mut().item_spacing.x = 12.0;
            if gear_button(ui, data.settings_open).clicked() {
                action = Some(TopBarAction::Settings);
            }
            if search_field(ui, query).changed() {
                action = Some(TopBarAction::QueryChanged);
            }
        });
    });
    action
}

/// A round button with a chevron; `right` points it forward.
fn arrow_button(ui: &mut Ui, label: &str, right: bool, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ROUND, ROUND), sense);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        p.circle_filled(rect.center(), ROUND / 2.0, RAISED);
        let color = match (enabled, resp.hovered()) {
            (false, _) => DISABLED,
            (true, true) => TEXT,
            (true, false) => TEXT_2,
        };
        let c = rect.center();
        let d = if right { 1.0 } else { -1.0 };
        let stroke = Stroke::new(2.0, color);
        let tip = Pos2::new(c.x + 3.0 * d, c.y);
        p.line_segment([Pos2::new(c.x - 3.0 * d, c.y - 6.0), tip], stroke);
        p.line_segment([tip, Pos2::new(c.x - 3.0 * d, c.y + 6.0)], stroke);
    }
    resp
}

/// A text tab; the active one is bright with a white underline.
fn tab_button(ui: &mut Ui, label: &str, active: bool) -> Response {
    let font = FontId::new(14.0, semibold());
    let color = if active { TEXT } else { TEXT_2 };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
    let height = ui.available_height();
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x, height), Sense::click());
    resp.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::Tab, true, label);
        info.selected = Some(active);
        info
    });
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let color = if resp.hovered() { TEXT } else { color };
        let pos = Pos2::new(rect.left(), rect.center().y - galley.size().y / 2.0);
        p.galley(pos, galley, color);
        if active {
            let line = egui::Rect::from_min_max(
                Pos2::new(rect.left(), rect.bottom() - 2.0),
                rect.right_bottom(),
            );
            p.rect_filled(line, CornerRadius::ZERO, TEXT);
        }
    }
    resp
}

fn search_field(ui: &mut Ui, query: &mut String) -> Response {
    let frame = egui::Frame::new()
        .fill(RAISED)
        .corner_radius(CornerRadius::same((SEARCH_HEIGHT / 2.0) as u8))
        .inner_margin(egui::Margin::symmetric(12, 0));
    frame
        .show(ui, |ui| {
            ui.set_width(SEARCH_WIDTH - 24.0);
            ui.set_height(SEARCH_HEIGHT);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                search_icon(ui);
                let edit = egui::TextEdit::singleline(query)
                    .id(search_id())
                    .hint_text(egui::RichText::new("Search artists, albums, tracks").color(TEXT_3))
                    .frame(egui::Frame::NONE)
                    .desired_width(f32::INFINITY);
                let resp = ui.add(edit);
                ui.ctx()
                    .accesskit_node_builder(resp.id, |b| b.set_label("Search"));
                resp
            })
            .inner
        })
        .inner
}

fn search_icon(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    let p = ui.painter();
    let stroke = Stroke::new(1.6, TEXT_3);
    let c = rect.center() + vec2(-1.5, -1.5);
    p.circle_stroke(c, 5.0, stroke);
    p.line_segment([c + vec2(3.6, 3.6), c + vec2(7.0, 7.0)], stroke);
}

/// A painted gear; highlighted while Settings is open.
fn gear_button(ui: &mut Ui, open: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Settings"));
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if open {
            p.circle_filled(rect.center(), 17.0, RAISED);
        }
        let color = if open || resp.hovered() { TEXT } else { TEXT_2 };
        let c = rect.center();
        // Eight square teeth around a ring: outline traced at two radii.
        let teeth = 8;
        let mut pts = Vec::with_capacity(teeth * 4);
        for i in 0..teeth {
            let a = i as f32 * std::f32::consts::TAU / teeth as f32;
            let half = std::f32::consts::TAU / teeth as f32 / 4.0;
            for (da, r) in [
                (-half * 1.6, 6.6),
                (-half * 0.8, 9.0),
                (half * 0.8, 9.0),
                (half * 1.6, 6.6),
            ] {
                pts.push(c + egui::Vec2::angled(a + da) * r);
            }
        }
        p.add(egui::Shape::closed_line(pts, Stroke::new(1.5, color)));
        p.circle_stroke(c, 2.8, Stroke::new(1.5, color));
    }
    resp
}

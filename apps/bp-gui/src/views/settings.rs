//! The settings page: the output device and its capabilities, music folders and rescan.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bp_core::{DeviceId, DeviceInfo};
use egui::{
    Align, Align2, Color32, Context, CornerRadius, FontId, Frame, Id, Layout, Margin, Pos2,
    RichText, Role, Sense, Stroke, Ui, WidgetInfo, vec2,
};

use crate::backend::{LibrarySource, ScanStatus};
use crate::fmt::khz_short;
use crate::state::caps_table::CapTable;
use crate::theme::{BORDER, DISABLED, RAISED, SURFACE, TEXT, TEXT_2, TEXT_3, semibold};
use crate::views::heading;

/// Width of the centred column.
const COLUMN: f32 = 880.0;
/// Width of a row's label column.
const LABEL_W: f32 = 120.0;
const CARD_BORDER: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);

/// How long a folder's existence check is reused.
const EXISTS_TTL: Duration = Duration::from_secs(1);
/// Progress refresh while a scan runs.
const SCAN_TICK: Duration = Duration::from_millis(250);
/// Height of the unreadable files list before it scrolls.
const UNREADABLE_HEIGHT: f32 = 200.0;

type ExistsCache = HashMap<PathBuf, (Instant, bool)>;

/// Whether `path` exists, hitting the filesystem at most once per second per path.
fn exists_cached(ctx: &Context, path: &Path) -> bool {
    let id = Id::NULL.with("settings-exists");
    let now = Instant::now();
    ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<ExistsCache>(id);
        if let Some((at, ok)) = cache.get(path)
            && now.duration_since(*at) < EXISTS_TTL
        {
            return *ok;
        }
        let ok = path.exists();
        cache.insert(path.to_path_buf(), (now, ok));
        ok
    })
}

/// "N files couldn't be read", opening to the files and reasons. The list is fetched
/// only while open.
fn unreadable_list(ui: &mut Ui, lib: &dyn LibrarySource, count: u64) {
    egui::CollapsingHeader::new(
        RichText::new(format!("{count} files couldn't be read")).color(TEXT_2),
    )
    .id_salt("settings-unreadable")
    .show(ui, |ui| {
        egui::ScrollArea::vertical()
            .max_height(UNREADABLE_HEIGHT)
            .show(ui, |ui| {
                for (path, reason) in lib.unreadable() {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    let folder = path.parent().unwrap_or(Path::new("")).display();
                    ui.label(name.as_ref());
                    ui.label(RichText::new(format!("{folder} — {reason}")).color(TEXT_3));
                    ui.add_space(4.0);
                }
            });
    });
}

pub struct SettingsData<'a> {
    pub devices: &'a [DeviceInfo],
    pub selected: Option<&'a DeviceId>,
    /// The selected device's capabilities.
    pub table: Option<&'a CapTable>,
}

pub enum SettingsAction {
    SelectDevice(DeviceId),
    Reprobe(DeviceId),
}

pub struct SettingsOutput {
    pub action: Option<SettingsAction>,
    /// A folder was added or removed, or a rescan started.
    pub library_changed: bool,
}

pub fn show(
    ui: &mut Ui,
    data: &SettingsData,
    lib: &dyn LibrarySource,
    pick_folder: &mut dyn FnMut() -> Option<PathBuf>,
) -> SettingsOutput {
    let mut out = SettingsOutput {
        action: None,
        library_changed: false,
    };
    egui::ScrollArea::vertical()
        .id_salt("settings")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let width = ui.available_width().min(COLUMN);
            let margin = ((ui.available_width() - width) / 2.0).max(0.0);
            ui.horizontal_top(|ui| {
                ui.add_space(margin);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    heading(ui, "Settings", None);
                    ui.add_space(22.0);
                    card(ui, |ui| out.action = audio(ui, data));
                    ui.add_space(22.0);
                    card(ui, |ui| out.library_changed = library(ui, lib, pick_folder));
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(format!("bitperfect {}", env!("CARGO_PKG_VERSION")))
                            .color(TEXT_3),
                    );
                    ui.add_space(crate::views::bar::CLEARANCE);
                });
            });
        });
    out
}

/// A rounded `SURFACE` panel spanning the column.
fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, CARD_BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(24, 22))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn card_heading(ui: &mut Ui, text: &str) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::new(18.0, semibold()), TEXT);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(Role::Heading, true, text));
    ui.painter().galley(rect.min, galley, TEXT);
}

/// A row label painted in the left column, without an accessibility node of its own: the
/// control beside it carries the name.
fn row_label(ui: &mut Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(LABEL_W, 20.0), Sense::hover());
    ui.painter().text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(13.0),
        TEXT_2,
    );
}

/// Output device, Re-probe and the capability table.
fn audio(ui: &mut Ui, data: &SettingsData) -> Option<SettingsAction> {
    let mut action = None;
    card_heading(ui, "Audio");
    ui.add_space(16.0);
    let name = data
        .selected
        .and_then(|id| data.devices.iter().find(|d| &d.id == id))
        .map_or("No output device", |d| d.name.as_str());
    ui.horizontal(|ui| {
        row_label(ui, "Output device");
        let combo = egui::ComboBox::from_id_salt("settings_output_device")
            .selected_text(name)
            .width(300.0)
            .show_ui(ui, |ui| {
                for d in data.devices {
                    let selected = data.selected == Some(&d.id);
                    if ui.selectable_label(selected, &d.name).clicked() && !selected {
                        action = Some(SettingsAction::SelectDevice(d.id.clone()));
                    }
                }
            });
        ui.ctx()
            .accesskit_node_builder(combo.response.id, |b| b.set_label("Output device"));
        ui.add_space(8.0);
        if let Some(id) = data.selected
            && ui.button("Re-probe").clicked()
        {
            action = Some(SettingsAction::Reprobe(id.clone()));
        }
    });
    if let Some(table) = data.table {
        ui.add_space(16.0);
        ui.horizontal_top(|ui| {
            row_label(ui, "Capabilities");
            cap_grid(ui, table);
        });
    }
    action
}

fn mono(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(RichText::new(text).monospace().size(12.0).color(color));
}

/// Encodings down, rates across, then the DSD rates.
fn cap_grid(ui: &mut Ui, table: &CapTable) {
    Frame::new()
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.vertical(|ui| cap_cells(ui, table));
        });
}

fn cap_cells(ui: &mut Ui, table: &CapTable) {
    {
        egui::Grid::new("settings_caps")
            .spacing(vec2(18.0, 8.0))
            .show(ui, |ui| {
                // Empty corner cell.
                ui.allocate_exact_size(vec2(0.0, 0.0), Sense::hover());
                for rate in &table.rates {
                    mono(ui, &khz_short(*rate), TEXT_2);
                }
                ui.end_row();
                for (label, cells) in &table.rows {
                    mono(ui, label, TEXT_2);
                    for ok in cells {
                        mark(ui, *ok);
                    }
                    ui.end_row();
                }
            });
        if !table.dsd.is_empty() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (multiple, ok) in &table.dsd {
                    mono(ui, &format!("DSD{multiple}"), TEXT_2);
                    mark(ui, *ok);
                    ui.add_space(10.0);
                }
            });
        }
    }
}

/// A painted check (the bundled fonts have none), or a grey dash.
fn mark(ui: &mut Ui, ok: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    if ok {
        let stroke = Stroke::new(1.6, TEXT);
        p.line_segment(
            [Pos2::new(c.x - 4.0, c.y), Pos2::new(c.x - 1.0, c.y + 3.0)],
            stroke,
        );
        p.line_segment(
            [
                Pos2::new(c.x - 1.0, c.y + 3.0),
                Pos2::new(c.x + 4.5, c.y - 4.0),
            ],
            stroke,
        );
    } else {
        p.line_segment(
            [Pos2::new(c.x - 3.0, c.y), Pos2::new(c.x + 3.0, c.y)],
            Stroke::new(1.2, DISABLED),
        );
    }
}

/// Folders, Add folder, Rescan and the scan status. Returns whether the library was
/// changed (folder added or removed, rescan).
fn library(
    ui: &mut Ui,
    lib: &dyn LibrarySource,
    pick_folder: &mut dyn FnMut() -> Option<PathBuf>,
) -> bool {
    let mut changed = false;
    let ctx = ui.ctx().clone();
    let status = lib.scan_status();
    let scanning = matches!(status, ScanStatus::Scanning { .. });

    ui.horizontal(|ui| {
        card_heading(ui, "Library");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let add = egui::Button::new(
                RichText::new("Add folder…")
                    .family(semibold())
                    .color(Color32::from_rgb(0x11, 0x11, 0x11)),
            )
            .fill(TEXT)
            .corner_radius(CornerRadius::same(6));
            if ui.add(add).clicked()
                && let Some(folder) = pick_folder()
            {
                lib.add_folder(folder);
                changed = true;
            }
            if ui
                .add_enabled(!scanning, egui::Button::new("Rescan"))
                .clicked()
            {
                lib.rescan();
                changed = true;
            }
            if let ScanStatus::Scanning { done, total } = status {
                ui.label(RichText::new(format!("Scanning {done} / {total}")).color(TEXT_2));
            }
        });
    });
    ui.add_space(14.0);

    let mut remove = None;
    for folder in lib.folders() {
        Frame::new()
            .fill(RAISED)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(folder.display().to_string()).monospace());
                    if !exists_cached(&ctx, &folder) {
                        ui.label(RichText::new("(missing)").color(TEXT_3));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Remove").clicked() {
                            remove = Some(folder.clone());
                        }
                    });
                });
            });
        ui.add_space(6.0);
    }
    if let Some(folder) = remove {
        lib.remove_folder(&folder);
        changed = true;
    }

    if let ScanStatus::Done { unreadable } = status
        && unreadable > 0
    {
        ui.add_space(6.0);
        unreadable_list(ui, lib, unreadable);
    }

    // Progress ticks only while scanning; otherwise repaint on input only.
    if scanning {
        ctx.request_repaint_after(SCAN_TICK);
    }
    changed
}

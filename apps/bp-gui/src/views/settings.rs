//! The settings page: the output device and its capabilities, music folders and rescan.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bp_core::{DeviceId, DeviceInfo};
use egui::{
    Align, Align2, Color32, Context, CornerRadius, FontId, Frame, Id, Layout, Margin, Pos2,
    RichText, Role, Sense, Stroke, Ui, WidgetInfo, vec2,
};

use crate::backend::{LibrarySource, ScanStatus};
use crate::fmt::khz_short;
use crate::plugins::host::{PluginInfo, Status};
use crate::plugins::panel::Panel;
use crate::plugins::rpc::Permission;
use crate::state::caps_table::CapTable;
use crate::theme::{BORDER, DISABLED, RAISED, SURFACE, TEXT, TEXT_2, TEXT_3, semibold};
use crate::views::heading;
use crate::views::panel::{self as panel_view, PanelAction};

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
    /// The folder holding `data\`, `plugins\` and the rest of nil's files.
    pub home: &'a Path,
    /// Why `home` is not beside the exe, when it is not.
    pub home_note: Option<&'a str>,
    /// Where plugin logs are written (`plugins\<folder>.log` inside).
    pub logs_dir: &'a Path,
    pub plugins: &'a [PluginInfo],
    /// A plugin whose switch change could not be saved, and why: `(folder, message)`.
    pub plugin_error: Option<(&'a str, &'a str)>,
    /// The panels plugins show in their entry, by plugin folder name.
    pub plugin_panels: &'a BTreeMap<String, Panel>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingsAction {
    SelectDevice(DeviceId),
    Reprobe(DeviceId),
    /// The DoP switch was turned off, or on and confirmed.
    SetDop {
        id: DeviceId,
        enabled: bool,
    },
    OpenFolder(PathBuf),
    OpenFile(PathBuf),
    /// The switch was turned off, or on and confirmed.
    PluginEnable {
        folder: String,
        on: bool,
    },
    PluginRestart(String),
    /// An element of the plugin's settings panel was used: `params` is `{id, value?}`.
    PanelEvent {
        folder: String,
        params: serde_json::Value,
    },
}

/// What the plugin API documentation says; plugin authors start here.
const PLUGIN_API_URL: &str = "https://github.com/reubinars/nil/blob/main/PLUGIN-API.md";
/// The warning every owner sees before trusting a plugin.
const PLUGIN_WARNING: &str = "A plugin is an ordinary program running as you. Permissions limit what it can do through nil, not what it can do on your PC.";
const DOP_WARNING: &str = "Only turn this on if your DAC decodes DSD over PCM (DoP). A DAC that doesn't plays DSD tracks as hiss instead of music, with a strong high-frequency tone you can't hear that can strain speakers at high volume.";
/// Height of a crashed plugin's stderr block before it scrolls.
const STDERR_HEIGHT: f32 = 160.0;

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
                    ui.add_space(22.0);
                    let mut other = None;
                    card(ui, |ui| other = data_folder(ui, data));
                    ui.add_space(22.0);
                    card(ui, |ui| {
                        out.action = out.action.take().or(plugins(ui, data))
                    });
                    out.action = out.action.take().or(other);
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

/// Output device, Re-probe, the capability table and the DoP switch.
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
        if let Some(id) = data.selected {
            ui.add_space(12.0);
            action = dop_row(ui, id, name, table).or(action);
        }
    }
    action
}

/// The device whose "turn DoP on" confirmation is open.
fn dop_confirm_id() -> Id {
    Id::NULL.with("settings-dop-confirm")
}

/// The DoP switch; turning it on asks first.
fn dop_row(ui: &mut Ui, id: &DeviceId, name: &str, table: &CapTable) -> Option<SettingsAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        row_label(ui, "DSD");
        let mut on = table.dop_on;
        // A device with no DoP rate cannot be turned on; one already on can be turned off.
        let switch = ui.add_enabled(
            table.dop_possible || on,
            egui::Checkbox::new(&mut on, "DSD over PCM (DoP)"),
        );
        if switch.changed() {
            if on {
                ui.ctx()
                    .data_mut(|d| d.insert_temp(dop_confirm_id(), id.clone()));
            } else {
                action = Some(SettingsAction::SetDop {
                    id: id.clone(),
                    enabled: false,
                });
            }
        }
    });
    if !table.dop_possible {
        ui.horizontal(|ui| {
            ui.add_space(LABEL_W + ui.spacing().item_spacing.x);
            ui.label(RichText::new("This device accepts no rate that carries DoP.").color(TEXT_3));
        });
    }
    let ctx = ui.ctx().clone();
    let pending: Option<DeviceId> = ctx.data(|d| d.get_temp(dop_confirm_id()));
    match pending {
        Some(p) if p == *id && table.dop_possible => match dop_confirm_dialog(&ctx, name) {
            Some(true) => {
                ctx.data_mut(|d| d.remove::<DeviceId>(dop_confirm_id()));
                action = Some(SettingsAction::SetDop {
                    id: id.clone(),
                    enabled: true,
                });
            }
            Some(false) => ctx.data_mut(|d| d.remove::<DeviceId>(dop_confirm_id())),
            None => {}
        },
        // Asked for another device, or one that can no longer take DoP.
        Some(_) => ctx.data_mut(|d| d.remove::<DeviceId>(dop_confirm_id())),
        None => {}
    }
    action
}

/// `Some(true)` = turn on, `Some(false)` = cancelled.
fn dop_confirm_dialog(ctx: &Context, name: &str) -> Option<bool> {
    let mut answer = None;
    let modal = egui::Modal::new(Id::unique("dop-confirm")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.label(RichText::new(format!("Turn on DoP for {name}?")).family(semibold()));
        ui.add_space(8.0);
        ui.label(DOP_WARNING);
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Turn on").clicked() {
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

/// The data folder's path, Open folder and the fallback note.
fn data_folder(ui: &mut Ui, data: &SettingsData) -> Option<SettingsAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        card_heading(ui, "Data folder");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Open folder").clicked() {
                action = Some(SettingsAction::OpenFolder(data.home.to_path_buf()));
            }
        });
    });
    ui.add_space(14.0);
    ui.label(RichText::new(data.home.display().to_string()).monospace());
    if let Some(note) = data.home_note {
        ui.add_space(8.0);
        ui.label(RichText::new(note).color(TEXT_2));
    }
    action
}

/// "Playback", "Library", "Playlists".
fn permission_label(p: Permission) -> &'static str {
    match p {
        Permission::Playback => "Playback",
        Permission::Library => "Library",
        Permission::Playlists => "Playlists",
    }
}

fn permission_list(perms: &[Permission]) -> String {
    perms
        .iter()
        .map(|p| permission_label(*p))
        .collect::<Vec<_>>()
        .join(", ")
}

fn status_text(status: &Status) -> String {
    match status {
        Status::Off => "Off".into(),
        Status::Starting => "Starting…".into(),
        Status::Running => "Running".into(),
        Status::Crashed { reason } => format!("Crashed: {reason}"),
        Status::Incompatible => "Incompatible (needs plugin API 1)".into(),
        Status::InvalidManifest(reason) => format!("Invalid manifest: {reason}"),
    }
}

/// The declared permissions the owner has not approved yet, for a plugin that is on.
fn new_permissions(info: &PluginInfo) -> Vec<Permission> {
    match &info.manifest {
        Some(m) if info.consent.enabled => m
            .permissions
            .iter()
            .copied()
            .filter(|p| !info.consent.granted.contains(p))
            .collect(),
        _ => Vec::new(),
    }
}

/// The folder whose "turn on" confirmation is open.
fn confirm_id() -> Id {
    Id::NULL.with("settings-plugin-confirm")
}

/// Whether the plugin's manifest can be run at all. One that cannot is never turned on.
fn loadable(info: &PluginInfo) -> bool {
    !matches!(
        info.status,
        Status::InvalidManifest(_) | Status::Incompatible
    )
}

/// Drops an open "turn on" confirmation whose plugin is gone or can no longer load.
pub fn forget_stale_confirm(ctx: &Context, plugins: &[PluginInfo]) {
    let Some(folder) = ctx.data(|d| d.get_temp::<String>(confirm_id())) else {
        return;
    };
    if !plugins.iter().any(|i| i.folder == folder && loadable(i)) {
        ctx.data_mut(|d| d.remove::<String>(confirm_id()));
    }
}

/// The warning, the API link and one row per plugin.
fn plugins(ui: &mut Ui, data: &SettingsData) -> Option<SettingsAction> {
    let mut action = None;
    card_heading(ui, "Plugins");
    ui.add_space(12.0);
    ui.label(RichText::new(PLUGIN_WARNING).color(TEXT_2));
    ui.add_space(4.0);
    ui.hyperlink_to("Plugin API", PLUGIN_API_URL);
    ui.add_space(14.0);
    if data.plugins.is_empty() {
        ui.label(RichText::new("No plugins installed.").color(TEXT_3));
    }
    for info in data.plugins {
        let error = data
            .plugin_error
            .filter(|(folder, _)| *folder == info.folder)
            .map(|(_, message)| message);
        let panel = data.plugin_panels.get(&info.folder);
        action = plugin_row(ui, info, error, panel, data.home, data.logs_dir).or(action);
        ui.add_space(8.0);
    }
    let ctx = ui.ctx().clone();
    let pending: Option<String> = ctx.data(|d| d.get_temp(confirm_id()));
    if let Some(folder) = pending
        && let Some(info) = data.plugins.iter().find(|i| i.folder == folder)
    {
        match confirm_dialog(&ctx, info) {
            Some(true) => {
                ctx.data_mut(|d| d.remove::<String>(confirm_id()));
                action = Some(SettingsAction::PluginEnable { folder, on: true });
            }
            Some(false) => ctx.data_mut(|d| d.remove::<String>(confirm_id())),
            None => {}
        }
    }
    action
}

fn plugin_row(
    ui: &mut Ui,
    info: &PluginInfo,
    error: Option<&str>,
    panel: Option<&Panel>,
    home: &Path,
    logs_dir: &Path,
) -> Option<SettingsAction> {
    let mut action = None;
    let wanted = new_permissions(info);
    let (name, version, description, perms) = match &info.manifest {
        Some(m) => (
            m.label(),
            m.version.as_str(),
            m.description.as_str(),
            m.permissions.as_slice(),
        ),
        None => (info.folder.as_str(), "", "", [].as_slice()),
    };
    Frame::new()
        .fill(RAISED)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).family(semibold()).color(TEXT));
                if !version.is_empty() {
                    ui.label(RichText::new(version).color(TEXT_3));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let loadable = loadable(info);
                    // One that is on but held back only by permissions it has not been
                    // given shows as off: turning it on asks for them.
                    let mut on = info.consent.enabled && !(loadable && !wanted.is_empty());
                    // A plugin that cannot load cannot be turned on; one already on can
                    // still be turned off.
                    let switch =
                        ui.add_enabled(loadable || on, egui::Checkbox::new(&mut on, "Enabled"));
                    ui.ctx().accesskit_node_builder(switch.id, |b| {
                        b.set_label(format!("Enable {name}"));
                    });
                    if switch.changed() {
                        if on {
                            let folder = info.folder.clone();
                            ui.ctx().data_mut(|d| d.insert_temp(confirm_id(), folder));
                        } else {
                            action = Some(SettingsAction::PluginEnable {
                                folder: info.folder.clone(),
                                on: false,
                            });
                        }
                    }
                });
            });
            if !description.is_empty() {
                ui.label(RichText::new(description).color(TEXT_2));
            }
            ui.add_space(4.0);
            ui.label(RichText::new(status_text(&info.status)).color(TEXT_2));
            if let Some(error) = error {
                ui.label(RichText::new(error).color(TEXT));
            }
            if !wanted.is_empty() {
                ui.label(
                    RichText::new(format!(
                        "Wants new permission: {}",
                        permission_list(&wanted)
                    ))
                    .color(TEXT),
                );
            }
            if info.manifest.is_some() {
                let shown = if perms.is_empty() {
                    "none".to_owned()
                } else {
                    permission_list(perms)
                };
                ui.label(RichText::new(format!("Permissions: {shown}")).color(TEXT_3));
            }
            if matches!(info.status, Status::Crashed { .. }) && !info.stderr_tail.is_empty() {
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .id_salt(("plugin-stderr", &info.folder))
                    .max_height(STDERR_HEIGHT)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(info.stderr_tail.join("\n"))
                                .monospace()
                                .size(12.0),
                        );
                    });
            }
            if let Some(panel) = panel {
                ui.add_space(6.0);
                let shown = ui.push_id(("plugin-panel", &info.folder), |ui| {
                    panel_view::show_settings(ui, panel)
                });
                if let Some(PanelAction::Event { id, value }) = shown.inner {
                    action = Some(SettingsAction::PanelEvent {
                        folder: info.folder.clone(),
                        params: panel_view::event_params(id, value),
                    });
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Restart").clicked() {
                    action = Some(SettingsAction::PluginRestart(info.folder.clone()));
                }
                if ui.button("Open log").clicked() {
                    action = Some(SettingsAction::OpenFile(plugin_log(logs_dir, &info.folder)));
                }
                if ui.button("Open folder").clicked() {
                    action = Some(SettingsAction::OpenFolder(
                        home.join("plugins").join(&info.folder),
                    ));
                }
            });
        });
    action
}

/// Where the host writes a plugin's stderr.
fn plugin_log(logs_dir: &Path, folder: &str) -> PathBuf {
    logs_dir.join("plugins").join(format!("{folder}.log"))
}

/// The consent dialog; `Some(true)` = allow and turn on, `Some(false)` = cancelled.
fn confirm_dialog(ctx: &Context, info: &PluginInfo) -> Option<bool> {
    let mut answer = None;
    let name = info
        .manifest
        .as_ref()
        .map_or(info.folder.as_str(), |m| m.label());
    let perms = info
        .manifest
        .as_ref()
        .map_or(&[][..], |m| m.permissions.as_slice());
    // Asked for since the owner last approved this plugin.
    let new = new_permissions(info);
    let modal = egui::Modal::new(Id::unique("plugin-confirm")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.label(RichText::new(format!("Turn on {name}?")).family(semibold()));
        ui.add_space(8.0);
        if perms.is_empty() {
            ui.label("It asks for no permissions.");
        } else {
            ui.label("It will be allowed to:");
            for p in perms {
                if new.contains(p) {
                    ui.label(format!("{} (new)", permission_label(*p)));
                } else {
                    ui.label(permission_label(*p));
                }
            }
        }
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Allow and turn on").clicked() {
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

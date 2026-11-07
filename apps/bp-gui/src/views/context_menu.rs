//! The context menu shared by every track and album row, and the row interaction that
//! opens it: selection clicks, activation and keyboard access.

use std::path::PathBuf;

use egui::{
    Align2, CornerRadius, Event, FontId, Key, Popup, Pos2, Rect, Response, Role, Sense,
    SetOpenCommand, Stroke, Ui, WidgetInfo, vec2,
};

use crate::backend::AlbumId;
use crate::state::selection::{Click, ListId, Selection};
use crate::theme::{BORDER, HOVER, TEXT, TEXT_2, TEXT_3};
use crate::views::device_list::{ROW_HEIGHT, WIDTH};

const NO_DEVICE: &str = "No output device. Turn on or connect your DAC.";

/// What a menu choice acts on.
pub enum MenuTarget {
    Tracks(Vec<PathBuf>),
    Album(AlbumId),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuPlace {
    Track {
        search: bool,
    },
    Album,
    PlaylistRow,
    /// A queue row: `upcoming` = how many of the rows chosen are after the current track,
    /// `play_next` = whether Play next would change the queue.
    QueueRow {
        upcoming: usize,
        play_next: bool,
    },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MenuAction {
    Play,
    PlayNext,
    Queue,
    AddTo(String),
    AddToNew,
    Remove,
    GoToAlbum,
    GoToArtist,
}

#[derive(Clone, Copy)]
pub struct MenuSpec<'a> {
    pub place: MenuPlace,
    pub count: usize,
    pub playlists: &'a [String],
    pub exclude: Option<&'a str>,
    pub device_ready: bool,
}

pub struct Item {
    pub label: String,
    /// `None` is the "Add to playlist" submenu.
    pub action: Option<MenuAction>,
    pub enabled: bool,
    pub hover: Option<&'static str>,
}

impl Item {
    fn new(label: &str, action: Option<MenuAction>) -> Item {
        Item {
            label: label.to_owned(),
            action,
            enabled: true,
            hover: None,
        }
    }

    /// An item that needs an output device.
    fn device(label: &str, action: MenuAction, ready: bool) -> Item {
        Item {
            enabled: ready,
            hover: (!ready).then_some(NO_DEVICE),
            ..Item::new(label, Some(action))
        }
    }
}

/// The items for `spec`, in order.
pub fn items(spec: &MenuSpec) -> Vec<Item> {
    let ready = spec.device_ready;
    let one = spec.count <= 1;
    let queue_label = if one {
        "Add to queue".to_owned()
    } else {
        format!("Add {} tracks to queue", spec.count)
    };
    let submenu = Item::new("Add to playlist", None);
    let go_album = Item::new("Go to album", Some(MenuAction::GoToAlbum));
    let go_artist = Item::new("Go to artist", Some(MenuAction::GoToArtist));
    let mut v = Vec::new();
    match spec.place {
        MenuPlace::Track { search } => {
            v.push(Item::device("Play", MenuAction::Play, ready));
            v.push(Item::device("Play next", MenuAction::PlayNext, ready));
            v.push(Item::device(&queue_label, MenuAction::Queue, ready));
            v.push(submenu);
            if one {
                if search {
                    v.push(go_album);
                }
                v.push(go_artist);
            }
        }
        MenuPlace::Album => {
            v.push(Item::device("Play album", MenuAction::Play, ready));
            v.push(Item::device("Add album to queue", MenuAction::Queue, ready));
            v.push(submenu);
            v.push(go_artist);
        }
        MenuPlace::PlaylistRow => {
            v.push(Item::device("Play from here", MenuAction::Play, ready));
            v.push(Item::device("Play next", MenuAction::PlayNext, ready));
            v.push(Item::device(&queue_label, MenuAction::Queue, ready));
            v.push(submenu);
            let remove = if one {
                "Remove from playlist".to_owned()
            } else {
                format!("Remove {} tracks", spec.count)
            };
            v.push(Item::new(&remove, Some(MenuAction::Remove)));
        }
        MenuPlace::QueueRow {
            upcoming,
            play_next,
        } => {
            v.push(Item::device("Play from here", MenuAction::Play, ready));
            // Queue edits need no device: they only reorder what is queued.
            if play_next {
                v.push(Item::new("Play next", Some(MenuAction::PlayNext)));
            }
            v.push(submenu);
            if upcoming > 0 {
                let remove = if upcoming == 1 {
                    "Remove from queue".to_owned()
                } else {
                    format!("Remove {upcoming} tracks")
                };
                v.push(Item::new(&remove, Some(MenuAction::Remove)));
            }
            if one {
                v.push(go_album);
            }
        }
    }
    v
}

/// The "Add to playlist" submenu: `New playlist…`, then the playlists minus `exclude`.
pub fn submenu_items(spec: &MenuSpec) -> Vec<Item> {
    let mut v = vec![Item::new("New playlist…", Some(MenuAction::AddToNew))];
    v.extend(
        spec.playlists
            .iter()
            .filter(|p| spec.exclude != Some(p.as_str()))
            .map(|p| Item::new(p, Some(MenuAction::AddTo(p.clone())))),
    );
    v
}

/// Draws the items inside an open menu `ui`; the choice, if one was made.
pub fn show(ui: &mut Ui, spec: &MenuSpec) -> Option<MenuAction> {
    let mut chosen = None;
    ui.set_width(WIDTH);
    ui.spacing_mut().item_spacing.y = 2.0;
    for item in items(spec) {
        match item.action.clone() {
            Some(action) => {
                if item_row(ui, &item, false).clicked() && item.enabled {
                    chosen = Some(action);
                }
            }
            None => {
                let row = item_row(ui, &item, true);
                let sub = egui::containers::menu::SubMenu::new().show(ui, &row, |ui| {
                    ui.set_width(WIDTH);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let mut picked = None;
                    for (i, sub) in submenu_items(spec).into_iter().enumerate() {
                        if item_row(ui, &sub, false).clicked() {
                            picked = sub.action;
                        }
                        if i == 0 {
                            separator(ui);
                        }
                    }
                    picked
                });
                if let Some(Some(a)) = sub.map(|s| s.inner) {
                    chosen = Some(a);
                    ui.close();
                }
            }
        }
    }
    chosen
}

fn separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 9.0), Sense::hover());
    ui.painter().hline(
        rect.x_range().shrink(4.0),
        rect.center().y,
        Stroke::new(1.0, BORDER),
    );
}

fn item_row(ui: &mut Ui, item: &Item, arrow: bool) -> Response {
    let sense = if item.enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(WIDTH, ROW_HEIGHT), sense);
    let enabled = item.enabled;
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, &item.label));
    if item.enabled && resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(5), HOVER);
    }
    let color = if !item.enabled {
        TEXT_3
    } else if resp.hovered() {
        TEXT
    } else {
        TEXT_2
    };
    ui.painter().text(
        Pos2::new(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        &item.label,
        FontId::proportional(14.0),
        color,
    );
    if arrow {
        // The bundled font has no ▸, so draw the triangle.
        let at = Rect::from_center_size(
            Pos2::new(rect.right() - 16.0, rect.center().y),
            vec2(14.0, 14.0),
        );
        ui.painter()
            .add(egui::containers::menu::SubMenuButton::arrow_shape(
                at, color,
            ));
    }
    match item.hover {
        Some(text) => resp.on_hover_text(text),
        None => resp,
    }
}

pub enum RowAction {
    Select(usize, Click),
    Activate(usize),
    Secondary(usize),
    Menu(Vec<usize>, MenuAction),
    /// Rows dragged in an open playlist and dropped: `rows` are the shown file indices
    /// moved, `gap` the shown position they go in front of (`shown.len()` = the end).
    Drop {
        rows: Vec<usize>,
        gap: usize,
    },
}

/// Wires one row's response: primary click selects, double-click or Enter while focused
/// activates, secondary click reports `Secondary` (the app calls `Selection::secondary`),
/// Shift+F10 or the Menu key while focused opens the menu, and a menu choice is returned
/// with the rows it acts on. `spec.count` is filled in here.
pub fn row(
    response: &Response,
    list: &ListId,
    shown: &[usize],
    row: usize,
    selection: &Selection,
    mut spec: MenuSpec,
) -> Option<RowAction> {
    let target = selection.target(list, shown, row);
    spec.count = target.len();

    let (modifiers, enter, menu_key) = response.ctx.input(|i| {
        (
            i.modifiers,
            i.key_pressed(Key::Enter),
            i.key_pressed(Key::Menu)
                // The key event's own modifiers: the input's may already be reset.
                || i.events.iter().any(|e| {
                    matches!(
                        e,
                        Event::Key { key: Key::F10, pressed: true, modifiers, .. }
                            if modifiers.shift
                    )
                }),
        )
    });
    let keyboard_menu = response.has_focus() && menu_key;
    // A menu opened from the keyboard sits below the row, not at the pointer; the
    // spot is kept for as long as that menu stays open.
    let ctx = &response.ctx;
    let popup_id = Popup::default_response_id(response);
    let anchor_id = popup_id.with("keyboard_anchor");
    if keyboard_menu {
        ctx.data_mut(|d| d.insert_temp(anchor_id, response.rect.left_bottom()));
    } else if response.secondary_clicked() {
        ctx.data_mut(|d| d.remove::<Pos2>(anchor_id));
    }
    let mut popup = Popup::context_menu(response);
    if keyboard_menu {
        popup = popup.open_memory(SetOpenCommand::Bool(true));
    }
    if let Some(pos) = ctx.data(|d| d.get_temp::<Pos2>(anchor_id)) {
        popup = popup.anchor(pos);
    }
    let chosen = popup.show(|ui| show(ui, &spec)).and_then(|r| r.inner);
    if !Popup::is_id_open(ctx, popup_id) {
        ctx.data_mut(|d| d.remove::<Pos2>(anchor_id));
    }
    if let Some(action) = chosen {
        return Some(RowAction::Menu(target, action));
    }

    if response.double_clicked() || (response.has_focus() && enter) {
        Some(RowAction::Activate(row))
    } else if response.secondary_clicked() || keyboard_menu {
        Some(RowAction::Secondary(row))
    } else if response.clicked() {
        Some(RowAction::Select(
            row,
            Click::from_modifiers(modifiers.command, modifiers.shift),
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(place: MenuPlace, count: usize) -> MenuSpec<'static> {
        MenuSpec {
            place,
            count,
            playlists: &[],
            exclude: None,
            device_ready: true,
        }
    }

    fn labels(s: &MenuSpec) -> Vec<String> {
        items(s).into_iter().map(|i| i.label).collect()
    }

    #[test]
    fn track_items_in_an_album() {
        assert_eq!(
            labels(&spec(MenuPlace::Track { search: false }, 1)),
            [
                "Play",
                "Play next",
                "Add to queue",
                "Add to playlist",
                "Go to artist"
            ]
        );
    }

    #[test]
    fn search_track_has_go_to_album() {
        assert_eq!(
            labels(&spec(MenuPlace::Track { search: true }, 1)),
            [
                "Play",
                "Play next",
                "Add to queue",
                "Add to playlist",
                "Go to album",
                "Go to artist"
            ]
        );
    }

    #[test]
    fn several_tracks_say_how_many() {
        assert_eq!(
            labels(&spec(MenuPlace::Track { search: true }, 5)),
            [
                "Play",
                "Play next",
                "Add 5 tracks to queue",
                "Add to playlist"
            ]
        );
    }

    #[test]
    fn playlist_row_items() {
        assert_eq!(
            labels(&spec(MenuPlace::PlaylistRow, 1)),
            [
                "Play from here",
                "Play next",
                "Add to queue",
                "Add to playlist",
                "Remove from playlist"
            ]
        );
        assert_eq!(
            labels(&spec(MenuPlace::PlaylistRow, 5)),
            [
                "Play from here",
                "Play next",
                "Add 5 tracks to queue",
                "Add to playlist",
                "Remove 5 tracks"
            ]
        );
    }

    #[test]
    fn queue_row_items() {
        let row = |upcoming, play_next| MenuPlace::QueueRow {
            upcoming,
            play_next,
        };
        assert_eq!(
            labels(&spec(row(0, false), 1)),
            ["Play from here", "Add to playlist", "Go to album"]
        );
        assert_eq!(
            labels(&spec(row(1, true), 1)),
            [
                "Play from here",
                "Play next",
                "Add to playlist",
                "Remove from queue",
                "Go to album"
            ]
        );
        assert_eq!(
            labels(&spec(row(2, false), 3)),
            ["Play from here", "Add to playlist", "Remove 2 tracks"]
        );
    }

    #[test]
    fn queue_row_edits_need_no_device() {
        let mut s = spec(
            MenuPlace::QueueRow {
                upcoming: 1,
                play_next: true,
            },
            1,
        );
        s.device_ready = false;
        for i in items(&s) {
            let device = i.action == Some(MenuAction::Play);
            assert_eq!(i.enabled, !device, "{}", i.label);
        }
    }

    #[test]
    fn album_items() {
        assert_eq!(
            labels(&spec(MenuPlace::Album, 1)),
            [
                "Play album",
                "Add album to queue",
                "Add to playlist",
                "Go to artist"
            ]
        );
    }

    #[test]
    fn device_items_disabled_without_a_device() {
        for place in [
            MenuPlace::Track { search: true },
            MenuPlace::Album,
            MenuPlace::PlaylistRow,
            MenuPlace::QueueRow {
                upcoming: 0,
                play_next: false,
            },
        ] {
            let mut s = spec(place, 1);
            s.device_ready = false;
            for i in items(&s) {
                let device = matches!(
                    i.action,
                    Some(MenuAction::Play | MenuAction::PlayNext | MenuAction::Queue)
                );
                assert_eq!(i.enabled, !device, "{}", i.label);
                assert_eq!(
                    i.hover,
                    device.then_some("No output device. Turn on or connect your DAC."),
                    "{}",
                    i.label
                );
            }
        }
    }

    #[test]
    fn submenu_lists_new_then_playlists_without_the_open_one() {
        let names = ["A".to_string(), "B".to_string(), "C".to_string()];
        let mut s = spec(MenuPlace::PlaylistRow, 1);
        s.playlists = &names;
        s.exclude = Some("B");
        let got: Vec<_> = submenu_items(&s).into_iter().map(|i| i.action).collect();
        assert_eq!(
            got,
            [
                Some(MenuAction::AddToNew),
                Some(MenuAction::AddTo("A".into())),
                Some(MenuAction::AddTo("C".into()))
            ]
        );
        assert_eq!(submenu_items(&s)[0].label, "New playlist…");
    }
}

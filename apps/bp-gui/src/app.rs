//! Top-level app: owns the state, drains player events, lays out the panels and
//! applies the views' actions.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bp_core::{DeviceId, DeviceInfo};
use bp_engine::home::Home;
use egui::{Frame, Key, Margin, Modifiers, Popup, PopupCloseBehavior, RectAlign, Stroke, Ui};

use crate::art::ArtLoader;
use crate::backend::mock_player::Clock;
use crate::backend::{
    Album, AlbumId, DeviceCaps, LibrarySource, PlayerCommand, PlayerEvent, PlayerHandle, QueueItem,
    ScanStatus, Track,
};
use crate::catalog::Catalog;
use crate::media_session::{self, MediaSession, Snapshot};
use crate::playlists;
use crate::plugin_bridge::{self, BridgeCtx};
use crate::plugins::host::{HostEvent, PluginHost, PluginInfo};
use crate::plugins::panel::{Panel, Place};
use crate::plugins::rpc::{Call, Permission, RpcError};
use crate::prefs::Prefs;
use crate::queue_edit::EditKind;
use crate::state::artists::{ArtistTile, artist_tiles};
use crate::state::caps_table::cap_table;
use crate::state::nav::{Nav, Page, Tab};
use crate::state::now_playing::{Badge, NOTICE_TIME, NowPlaying};
use crate::state::search::{SearchResults, search};
use crate::state::selection::{ListId, Selection};
use crate::state::spectrum::{FFT_SIZE, Spectrum};
use crate::theme;
use crate::views::album_page::{self, PageAction, PageData};
use crate::views::albums::{TileAction, TileMenu};
use crate::views::bar::{self, BarAction, BarData};
use crate::views::context_menu::{MenuPlace, MenuSpec};
use crate::views::device_list::{self, DeviceListAction};
use crate::views::name_dialog::{DialogEvent, NameDialog};
use crate::views::now_playing::{self as now_playing_view, NowPlayingAction, NowPlayingData};
use crate::views::panel::{self as panel_view, PanelAction, PanelState};
use crate::views::playlists::{self as playlists_view, PageRows, PlaylistAction, PlaylistsAction};
use crate::views::search::{self as search_view, SearchAction, TrackRows};
use crate::views::settings::{self, SettingsAction, SettingsData};
use crate::views::top_bar::{self, TopBarAction, TopBarData};
use crate::views::track_list::{self, RowSelect, TrackListAction, TrackRow};
use crate::views::{albums, artists, count, heading};

mod menu_actions;
mod queue_edits;

use menu_actions::Origin;

const TOP_BAR_HEIGHT: f32 = 56.0;
const ART_WORKERS: usize = 2;
const TICK: Duration = Duration::from_millis(250);
/// Albums on the Home page.
const RECENT_ALBUMS: usize = 30;
/// Arrow-key seek step.
const SEEK_STEP: Duration = Duration::from_secs(10);
/// Repaint interval while the spectrum moves.
const SPECTRUM_FRAME: Duration = Duration::from_millis(33);

pub struct Deps {
    pub player: Box<dyn PlayerHandle>,
    pub library: Arc<dyn LibrarySource>,
    /// The player's clock; times in `NowPlaying` are on it.
    pub clock: Arc<dyn Clock>,
    /// `None` = don't persist prefs.
    pub prefs_path: Option<PathBuf>,
    pub thumbs_dir: PathBuf,
    /// Where the `.m3u8` playlists live.
    pub playlists_dir: PathBuf,
    pub pick_folder: Box<dyn FnMut() -> Option<PathBuf>>,
    /// The plugin host, already started with a waker for this window.
    pub plugins: Option<PluginHost>,
    /// Where nil keeps its files, for Settings.
    pub home: Home,
    /// nil's entry in Windows' media session; `None` where there is none.
    pub media: Option<MediaSession>,
}

/// A modal over the app.
enum Dialog {
    /// Save the play queue, or `rows` when set, as a playlist.
    Save {
        dialog: NameDialog,
        rows: Option<Vec<TrackRow>>,
    },
    Rename {
        from: String,
        dialog: NameDialog,
    },
    Delete {
        name: String,
        error: Option<String>,
    },
    /// Name a new playlist. With `add` set, it is created, those tracks go in, and
    /// the page stays; without, the new playlist opens.
    NewPlaylist {
        dialog: NameDialog,
        add: Option<Vec<playlists::Entry>>,
    },
}

/// The open playlist page: its name and what was read, or why it could not be read.
struct OpenPlaylist {
    name: String,
    loaded: Result<Loaded, String>,
}

/// A playlist as read: every file row, and the rows the page shows.
struct Loaded {
    /// The file's rows, hidden ones included.
    file: Vec<playlists::Entry>,
    /// The file index of each shown row: `rows[k]` is file row `shown[k]`.
    shown: Vec<usize>,
    /// The rows whose file exists, in file order.
    rows: Vec<TrackRow>,
}

/// The last edit, for Undo.
enum Undo {
    /// A playlist Remove or move: the playlist as read before it and as written.
    Playlist {
        name: String,
        before: Vec<playlists::Entry>,
        after: Vec<playlists::Entry>,
    },
    /// A queue edit: the tracks from `from` on before and after it.
    Queue {
        from: usize,
        before: Vec<QueueItem>,
        after: Vec<QueueItem>,
    },
}

/// What a Rearrange was sent for; one per Rearrange, popped by its answer.
pub(crate) struct Ticket {
    from: usize,
    /// The tracks from `from` on as sent (the Rearrange's `expected`).
    before: Vec<QueueItem>,
    /// What replaces them (the Rearrange's `items`).
    after: Vec<QueueItem>,
    kind: EditKind,
    by: By,
    /// The queue indices where a Move's or Play next's rows land.
    moved: Vec<usize>,
    /// The position in `before` each row of `after` came from, so an edit held behind
    /// this one can follow its rows; None if that is not known.
    order: Option<Vec<usize>>,
    /// Play, Stop or the queue's end came after it: its answer is ignored.
    void: bool,
}

/// Who asked for an edit.
#[derive(Clone)]
pub(crate) enum By {
    Gui,
    /// A plugin call, answered with [`BpApp::reply`]: the host's `plugin` and `id` for it.
    Plugin {
        plugin: String,
        id: serde_json::Value,
    },
}

/// An edit made in Now Playing; rows are queue indices.
pub(crate) enum GuiEdit {
    Remove(Vec<usize>),
    PlayNext(Vec<usize>),
    /// The rows dragged in front of queue row `gap` (a drop between rows, `0..=len`).
    Move {
        rows: Vec<usize>,
        gap: usize,
    },
    /// Every upcoming row.
    Clear,
}

/// A line the bar shows for a few seconds after an action.
pub(crate) struct Notice {
    pub text: String,
    /// Whether the bar offers Undo beside it.
    pub undo: bool,
    /// Player-clock time it goes away.
    pub until: Duration,
}

pub struct BpApp {
    player: Box<dyn PlayerHandle>,
    library: Arc<dyn LibrarySource>,
    clock: Arc<dyn Clock>,
    prefs_path: Option<PathBuf>,
    playlists_dir: PathBuf,
    pick_folder: Box<dyn FnMut() -> Option<PathBuf>>,
    plugins: Option<PluginHost>,
    home: Home,
    media: Option<MediaSession>,
    /// What the host reports, read once per frame. With no host, what
    /// [`BpApp::set_plugin_infos`] set.
    plugin_infos: Vec<PluginInfo>,
    /// The last Settings plugin switch change that could not be saved: (folder, message).
    plugin_error: Option<(String, String)>,
    /// With no plugin host, the Settings plugin actions that would have been applied.
    #[doc(hidden)]
    pub plugin_actions: Vec<SettingsAction>,
    /// Each plugin's panel, by plugin folder name.
    panels: BTreeMap<String, Panel>,
    /// The panels plugins show in their Settings entry, by plugin folder name. They
    /// hold no inputs, so they keep no state between frames.
    settings_panels: BTreeMap<String, Panel>,
    /// What each open panel keeps between frames, by plugin folder name.
    panel_states: HashMap<String, PanelState>,
    /// With no plugin host, the `panel.event`s that would have been sent: (plugin, params).
    #[doc(hidden)]
    pub sent_panel_events: Vec<(String, serde_json::Value)>,
    /// With no plugin host, the answers to plugin calls that came after the call: see
    /// [`BpApp::take_plugin_replies`].
    unhosted_replies: Vec<Result<serde_json::Value, RpcError>>,
    /// The player state last announced to plugins with `player.changed`.
    announced: (u8, Option<usize>, usize),
    art: ArtLoader,
    prefs: Prefs,
    nav: Nav,
    now_playing: NowPlaying,
    query: String,
    scan_status: ScanStatus,
    /// The library's revision when it was last picked up.
    library_revision: u64,
    /// The library was changed from the UI; refresh every frame until its scan is
    /// done. A fast scan can finish between frames and leave the status unchanged.
    library_changed: bool,
    /// Albums shown by the grid, and the page (Home, Albums or an artist) they are for.
    grid_albums: Option<(Page, Vec<Album>)>,
    grid_scroll: f32,
    /// Grid scroll to apply on the next grid frame (after leaving an album page).
    restore_scroll: Option<f32>,
    /// The open album page's album and tracks.
    page: Option<(Album, Vec<Track>)>,
    /// Results for the current search, rebuilt when the query or the library changes.
    search: Option<SearchResults>,
    /// Artist tiles, rebuilt after a library change.
    artists: Option<Vec<ArtistTile>>,
    /// Path catalog, rebuilt by `catalog()` when the library's revision moves.
    catalog: Option<Catalog>,
    /// Playlist names, read when the Playlists page opens.
    playlist_names: Option<Vec<String>>,
    /// The open playlist page's rows, read when it opens and after every edit.
    playlist: Option<OpenPlaylist>,
    /// What Undo puts back; one level.
    undo: Option<Undo>,
    /// One per Rearrange sent and not yet answered, in the order sent.
    tickets: VecDeque<Ticket>,
    /// A queue edit made while an insert or rearrange was pending; made once they land.
    held_edit: Option<GuiEdit>,
    /// The queue's rows moved since the frame began: a drag or a row menu on them is
    /// ended before the rows are drawn again.
    queue_rows_moved: bool,
    /// The modal being shown, if any.
    dialog: Option<Dialog>,
    /// The bar's notice for the last playlist edit.
    notice: Option<Notice>,
    /// The album the play queue was started from.
    playing_album: Option<Album>,
    /// The playlist the play queue was started from, while it is still playing.
    playing_playlist: Option<String>,
    devices: Vec<DeviceInfo>,
    selected_device: Option<DeviceId>,
    selected_caps: Option<DeviceCaps>,
    /// The saved device has been looked for in a non-empty list; see `restore_device`.
    device_restored: bool,
    popover_open: bool,
    /// The full-window now-playing view is shown.
    now_playing_open: bool,
    spectrum: Spectrum,
    /// A text field had focus at the end of the last frame. egui drops focus on
    /// `Esc` before the frame runs, so the current focus alone misses that key.
    typing: bool,
    /// The bar's focusable controls, from the last frame.
    bar_controls: Vec<egui::Id>,
    /// The selected rows of the list on the current page.
    selection: Selection,
    /// The page the selection belongs to; another page clears it.
    selection_page: Page,
    /// Whether Now Playing was open when the selection was last scoped.
    selection_np: bool,
    /// An album page row to scroll into view on the next frame.
    reveal: Option<usize>,
}

impl BpApp {
    /// The library's path catalog, rebuilt first if a scan changed the library.
    fn catalog(&mut self) -> &Catalog {
        let rev = self.library.revision();
        if self.catalog.as_ref().is_none_or(|c| c.revision() != rev) {
            self.catalog = Some(Catalog::build(self.library.as_ref()));
        }
        self.catalog.as_ref().expect("just built")
    }

    /// The library album of the file at `path`, if the library has it.
    fn album_of(&mut self, path: &Path) -> Option<Album> {
        let id = self.catalog().get(path)?.album_id;
        self.library.album(id)
    }

    /// The album of the queue entry that is playing or paused.
    fn playing_track_album(&mut self) -> Option<Album> {
        let i = self.now_playing.index?;
        let path = self.now_playing.queue.get(i)?.path.clone();
        self.album_of(&path)
    }

    /// Shows `text` on the bar for `NOTICE_TIME`.
    fn notify(&mut self, text: String, undo: bool) {
        let until = self.clock.now() + NOTICE_TIME;
        self.notice = Some(Notice { text, undo, until });
    }

    /// Adds `entries` to the end of playlist `name` and says what happened.
    fn add_to_playlist(&mut self, name: &str, entries: Vec<playlists::Entry>) {
        let text = match playlists::add(&self.playlists_dir, name, &entries) {
            Ok(r) if r.added == 0 => format!("Already in {name}"),
            Ok(r) => {
                let tracks = if r.added == 1 { "track" } else { "tracks" };
                let mut text = format!("Added {} {tracks} to {name}", r.added);
                if r.skipped > 0 {
                    text.push_str(&format!(" ({} already there)", r.skipped));
                }
                text
            }
            Err(e) => {
                // It may be gone: the submenus read the folder again.
                self.playlist_names = None;
                format!("Couldn't change {name}: {e}")
            }
        };
        self.drop_undo();
        // An open copy of it would show the old rows.
        if self.playlist.as_ref().is_some_and(|p| p.name == name) {
            self.reload_playlist();
        }
        self.notify(text, false);
    }

    /// Re-reads the open playlist page and clears its selection: its rows may have moved.
    fn reload_playlist(&mut self) {
        let Some(name) = self.playlist.as_ref().map(|p| p.name.clone()) else {
            return;
        };
        let loaded = self.load_playlist(&name);
        self.playlist = Some(OpenPlaylist {
            name: name.clone(),
            loaded,
        });
        if self.selection.list() == Some(&ListId::Playlist(name)) {
            self.selection.clear();
        }
    }

    /// Forgets the undo, and the Undo button with it.
    fn drop_undo(&mut self) {
        self.undo = None;
        if let Some(n) = &mut self.notice {
            n.undo = false;
        }
    }

    /// Forgets the undo if it is for a queue edit.
    fn drop_queue_undo(&mut self) {
        if matches!(self.undo, Some(Undo::Queue { .. })) {
            self.drop_undo();
        }
    }

    /// Leaves Now Playing; a queue edit can no longer be undone.
    fn close_now_playing(&mut self) {
        self.now_playing_open = false;
        self.drop_queue_undo();
    }

    /// Removes file rows `rows` from the open playlist `name`, keeping an undo.
    fn remove_rows(&mut self, name: &str, rows: &[usize]) {
        let Some(Ok(loaded)) = self
            .playlist
            .as_ref()
            .filter(|p| p.name == name)
            .map(|p| p.loaded.as_ref())
        else {
            return;
        };
        if rows.is_empty() {
            return;
        }
        let before = loaded.file.clone();
        self.drop_undo();
        let text = match playlists::remove(&self.playlists_dir, name, &before, rows) {
            Ok(()) => {
                let after = before
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !rows.contains(i))
                    .map(|(_, e)| e.clone())
                    .collect();
                self.undo = Some(Undo::Playlist {
                    name: name.to_owned(),
                    before,
                    after,
                });
                let tracks = if rows.len() == 1 { "track" } else { "tracks" };
                format!("Removed {} {tracks} from {name}", rows.len())
            }
            Err(e) => format!("Couldn't change {name}: {e}"),
        };
        let undo = self.undo.is_some();
        self.reload_playlist();
        self.notify(text, undo);
    }

    /// Moves shown rows `rows` (file indices) of the open playlist `name` in front of
    /// shown position `gap`, each with the hidden rows after it, keeping an undo. The page
    /// shows the new order at once and the moved rows stay selected.
    fn move_rows(&mut self, name: &str, rows: &[usize], gap: usize) {
        let Some(Ok(loaded)) = self
            .playlist
            .as_mut()
            .filter(|p| p.name == name)
            .map(|p| p.loaded.as_mut())
        else {
            return;
        };
        let (indices, to) = track_list::drop_indices(loaded.file.len(), &loaded.shown, rows, gap);
        let order = playlists::reordered(loaded.file.len(), &indices, to);
        if rows.is_empty() || order.iter().enumerate().all(|(pos, &i)| pos == i) {
            return;
        }
        let before = loaded.file.clone();
        let after: Vec<playlists::Entry> = order.iter().map(|&i| before[i].clone()).collect();
        // The new order at once: each shown row keeps its TrackRow.
        let old_rows = std::mem::take(&mut loaded.rows);
        let mut shown = Vec::new();
        let mut new_rows = Vec::new();
        for (pos, &old) in order.iter().enumerate() {
            if let Some(k) = loaded.shown.iter().position(|&s| s == old) {
                shown.push(pos);
                new_rows.push(old_rows[k].clone());
            }
        }
        loaded.file = after.clone();
        loaded.shown = shown;
        loaded.rows = new_rows;
        self.drop_undo();
        match playlists::reorder(&self.playlists_dir, name, &before, &indices, to) {
            Ok(()) => {
                let moved = rows
                    .iter()
                    .filter_map(|r| order.iter().position(|o| o == r))
                    .collect();
                self.selection
                    .set(&ListId::Playlist(name.to_owned()), moved);
                self.undo = Some(Undo::Playlist {
                    name: name.to_owned(),
                    before,
                    after,
                });
                let tracks = if rows.len() == 1 { "track" } else { "tracks" };
                self.notify(format!("Moved {} {tracks}", rows.len()), true);
            }
            Err(e) => {
                self.reload_playlist();
                self.notify(format!("Couldn't change {name}: {e}"), false);
            }
        }
    }

    /// Undoes the last edit: a playlist Remove or move while its playlist is the open page,
    /// or a queue edit while Now Playing is open.
    fn undo(&mut self) {
        match &self.undo {
            Some(Undo::Playlist { name, .. }) => {
                if !matches!(self.nav.current(), Page::Playlist(open) if open == name) {
                    return;
                }
            }
            Some(Undo::Queue { .. }) => {
                if self.now_playing_open {
                    self.undo_queue();
                }
                return;
            }
            None => return,
        }
        let Some(Undo::Playlist {
            name,
            before,
            after,
        }) = self.undo.take()
        else {
            return;
        };
        let result = playlists::restore(&self.playlists_dir, &name, &after, &before);
        self.reload_playlist();
        match result {
            Ok(()) => self.notice = None,
            Err(e) => self.notify(e.to_string(), false),
        }
    }

    /// The names of the saved playlists, read once until something changes them.
    fn playlist_names(&mut self) -> &[String] {
        self.playlist_names
            .get_or_insert_with(|| playlists::list(&self.playlists_dir).unwrap_or_default())
    }

    /// Forgets what the Playlists pages read, so they read the folder again; a page
    /// opened next reads its file then.
    fn forget_playlists(&mut self) {
        self.playlist_names = None;
        self.playlist = None;
    }

    /// Playlist `name` as read; its shown rows are those whose file exists, titled from
    /// the library where it knows the file.
    fn load_playlist(&mut self, name: &str) -> Result<Loaded, String> {
        let file = playlists::read(&self.playlists_dir, name).map_err(|e| e.to_string())?;
        let catalog = self.catalog();
        let mut shown = Vec::new();
        let mut rows = Vec::new();
        for (i, e) in file.iter().enumerate() {
            if !e.path.exists() {
                continue;
            }
            let known = catalog.get(&e.path);
            let stem = e.path.file_stem().map(|s| s.to_string_lossy().into_owned());
            shown.push(i);
            rows.push(TrackRow {
                title: known
                    .map(|t| t.title.clone())
                    .or_else(|| e.title.clone())
                    .or(stem)
                    .unwrap_or_default(),
                artist: known
                    .map(|t| t.artist.clone())
                    .or_else(|| e.artist.clone())
                    .unwrap_or_default(),
                duration_s: known
                    .and_then(|t| t.duration_s)
                    .or(e.duration_s.map(|s| s as f64)),
                missing: false,
                path: e.path.clone(),
            });
        }
        Ok(Loaded { file, shown, rows })
    }

    /// The playlist lines for `rows`.
    fn rows_entries(&mut self, rows: &[TrackRow]) -> Vec<playlists::Entry> {
        let catalog = self.catalog();
        rows.iter()
            .map(|r| match catalog.get(&r.path) {
                Some(t) => playlists::entry_for(t),
                None => playlists::Entry {
                    path: r.path.clone(),
                    title: Some(r.title.clone()),
                    artist: (!r.artist.is_empty()).then(|| r.artist.clone()),
                    duration_s: r.duration_s.map(|s| s as u64),
                },
            })
            .collect()
    }

    /// The playlist lines for the play queue.
    fn queue_entries(&mut self) -> Vec<playlists::Entry> {
        let queue = self.now_playing.queue.clone();
        self.item_entries(queue)
    }

    /// Playlist lines for queue items: the library's data where it knows the file.
    fn item_entries(&mut self, items: Vec<QueueItem>) -> Vec<playlists::Entry> {
        let catalog = self.catalog();
        items
            .into_iter()
            .map(|item| match catalog.get(&item.path) {
                Some(t) => playlists::entry_for(t),
                None => playlists::Entry {
                    duration_s: (!item.duration.is_zero()).then_some(item.duration.as_secs()),
                    path: item.path,
                    title: Some(item.title),
                    artist: None,
                },
            })
            .collect()
    }

    /// Plays `rows` from the first playable one at or after `from`.
    /// Returns whether a Play was sent.
    fn play_rows(&mut self, rows: &[TrackRow], from: usize) -> bool {
        let Some(first) = (from..rows.len()).find(|&i| !rows[i].missing) else {
            return false;
        };
        let start = rows[..first].iter().filter(|r| !r.missing).count();
        let items = rows_to_items(rows, self.catalog());
        self.playing_album = None;
        self.send(PlayerCommand::Play { items, start });
        true
    }

    /// Plays a playlist's shown `rows` from `from`, and remembers which playlist it is.
    fn play_playlist_rows(&mut self, name: &str, rows: &[TrackRow], from: usize) {
        if self.play_rows(rows, from) {
            self.playing_playlist = Some(name.to_owned());
        }
    }

    fn queue_rows(&mut self, rows: &[TrackRow]) {
        let items = rows_to_items(rows, self.catalog());
        if !items.is_empty() {
            self.send(PlayerCommand::Append { items });
        }
    }

    pub fn new(ctx: &egui::Context, deps: Deps) -> Self {
        theme::apply(ctx);
        let waker = {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        };
        deps.player.set_waker(Box::new(waker.clone()));
        deps.library.set_waker(Box::new(waker.clone()));
        let art = ArtLoader::new(
            deps.library.clone(),
            deps.thumbs_dir,
            ART_WORKERS,
            Arc::new(waker),
        );
        let prefs = deps
            .prefs_path
            .as_deref()
            .map(Prefs::load)
            .unwrap_or_default();
        let scan_status = deps.library.scan_status();
        let library_revision = deps.library.revision();
        let mut app = BpApp {
            player: deps.player,
            library: deps.library,
            clock: deps.clock,
            prefs_path: deps.prefs_path,
            playlists_dir: deps.playlists_dir,
            pick_folder: deps.pick_folder,
            plugins: deps.plugins,
            home: deps.home,
            media: deps.media,
            plugin_infos: Vec::new(),
            plugin_error: None,
            plugin_actions: Vec::new(),
            panels: BTreeMap::new(),
            settings_panels: BTreeMap::new(),
            panel_states: HashMap::new(),
            sent_panel_events: Vec::new(),
            unhosted_replies: Vec::new(),
            announced: plugin_bridge::state_key(&NowPlaying::default()),
            art,
            prefs,
            nav: Nav::default(),
            now_playing: NowPlaying::default(),
            query: String::new(),
            scan_status,
            library_revision,
            library_changed: false,
            grid_albums: None,
            grid_scroll: 0.0,
            restore_scroll: None,
            page: None,
            search: None,
            artists: None,
            catalog: None,
            playlist_names: None,
            playlist: None,
            undo: None,
            tickets: VecDeque::new(),
            held_edit: None,
            queue_rows_moved: false,
            dialog: None,
            notice: None,
            playing_album: None,
            playing_playlist: None,
            devices: Vec::new(),
            selected_device: None,
            selected_caps: None,
            device_restored: false,
            popover_open: false,
            now_playing_open: false,
            spectrum: Spectrum::new(),
            typing: false,
            bar_controls: Vec::new(),
            selection: Selection::default(),
            selection_page: Page::Home,
            selection_np: false,
            reveal: None,
        };
        app.refresh_devices();
        app.restore_device();
        app
    }

    pub fn nav(&self) -> &Nav {
        &self.nav
    }

    pub fn now_playing(&self) -> &NowPlaying {
        &self.now_playing
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// The panels plugins have set, by plugin folder name.
    pub fn panels(&self) -> &BTreeMap<String, Panel> {
        &self.panels
    }

    /// The panels plugins show in Settings, by plugin folder name.
    pub fn settings_panels(&self) -> &BTreeMap<String, Panel> {
        &self.settings_panels
    }

    /// The bar floats over the page's bottom on a wash; pages run underneath it.
    fn floating_bar(&mut self, ctx: &egui::Context, screen: egui::Rect) {
        let bar = egui::Rect::from_min_max(
            egui::pos2(screen.left(), screen.bottom() - bar::HEIGHT),
            screen.max,
        );
        egui::Area::new(egui::Id::unique("floating_bar"))
            .order(egui::Order::Middle)
            .fixed_pos(bar.min)
            .constrain(false)
            .show(ctx, |ui| {
                let wash = egui::Rect::from_min_max(
                    egui::pos2(screen.left(), screen.bottom() - bar::WASH),
                    screen.max,
                );
                // Drawn first and outside the bar's own rect: under the controls, and
                // it takes no clicks from the page above them.
                bar::wash(&ui.painter().with_clip_rect(screen), wash);
                ui.set_min_size(bar.size());
                ui.set_max_size(bar.size());
                self.bar(ui);
            });
    }

    pub fn spectrum(&self) -> &Spectrum {
        &self.spectrum
    }

    pub fn now_playing_open(&self) -> bool {
        self.now_playing_open
    }

    pub fn prefs(&self) -> &Prefs {
        &self.prefs
    }

    /// Every player command goes through here so `NowPlaying` sees it.
    pub fn send(&mut self, cmd: PlayerCommand) {
        let np = &self.now_playing;
        // Append and Insert into a queue with nothing playing start a new one.
        let fresh = match &cmd {
            PlayerCommand::Append { items } | PlayerCommand::Insert { items } => {
                !items.is_empty() && (np.index.is_none() || np.fatal.is_some())
            }
            _ => false,
        };
        let new_queue = fresh || matches!(cmd, PlayerCommand::Play { .. });
        if new_queue || matches!(cmd, PlayerCommand::Stop) {
            self.playing_playlist = None;
        }
        // A new queue starts: the old rows' indices mean nothing now.
        if new_queue {
            self.clear_queue_selection();
        }
        if matches!(cmd, PlayerCommand::Play { .. } | PlayerCommand::Stop) {
            self.void_tickets();
        }
        self.now_playing.on_command(&cmd);
        self.player.send(cmd);
    }

    /// Drops the selection if it is in the queue, whose row indices just changed. A drag
    /// or a row menu on the old rows goes too, see [`BpApp::end_queue_gestures`].
    fn clear_queue_selection(&mut self) {
        self.queue_rows_moved = true;
        if self.selection.list() == Some(&ListId::Queue) {
            self.selection.clear();
        }
    }

    /// Ends a queue drag and closes a queue row's menu once the rows moved under them:
    /// the drop or the menu choice would act on rows that now hold other tracks.
    fn end_queue_gestures(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.queue_rows_moved) && self.now_playing_open {
            track_list::cancel_drag(ctx);
            // In Now Playing the only menus are the queue rows'.
            Popup::close_all(ctx);
        }
    }

    /// A selected output device that is in the device list.
    fn device_ready(&self) -> bool {
        self.selected_device
            .as_ref()
            .is_some_and(|id| self.devices.iter().any(|d| &d.id == id))
    }

    /// Answers one plugin call, `by`; the player command it asks for is sent here. None
    /// when the answer comes later: a queue edit is answered when the engine has.
    fn answer_call(&mut self, call: Call, by: By) -> Option<Result<serde_json::Value, RpcError>> {
        if matches!(
            call,
            Call::PlayerRemove { .. } | Call::PlayerMove { .. } | Call::PlayerClearUpcoming
        ) {
            return self.plugin_edit(&call, by);
        }
        self.catalog();
        let ctx = BridgeCtx {
            catalog: self.catalog.as_ref().expect("just built"),
            np: &self.now_playing,
            folders: self.library.folders(),
            playlists_dir: &self.playlists_dir,
            device_ready: self.device_ready(),
        };
        let touched = match &call {
            Call::PlaylistsSave { name, .. } | Call::PlaylistsAdd { name, .. } => {
                Some(name.clone())
            }
            _ => None,
        };
        let (result, cmd) = plugin_bridge::answer(call, &ctx);
        if let Some(cmd) = cmd {
            self.send(cmd);
        }
        if let (Some(name), true) = (touched, result.is_ok()) {
            self.playlist_names = None;
            // The plugin may have changed what Undo would put back.
            self.drop_undo();
            // The open page re-reads, and its selection goes with the old rows.
            if self.playlist.as_ref().is_some_and(|p| p.name == name) {
                self.reload_playlist();
            }
        }
        Some(result)
    }

    /// Sends the answer to the plugin call `by` that came after the call. With no host it
    /// is kept for [`BpApp::take_plugin_replies`].
    fn reply(&mut self, by: &By, result: Result<serde_json::Value, RpcError>) {
        let By::Plugin { plugin, id } = by else {
            return;
        };
        match &self.plugins {
            Some(host) => host.reply(plugin, id.clone(), result),
            None => self.unhosted_replies.push(result),
        }
    }

    /// Answers a plugin call as if a plugin had sent it. For tests that run without a host.
    /// A queue edit is answered later: this returns `Ok(Value::Null)` for it; see
    /// [`BpApp::take_plugin_replies`]. Test-only: with a host attached, its deferred
    /// replies have no route (`plugin: ""`).
    #[doc(hidden)]
    pub fn answer_plugin_call(&mut self, call: Call) -> Result<serde_json::Value, RpcError> {
        let by = By::Plugin {
            plugin: String::new(),
            id: serde_json::Value::Null,
        };
        self.answer_call(call, by)
            .unwrap_or(Ok(serde_json::Value::Null))
    }

    /// With no plugin host, the answers to queue edits made with
    /// [`BpApp::answer_plugin_call`] that came since the last call, in the order they came.
    #[doc(hidden)]
    pub fn take_plugin_replies(&mut self) -> Vec<Result<serde_json::Value, RpcError>> {
        std::mem::take(&mut self.unhosted_replies)
    }

    /// Shows `panel` as the plugin's tab. Public for tests that run without a host.
    #[doc(hidden)]
    pub fn set_panel(&mut self, plugin: String, panel: Panel) {
        let state = self.panel_states.entry(plugin.clone()).or_default();
        state.sync(self.panels.get(&plugin), &panel);
        self.panels.insert(plugin, panel);
    }

    /// Shows `panel` in the plugin's Settings entry, or removes it. Public for tests
    /// that run without a host.
    #[doc(hidden)]
    pub fn set_settings_panel(&mut self, plugin: String, panel: Option<Panel>) {
        match panel {
            Some(panel) => {
                self.settings_panels.insert(plugin, panel);
            }
            None => {
                self.settings_panels.remove(&plugin);
            }
        }
    }

    /// The plugin's panel is gone: its tab and pages go with it.
    #[doc(hidden)]
    pub fn remove_panel(&mut self, plugin: &str) {
        self.panels.remove(plugin);
        self.panel_states.remove(plugin);
        if *self.nav.current() == Page::Plugin(plugin.to_owned()) {
            self.nav.drop_plugin(plugin);
            self.restore_scroll = Some(self.nav.scroll());
        } else {
            self.nav.drop_plugin(plugin);
        }
    }

    /// The tab label for a plugin: its title or name, or its folder name when the host
    /// does not know it.
    fn plugin_label(&self, plugin: &str) -> String {
        self.plugin_infos
            .iter()
            .find(|i| i.folder == plugin)
            .and_then(|i| i.manifest.as_ref())
            .map_or_else(|| plugin.to_owned(), |m| m.label().to_owned())
    }

    /// Test hook: the plugins Settings lists when there is no host.
    #[doc(hidden)]
    pub fn set_plugin_infos(&mut self, infos: Vec<PluginInfo>) {
        self.plugin_infos = infos;
    }

    /// Reads the host's plugins, once per frame: the top bar and Settings share the
    /// result. With no host the list is left as `set_plugin_infos` made it. A pending
    /// "turn on" confirmation for a plugin that is gone or cannot load is dropped.
    fn refresh_plugin_infos(&mut self, ctx: &egui::Context) {
        if let Some(host) = &self.plugins {
            self.plugin_infos = host.plugins();
        }
        settings::forget_stale_confirm(ctx, &self.plugin_infos);
    }

    /// Opens a folder or file in Explorer without waiting for it. Does nothing off
    /// Windows, and without a plugin host (tests, which record the action instead).
    fn open_external(&self, path: &Path) {
        #[cfg(windows)]
        if self.plugins.is_some() {
            // The child is dropped unwaited.
            let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
        }
        #[cfg(not(windows))]
        let _ = path;
    }

    /// Applies a Settings plugin action. With no host it is only recorded.
    fn plugin_action(&mut self, action: SettingsAction) {
        // An error stays on its plugin's row until the next action.
        self.plugin_error = None;
        match (&mut self.plugins, &action) {
            (Some(host), SettingsAction::PluginEnable { folder, on }) => {
                if let Err(e) = host.set_enabled(folder, *on) {
                    tracing::warn!("cannot save the consent for {folder}: {e}");
                    self.plugin_error =
                        Some((folder.clone(), format!("Couldn't save the change: {e}")));
                }
            }
            (Some(host), SettingsAction::PluginRestart(folder)) => host.restart(folder),
            (_, SettingsAction::OpenFolder(p) | SettingsAction::OpenFile(p)) => {
                self.open_external(p);
            }
            _ => {}
        }
        if self.plugins.is_none() {
            self.plugin_actions.push(action);
        }
    }

    /// Tells the plugin its panel's element `params.id` was used. Events from the
    /// settings panel say so with `"place": "settings"`.
    fn send_panel_event(&mut self, plugin: &str, place: Place, mut params: serde_json::Value) {
        if place == Place::Settings {
            params["place"] = "settings".into();
        }
        match &self.plugins {
            Some(host) => host.notify(plugin, "panel.event", params),
            None => self.sent_panel_events.push((plugin.to_owned(), params)),
        }
    }

    /// Handles everything the plugin host has queued: calls get an answer, panels are
    /// kept or dropped.
    fn drain_plugins(&mut self) {
        let Some(host) = &self.plugins else { return };
        let mut events = Vec::new();
        while let Some(ev) = host.try_recv() {
            events.push(ev);
        }
        for ev in events {
            match ev {
                HostEvent::Call { plugin, id, call } => {
                    let by = By::Plugin { plugin, id };
                    if let Some(result) = self.answer_call(call, by.clone()) {
                        self.reply(&by, result);
                    }
                }
                HostEvent::Panel {
                    plugin,
                    place: Place::Tab,
                    panel,
                } => match panel {
                    Some(mut panel) => {
                        panel.mark_missing();
                        self.set_panel(plugin, panel);
                    }
                    None => self.remove_panel(&plugin),
                },
                HostEvent::Panel {
                    plugin,
                    place: Place::Settings,
                    panel,
                } => self.set_settings_panel(plugin, panel),
                HostEvent::Changed => {}
            }
        }
    }

    /// Sends `player.changed` to the plugins granted `playback` when the player's
    /// state moved since the last one. Called once at the end of a frame, so a frame
    /// sends at most one however many events and commands it saw.
    fn announce_player(&mut self) {
        let key = plugin_bridge::state_key(&self.now_playing);
        if key == self.announced {
            return;
        }
        self.announced = key;
        let Some(host) = &self.plugins else { return };
        let params = plugin_bridge::changed_params(&self.now_playing);
        for info in &self.plugin_infos {
            if host.granted(&info.folder, Permission::Playback) {
                host.notify(&info.folder, "player.changed", params.clone());
            }
        }
    }

    /// Runs the presses from Windows' media session as the bar's buttons would.
    fn media_presses(&mut self) {
        let Some(media) = &self.media else { return };
        for button in media.presses() {
            if let Some(cmd) = media_session::command(button, &self.now_playing) {
                self.transport(cmd);
            }
        }
    }

    /// Tells Windows' media session what changed this frame.
    fn publish_media(&mut self, ctx: &egui::Context) {
        if self.media.is_none() {
            return;
        }
        let status = media_session::status(&self.now_playing);
        let track = match status {
            media_session::Status::Stopped => None,
            _ => self.media_track(ctx),
        };
        let snap = Snapshot {
            status,
            track,
            timeline: media_session::timeline(&self.now_playing),
        };
        let now = self.clock.now();
        if let Some(media) = &mut self.media {
            media.publish(snap, now);
        }
    }

    /// The playing track as the media session shows it.
    fn media_track(&mut self, ctx: &egui::Context) -> Option<media_session::Track> {
        let item = self.now_playing.queue.get(self.now_playing.index?)?;
        let title = item.title.clone();
        let album = self.playing_track_album();
        let art = album.as_ref().and_then(|a| self.art.thumb_file(ctx, a.id));
        let (artist, album) = album.map(|a| (a.artist, a.title)).unwrap_or_default();
        Some(media_session::Track {
            title,
            artist,
            album,
            art,
        })
    }

    /// The whole frame.
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        while let Some(ev) = self.player.try_recv() {
            if matches!(ev, PlayerEvent::DevicesChanged) {
                self.refresh_devices();
                self.restore_device();
            }
            if matches!(ev, PlayerEvent::Engine(bp_engine::Event::QueueFinished)) {
                self.playing_playlist = None;
            }
            if let PlayerEvent::Engine(
                bp_engine::Event::QueueFinished | bp_engine::Event::Error(_),
            ) = ev
            {
                self.void_tickets();
            }
            // Rows spliced in move the queue's row indices. A stale insert splices none.
            if let PlayerEvent::Engine(bp_engine::Event::Inserted { at, count }) = ev
                && self.now_playing.insert_is_live()
            {
                self.clear_queue_selection();
                if let Some(edit) = &mut self.held_edit {
                    edit.shift(at, count);
                }
            }
            // Before `apply`, which splices the queue: the answer reads it as it was.
            if let PlayerEvent::Engine(e) = &ev {
                self.queue_answer(e);
            }
            self.now_playing.apply(&ev, self.clock.now());
        }
        // An edit kept while the queue was settling is made now.
        if !self.now_playing.settling()
            && let Some(edit) = self.held_edit.take()
        {
            self.edit_queue(edit);
        }
        self.poll_library();
        self.drain_plugins();
        self.end_queue_gestures(&ctx);
        self.refresh_plugin_infos(&ctx);
        if *self.nav.current() != self.selection_page {
            self.selection.clear();
            self.drop_undo();
            self.selection_page = self.nav.current().clone();
        }
        // Opening or closing Now Playing swaps the list on screen.
        if self.now_playing_open != self.selection_np {
            self.selection.clear();
            self.selection_np = self.now_playing_open;
        }
        self.keyboard(&ctx);
        self.media_presses();
        let now = std::time::Instant::now();
        let window = self
            .player
            .analysis()
            .and_then(|tap| tap.window(FFT_SIZE, now));
        if self.spectrum.update(window.as_ref(), now) {
            ctx.request_repaint_after(SPECTRUM_FRAME);
        }

        // The view closes once the queue has ended; a fatal error keeps it to show Retry.
        let np = &self.now_playing;
        if np.index.is_none() && np.fatal.is_none() {
            self.close_now_playing();
        }
        if self.now_playing_open {
            egui::CentralPanel::no_frame()
                .frame(Frame::new())
                .show(ui, |ui| self.now_playing_view(ui));
        } else {
            let screen = ui.max_rect();
            egui::Panel::top("top_bar")
                .exact_size(TOP_BAR_HEIGHT)
                .frame(Frame::new().fill(theme::WINDOW))
                .show(ui, |ui| self.top_bar(ui));

            egui::CentralPanel::no_frame()
                .frame(Frame::new().fill(theme::WINDOW).inner_margin(Margin {
                    left: 32,
                    right: 32,
                    top: 28,
                    bottom: 0,
                }))
                .show(ui, |ui| self.central(ui));
            self.floating_bar(&ctx, screen);
        }
        self.dialogs(&ctx);
        crate::views::window_controls::resize_edges(&ctx, ctx.content_rect());
        self.announce_player();
        self.publish_media(&ctx);

        self.typing = ctx.text_edit_focused();
        let scanning = matches!(self.scan_status, ScanStatus::Scanning { .. });
        if self.now_playing.is_playing()
            || self.now_playing.badge == Badge::Opening
            || scanning
            || self.library_changed
        {
            ctx.request_repaint_after(TICK);
        }
        // A visible notice must also disappear on time when nothing else repaints.
        let now = self.clock.now();
        if let Some((_, expiry)) = &self.now_playing.notice
            && *expiry > now
        {
            ctx.request_repaint_after(*expiry - now);
        }
        if let Some(n) = &self.notice
            && n.until > now
        {
            ctx.request_repaint_after(n.until - now);
        }
    }

    /// Shortcuts. `Ctrl+F` works while typing; the rest only when no text field has
    /// focus. Keys are consumed so focused widgets don't also see them.
    fn keyboard(&mut self, ctx: &egui::Context) {
        // A modal owns the keys.
        if self.dialog.is_some() {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            ctx.memory_mut(|m| m.request_focus(top_bar::search_id()));
        }
        if self.typing || ctx.text_edit_focused() {
            return;
        }
        let (back, forward) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::ALT, Key::ArrowLeft)
                    | i.pointer.button_pressed(egui::PointerButton::Extra1),
                i.consume_key(Modifiers::ALT, Key::ArrowRight)
                    | i.pointer.button_pressed(egui::PointerButton::Extra2),
            )
        });
        if back {
            self.back();
        }
        if forward {
            self.forward();
        }
        let page_list = match self.nav.current() {
            _ if self.now_playing_open => Some(Origin::Queue),
            Page::Album { album } => Some(Origin::Album(*album)),
            Page::Search(_) => Some(Origin::Search),
            Page::Playlist(name) => Some(Origin::Playlist(name.clone())),
            _ => None,
        };
        if let Some(origin) = page_list
            && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A))
        {
            self.select_all(&origin);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z)) {
            self.undo();
        }
        // Esc during a drag cancels only the drag; the selection stays.
        if track_list::dragging(ctx)
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            track_list::cancel_drag(ctx);
        }
        // An open context menu closes itself on Esc.
        let menu_open = Popup::is_any_open(ctx);
        // Arrows belong to a focused widget (slider, focus moves), except bar controls.
        let focused = ctx.memory(|m| m.focused());
        let arrows_seek = focused.is_none_or(|id| self.bar_controls.contains(&id));
        let (space, prev, next, esc, backspace) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Space),
                i.consume_key(Modifiers::COMMAND, Key::ArrowLeft),
                i.consume_key(Modifiers::COMMAND, Key::ArrowRight),
                !menu_open && i.consume_key(Modifiers::NONE, Key::Escape),
                !menu_open && i.consume_key(Modifiers::NONE, Key::Backspace),
            )
        });
        let (back_10, fwd_10) = if arrows_seek {
            ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowLeft),
                    i.consume_key(Modifiers::NONE, Key::ArrowRight),
                )
            })
        } else {
            (false, false)
        };
        if space {
            self.play_pause();
        }
        if prev {
            self.transport(PlayerCommand::Previous);
        }
        if next {
            self.transport(PlayerCommand::Next);
        }
        if back_10 || fwd_10 {
            let pos = self.now_playing.position;
            let np = &self.now_playing;
            let item = np.index.and_then(|i| np.queue.get(i));
            let total = np
                .duration
                .or(item.map(|i| i.duration))
                .unwrap_or(Duration::ZERO);
            let to = if fwd_10 {
                (pos + SEEK_STEP).min(total)
            } else {
                pos.saturating_sub(SEEK_STEP)
            };
            self.transport(PlayerCommand::Seek { position: to });
        }
        // Esc closes the device popover first, then clears the selection, then goes back.
        if esc {
            if self.popover_open {
                self.popover_open = false;
            } else if !self.selection.is_empty() {
                self.selection.clear();
            } else if self.now_playing_open {
                self.close_now_playing();
            } else {
                self.back();
            }
        }
        // Delete removes the selected upcoming tracks from the queue in Now Playing, unless
        // a menu or a drag is acting on them.
        if self.now_playing_open
            && !menu_open
            && !track_list::dragging(ctx)
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete))
        {
            let shown: Vec<usize> = (0..self.now_playing.queue.len()).collect();
            let rows = self.selection.rows(&ListId::Queue, &shown);
            if !rows.is_empty() {
                self.edit_queue(GuiEdit::Remove(rows));
            }
        }
        // Backspace always goes back.
        if backspace {
            if self.popover_open {
                self.popover_open = false;
            } else if self.now_playing_open {
                self.close_now_playing();
            } else {
                self.back();
            }
        }
    }

    /// Pause/resume, or start the open album page when nothing is loaded.
    fn play_pause(&mut self) {
        let np = &self.now_playing;
        if np.index.is_some() && np.fatal.is_none() {
            let cmd = if np.paused {
                PlayerCommand::Resume
            } else {
                PlayerCommand::Pause
            };
            self.send(cmd);
        } else if let Page::Album { .. } = *self.nav.current()
            && let Some((album, tracks)) = self.page.clone()
        {
            // From the first selected track, if any.
            let shown: Vec<usize> = (0..tracks.len()).collect();
            let start = self
                .selection
                .rows(&ListId::Album(album.id), &shown)
                .first()
                .copied()
                .unwrap_or(0);
            self.play_album(album, &tracks, start);
        }
    }

    /// Sends a command that needs a loaded track; dropped otherwise.
    fn transport(&mut self, cmd: PlayerCommand) {
        if self.now_playing.index.is_some() && self.now_playing.fatal.is_none() {
            self.send(cmd);
        }
    }

    fn play_album(&mut self, album: Album, tracks: &[Track], start: usize) {
        let items = album_items(tracks);
        self.playing_album = Some(album);
        self.send(PlayerCommand::Play { items, start });
    }

    fn back(&mut self) {
        self.forget_playlists();
        if self.nav.back(self.page_scroll()) {
            self.restore_scroll = Some(self.nav.scroll());
        }
    }

    fn forward(&mut self) {
        self.forget_playlists();
        if self.nav.forward(self.page_scroll()) {
            self.restore_scroll = Some(self.nav.scroll());
        }
    }

    /// Opens `page`, remembering the current page's scroll.
    fn open(&mut self, page: Page) {
        self.forget_playlists();
        self.nav.open(page, self.page_scroll());
        self.restore_scroll = Some(self.nav.scroll());
    }

    /// The live scroll offset of the current page; only the grid keeps one.
    pub fn page_scroll(&self) -> f32 {
        match self.nav.current() {
            Page::Home | Page::Albums | Page::Artists | Page::Artist(_) => self.grid_scroll,
            _ => 0.0,
        }
    }

    /// Re-reads the device list, the selection and the selected device's capabilities.
    fn refresh_devices(&mut self) {
        let (devices, selected) = self.player.devices();
        self.selected_caps = selected.as_ref().and_then(|id| self.player.device_caps(id));
        self.devices = devices;
        self.selected_device = selected;
    }

    /// Selects the device saved in prefs, once, when it first appears in the list. The
    /// real engine publishes its list after startup, so this also runs on
    /// `DevicesChanged`; the flag stops a stale one from undoing the user's choice.
    fn restore_device(&mut self) {
        if self.device_restored {
            return;
        }
        let Some(id) = self.prefs.device.clone().map(DeviceId) else {
            return;
        };
        if !self.devices.iter().any(|d| d.id == id) {
            return;
        }
        self.device_restored = true;
        if self.selected_device.as_ref() != Some(&id) {
            self.send(PlayerCommand::SelectDevice { id });
            self.refresh_devices();
        }
    }

    fn select_device(&mut self, id: DeviceId) {
        if !self.devices.iter().any(|d| d.id == id) {
            return;
        }
        self.prefs.device = Some(id.0.clone());
        self.save_prefs();
        self.send(PlayerCommand::SelectDevice { id });
        self.refresh_devices();
    }

    /// The selected device's display name.
    fn selected_name<'a>(
        devices: &'a [DeviceInfo],
        selected: Option<&DeviceId>,
    ) -> Option<&'a str> {
        let id = selected?;
        let device = devices.iter().find(|d| &d.id == id)?;
        Some(&device.name)
    }

    fn bar(&mut self, ui: &mut Ui) {
        let album = self.playing_track_album();
        let mini = self.spectrum.mini();
        let device =
            Self::selected_name(&self.devices, self.selected_device.as_ref()).map(str::to_owned);
        let now = self.clock.now();
        let data = BarData {
            np: &self.now_playing,
            now,
            album: album.as_ref(),
            device: device.as_deref(),
            spectrum: self.now_playing.index.map(|_| mini.as_slice()),
            notice: self
                .notice
                .as_ref()
                .filter(|n| n.until > now)
                .map(|n| (n.text.as_str(), n.undo)),
        };
        let out = bar::show(ui, &data, &mut self.art);
        self.bar_controls = out.controls;
        match out.action {
            Some(BarAction::PlayPause) => self.play_pause(),
            Some(BarAction::Previous) => self.transport(PlayerCommand::Previous),
            Some(BarAction::Next) => self.transport(PlayerCommand::Next),
            Some(BarAction::Seek(position)) => self.transport(PlayerCommand::Seek { position }),
            Some(BarAction::OpenNowPlaying) => {
                if self.now_playing.index.is_some() {
                    self.now_playing_open = true;
                }
            }
            Some(BarAction::Retry) => {
                if let Some(cmd) = self.now_playing.retry_command() {
                    self.send(cmd);
                }
            }
            Some(BarAction::Undo) => self.undo(),
            None => {}
        }

        if out.device_button.clicked() {
            self.popover_open = !self.popover_open;
            // The engine relists on hot-plug notifications; listing again here covers a
            // backend that could not register for them.
            if self.popover_open {
                self.send(PlayerCommand::RefreshDevices);
            }
        }
        let mut action = None;
        Popup::from_response(&out.device_button)
            .open_bool(&mut self.popover_open)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .align(RectAlign::TOP_END)
            .gap(8.0)
            .frame(
                Frame::new()
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(Margin::same(6)),
            )
            .show(|ui| {
                action = device_list::show(ui, &self.devices, self.selected_device.as_ref());
            });
        match action {
            Some(DeviceListAction::Select(id)) => {
                self.popover_open = false;
                self.select_device(id);
            }
            Some(DeviceListAction::Capabilities) => {
                self.popover_open = false;
                self.open(Page::Settings);
            }
            None => {}
        }
    }

    /// Picks up library changes: a changed scan status or revision means the index
    /// may have changed, and so does a change made from the UI once no scan is running.
    fn poll_library(&mut self) {
        let status = self.library.scan_status();
        let revision = self.library.revision();
        let scanning = matches!(status, ScanStatus::Scanning { .. });
        if status != self.scan_status
            || revision != self.library_revision
            || (self.library_changed && !scanning)
        {
            // A scan's progress ticks leave the rows as they are; a finished scan, a new
            // revision or a change made from the UI may have changed the list.
            let mut changed = revision != self.library_revision
                || (self.library_changed && !scanning)
                || (matches!(self.scan_status, ScanStatus::Scanning { .. }) && !scanning);
            self.library_revision = revision;
            if !scanning {
                self.library_changed = false;
            }
            if matches!(status, ScanStatus::Done { .. }) {
                self.art.forget_missing();
                self.art
                    .prune(self.library.albums(None).iter().map(|a| a.id).collect());
            }
            self.scan_status = status;
            self.nav.validate(&*self.library);
            self.grid_albums = None;
            self.artists = None;
            self.search = None;
            self.playlist = None;
            // Reload the open page in place; if its album vanished, `validate` went
            // to the grid, which releases the page.
            if let Some((album, tracks)) = &mut self.page
                && let Some(fresh) = self.library.album(album.id)
            {
                let reloaded = self.library.tracks(fresh.id);
                let same = |a: &Track, b: &Track| a.id == b.id && a.path == b.path;
                if reloaded.len() != tracks.len()
                    || !reloaded.iter().zip(tracks.iter()).all(|(a, b)| same(a, b))
                {
                    changed = true;
                }
                *tracks = reloaded;
                *album = fresh;
            }
            if changed {
                self.selection.clear();
            }
        }
    }

    fn central(&mut self, ui: &mut Ui) {
        if self.library.folders().is_empty() {
            if albums::empty(ui)
                && let Some(folder) = (self.pick_folder)()
            {
                // Adding a folder starts a scan in the library.
                self.library.add_folder(folder);
                self.library_changed = true;
            }
            return;
        }
        let page = self.nav.current().clone();
        match page {
            Page::Home | Page::Albums | Page::Artist(_) => {}
            Page::Album { album } => return self.album_page(ui, album),
            Page::Settings => return self.settings_page(ui),
            Page::Artists => return self.artists_page(ui),
            Page::Search(query) => return self.search_page(ui, &query),
            Page::Playlists => return self.playlists_page(ui),
            Page::Playlist(name) => return self.playlist_page(ui, &name),
            Page::Plugin(id) => return self.plugin_page(ui, &id),
        }
        if self.page.take().is_some() {
            self.art.release_page();
        }
        if self.grid_albums.as_ref().is_none_or(|(p, _)| *p != page) {
            let albums = match page {
                Page::Home => self.library.recently_added(RECENT_ALBUMS),
                Page::Artist(a) => self.library.albums(Some(a)),
                _ => self.library.albums(None),
            };
            self.grid_albums = Some((page.clone(), albums));
        }
        let n = self.grid_albums.as_ref().map_or(0, |(_, a)| a.len());
        let title = match page {
            Page::Home => "Recently added".to_owned(),
            Page::Artist(a) => self
                .artist_tiles()
                .iter()
                .find(|t| t.id == a)
                .map_or_else(String::new, |t| t.name.clone()),
            _ => "Albums".to_owned(),
        };
        let sub = (page != Page::Home).then(|| count(n, "album", "albums"));
        heading(ui, &title, sub.as_deref());
        ui.add_space(24.0);
        let restore = self.restore_scroll.take();
        let device_ready = self.device_ready();
        self.playlist_names();
        let menu = TileMenu {
            playlists: self.playlist_names.as_deref().unwrap_or_default(),
            device_ready,
        };
        let albums = self.grid_albums.as_ref().map_or(&[][..], |(_, a)| a);
        let (reported, scroll) = albums::show(ui, albums, &mut self.art, restore, menu);
        self.grid_scroll = scroll;
        match reported {
            Some(TileAction::Open(id)) => self.open(Page::Album { album: id }),
            Some(TileAction::Menu(id, action)) => {
                self.menu(Origin::AlbumTile(id), vec![0], action);
            }
            None => {}
        }
    }

    fn artist_tiles(&mut self) -> &[ArtistTile] {
        self.artists
            .get_or_insert_with(|| artist_tiles(&*self.library))
    }

    fn now_playing_view(&mut self, ui: &mut Ui) {
        let levels = *self.spectrum.levels();
        let device =
            Self::selected_name(&self.devices, self.selected_device.as_ref()).map(str::to_owned);
        let album = self.playing_track_album();
        let paths: Vec<PathBuf> = self
            .now_playing
            .queue
            .iter()
            .map(|i| i.path.clone())
            .collect();
        let row_albums: Vec<Option<Album>> = paths.iter().map(|p| self.album_of(p)).collect();
        let device_ready = self.device_ready();
        self.playlist_names();
        let now = self.clock.now();
        let data = NowPlayingData {
            notice: self
                .notice
                .as_ref()
                .filter(|n| n.until > now)
                .map(|n| (n.text.as_str(), n.undo)),
            selection: &self.selection,
            playlists: self.playlist_names.as_deref().unwrap_or_default(),
            device_ready,
            np: &self.now_playing,
            album: album.as_ref(),
            queue_album: self.playing_album.as_ref(),
            row_albums: &row_albums,
            device: device.as_deref(),
            spectrum: self.now_playing.index.map(|_| levels.as_slice()),
        };
        match now_playing_view::show(ui, &data, &mut self.art) {
            Some(NowPlayingAction::Close) => self.close_now_playing(),
            Some(NowPlayingAction::PlayPause) => self.play_pause(),
            Some(NowPlayingAction::Previous) => self.transport(PlayerCommand::Previous),
            Some(NowPlayingAction::Next) => self.transport(PlayerCommand::Next),
            Some(NowPlayingAction::Seek(position)) => {
                self.transport(PlayerCommand::Seek { position })
            }
            Some(NowPlayingAction::Row(action)) => self.row_action(Origin::Queue, action),
            Some(NowPlayingAction::Undo) => self.undo(),
            Some(NowPlayingAction::ClearUpcoming) => self.edit_queue(GuiEdit::Clear),
            Some(NowPlayingAction::Retry) => {
                if let Some(cmd) = self.now_playing.retry_command() {
                    self.send(cmd);
                }
            }
            Some(NowPlayingAction::SaveQueue) => {
                self.dialog = Some(Dialog::Save {
                    dialog: NameDialog::new("Save queue as playlist", "Save", "", true),
                    rows: None,
                });
            }
            None => {}
        }
    }

    fn playlists_page(&mut self, ui: &mut Ui) {
        if self.playlist_names.is_none() {
            self.playlist_names = Some(playlists::list(&self.playlists_dir).unwrap_or_default());
        }
        let names = self.playlist_names.as_deref().unwrap_or_default();
        match playlists_view::list_page(ui, names) {
            Some(PlaylistsAction::Open(name)) => self.open(Page::Playlist(name)),
            Some(PlaylistsAction::New) => {
                self.dialog = Some(Dialog::NewPlaylist {
                    dialog: NameDialog::new("New playlist", "Create", "", false),
                    add: None,
                });
            }
            None => {}
        }
    }

    fn playlist_page(&mut self, ui: &mut Ui, name: &str) {
        if self.playlist.as_ref().is_none_or(|p| p.name != name) {
            let loaded = self.load_playlist(name);
            self.playlist = Some(OpenPlaylist {
                name: name.to_owned(),
                loaded,
            });
        }
        let device_ready = self.device_ready();
        self.playlist_names();
        let np = &self.now_playing;
        let playing = np
            .index
            .and_then(|i| np.queue.get(i))
            .map(|item| item.path.as_path());
        let Some(open) = &self.playlist else {
            return;
        };
        let list = ListId::Playlist(name.to_owned());
        let rows = match &open.loaded {
            Ok(l) => Ok(PageRows {
                rows: &l.rows,
                file_len: l.file.len(),
                select: RowSelect {
                    list: &list,
                    shown: &l.shown,
                    selection: &self.selection,
                    menu: MenuSpec {
                        place: MenuPlace::PlaylistRow,
                        count: 0,
                        playlists: self.playlist_names.as_deref().unwrap_or_default(),
                        exclude: Some(name),
                        device_ready,
                    },
                    drag: true,
                },
            }),
            Err(e) => Err(e.as_str()),
        };
        let playing_now = self.playing_playlist.as_deref() == Some(name);
        let action = playlists_view::playlist_page(ui, name, rows, playing, playing_now);
        let rows = match &self.playlist {
            Some(OpenPlaylist { loaded: Ok(l), .. }) if action.is_some() => l.rows.clone(),
            _ => Vec::new(),
        };
        match action {
            Some(PlaylistAction::Play) => self.play_playlist_rows(name, &rows, 0),
            Some(PlaylistAction::PlayFrom(i)) => self.play_playlist_rows(name, &rows, i),
            Some(PlaylistAction::Queue) => self.queue_rows(&rows),
            Some(PlaylistAction::Row(a)) => self.row_action(Origin::Playlist(name.to_owned()), a),
            Some(PlaylistAction::Rename) => {
                self.dialog = Some(Dialog::Rename {
                    from: name.to_owned(),
                    dialog: NameDialog::new("Rename playlist", "Rename", name, false),
                });
            }
            Some(PlaylistAction::Delete) => {
                self.dialog = Some(Dialog::Delete {
                    name: name.to_owned(),
                    error: None,
                });
            }
            None => {}
        }
    }

    fn plugin_page(&mut self, ui: &mut Ui, id: &str) {
        let paths = self
            .panels
            .get(id)
            .map(Panel::track_paths)
            .unwrap_or_default();
        let mut albums = HashMap::new();
        for path in paths {
            if let Some(album) = self.album_of(&path) {
                albums.insert(path, album);
            }
        }
        let title = self.plugin_label(id);
        let np = &self.now_playing;
        let playing = np
            .index
            .and_then(|i| np.queue.get(i))
            .map(|item| item.path.as_path());
        let (Some(panel), Some(state)) = (self.panels.get(id), self.panel_states.get_mut(id))
        else {
            return;
        };
        match panel_view::show(ui, panel, state, playing, &albums, &mut self.art, &title) {
            Some(PanelAction::Event { id: element, value }) => {
                let params = panel_view::event_params(element, value);
                self.send_panel_event(id, Place::Tab, params);
            }
            Some(PanelAction::Tracks(action, rows)) => match action {
                TrackListAction::PlayAll => {
                    self.play_rows(&rows, 0);
                }
                TrackListAction::PlayFrom(i) => {
                    self.play_rows(&rows, i);
                }
                TrackListAction::QueueAll => self.queue_rows(&rows),
                // Panel lists have no selection or menu.
                TrackListAction::Row(_) => {}
                TrackListAction::SaveAs => {
                    self.dialog = Some(Dialog::Save {
                        dialog: NameDialog::new("Save as playlist", "Save", "", true),
                        rows: Some(rows),
                    });
                }
            },
            None => {}
        }
    }

    /// Shows the open modal and applies its answer.
    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let dir = self.playlists_dir.clone();
        let keep = match &mut dialog {
            Dialog::Save { dialog: d, rows } => match d.show(ctx, |n| playlists::exists(&dir, n)) {
                Some(DialogEvent::Cancel) => false,
                Some(DialogEvent::Submit(name)) => {
                    let entries = match rows {
                        Some(rows) => self.rows_entries(rows),
                        None => self.queue_entries(),
                    };
                    match playlists::save(&dir, &name, &entries) {
                        Ok(_) => {
                            self.forget_playlists();
                            false
                        }
                        Err(e) => {
                            d.error = Some(e.to_string());
                            true
                        }
                    }
                }
                None => true,
            },
            Dialog::Rename { from, dialog: d } => match d.show(ctx, |_| false) {
                Some(DialogEvent::Cancel) => false,
                Some(DialogEvent::Submit(name)) => match playlists::rename(&dir, from, &name) {
                    Ok(new) => {
                        self.forget_playlists();
                        self.drop_undo();
                        self.nav.rename_playlist(from, &new);
                        if self.playing_playlist.as_deref() == Some(from.as_str()) {
                            self.playing_playlist = Some(new);
                        }
                        false
                    }
                    Err(e) => {
                        d.error = Some(e.to_string());
                        true
                    }
                },
                None => true,
            },
            Dialog::NewPlaylist { dialog: d, add } => {
                match d.show(ctx, |n| playlists::exists(&dir, n)) {
                    Some(DialogEvent::Cancel) => false,
                    Some(DialogEvent::Submit(name)) => match playlists::create(&dir, &name) {
                        Ok(new) => {
                            self.forget_playlists();
                            match add.take() {
                                Some(entries) => self.add_to_playlist(&new, entries),
                                None => self.open(Page::Playlist(new)),
                            }
                            false
                        }
                        Err(e) => {
                            d.error = Some(e.to_string());
                            true
                        }
                    },
                    None => true,
                }
            }
            Dialog::Delete { name, error } => {
                match playlists_view::delete_dialog(ctx, name, error.as_deref()) {
                    Some(true) => match playlists::delete(&dir, name) {
                        Ok(()) => {
                            self.forget_playlists();
                            self.drop_undo();
                            self.nav.drop_playlist(name);
                            if self.playing_playlist.as_deref() == Some(name.as_str()) {
                                self.playing_playlist = None;
                            }
                            false
                        }
                        Err(e) => {
                            *error = Some(e.to_string());
                            true
                        }
                    },
                    Some(false) => false,
                    None => true,
                }
            }
        };
        if keep {
            self.dialog = Some(dialog);
        }
    }

    fn settings_page(&mut self, ui: &mut Ui) {
        let table = self.selected_caps.as_ref().map(cap_table);
        let logs_dir = self.home.logs_dir();
        let data = SettingsData {
            devices: &self.devices,
            selected: self.selected_device.as_ref(),
            table: table.as_ref(),
            home: &self.home.root,
            home_note: self.home.fallback_reason.as_deref(),
            logs_dir: &logs_dir,
            plugins: &self.plugin_infos,
            plugin_error: self
                .plugin_error
                .as_ref()
                .map(|(folder, message)| (folder.as_str(), message.as_str())),
            plugin_panels: &self.settings_panels,
        };
        let out = settings::show(ui, &data, self.library.as_ref(), &mut *self.pick_folder);
        self.library_changed |= out.library_changed;
        match out.action {
            Some(SettingsAction::SelectDevice(id)) => self.select_device(id),
            Some(SettingsAction::Reprobe(id)) => self.send(PlayerCommand::Reprobe { id }),
            Some(SettingsAction::SetDop { id, enabled }) => {
                self.send(PlayerCommand::SetDop { id, enabled });
            }
            Some(SettingsAction::PanelEvent { folder, params }) => {
                self.send_panel_event(&folder, Place::Settings, params);
            }
            Some(other) => self.plugin_action(other),
            None => {}
        }
    }

    fn search_page(&mut self, ui: &mut Ui, query: &str) {
        if self.search.as_ref().is_none_or(|r| r.query != query) {
            self.artist_tiles();
            let tiles = self.artists.as_deref().unwrap_or_default();
            self.search = Some(search(&*self.library, tiles, query));
        }
        let device_ready = self.device_ready();
        self.playlist_names();
        let Some(results) = &self.search else {
            return;
        };
        let rows = TrackRows {
            selection: &self.selection,
            playlists: self.playlist_names.as_deref().unwrap_or_default(),
            device_ready,
        };
        match search_view::show(ui, results, &mut self.art, &rows) {
            Some(SearchAction::Artist(id)) => self.open(Page::Artist(id)),
            Some(SearchAction::Album(album)) => self.open(Page::Album { album }),
            Some(SearchAction::AlbumMenu(id, action)) => {
                self.menu(Origin::AlbumTile(id), vec![0], action);
            }
            Some(SearchAction::Row(action)) => self.row_action(Origin::Search, action),
            None => {}
        }
    }

    fn artists_page(&mut self, ui: &mut Ui) {
        let n = self.artist_tiles().len();
        heading(ui, "Artists", Some(&count(n, "artist", "artists")));
        ui.add_space(24.0);
        let restore = self.restore_scroll.take();
        let tiles = self.artists.as_deref().unwrap_or_default();
        let (open, scroll) = artists::show(ui, tiles, &mut self.art, restore);
        self.grid_scroll = scroll;
        if let Some(id) = open {
            self.open(Page::Artist(id));
        }
    }

    fn album_page(&mut self, ui: &mut Ui, id: AlbumId) {
        if self.page.as_ref().is_none_or(|(a, _)| a.id != id) {
            let Some(album) = self.library.album(id) else {
                return;
            };
            self.page = Some((album, self.library.tracks(id)));
        }
        let reveal = self.reveal.take();
        let device_ready = self.device_ready();
        self.playlist_names();
        let Some((album, tracks)) = &self.page else {
            return;
        };
        let np = &self.now_playing;
        let playing = np
            .index
            .and_then(|i| np.queue.get(i))
            .map(|item| item.path.as_path());
        let data = PageData {
            album,
            tracks,
            selection: &self.selection,
            reveal,
            playlists: self.playlist_names.as_deref().unwrap_or_default(),
            device_ready,
            playing,
            device: Self::selected_name(&self.devices, self.selected_device.as_ref())
                .zip(self.selected_caps.as_ref()),
            track_errors: &np.track_errors,
        };
        let action = album_page::show(ui, &data, &mut self.art);
        match action {
            Some(PageAction::OpenArtist(artist)) => self.open(Page::Artist(artist)),
            Some(PageAction::Row(action)) => self.row_action(Origin::Album(id), action),
            Some(PageAction::Play(start)) => {
                let (album, tracks) = self.page.clone().expect("page is loaded");
                self.play_album(album, &tracks, start);
            }
            None => {}
        }
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        // Unless the user is typing in it, the field shows the current page's query.
        if !ui.ctx().memory(|m| m.has_focus(top_bar::search_id())) {
            match self.nav.current() {
                Page::Search(q) if *q != self.query => self.query = q.clone(),
                Page::Search(_) => {}
                _ => self.query.clear(),
            }
        }
        let data = TopBarData {
            tab: self.nav.tab(),
            plugins: self
                .panels
                .keys()
                .map(|id| (id.clone(), self.plugin_label(id)))
                .collect(),
            can_back: self.nav.can_back(),
            can_forward: self.nav.can_forward(),
            settings_open: *self.nav.current() == Page::Settings,
        };
        let rect = ui.max_rect();
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, theme::BORDER),
        );
        match top_bar::show(ui, &data, &mut self.query) {
            Some(TopBarAction::Back) => self.back(),
            Some(TopBarAction::Forward) => self.forward(),
            Some(TopBarAction::Tab(Tab::Home)) => self.open(Page::Home),
            Some(TopBarAction::Tab(Tab::Albums)) => self.open(Page::Albums),
            Some(TopBarAction::Tab(Tab::Artists)) => self.open(Page::Artists),
            Some(TopBarAction::Tab(Tab::Playlists)) => self.open(Page::Playlists),
            Some(TopBarAction::Tab(Tab::Plugin(id))) => self.open(Page::Plugin(id)),
            Some(TopBarAction::QueryChanged) => {
                let query = self.query.trim().to_owned();
                self.nav.search(&query, self.page_scroll());
                self.restore_scroll = Some(self.nav.scroll());
            }
            Some(TopBarAction::Settings) => self.open(Page::Settings),
            None => {}
        }
    }

    fn save_prefs(&self) {
        if let Some(path) = &self.prefs_path {
            let _ = self.prefs.save(path);
        }
    }
}

impl eframe::App for BpApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(host) = self.plugins.take() {
            host.shutdown_all();
        }
    }
}

/// The queue items for the playable rows of a playlist. Rows whose file is missing are
/// skipped. A file the library does not know still plays, titled by its file name.
fn rows_to_items(rows: &[TrackRow], catalog: &Catalog) -> Vec<QueueItem> {
    rows.iter()
        .filter(|r| !r.missing)
        .map(|r| match catalog.get(&r.path) {
            Some(t) => t.item.clone(),
            None => unlisted_item(r),
        })
        .collect()
}

/// The queue items for an album's tracks, in the order given (disc, then track).
fn album_items(tracks: &[Track]) -> Vec<QueueItem> {
    tracks
        .iter()
        .map(|t| QueueItem {
            path: t.path.clone(),
            source: t.source,
            duration: t.duration,
            title: t.title.clone(),
        })
        .collect()
}

/// A queue item for a playlist row outside the library; its format is read from the
/// file, and its length falls back to the playlist's.
fn unlisted_item(row: &TrackRow) -> QueueItem {
    let mut item = unlisted_path_item(&row.path);
    if item.duration.is_zero()
        && let Some(s) = row.duration_s
    {
        item.duration = Duration::from_secs_f64(s);
    }
    item
}

/// A queue item for a file outside the library, titled by its file name; its format
/// and length are read from the file.
fn unlisted_path_item(path: &Path) -> QueueItem {
    let probed = bp_decode::open(path).ok();
    let source = probed.as_ref().map_or(
        bp_core::SourceFormat {
            codec: bp_core::Codec::Flac,
            sample_rate: 44_100,
            channels: 2,
            kind: bp_core::SourceKind::Pcm {
                bits: 16,
                float: false,
            },
        },
        |s| s.format(),
    );
    let duration = probed.and_then(|s| s.duration()).unwrap_or(Duration::ZERO);
    QueueItem {
        path: path.to_path_buf(),
        source,
        duration,
        title: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

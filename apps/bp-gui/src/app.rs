//! Top-level app: owns the state, drains player events, lays out the panels and
//! applies the views' actions.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bp_core::{DeviceId, DeviceInfo};
use egui::{Frame, Key, Margin, Modifiers, Popup, PopupCloseBehavior, RectAlign, Stroke, Ui};

use crate::art::ArtLoader;
use crate::backend::mock_player::Clock;
use crate::backend::{
    Album, AlbumId, DeviceCaps, LibrarySource, PlayerCommand, PlayerEvent, PlayerHandle, QueueItem,
    ScanStatus, Track, TrackId,
};
use crate::prefs::Prefs;
use crate::state::artists::{ArtistTile, artist_tiles};
use crate::state::caps_table::cap_table;
use crate::state::nav::{Nav, Page, Tab};
use crate::state::now_playing::{Badge, NowPlaying};
use crate::state::search::{SearchResults, search};
use crate::state::spectrum::{FFT_SIZE, Spectrum};
use crate::theme;
use crate::views::album_page::{self, PageAction, PageData};
use crate::views::bar::{self, BarAction, BarData};
use crate::views::device_list::{self, DeviceListAction};
use crate::views::now_playing::{self as now_playing_view, NowPlayingAction, NowPlayingData};
use crate::views::search::{self as search_view, SearchAction};
use crate::views::settings::{self, SettingsAction, SettingsData};
use crate::views::top_bar::{self, TopBarAction, TopBarData};
use crate::views::{albums, artists, count, heading};

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
    pub pick_folder: Box<dyn FnMut() -> Option<PathBuf>>,
}

pub struct BpApp {
    player: Box<dyn PlayerHandle>,
    library: Arc<dyn LibrarySource>,
    clock: Arc<dyn Clock>,
    prefs_path: Option<PathBuf>,
    pick_folder: Box<dyn FnMut() -> Option<PathBuf>>,
    art: ArtLoader,
    prefs: Prefs,
    nav: Nav,
    now_playing: NowPlaying,
    query: String,
    scan_status: ScanStatus,
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
    /// The album the play queue was started from.
    playing_album: Option<Album>,
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
}

impl BpApp {
    pub fn new(ctx: &egui::Context, deps: Deps) -> Self {
        theme::apply(ctx);
        let waker = {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        };
        deps.player.set_waker(Box::new(waker.clone()));
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
        let mut app = BpApp {
            player: deps.player,
            library: deps.library,
            clock: deps.clock,
            prefs_path: deps.prefs_path,
            pick_folder: deps.pick_folder,
            art,
            prefs,
            nav: Nav::default(),
            now_playing: NowPlaying::default(),
            query: String::new(),
            scan_status,
            library_changed: false,
            grid_albums: None,
            grid_scroll: 0.0,
            restore_scroll: None,
            page: None,
            search: None,
            artists: None,
            playing_album: None,
            devices: Vec::new(),
            selected_device: None,
            selected_caps: None,
            device_restored: false,
            popover_open: false,
            now_playing_open: false,
            spectrum: Spectrum::new(),
            typing: false,
            bar_controls: Vec::new(),
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
        self.now_playing.on_command(&cmd);
        self.player.send(cmd);
    }

    /// The whole frame.
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        while let Some(ev) = self.player.try_recv() {
            if matches!(ev, PlayerEvent::DevicesChanged) {
                self.refresh_devices();
                self.restore_device();
            }
            self.now_playing.apply(&ev, self.clock.now());
        }
        self.poll_library();
        self.keyboard(&ctx);
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
            self.now_playing_open = false;
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
        crate::views::window_controls::resize_edges(&ctx, ctx.content_rect());

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
    }

    /// Shortcuts. `Ctrl+F` works while typing; the rest only when no text field has
    /// focus. Keys are consumed so focused widgets don't also see them.
    fn keyboard(&mut self, ctx: &egui::Context) {
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
        // Arrows belong to a focused widget (slider, focus moves), except bar controls.
        let focused = ctx.memory(|m| m.focused());
        let arrows_seek = focused.is_none_or(|id| self.bar_controls.contains(&id));
        let (space, prev, next, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Space),
                i.consume_key(Modifiers::COMMAND, Key::ArrowLeft),
                i.consume_key(Modifiers::COMMAND, Key::ArrowRight),
                i.consume_key(Modifiers::NONE, Key::Escape)
                    | i.consume_key(Modifiers::NONE, Key::Backspace),
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
        if esc {
            if self.popover_open {
                self.popover_open = false;
            } else if self.now_playing_open {
                self.now_playing_open = false;
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
        } else if let Page::Album { highlight, .. } = *self.nav.current()
            && let Some((album, tracks)) = self.page.clone()
        {
            let start = highlight
                .and_then(|h| tracks.iter().position(|t| t.id == h))
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
        let items = tracks
            .iter()
            .map(|t| QueueItem {
                path: t.path.clone(),
                source: t.source,
                duration: t.duration,
                title: t.title.clone(),
            })
            .collect();
        self.playing_album = Some(album);
        self.send(PlayerCommand::Play { items, start });
    }

    fn back(&mut self) {
        if self.nav.back(self.page_scroll()) {
            self.restore_scroll = Some(self.nav.scroll());
        }
    }

    fn forward(&mut self) {
        if self.nav.forward(self.page_scroll()) {
            self.restore_scroll = Some(self.nav.scroll());
        }
    }

    /// Opens `page`, remembering the current page's scroll.
    fn open(&mut self, page: Page) {
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
        let mini = self.spectrum.mini();
        let device =
            Self::selected_name(&self.devices, self.selected_device.as_ref()).map(str::to_owned);
        let data = BarData {
            np: &self.now_playing,
            now: self.clock.now(),
            album: self.playing_album.as_ref(),
            device: device.as_deref(),
            spectrum: self.now_playing.index.map(|_| mini.as_slice()),
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

    /// Picks up library changes: a changed scan status means the index may have
    /// changed, and so does a change made from the UI once no scan is running.
    fn poll_library(&mut self) {
        let status = self.library.scan_status();
        let scanning = matches!(status, ScanStatus::Scanning { .. });
        if status != self.scan_status || (self.library_changed && !scanning) {
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
            // Reload the open page in place; if its album vanished, `validate` went
            // to the grid, which releases the page.
            if let Some((album, tracks)) = &mut self.page
                && let Some(fresh) = self.library.album(album.id)
            {
                *tracks = self.library.tracks(fresh.id);
                *album = fresh;
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
            Page::Album { album, highlight } => return self.album_page(ui, album, highlight),
            Page::Settings => return self.settings_page(ui),
            Page::Artists => return self.artists_page(ui),
            Page::Search(query) => return self.search_page(ui, &query),
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
        let albums = self.grid_albums.as_ref().map_or(&[][..], |(_, a)| a);
        let (open, scroll) = albums::show(ui, albums, &mut self.art, restore);
        self.grid_scroll = scroll;
        if let Some(id) = open {
            self.open(Page::Album {
                album: id,
                highlight: None,
            });
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
        let album = self.playing_album.clone();
        let data = NowPlayingData {
            np: &self.now_playing,
            album: album.as_ref(),
            device: device.as_deref(),
            spectrum: self.now_playing.index.map(|_| levels.as_slice()),
        };
        match now_playing_view::show(ui, &data, &mut self.art) {
            Some(NowPlayingAction::Close) => self.now_playing_open = false,
            Some(NowPlayingAction::PlayPause) => self.play_pause(),
            Some(NowPlayingAction::Previous) => self.transport(PlayerCommand::Previous),
            Some(NowPlayingAction::Next) => self.transport(PlayerCommand::Next),
            Some(NowPlayingAction::Seek(position)) => {
                self.transport(PlayerCommand::Seek { position })
            }
            Some(NowPlayingAction::PlayIndex(start)) => {
                let items = self.now_playing.queue.clone();
                self.send(PlayerCommand::Play { items, start });
            }
            Some(NowPlayingAction::Retry) => {
                if let Some(cmd) = self.now_playing.retry_command() {
                    self.send(cmd);
                }
            }
            None => {}
        }
    }

    fn settings_page(&mut self, ui: &mut Ui) {
        let table = self.selected_caps.as_ref().map(cap_table);
        let data = SettingsData {
            devices: &self.devices,
            selected: self.selected_device.as_ref(),
            table: table.as_ref(),
        };
        let out = settings::show(ui, &data, self.library.as_ref(), &mut *self.pick_folder);
        self.library_changed |= out.library_changed;
        match out.action {
            Some(SettingsAction::SelectDevice(id)) => self.select_device(id),
            Some(SettingsAction::Reprobe(id)) => self.send(PlayerCommand::Reprobe { id }),
            None => {}
        }
    }

    fn search_page(&mut self, ui: &mut Ui, query: &str) {
        if self.search.as_ref().is_none_or(|r| r.query != query) {
            self.artist_tiles();
            let tiles = self.artists.as_deref().unwrap_or_default();
            self.search = Some(search(&*self.library, tiles, query));
        }
        let Some(results) = &self.search else {
            return;
        };
        match search_view::show(ui, results, &mut self.art) {
            Some(SearchAction::Artist(id)) => self.open(Page::Artist(id)),
            Some(SearchAction::Album(album)) => self.open(Page::Album {
                album,
                highlight: None,
            }),
            Some(SearchAction::OpenTrack { album, track }) => self.open(Page::Album {
                album,
                highlight: Some(track),
            }),
            Some(SearchAction::PlayTrack { album, track }) => {
                if let Some(a) = self.library.album(album) {
                    let tracks = self.library.tracks(album);
                    let start = tracks.iter().position(|t| t.id == track).unwrap_or(0);
                    self.play_album(a, &tracks, start);
                }
            }
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

    fn album_page(&mut self, ui: &mut Ui, id: AlbumId, highlight: Option<TrackId>) {
        if self.page.as_ref().is_none_or(|(a, _)| a.id != id) {
            let Some(album) = self.library.album(id) else {
                return;
            };
            self.page = Some((album, self.library.tracks(id)));
        }
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
            highlight,
            playing,
            device: Self::selected_name(&self.devices, self.selected_device.as_ref())
                .zip(self.selected_caps.as_ref()),
            track_errors: &np.track_errors,
        };
        let action = album_page::show(ui, &data, &mut self.art);
        match action {
            Some(PageAction::OpenArtist(artist)) => self.open(Page::Artist(artist)),
            Some(PageAction::Select(track)) => self.nav.replace(Page::Album {
                album: id,
                highlight: Some(track),
            }),
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
}

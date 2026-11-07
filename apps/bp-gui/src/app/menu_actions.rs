//! What the clicks and menu choices on track and album rows do: selection, Play,
//! Play next, Add to queue, Add to playlist, Remove from playlist or queue, drag to reorder
//! and Go to.

use crate::backend::{AlbumId, PlayerCommand, QueueItem};
use crate::catalog::path_key;
use crate::state::nav::Page;
use crate::state::selection::ListId;
use crate::views::context_menu::{MenuAction, MenuTarget, RowAction};

use crate::playlists::Entry;
use crate::views::name_dialog::NameDialog;

use super::{BpApp, Dialog, GuiEdit, album_items, unlisted_path_item};

/// The list a clicked row belongs to.
pub(super) enum Origin {
    /// The open album page's tracks; rows are track indices.
    Album(AlbumId),
    /// The search page's tracks; rows index the results' tracks.
    Search,
    /// An album tile: the menu acts on the whole album.
    AlbumTile(AlbumId),
    /// The open playlist's rows; rows are file indices.
    Playlist(String),
    /// The play queue; rows are queue indices.
    Queue,
}

impl BpApp {
    /// Applies what a row reported.
    pub(super) fn row_action(&mut self, origin: Origin, action: RowAction) {
        match action {
            RowAction::Select(row, how) => {
                if let Some((list, shown)) = self.list(&origin) {
                    self.selection.click(&list, &shown, row, how);
                }
            }
            RowAction::Secondary(row) => {
                if let Some((list, _)) = self.list(&origin) {
                    self.selection.secondary(&list, row);
                }
            }
            RowAction::Activate(row) => match origin {
                Origin::Album(_) => self.play_page_from(row),
                Origin::Search => self.play_search_track(row),
                Origin::Queue => self.play_queue_from(row),
                Origin::Playlist(name) => self.play_playlist_from(&name, row),
                // A tile opens its album on a click; it has no row to activate.
                Origin::AlbumTile(_) => {}
            },
            RowAction::Menu(rows, action) => self.menu(origin, rows, action),
            RowAction::Drop { rows, gap } => match &origin {
                Origin::Playlist(name) => self.move_rows(name, &rows, gap),
                Origin::Queue => self.edit_queue(GuiEdit::Move { rows, gap }),
                _ => {}
            },
        }
    }

    /// Applies a menu choice made on `rows` of `origin`.
    pub(super) fn menu(&mut self, origin: Origin, rows: Vec<usize>, action: MenuAction) {
        match action {
            MenuAction::Play => self.menu_play(&origin, &rows),
            // In the queue, Play next moves the rows rather than adding them again.
            MenuAction::PlayNext if matches!(origin, Origin::Queue) => {
                self.edit_queue(GuiEdit::PlayNext(rows));
            }
            MenuAction::PlayNext => {
                let items = self.items_of(&origin, &rows);
                if !items.is_empty() {
                    self.send(PlayerCommand::Insert { items });
                }
            }
            MenuAction::Queue => {
                let items = self.items_of(&origin, &rows);
                if !items.is_empty() {
                    self.send(PlayerCommand::Append { items });
                }
            }
            MenuAction::GoToAlbum => self.go_to_album(&origin, &rows),
            MenuAction::GoToArtist => self.go_to_artist(&origin, &rows),
            MenuAction::AddTo(name) => {
                let entries = self.entries_of(&origin, &rows);
                if !entries.is_empty() {
                    self.add_to_playlist(&name, entries);
                }
            }
            MenuAction::AddToNew => {
                let entries = self.entries_of(&origin, &rows);
                if !entries.is_empty() {
                    self.dialog = Some(Dialog::NewPlaylist {
                        dialog: NameDialog::new("New playlist", "Create", "", false),
                        add: Some(entries),
                    });
                }
            }
            MenuAction::Remove => match &origin {
                Origin::Playlist(name) => self.remove_rows(name, &rows),
                Origin::Queue => self.edit_queue(GuiEdit::Remove(rows)),
                _ => {}
            },
        }
    }

    /// Ctrl+A: every row of `origin`'s list.
    pub(super) fn select_all(&mut self, origin: &Origin) {
        if let Some((list, shown)) = self.list(origin) {
            self.selection.select_all(&list, &shown);
        }
    }

    /// `origin`'s list and its row ids in display order; `None` for an album tile.
    fn list(&self, origin: &Origin) -> Option<(ListId, Vec<usize>)> {
        let (list, n) = match origin {
            Origin::Album(id) => (
                ListId::Album(*id),
                self.page
                    .as_ref()
                    .filter(|(a, _)| a.id == *id)
                    .map_or(0, |(_, tracks)| tracks.len()),
            ),
            Origin::Search => {
                let r = self.search.as_ref()?;
                (ListId::SearchTracks(r.query.clone()), r.tracks.len())
            }
            Origin::Playlist(name) => {
                let shown = match &self.playlist {
                    Some(p) if p.name == *name => p
                        .loaded
                        .as_ref()
                        .map_or_else(|_| Vec::new(), |l| l.shown.clone()),
                    _ => Vec::new(),
                };
                return Some((ListId::Playlist(name.clone()), shown));
            }
            Origin::Queue => (ListId::Queue, self.now_playing.queue.len()),
            Origin::AlbumTile(_) => return None,
        };
        Some((list, (0..n).collect()))
    }

    /// What a menu choice on `rows` of `origin` acts on.
    fn target(&self, origin: &Origin, rows: &[usize]) -> MenuTarget {
        let paths = match origin {
            Origin::AlbumTile(id) => return MenuTarget::Album(*id),
            Origin::Album(id) => match &self.page {
                Some((a, tracks)) if a.id == *id => rows
                    .iter()
                    .filter_map(|&r| tracks.get(r))
                    .map(|t| t.path.clone())
                    .collect(),
                _ => Vec::new(),
            },
            Origin::Search => match &self.search {
                Some(r) => rows
                    .iter()
                    .filter_map(|&i| r.tracks.get(i))
                    .map(|(t, _)| t.path.clone())
                    .collect(),
                None => Vec::new(),
            },
            // File indices; hidden rows are never chosen, but their paths are in `file`.
            Origin::Playlist(name) => match &self.playlist {
                Some(p) if p.name == *name => match &p.loaded {
                    Ok(l) => rows
                        .iter()
                        .filter_map(|&i| l.file.get(i))
                        .map(|e| e.path.clone())
                        .collect(),
                    Err(_) => Vec::new(),
                },
                _ => Vec::new(),
            },
            Origin::Queue => rows
                .iter()
                .filter_map(|&i| self.now_playing.queue.get(i))
                .map(|item| item.path.clone())
                .collect(),
        };
        MenuTarget::Tracks(paths)
    }

    /// The queue items for `target`: library tracks by path, or files outside the
    /// library read from disk; an album in disc and track order.
    fn target_items(&mut self, target: &MenuTarget) -> Vec<QueueItem> {
        match target {
            MenuTarget::Album(id) => album_items(&self.library.tracks(*id)),
            MenuTarget::Tracks(paths) => {
                let catalog = self.catalog();
                paths
                    .iter()
                    .map(|p| {
                        catalog
                            .get(p)
                            .map_or_else(|| unlisted_path_item(p), |t| t.item.clone())
                    })
                    .collect()
            }
        }
    }

    /// The playlist lines for a menu choice on `rows` of `origin`.
    fn entries_of(&mut self, origin: &Origin, rows: &[usize]) -> Vec<Entry> {
        let items = self.items_of(origin, rows);
        self.item_entries(items)
    }

    fn items_of(&mut self, origin: &Origin, rows: &[usize]) -> Vec<QueueItem> {
        let target = self.target(origin, rows);
        self.target_items(&target)
    }

    /// Play: one album or search track plays its album from it; several tracks play
    /// just those; an album tile plays the album.
    fn menu_play(&mut self, origin: &Origin, rows: &[usize]) {
        match (origin, rows) {
            (Origin::Album(_), [row]) => self.play_page_from(*row),
            (Origin::Search, [row]) => self.play_search_track(*row),
            (Origin::AlbumTile(id), _) => {
                if let Some(album) = self.library.album(*id) {
                    let tracks = self.library.tracks(*id);
                    self.play_album(album, &tracks, 0);
                }
            }
            (Origin::Queue, _) => {
                if let Some(&first) = rows.iter().min() {
                    self.play_queue_from(first);
                }
            }
            (Origin::Playlist(name), _) => {
                if let Some(&first) = rows.first() {
                    self.play_playlist_from(name, first);
                }
            }
            (Origin::Album(_) | Origin::Search, _) => {
                let items = self.items_of(origin, rows);
                if !items.is_empty() {
                    self.playing_album = None;
                    self.send(PlayerCommand::Play { items, start: 0 });
                }
            }
        }
    }

    /// Plays the queue again from index `row`.
    fn play_queue_from(&mut self, row: usize) {
        if row < self.now_playing.queue.len() {
            let items = self.now_playing.queue.clone();
            self.send(PlayerCommand::Play { items, start: row });
        }
    }

    /// Plays the open playlist's shown rows from file row `row`, as Play from a
    /// double-click does.
    fn play_playlist_from(&mut self, name: &str, row: usize) {
        let Some(Ok(l)) = self
            .playlist
            .as_ref()
            .filter(|p| p.name == name)
            .map(|p| p.loaded.as_ref())
        else {
            return;
        };
        let Some(k) = l.shown.iter().position(|&s| s == row) else {
            return;
        };
        let rows = l.rows.clone();
        self.play_playlist_rows(name, &rows, k);
    }

    /// Plays the open album page from track index `row`.
    fn play_page_from(&mut self, row: usize) {
        if let Some((album, tracks)) = self.page.clone() {
            self.play_album(album, &tracks, row);
        }
    }

    /// Plays search track `i`'s album from that track.
    fn play_search_track(&mut self, i: usize) {
        let Some((track, album)) = self.search.as_ref().and_then(|r| r.tracks.get(i)) else {
            return;
        };
        let (track, album) = (track.id, album.id);
        if let Some(a) = self.library.album(album) {
            let tracks = self.library.tracks(album);
            let start = tracks.iter().position(|t| t.id == track).unwrap_or(0);
            self.play_album(a, &tracks, start);
        }
    }

    /// Opens the album of the one track chosen, with that track selected and scrolled
    /// into view.
    fn go_to_album(&mut self, origin: &Origin, rows: &[usize]) {
        let MenuTarget::Tracks(paths) = self.target(origin, rows) else {
            return;
        };
        let Some(path) = paths.first() else { return };
        let Some(album) = self.album_of(path) else {
            return;
        };
        let key = path_key(path);
        let row = self
            .library
            .tracks(album.id)
            .iter()
            .position(|t| path_key(&t.path) == key);
        self.open(Page::Album { album: album.id });
        // Going to an album from Now Playing leaves it.
        self.close_now_playing();
        // The selection belongs to the page just opened, not the one left.
        self.selection_page = self.nav.current().clone();
        self.selection_np = false;
        self.selection
            .set(&ListId::Album(album.id), row.into_iter().collect());
        self.reveal = row;
    }

    fn go_to_artist(&mut self, origin: &Origin, rows: &[usize]) {
        let album = match self.target(origin, rows) {
            MenuTarget::Album(id) => self.library.album(id),
            MenuTarget::Tracks(paths) => match paths.first() {
                Some(p) => self.album_of(p),
                None => None,
            },
        };
        if let Some(album) = album {
            self.open(Page::Artist(album.artist_id));
        }
    }
}

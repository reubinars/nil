//! Browser-style page history: back and forward stacks of pages with their scroll offsets.

use crate::backend::{AlbumId, ArtistId, LibrarySource, TrackId};

#[derive(Debug, Clone, PartialEq)]
pub enum Page {
    /// Recently added albums; the start page.
    Home,
    Albums,
    Artists,
    Artist(ArtistId),
    Album {
        album: AlbumId,
        highlight: Option<TrackId>,
    },
    Search(String),
    Settings,
}

/// The top bar's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Albums,
    Artists,
}

/// Most pages kept behind the current one.
pub const HISTORY_CAP: usize = 100;

#[derive(Debug, Clone)]
struct Entry {
    page: Page,
    /// Scroll offset when the page was left.
    scroll: f32,
}

impl Entry {
    fn new(page: Page) -> Self {
        Entry { page, scroll: 0.0 }
    }
}

#[derive(Debug)]
pub struct Nav {
    back: Vec<Entry>,
    current: Entry,
    forward: Vec<Entry>,
}

impl Default for Nav {
    fn default() -> Self {
        Nav {
            back: Vec::new(),
            current: Entry::new(Page::Home),
            forward: Vec::new(),
        }
    }
}

impl Nav {
    pub fn current(&self) -> &Page {
        &self.current.page
    }

    /// The scroll offset the current page had when it was last left.
    pub fn scroll(&self) -> f32 {
        self.current.scroll
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Goes to `page`, remembering the current one at `scroll`. No-op on the current page.
    pub fn open(&mut self, page: Page, scroll: f32) {
        if page == self.current.page {
            return;
        }
        self.current.scroll = scroll;
        let left = std::mem::replace(&mut self.current, Entry::new(page));
        self.back.push(left);
        if self.back.len() > HISTORY_CAP {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    /// Swaps the current page in place, without a history entry.
    pub fn replace(&mut self, page: Page) {
        self.current.page = page;
    }

    /// A search query changed: the first keystroke opens a Search page, later ones update
    /// it in place; an empty query leaves the Search page.
    pub fn search(&mut self, query: &str, scroll: f32) {
        let on_search = matches!(self.current.page, Page::Search(_));
        if query.is_empty() {
            if on_search {
                self.back(scroll);
            }
        } else if on_search {
            self.replace(Page::Search(query.to_owned()));
        } else {
            self.open(Page::Search(query.to_owned()), scroll);
        }
    }

    pub fn back(&mut self, scroll: f32) -> bool {
        let Some(prev) = self.back.pop() else {
            return false;
        };
        self.current.scroll = scroll;
        let left = std::mem::replace(&mut self.current, prev);
        self.forward.push(left);
        true
    }

    pub fn forward(&mut self, scroll: f32) -> bool {
        let Some(next) = self.forward.pop() else {
            return false;
        };
        self.current.scroll = scroll;
        let left = std::mem::replace(&mut self.current, next);
        self.back.push(left);
        true
    }

    /// The highlighted tab: an album page belongs to the tab it was reached from.
    pub fn tab(&self) -> Option<Tab> {
        let pages = std::iter::once(&self.current).chain(self.back.iter().rev());
        for e in pages {
            match e.page {
                Page::Home => return Some(Tab::Home),
                Page::Albums => return Some(Tab::Albums),
                Page::Artists | Page::Artist(_) => return Some(Tab::Artists),
                Page::Search(_) | Page::Settings => return None,
                Page::Album { .. } => {}
            }
        }
        None
    }

    /// Drops every page whose artist or album no longer exists (after a rescan). A dropped
    /// current page gives way to the nearest page left behind it, else Home.
    pub fn validate(&mut self, lib: &dyn LibrarySource) {
        let artists = lib.artists();
        let valid = |p: &Page| match p {
            Page::Artist(a) => artists.iter().any(|(id, _)| id == a),
            Page::Album { album, .. } => lib.album(*album).is_some(),
            _ => true,
        };
        self.back.retain(|e| valid(&e.page));
        self.forward.retain(|e| valid(&e.page));
        if !valid(&self.current.page) {
            self.current = self.back.pop().unwrap_or_else(|| Entry::new(Page::Home));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use bp_core::{Codec, SourceFormat, SourceKind};

    use super::*;
    use crate::backend::index::{MemLibrary, ScannedTrack};

    fn st(artist: &str, album: &str, title: &str) -> ScannedTrack {
        ScannedTrack {
            compilation: false,
            modified_ns: 0,
            path: PathBuf::from(format!("/m/{artist}/{album}/{title}.flac")),
            album_artist: Some(artist.into()),
            artist: None,
            album: Some(album.into()),
            title: Some(title.into()),
            year: None,
            disc: None,
            number: None,
            source: SourceFormat {
                codec: Codec::Flac,
                sample_rate: 44100,
                channels: 2,
                kind: SourceKind::Pcm {
                    bits: 16,
                    float: false,
                },
            },
            duration: Duration::from_secs(10),
        }
    }

    fn lib() -> (MemLibrary, ArtistId, AlbumId, AlbumId) {
        let lib = MemLibrary::new(
            vec![
                st("Miles", "Kob", "a"),
                st("Miles", "Other", "b"),
                st("Coltrane", "Giant", "c"),
            ],
            vec![],
        );
        let albums = lib.albums(None);
        let find = |t: &str| albums.iter().find(|a| a.title == t).unwrap().clone();
        let kob = find("Kob");
        (lib, kob.artist_id, kob.id, find("Other").id)
    }

    #[test]
    fn back_and_forward_restore_pages_and_scroll() {
        let (_, miles, _, _) = lib();
        let mut n = Nav::default();
        n.open(Page::Artists, 340.0);
        n.open(Page::Artist(miles), 12.0);
        assert!(n.back(0.0));
        assert_eq!(n.current(), &Page::Artists);
        assert_eq!(n.scroll(), 12.0);
        assert!(n.back(0.0));
        assert_eq!(n.current(), &Page::Home);
        assert_eq!(n.scroll(), 340.0);
        assert!(!n.can_back());
        assert!(n.forward(0.0));
        assert_eq!(n.current(), &Page::Artists);
    }

    #[test]
    fn opening_a_page_clears_forward() {
        let mut n = Nav::default();
        n.open(Page::Artists, 0.0);
        n.back(0.0);
        assert!(n.can_forward());
        n.open(Page::Settings, 0.0);
        assert!(!n.can_forward());
    }

    #[test]
    fn opening_the_current_page_does_nothing() {
        let mut n = Nav::default();
        n.open(Page::Home, 5.0);
        assert!(!n.can_back());
    }

    #[test]
    fn typing_replaces_the_search_entry() {
        let mut n = Nav::default();
        n.search("m", 0.0);
        n.search("mi", 0.0);
        n.search("mil", 0.0);
        assert_eq!(n.current(), &Page::Search("mil".into()));
        assert!(n.back(0.0));
        assert_eq!(n.current(), &Page::Home);
        assert!(!n.can_back());
        n.search("x", 0.0);
        n.search("", 0.0);
        assert_eq!(n.current(), &Page::Home);
    }

    #[test]
    fn replace_keeps_history_length() {
        let (_, _, kob, _) = lib();
        let mut n = Nav::default();
        n.open(
            Page::Album {
                album: kob,
                highlight: None,
            },
            0.0,
        );
        n.replace(Page::Album {
            album: kob,
            highlight: Some(TrackId(3)),
        });
        assert_eq!(
            n.current(),
            &Page::Album {
                album: kob,
                highlight: Some(TrackId(3))
            }
        );
        assert!(n.back(0.0));
        assert_eq!(n.current(), &Page::Home);
        assert!(!n.can_back());
    }

    #[test]
    fn history_is_capped() {
        let mut n = Nav::default();
        for i in 0..150 {
            n.open(
                if i % 2 == 0 {
                    Page::Artists
                } else {
                    Page::Albums
                },
                0.0,
            );
        }
        let mut backs = 0;
        while n.back(0.0) {
            backs += 1;
        }
        assert_eq!(backs, HISTORY_CAP);
    }

    #[test]
    fn tab_follows_the_page_and_album_origin() {
        let (_, miles, kob, _) = lib();
        let album = Page::Album {
            album: kob,
            highlight: None,
        };
        let mut n = Nav::default();
        assert_eq!(n.tab(), Some(Tab::Home));
        n.open(album.clone(), 0.0);
        assert_eq!(n.tab(), Some(Tab::Home));
        n.open(Page::Albums, 0.0);
        n.open(album.clone(), 0.0);
        assert_eq!(n.tab(), Some(Tab::Albums));
        n.open(Page::Artists, 0.0);
        n.open(Page::Artist(miles), 0.0);
        assert_eq!(n.tab(), Some(Tab::Artists));
        n.open(album.clone(), 0.0);
        assert_eq!(n.tab(), Some(Tab::Artists));
        n.open(Page::Settings, 0.0);
        assert_eq!(n.tab(), None);
        n.search("k", 0.0);
        n.open(album, 0.0);
        assert_eq!(n.tab(), None);
    }

    #[test]
    fn validate_drops_vanished_entries_everywhere() {
        let (lib, miles, kob, other) = lib();
        let mut n = Nav::default();
        n.open(Page::Artist(miles), 0.0);
        n.open(
            Page::Album {
                album: kob,
                highlight: None,
            },
            0.0,
        );
        n.open(
            Page::Album {
                album: other,
                highlight: None,
            },
            0.0,
        );
        n.back(0.0);
        // back [Home, Artist(miles)], current Album(kob), forward [Album(other)]
        assert!(n.can_forward());
        lib.set_tracks(vec![
            st("Miles", "Third", "d"),
            st("Coltrane", "Giant", "c"),
        ]);
        n.validate(&lib);
        assert_eq!(n.current(), &Page::Artist(miles));
        assert!(!n.can_forward());
        lib.set_tracks(vec![st("Coltrane", "Giant", "c")]);
        n.validate(&lib);
        assert_eq!(n.current(), &Page::Home);
        assert!(!n.can_back());
    }
}

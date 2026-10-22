//! In-memory library index: grouping, ordering, ids and filtering, plus `MemLibrary`,
//! a `LibrarySource` over a prebuilt index for tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use bp_core::SourceFormat;
use serde::{Deserialize, Serialize};

use crate::backend::{
    Album, AlbumId, ArtistId, FilterResult, LibrarySource, ScanStatus, Track, TrackId,
};
use crate::fmt::short_format;

const UNKNOWN_ARTIST: &str = "Unknown Artist";
/// Album artist for tracks flagged as a compilation that have no album artist.
pub const VARIOUS_ARTISTS: &str = "Various Artists";

/// FNV-1a, 64 bit. Ids derived from it must stay stable: thumbnails are keyed by `AlbumId`.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// One audio file as read from disk, before grouping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScannedTrack {
    pub path: PathBuf,
    pub album_artist: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub year: Option<u16>,
    pub disc: Option<u16>,
    pub number: Option<u16>,
    /// Tagged as part of a compilation (COMPILATION / TCMP).
    #[serde(default)]
    pub compilation: bool,
    /// The file's modified time in ns since the epoch, 0 if unknown. Not saved: loading
    /// the cache takes it from the entry's stamp.
    #[serde(skip)]
    pub modified_ns: u64,
    pub source: SourceFormat,
    pub duration: Duration,
}

struct AlbumEntry {
    album: Album,
    tracks: Vec<Track>,
}

/// Immutable, fully sorted snapshot of the library.
#[derive(Default)]
pub struct LibraryIndex {
    artists: Vec<(ArtistId, String)>,
    /// Sorted by artist, then `(year, title)`.
    albums: Vec<AlbumEntry>,
    album_pos: HashMap<AlbumId, usize>,
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl LibraryIndex {
    pub fn build(tracks: Vec<ScannedTrack>) -> Self {
        struct Group {
            artist: String,
            title: String,
            tracks: Vec<ScannedTrack>,
        }
        let mut groups: HashMap<AlbumId, Group> = HashMap::new();
        for mut t in tracks {
            let artist = t
                .album_artist
                .clone()
                .or_else(|| t.compilation.then(|| VARIOUS_ARTISTS.to_owned()))
                .or_else(|| t.artist.clone())
                .unwrap_or_else(|| UNKNOWN_ARTIST.to_owned());
            let (folder, folder_disc) = album_folder(t.path.parent().unwrap_or(Path::new("")));
            let title = t.album.clone().unwrap_or_else(|| file_name(folder));
            let key = format!("{artist}\0{title}\0{}", folder.to_string_lossy());
            t.disc = t.disc.or(folder_disc);
            groups
                .entry(AlbumId(fnv1a(key.as_bytes())))
                .or_insert_with(|| Group {
                    artist,
                    title,
                    tracks: Vec::new(),
                })
                .tracks
                .push(t);
        }

        let mut entries: Vec<AlbumEntry> = groups
            .into_iter()
            .map(|(id, g)| {
                let mut scanned = g.tracks;
                scanned.sort_by(|a, b| {
                    (a.disc.unwrap_or(1), a.number.unwrap_or(u16::MAX))
                        .cmp(&(b.disc.unwrap_or(1), b.number.unwrap_or(u16::MAX)))
                        .then_with(|| natord::compare(&file_name(&a.path), &file_name(&b.path)))
                });
                let first = scanned[0].source;
                let uniform = scanned.iter().all(|t| {
                    t.source.codec == first.codec && short_format(&t.source) == short_format(&first)
                });
                let format_summary = if uniform {
                    format!("{} {}", first.codec, short_format(&first))
                } else {
                    "Mixed".to_owned()
                };
                let album = Album {
                    id,
                    artist_id: artist_id(&g.artist),
                    title: g.title,
                    artist: g.artist,
                    year: scanned.iter().filter_map(|t| t.year).min(),
                    duration: scanned.iter().map(|t| t.duration).sum(),
                    format_summary,
                    added_ns: scanned.iter().map(|t| t.modified_ns).max().unwrap_or(0),
                };
                let tracks = scanned
                    .into_iter()
                    .map(|t| Track {
                        id: TrackId(fnv1a(t.path.to_string_lossy().as_bytes())),
                        album: id,
                        disc: t.disc.unwrap_or(1),
                        number: t.number.unwrap_or(0),
                        title: t.title.unwrap_or_else(|| {
                            t.path
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        }),
                        duration: t.duration,
                        source: t.source,
                        path: t.path,
                    })
                    .collect();
                AlbumEntry { album, tracks }
            })
            .collect();
        entries.sort_by(|a, b| {
            natord::compare_ignore_case(&a.album.artist, &b.album.artist)
                .then_with(|| a.album.artist_id.cmp(&b.album.artist_id))
                .then_with(|| a.album.year.cmp(&b.album.year))
                .then_with(|| natord::compare_ignore_case(&a.album.title, &b.album.title))
                .then_with(|| a.album.id.cmp(&b.album.id))
        });

        // One artist per lowercased name; the smallest spelling is shown, so the result
        // does not depend on scan order.
        let mut names: HashMap<ArtistId, String> = HashMap::new();
        for e in &entries {
            names
                .entry(e.album.artist_id)
                .and_modify(|n| {
                    if e.album.artist < *n {
                        n.clone_from(&e.album.artist);
                    }
                })
                .or_insert_with(|| e.album.artist.clone());
        }
        let mut artists: Vec<_> = names.into_iter().collect();
        artists.sort_by(|a, b| natord::compare_ignore_case(&a.1, &b.1).then_with(|| a.0.cmp(&b.0)));

        let mut album_pos = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            album_pos.insert(e.album.id, i);
        }
        Self {
            artists,
            albums: entries,
            album_pos,
        }
    }

    pub fn artists(&self) -> Vec<(ArtistId, String)> {
        self.artists.clone()
    }

    pub fn is_empty(&self) -> bool {
        self.albums.is_empty()
    }

    /// Albums of one artist, or all albums (sorted by artist) when `artist` is `None`.
    pub fn albums(&self, artist: Option<ArtistId>) -> Vec<Album> {
        self.albums
            .iter()
            .filter(|e| artist.is_none_or(|a| e.album.artist_id == a))
            .map(|e| e.album.clone())
            .collect()
    }

    /// At most `limit` albums, newest file first; undated albums last, in artist order.
    pub fn recently_added(&self, limit: usize) -> Vec<Album> {
        let mut albums: Vec<&Album> = self.albums.iter().map(|e| &e.album).collect();
        albums.sort_by_key(|a| std::cmp::Reverse(a.added_ns));
        albums.into_iter().take(limit).cloned().collect()
    }

    pub fn album(&self, id: AlbumId) -> Option<Album> {
        self.album_pos
            .get(&id)
            .map(|&i| self.albums[i].album.clone())
    }

    pub fn tracks(&self, album: AlbumId) -> Vec<Track> {
        self.album_pos
            .get(&album)
            .map(|&i| self.albums[i].tracks.clone())
            .unwrap_or_default()
    }

    pub fn track(&self, id: TrackId) -> Option<Track> {
        self.albums
            .iter()
            .flat_map(|e| &e.tracks)
            .find(|t| t.id == id)
            .cloned()
    }

    pub fn filter(&self, query: &str) -> FilterResult {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return FilterResult::default();
        }
        let hit = |s: &str| s.to_lowercase().contains(&q);
        FilterResult {
            artists: self
                .artists
                .iter()
                .filter(|(_, n)| hit(n))
                .map(|(id, _)| *id)
                .collect(),
            albums: self
                .albums
                .iter()
                .filter(|e| hit(&e.album.title))
                .map(|e| e.album.id)
                .collect(),
            tracks: self
                .albums
                .iter()
                .flat_map(|e| &e.tracks)
                .filter(|t| hit(&t.title))
                .map(|t| t.id)
                .collect(),
        }
    }
}

/// The folder that names an album for tracks in `track_dir`: its parent when
/// `track_dir` is a disc folder (`CD1`, `Disc 2`, `disk03`), with that disc number.
pub fn album_folder(track_dir: &Path) -> (&Path, Option<u16>) {
    let disc = track_dir
        .file_name()
        .and_then(|n| disc_number(&n.to_string_lossy()));
    match (disc, track_dir.parent()) {
        (Some(disc), Some(parent)) => (parent, Some(disc)),
        _ => (track_dir, None),
    }
}

/// `n` for a folder named `cd`/`disc`/`disk`, optional whitespace, then digits `n`.
fn disc_number(name: &str) -> Option<u16> {
    let lower = name.to_lowercase();
    let rest = ["disc", "disk", "cd"]
        .iter()
        .find_map(|p| lower.strip_prefix(p))?
        .trim_start();
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

fn artist_id(name: &str) -> ArtistId {
    ArtistId(fnv1a(name.to_lowercase().as_bytes()))
}

/// A `LibrarySource` over an in-memory index. Public so integration tests can use it.
pub struct MemLibrary {
    index: RwLock<Arc<LibraryIndex>>,
    folders: Mutex<Vec<PathBuf>>,
    status: Mutex<ScanStatus>,
    unreadable: Mutex<Vec<(PathBuf, String)>>,
    art: Mutex<HashMap<AlbumId, Vec<u8>>>,
    rescans: Mutex<usize>,
    artist_reads: Mutex<usize>,
    album_reads: Mutex<usize>,
}

impl MemLibrary {
    pub fn new(tracks: Vec<ScannedTrack>, folders: Vec<PathBuf>) -> Self {
        Self {
            index: RwLock::new(Arc::new(LibraryIndex::build(tracks))),
            folders: Mutex::new(folders),
            status: Mutex::new(ScanStatus::Done { unreadable: 0 }),
            unreadable: Mutex::new(Vec::new()),
            art: Mutex::new(HashMap::new()),
            rescans: Mutex::new(0),
            artist_reads: Mutex::new(0),
            album_reads: Mutex::new(0),
        }
    }

    /// Replaces the index, as a finished rescan would.
    pub fn set_tracks(&self, tracks: Vec<ScannedTrack>) {
        *self.index.write().unwrap() = Arc::new(LibraryIndex::build(tracks));
    }

    pub fn set_scan_status(&self, s: ScanStatus) {
        *self.status.lock().unwrap() = s;
    }

    /// Served as is by `unreadable()`.
    pub fn set_unreadable(&self, files: Vec<(PathBuf, String)>) {
        *self.unreadable.lock().unwrap() = files;
    }

    pub fn set_art(&self, album: AlbumId, bytes: Vec<u8>) {
        self.art.lock().unwrap().insert(album, bytes);
    }

    pub fn rescan_count(&self) -> usize {
        *self.rescans.lock().unwrap()
    }

    /// Calls to `artists()`; the app makes them when it refreshes from the library.
    pub fn artist_reads(&self) -> usize {
        *self.artist_reads.lock().unwrap()
    }

    /// Calls to `albums()`.
    pub fn album_reads(&self) -> usize {
        *self.album_reads.lock().unwrap()
    }

    fn snapshot(&self) -> Arc<LibraryIndex> {
        self.index.read().unwrap().clone()
    }
}

impl LibrarySource for MemLibrary {
    fn artists(&self) -> Vec<(ArtistId, String)> {
        *self.artist_reads.lock().unwrap() += 1;
        self.snapshot().artists()
    }
    fn albums(&self, artist: Option<ArtistId>) -> Vec<Album> {
        *self.album_reads.lock().unwrap() += 1;
        self.snapshot().albums(artist)
    }
    fn recently_added(&self, limit: usize) -> Vec<Album> {
        self.snapshot().recently_added(limit)
    }
    fn album(&self, id: AlbumId) -> Option<Album> {
        self.snapshot().album(id)
    }
    fn tracks(&self, album: AlbumId) -> Vec<Track> {
        self.snapshot().tracks(album)
    }
    fn track(&self, id: TrackId) -> Option<Track> {
        self.snapshot().track(id)
    }
    fn art(&self, album: AlbumId) -> Option<Vec<u8>> {
        self.art.lock().unwrap().get(&album).cloned()
    }
    fn filter(&self, query: &str) -> FilterResult {
        self.snapshot().filter(query)
    }
    fn folders(&self) -> Vec<PathBuf> {
        self.folders.lock().unwrap().clone()
    }
    fn add_folder(&self, path: PathBuf) {
        let mut f = self.folders.lock().unwrap();
        if !f.contains(&path) {
            f.push(path);
        }
    }
    fn remove_folder(&self, path: &Path) {
        self.folders.lock().unwrap().retain(|f| f != path);
    }
    /// Counts the call and does nothing else.
    fn rescan(&self) {
        *self.rescans.lock().unwrap() += 1;
    }
    fn scan_status(&self) -> ScanStatus {
        self.status.lock().unwrap().clone()
    }
    fn unreadable(&self) -> Vec<(PathBuf, String)> {
        self.unreadable.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use bp_core::{Codec, SourceFormat, SourceKind};

    use super::*;
    use crate::backend::FilterResult;

    fn fmt(codec: Codec, bits: u8, rate: u32) -> SourceFormat {
        SourceFormat {
            codec,
            sample_rate: rate,
            channels: 2,
            kind: SourceKind::Pcm { bits, float: false },
        }
    }

    fn st(path: &str) -> ScannedTrack {
        ScannedTrack {
            compilation: false,
            modified_ns: 0,
            path: PathBuf::from(path),
            album_artist: None,
            artist: None,
            album: None,
            title: None,
            year: None,
            disc: None,
            number: None,
            source: fmt(Codec::Dsf, 1, 44100),
            duration: Duration::from_secs(10),
        }
    }

    fn untagged(paths: &[&str]) -> Vec<ScannedTrack> {
        paths.iter().map(|p| st(p)).collect()
    }

    #[test]
    fn track_looks_up_by_id() {
        let idx = LibraryIndex::build(untagged(&["/m/A/1 a.dsf", "/m/B/1 b.dsf"]));
        let album = idx.albums(None).remove(1);
        let t = idx.tracks(album.id).remove(0);
        assert_eq!(idx.track(t.id).map(|x| x.path), Some(t.path));
        assert!(idx.track(TrackId(12345)).is_none());
    }

    #[test]
    fn untagged_folder_groups_by_folder_in_natural_order() {
        let idx = LibraryIndex::build(untagged(&[
            "/m/Some Album/2 b.dsf",
            "/m/Some Album/10 c.dsf",
            "/m/Some Album/1 a.dsf",
        ]));
        let albums = idx.albums(None);
        assert_eq!(albums.len(), 1);
        assert_eq!(
            (albums[0].title.as_str(), albums[0].artist.as_str()),
            ("Some Album", "Unknown Artist")
        );
        let titles: Vec<_> = idx
            .tracks(albums[0].id)
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(titles, ["1 a", "2 b", "10 c"]);
    }

    #[test]
    fn multi_disc_order_and_mixed_summary() {
        let mut a = st("/m/X/a.flac");
        (a.artist, a.album, a.disc, a.number) =
            (Some("A".into()), Some("X".into()), Some(2), Some(1));
        a.source = fmt(Codec::Flac, 24, 96000);
        let mut b = st("/m/X/b.flac");
        (b.artist, b.album, b.disc, b.number) =
            (Some("A".into()), Some("X".into()), Some(1), Some(9));
        b.source = fmt(Codec::Flac, 16, 44100);
        let idx = LibraryIndex::build(vec![a, b]);
        let album = &idx.albums(None)[0];
        assert_eq!(album.format_summary, "Mixed");
        assert_eq!(album.duration, Duration::from_secs(20));
        let order: Vec<_> = idx
            .tracks(album.id)
            .iter()
            .map(|t| (t.disc, t.number))
            .collect();
        assert_eq!(order, [(1, 9), (2, 1)]);
    }

    #[test]
    fn uniform_summary_uses_codec_and_short_format() {
        let mut a = st("/m/X/a.flac");
        a.source = fmt(Codec::Flac, 24, 192000);
        let idx = LibraryIndex::build(vec![a]);
        assert_eq!(idx.albums(None)[0].format_summary, "FLAC 24/192");
    }

    #[test]
    fn same_album_title_in_two_folders_is_two_albums() {
        let mut a = st("/m/A/Greatest Hits/1.flac");
        (a.artist, a.album) = (Some("A".into()), Some("Greatest Hits".into()));
        let mut b = st("/m/B/Greatest Hits/1.flac");
        (b.artist, b.album) = (Some("B".into()), Some("Greatest Hits".into()));
        let idx = LibraryIndex::build(vec![a, b]);
        assert_eq!(idx.albums(None).len(), 2);
        assert_eq!(idx.artists().len(), 2);
    }

    #[test]
    fn artists_and_albums_sort_naturally_and_by_year() {
        let mk = |p: &str, artist: &str, album: &str, year| {
            let mut t = st(p);
            (t.artist, t.album, t.year) = (Some(artist.into()), Some(album.into()), year);
            t
        };
        let idx = LibraryIndex::build(vec![
            mk("/m/1/a.flac", "beta", "Z", Some(2000)),
            mk("/m/2/a.flac", "Alpha 10", "Late", Some(2010)),
            mk("/m/3/a.flac", "Alpha 10", "Early", Some(1990)),
            mk("/m/4/a.flac", "alpha 2", "Q", None),
        ]);
        let names: Vec<_> = idx.artists().into_iter().map(|a| a.1).collect();
        assert_eq!(names, ["alpha 2", "Alpha 10", "beta"]);
        let a10 = idx.artists()[1].0;
        let titles: Vec<_> = idx.albums(Some(a10)).into_iter().map(|a| a.title).collect();
        assert_eq!(titles, ["Early", "Late"]);
    }

    fn miles() -> LibraryIndex {
        let mut a = st("/m/Kind of Blue/01.flac");
        (a.artist, a.album, a.title) = (
            Some("Miles Davis".into()),
            Some("Kind of Blue".into()),
            Some("So What".into()),
        );
        let mut b = st("/m/Kind of Blue/03.flac");
        (b.artist, b.album, b.title) = (
            Some("Miles Davis".into()),
            Some("Kind of Blue".into()),
            Some("Blue in Green".into()),
        );
        b.number = Some(3);
        a.number = Some(1);
        LibraryIndex::build(vec![a, b])
    }

    #[test]
    fn filter_matches_each_level_case_insensitively() {
        let idx = miles();
        let album = idx.albums(None)[0].id;
        let blue_in_green = idx.tracks(album)[1].id;
        let r = idx.filter("BLUE");
        assert_eq!(r.albums, vec![album]);
        assert_eq!(r.tracks, vec![blue_in_green]);
        assert!(r.artists.is_empty());
        let r = idx.filter("davis");
        assert_eq!(r.artists.len(), 1);
        assert!(r.albums.is_empty() && r.tracks.is_empty());
        assert_eq!(idx.filter("  "), FilterResult::default());
        assert_eq!(idx.filter(""), FilterResult::default());
    }

    #[test]
    fn ids_are_stable_across_builds() {
        let a = miles();
        let b = miles();
        let (aa, bb) = (&a.albums(None)[0], &b.albums(None)[0]);
        assert_eq!(aa.id, bb.id);
        assert_eq!(aa.artist_id, bb.artist_id);
        assert_eq!(a.tracks(aa.id)[0].id, b.tracks(bb.id)[0].id);
        // pinned value: thumbnails on disk are keyed by AlbumId
        assert_eq!(fnv1a(b"a"), 0xaf63dc4c8601ec8c);
    }

    fn tagged(path: &str, artist: &str, album: &str) -> ScannedTrack {
        let mut t = st(path);
        (t.artist, t.album) = (Some(artist.into()), Some(album.into()));
        t
    }

    fn dated(path: &str, album: &str, modified_ns: u64) -> ScannedTrack {
        let mut t = tagged(path, "A", album);
        t.modified_ns = modified_ns;
        t
    }

    #[test]
    fn recently_added_is_newest_file_first_and_capped() {
        let idx = LibraryIndex::build(vec![
            dated("/m/Old/1.flac", "Old", 10),
            dated("/m/Mid/1.flac", "Mid", 20),
            // One new file makes the whole album new.
            dated("/m/New/1.flac", "New", 5),
            dated("/m/New/2.flac", "New", 30),
            dated("/m/Undated/1.flac", "Undated", 0),
        ]);
        let titles =
            |n| -> Vec<String> { idx.recently_added(n).into_iter().map(|a| a.title).collect() };
        assert_eq!(titles(10), ["New", "Mid", "Old", "Undated"]);
        assert_eq!(titles(2), ["New", "Mid"]);
        assert_eq!(idx.album(idx.recently_added(1)[0].id).unwrap().added_ns, 30);
    }

    fn discs(idx: &LibraryIndex, album: AlbumId) -> Vec<(u16, u16)> {
        idx.tracks(album)
            .iter()
            .map(|t| (t.disc, t.number))
            .collect()
    }

    #[test]
    fn disc_folders_merge_into_one_album() {
        let idx = LibraryIndex::build(vec![
            tagged("/m/Al/CD2/01.flac", "A", "Al"),
            tagged("/m/Al/CD1/01.flac", "A", "Al"),
        ]);
        let albums = idx.albums(None);
        assert_eq!(albums.len(), 1);
        assert_eq!(discs(&idx, albums[0].id), [(1, 0), (2, 0)]);
    }

    #[test]
    fn disc_folder_variants() {
        for (name, disc) in [("Disc 2", 2), ("disk03", 3), ("cd 1", 1), ("CD01", 1)] {
            let dir = PathBuf::from("/m/Al").join(name);
            assert_eq!(
                album_folder(&dir),
                (Path::new("/m/Al"), Some(disc)),
                "{name}"
            );
        }
    }

    #[test]
    fn disc_folder_needs_a_number() {
        for name in ["CD", "Disc", "CD Collection", "Discography"] {
            let dir = PathBuf::from("/m").join(name);
            assert_eq!(album_folder(&dir), (dir.as_path(), None), "{name}");
        }
    }

    #[test]
    fn disc_tag_wins_over_folder_number() {
        let mut t = tagged("/m/Al/CD2/01.flac", "A", "Al");
        t.disc = Some(5);
        let idx = LibraryIndex::build(vec![t]);
        assert_eq!(discs(&idx, idx.albums(None)[0].id), [(5, 0)]);
    }

    #[test]
    fn untagged_disc_folders_take_the_parent_name() {
        let idx = LibraryIndex::build(untagged(&[
            "/m/Some Album/CD1/a.dsf",
            "/m/Some Album/CD2/a.dsf",
        ]));
        let albums = idx.albums(None);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].title, "Some Album");
    }

    fn hits(compilation: bool, album_artist: Option<&str>) -> LibraryIndex {
        let mut x = tagged("/m/x.flac", "X", "Hits");
        let mut y = tagged("/m/y.flac", "Y", "Hits");
        for t in [&mut x, &mut y] {
            t.compilation = compilation;
            t.album_artist = album_artist.map(Into::into);
        }
        LibraryIndex::build(vec![x, y])
    }

    #[test]
    fn flagged_compilation_without_album_artist_is_various_artists() {
        let idx = hits(true, None);
        let albums = idx.albums(None);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].artist, "Various Artists");
        assert_eq!(idx.tracks(albums[0].id).len(), 2);
    }

    #[test]
    fn compilation_with_album_artist_is_unchanged() {
        let albums = hits(true, Some("DJ Z")).albums(None);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].artist, "DJ Z");
    }

    #[test]
    fn unflagged_multi_artist_album_still_splits() {
        assert_eq!(hits(false, None).albums(None).len(), 2);
    }

    #[test]
    fn mem_library_serves_index_and_records_calls() {
        use crate::backend::{LibrarySource, ScanStatus};
        let lib = MemLibrary::new(untagged(&["/m/Al/1.dsf"]), vec![PathBuf::from("/m")]);
        assert_eq!(lib.albums(None).len(), 1);
        assert_eq!(lib.scan_status(), ScanStatus::Done { unreadable: 0 });
        assert_eq!(lib.folders(), vec![PathBuf::from("/m")]);
        let id = lib.albums(None)[0].id;
        assert!(lib.art(id).is_none());
        lib.set_art(id, vec![1, 2]);
        assert_eq!(lib.art(id), Some(vec![1, 2]));
        lib.rescan();
        assert_eq!(lib.rescan_count(), 1);
        lib.set_tracks(Vec::new());
        assert!(lib.albums(None).is_empty());
        lib.set_scan_status(ScanStatus::Idle);
        assert_eq!(lib.scan_status(), ScanStatus::Idle);
        lib.add_folder(PathBuf::from("/n"));
        lib.remove_folder(Path::new("/m"));
        assert_eq!(lib.folders(), vec![PathBuf::from("/n")]);
    }
}

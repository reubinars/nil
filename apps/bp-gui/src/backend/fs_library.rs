//! `LibrarySource` backed by folders on disk. Scanning runs on a worker thread. The
//! first scan (empty index) publishes a partial `LibraryIndex` every second; later
//! scans publish only progress until the full index replaces the old one at the end.
//! The last scan is saved to `library-cache.json`: it is shown at launch, and a rescan
//! only re-reads files whose size or modified time changed.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lofty::picture::PictureType;
use lofty::prelude::*;
use serde::{Deserialize, Serialize};

use crate::backend::cache::{self, CachedFile, Entry};
use crate::backend::index::{LibraryIndex, ScannedTrack, album_folder};
use crate::backend::{
    Album, AlbumId, ArtistId, FilterResult, LibrarySource, ScanStatus, Track, TrackId,
};

/// Files with other extensions are not looked at.
const AUDIO_EXTENSIONS: [&str; 7] = ["flac", "wav", "aif", "aiff", "m4a", "dsf", "dff"];
/// Folder art file names, in priority order.
const ART_NAMES: [&str; 5] = [
    "folder.jpg",
    "cover.jpg",
    "front.jpg",
    "folder.png",
    "cover.png",
];
const PUBLISH_EVERY: Duration = Duration::from_secs(1);
const PANIC_REASON: &str = "reading the file crashed the decoder";

#[derive(Serialize, Deserialize, Default)]
struct Config {
    #[serde(default)]
    folders: Vec<PathBuf>,
}

struct Shared {
    config_file: PathBuf,
    cache_file: PathBuf,
    /// The last finished scan's files, by path.
    cache: Mutex<HashMap<PathBuf, CachedFile>>,
    folders: Mutex<Vec<PathBuf>>,
    index: RwLock<Arc<LibraryIndex>>,
    status: Mutex<ScanStatus>,
    /// The last finished scan's unreadable files with the reason, in natural path order.
    unreadable: Mutex<Vec<(PathBuf, String)>>,
    /// Bumped by every `rescan`; a scan thread whose generation is stale stops publishing.
    generation: AtomicU64,
}

pub struct FsLibrary {
    shared: Arc<Shared>,
}

impl FsLibrary {
    /// Loads the folder list from `config_file` (missing or malformed means empty) and
    /// the saved index, then starts a scan.
    pub fn open(config_file: PathBuf) -> Self {
        let lib = Self::load_cached(config_file);
        lib.rescan();
        lib
    }

    /// Loads the folder list and shows the saved index, without scanning. Cached files
    /// outside the folder list are dropped; a missing or unusable cache gives an empty
    /// library.
    fn load_cached(config_file: PathBuf) -> Self {
        let folders = fs::read_to_string(&config_file)
            .ok()
            .and_then(|s| toml::from_str::<Config>(&s).ok())
            .unwrap_or_default()
            .folders;
        let cache_file = cache::cache_path(&config_file);
        let cached: HashMap<PathBuf, CachedFile> = cache::load(&cache_file)
            .unwrap_or_default()
            .into_iter()
            .filter(|f| folders.iter().any(|d| f.path.starts_with(d)))
            .map(|f| (f.path.clone(), f))
            .collect();
        let (tracks, unreadable) = split_entries(cached.values());
        let unreadable = sorted(unreadable);
        let status = if cached.is_empty() {
            ScanStatus::Idle
        } else {
            ScanStatus::Done {
                unreadable: unreadable.len() as u64,
            }
        };
        Self {
            shared: Arc::new(Shared {
                config_file,
                cache_file,
                cache: Mutex::new(cached),
                folders: Mutex::new(folders),
                index: RwLock::new(Arc::new(LibraryIndex::build(tracks))),
                status: Mutex::new(status),
                unreadable: Mutex::new(unreadable),
                generation: AtomicU64::new(0),
            }),
        }
    }

    /// Blocks until no scan is running, or `timeout` passes. Returns whether it is idle.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if !matches!(self.scan_status(), ScanStatus::Scanning { .. }) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn save_and_rescan(&self) {
        let config = Config {
            folders: self.folders(),
        };
        if let Ok(text) = toml::to_string(&config) {
            if let Some(dir) = self.shared.config_file.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let _ = fs::write(&self.shared.config_file, text);
        }
        self.rescan();
    }

    fn snapshot(&self) -> Arc<LibraryIndex> {
        self.shared.index.read().unwrap().clone()
    }
}

impl Drop for FsLibrary {
    fn drop(&mut self) {
        // Stops a running scan from doing further work.
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
    }
}

impl LibrarySource for FsLibrary {
    fn artists(&self) -> Vec<(ArtistId, String)> {
        self.snapshot().artists()
    }
    fn albums(&self, artist: Option<ArtistId>) -> Vec<Album> {
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
        let tracks = self.snapshot().tracks(album);
        let first = tracks.first()?;
        let dir = first.path.parent()?;
        embedded_cover(&first.path)
            .or_else(|| folder_cover(dir))
            .or_else(|| {
                let album = album_folder(dir).0;
                (album != dir).then(|| folder_cover(album)).flatten()
            })
    }
    fn filter(&self, query: &str) -> FilterResult {
        self.snapshot().filter(query)
    }
    fn folders(&self) -> Vec<PathBuf> {
        self.shared.folders.lock().unwrap().clone()
    }
    fn add_folder(&self, path: PathBuf) {
        {
            let mut f = self.shared.folders.lock().unwrap();
            if f.contains(&path) {
                return;
            }
            f.push(path);
        }
        self.save_and_rescan();
    }
    fn remove_folder(&self, path: &Path) {
        self.shared.folders.lock().unwrap().retain(|f| f != path);
        self.save_and_rescan();
    }
    /// Returns immediately; the scan runs on its own thread, replacing any running one.
    fn rescan(&self) {
        let gen_ = self.shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.shared.status.lock().unwrap() = ScanStatus::Scanning { done: 0, total: 0 };
        let shared = self.shared.clone();
        let folders = self.folders();
        std::thread::spawn(move || scan_with(&shared, gen_, &folders, worker_count(), probe));
    }
    fn scan_status(&self) -> ScanStatus {
        self.shared.status.lock().unwrap().clone()
    }
    fn unreadable(&self) -> Vec<(PathBuf, String)> {
        self.shared.unreadable.lock().unwrap().clone()
    }
}

enum Probed {
    Track(Box<ScannedTrack>),
    /// Why the file couldn't be read.
    Unreadable(String),
}

/// The tracks among `files`, and the unreadable files with the reason.
fn split_entries<'a>(
    files: impl Iterator<Item = &'a CachedFile>,
) -> (Vec<ScannedTrack>, Vec<(PathBuf, String)>) {
    let mut tracks = Vec::new();
    let mut unreadable = Vec::new();
    for f in files {
        match &f.entry {
            Entry::Track(t) => {
                let mut t = t.clone();
                // An untrusted stamp is read again by the next scan.
                if f.mtime_ns != cache::UNTRUSTED {
                    t.modified_ns = f.mtime_ns;
                }
                tracks.push(t);
            }
            Entry::Unreadable(reason) => unreadable.push((f.path.clone(), reason.clone())),
        }
    }
    (tracks, unreadable)
}

/// `files` in natural path order.
fn sorted(mut files: Vec<(PathBuf, String)>) -> Vec<(PathBuf, String)> {
    files.sort_by(|a, b| natord::compare(&a.0.to_string_lossy(), &b.0.to_string_lossy()));
    files
}

fn scan_with(
    shared: &Shared,
    gen_: u64,
    folders: &[PathBuf],
    workers: usize,
    probe: impl Fn(PathBuf) -> Probed + Sync,
) {
    let current = || shared.generation.load(Ordering::SeqCst) == gen_;
    // Files modified after this were possibly changed again after being read.
    let racy_after = SystemTime::now()
        .checked_sub(cache::RACY_WINDOW)
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let mut files = Vec::new();
    for f in folders {
        walk(f, &mut files, &current);
    }
    files.retain(|p| has_audio_extension(p));
    let total = files.len() as u64;
    publish(shared, gen_, None, ScanStatus::Scanning { done: 0, total });
    // A partial index would shrink a populated library mid-scan.
    let partial = shared.index.read().unwrap().is_empty();

    // Files whose size and modified time match the cache are reused without reading;
    // they count as done. Reading the stamps takes seconds on a slow drive.
    let old_cache = shared.cache.lock().unwrap().clone();
    let mut new_cache = Vec::new();
    let mut to_probe = Vec::new();
    let mut last_publish = Instant::now();
    for path in files {
        let stamp = cache::stamp(&path);
        match old_cache.get(&path) {
            Some(c) if stamp == Some((c.size, c.mtime_ns)) => new_cache.push(c.clone()),
            _ => to_probe.push((path, stamp)),
        }
        if last_publish.elapsed() >= PUBLISH_EVERY {
            last_publish = Instant::now();
            let done = new_cache.len() as u64;
            publish(shared, gen_, None, ScanStatus::Scanning { done, total });
        }
    }
    let (mut tracks, mut unreadable) = split_entries(new_cache.iter());
    let hits = new_cache.len() as u64;
    publish(
        shared,
        gen_,
        None,
        ScanStatus::Scanning { done: hits, total },
    );

    // Workers take the next file from `next` and send what they read; this thread
    // collects the results and publishes.
    let next = AtomicUsize::new(0);
    let (tx, rx) = crossbeam_channel::unbounded();
    let finished = std::thread::scope(|scope| {
        let (next, to_probe, probe, current) = (&next, &to_probe, &probe, &current);
        for _ in 0..workers.max(1) {
            let tx = tx.clone();
            scope.spawn(move || {
                while let Some((path, stamp)) = to_probe.get(next.fetch_add(1, Ordering::SeqCst)) {
                    if !current() {
                        return;
                    }
                    // Decoders and tag parsers run on arbitrary user files; a panic must
                    // not leave the scan (and its status) hanging.
                    let probed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        probe(path.clone())
                    }))
                    .unwrap_or_else(|_| Probed::Unreadable(PANIC_REASON.to_owned()));
                    if tx.send((path.clone(), *stamp, probed)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(tx);
        let mut last_publish = Instant::now();
        for (i, (path, stamp, probed)) in rx.iter().enumerate() {
            if !current() {
                return false;
            }
            let entry = match probed {
                Probed::Track(mut t) => {
                    t.modified_ns = stamp.map_or(0, |(_, mtime_ns)| mtime_ns);
                    tracks.push((*t).clone());
                    Entry::Track(*t)
                }
                Probed::Unreadable(reason) => {
                    unreadable.push((path.clone(), reason.clone()));
                    Entry::Unreadable(reason)
                }
            };
            // A file without a stamp is not cached: the next scan reads it again.
            if let Some((size, mtime_ns)) = stamp {
                let mtime_ns = if mtime_ns >= racy_after {
                    cache::UNTRUSTED
                } else {
                    mtime_ns
                };
                new_cache.push(CachedFile {
                    path,
                    size,
                    mtime_ns,
                    entry,
                });
            }
            if last_publish.elapsed() >= PUBLISH_EVERY {
                last_publish = Instant::now();
                publish(
                    shared,
                    gen_,
                    partial.then_some(&tracks[..]),
                    ScanStatus::Scanning {
                        done: hits + i as u64 + 1,
                        total,
                    },
                );
            }
        }
        true
    });
    // Saved before `Done` is published, so a finished scan is always on disk.
    if !finished || !current() {
        return;
    }
    let _ = cache::save(&shared.cache_file, &new_cache);
    let cache = new_cache.into_iter().map(|f| (f.path.clone(), f)).collect();
    publish_final(shared, gen_, &tracks, cache, unreadable);
}

/// Probe threads for a scan: the core count, at most 8.
fn worker_count() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().min(8))
}

/// Sets `status` and, given `tracks`, replaces the index. Publishes only if no newer
/// scan has started; checked under the status lock so a superseded scan can never
/// overwrite the newer scan's status.
fn publish(shared: &Shared, gen_: u64, tracks: Option<&[ScannedTrack]>, status: ScanStatus) {
    let index = tracks.map(|t| Arc::new(LibraryIndex::build(t.to_vec())));
    let mut st = shared.status.lock().unwrap();
    if shared.generation.load(Ordering::SeqCst) != gen_ {
        return;
    }
    if let Some(index) = index {
        *shared.index.write().unwrap() = index;
    }
    *st = status;
}

/// Publishes a finished scan: index, cache and `Done`, under the status lock like
/// `publish`.
fn publish_final(
    shared: &Shared,
    gen_: u64,
    tracks: &[ScannedTrack],
    cache: HashMap<PathBuf, CachedFile>,
    unreadable: Vec<(PathBuf, String)>,
) {
    let index = Arc::new(LibraryIndex::build(tracks.to_vec()));
    let unreadable = sorted(unreadable);
    let mut st = shared.status.lock().unwrap();
    if shared.generation.load(Ordering::SeqCst) != gen_ {
        return;
    }
    *shared.index.write().unwrap() = index;
    *shared.cache.lock().unwrap() = cache;
    *st = ScanStatus::Done {
        unreadable: unreadable.len() as u64,
    };
    *shared.unreadable.lock().unwrap() = unreadable;
}

/// Recursive walk; symlinks and unreadable directories are skipped.
fn walk(dir: &Path, out: &mut Vec<PathBuf>, current: &impl Fn() -> bool) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !current() {
            return;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(&entry.path(), out, current);
        } else if kind.is_file() {
            out.push(entry.path());
        }
    }
}

fn has_audio_extension(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()))
}

fn probe(path: PathBuf) -> Probed {
    let source = match bp_decode::open(&path) {
        Ok(source) => source,
        Err(e) => return Probed::Unreadable(e.to_string()),
    };
    let mut t = ScannedTrack {
        compilation: false,
        modified_ns: 0,
        source: source.format(),
        duration: source.duration().unwrap_or(Duration::ZERO),
        album_artist: None,
        artist: None,
        album: None,
        title: None,
        year: None,
        disc: None,
        number: None,
        path,
    };
    let text = |s: Option<&str>| {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    if let Ok(tagged) = lofty::read_from_path(&t.path)
        && let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag())
    {
        t.album_artist = text(tag.get_string(ItemKey::AlbumArtist));
        t.artist = text(tag.artist().as_deref());
        t.album = text(tag.album().as_deref());
        t.title = text(tag.title().as_deref());
        t.year = tag.date().map(|d| d.year);
        t.disc = tag.disk().and_then(|n| u16::try_from(n).ok());
        t.number = tag.track().and_then(|n| u16::try_from(n).ok());
        t.compilation = tag
            .get_string(ItemKey::FlagCompilation)
            .is_some_and(is_true_flag);
    } else if let Some(tag) = dsf_tag(&t.path) {
        use id3::TagLike;
        t.album_artist = text(tag.album_artist());
        t.artist = text(tag.artist());
        t.album = text(tag.album());
        t.title = text(tag.title());
        t.year = tag
            .year()
            .or_else(|| tag.date_recorded().map(|d| d.year))
            .and_then(|y| u16::try_from(y).ok());
        t.disc = tag.disc().and_then(|n| u16::try_from(n).ok());
        t.number = tag.track().and_then(|n| u16::try_from(n).ok());
        t.compilation = tag
            .get("TCMP")
            .and_then(|f| f.content().text())
            .is_some_and(is_true_flag);
    }
    Probed::Track(Box::new(t))
}

/// A tag flag set to `1` or `true`.
fn is_true_flag(value: &str) -> bool {
    let v = value.trim();
    v == "1" || v.eq_ignore_ascii_case("true")
}

/// The front cover, else the first picture: many taggers (and every MP4 `covr` atom)
/// store the cover without marking it as the front.
fn embedded_cover(path: &Path) -> Option<Vec<u8>> {
    if let Ok(tagged) = lofty::read_from_path(path) {
        let pictures = || tagged.tags().iter().flat_map(|t| t.pictures());
        let cover = pictures()
            .find(|p| p.pic_type() == PictureType::CoverFront)
            .or_else(|| pictures().next());
        if let Some(p) = cover {
            return Some(p.data().to_vec());
        }
    }
    let tag = dsf_tag(path)?;
    let pictures = || tag.pictures();
    pictures()
        .find(|p| p.picture_type == id3::frame::PictureType::CoverFront)
        .or_else(|| pictures().next())
        .map(|p| p.data.clone())
}

/// A DSF file's ID3v2 tag, which lofty can't read: it sits at the offset stored in
/// bytes 20..28 of the "DSD " chunk (0 when there is none).
fn dsf_tag(path: &Path) -> Option<id3::Tag> {
    let mut file = File::open(path).ok()?;
    let mut head = [0u8; 28];
    file.read_exact(&mut head).ok()?;
    if &head[..4] != b"DSD " {
        return None;
    }
    let at = u64::from_le_bytes(head[20..28].try_into().expect("8 bytes"));
    if at == 0 {
        return None;
    }
    file.seek(SeekFrom::Start(at)).ok()?;
    id3::Tag::read_from2(BufReader::new(file)).ok()
}

fn folder_cover(dir: &Path) -> Option<Vec<u8>> {
    let names: HashMap<String, PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().to_lowercase(), e.path()))
        .collect();
    ART_NAMES
        .iter()
        .find_map(|n| names.get(*n))
        .and_then(|p| fs::read(p).ok())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::backend::{LibrarySource, ScanStatus};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/bp-decode/tests/fixtures")
            .join(name)
    }

    const WAIT: Duration = Duration::from_secs(10);

    #[test]
    fn fs_library_scans_tags_formats_and_art() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("music/Artist/Album");
        fs::create_dir_all(&album).unwrap();
        fs::copy(fixture("flac_24_96.flac"), album.join("01.flac")).unwrap();
        fs::write(album.join("Cover.JPG"), b"jpegbytes").unwrap();
        fs::write(album.join("notes.txt"), b"hello").unwrap();
        let lib = FsLibrary::open(dir.path().join("cfg/library.toml"));
        lib.add_folder(dir.path().join("music"));
        assert!(lib.wait_idle(WAIT));
        let albums = lib.albums(None);
        assert_eq!(albums.len(), 1);
        let a = &albums[0];
        assert_eq!(a.title, "Album");
        assert_eq!(a.format_summary, "FLAC 24/96");
        assert_eq!(lib.art(a.id), Some(b"jpegbytes".to_vec()));
        assert_eq!(lib.scan_status(), ScanStatus::Done { unreadable: 0 });
        assert_eq!(lib.tracks(a.id).len(), 1);
    }

    #[test]
    fn fs_library_scans_dsd_and_mixed_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m/Mix");
        fs::create_dir_all(&root).unwrap();
        let ch = vec![vec![0x69u8; 4096], vec![0x96u8; 4096]];
        fs::write(
            root.join("a.dsf"),
            bp_decode::testgen::dsf_bytes(2, 1, &ch, None),
        )
        .unwrap();
        fs::copy(fixture("flac_16_44.flac"), root.join("b.flac")).unwrap();
        let lib = FsLibrary::open(dir.path().join("library.toml"));
        lib.add_folder(dir.path().join("m"));
        assert!(lib.wait_idle(WAIT));
        let albums = lib.albums(None);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].format_summary, "Mixed");
        assert_eq!(lib.tracks(albums[0].id).len(), 2);
    }

    #[test]
    fn fs_library_counts_unreadable_and_persists_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("broken.flac"), b"").unwrap();
        let cfg = dir.path().join("library.toml");
        let lib = FsLibrary::open(cfg.clone());
        lib.add_folder(root.clone());
        assert!(lib.wait_idle(WAIT));
        assert_eq!(lib.scan_status(), ScanStatus::Done { unreadable: 1 });
        drop(lib);
        let lib = FsLibrary::open(cfg);
        assert_eq!(lib.folders(), vec![root.clone()]);
        lib.remove_folder(&root);
        assert!(lib.wait_idle(WAIT));
        assert!(lib.folders().is_empty());
        drop(lib);
        assert!(
            FsLibrary::open(dir.path().join("library.toml"))
                .folders()
                .is_empty()
        );
    }

    #[test]
    fn missing_folder_is_listed_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let lib = FsLibrary::open(dir.path().join("library.toml"));
        let gone = dir.path().join("does-not-exist");
        lib.add_folder(gone.clone());
        assert!(lib.wait_idle(WAIT));
        assert_eq!(lib.folders(), vec![gone]);
        assert_eq!(lib.scan_status(), ScanStatus::Done { unreadable: 0 });
    }

    /// A `Shared` whose config (and cache) live in `dir/cfg`.
    fn shared_for_test(dir: &Path) -> Shared {
        let config_file = dir.join("cfg/library.toml");
        Shared {
            cache_file: cache::cache_path(&config_file),
            cache: Mutex::new(HashMap::new()),
            config_file,
            folders: Mutex::new(Vec::new()),
            index: RwLock::new(Arc::new(LibraryIndex::default())),
            status: Mutex::new(ScanStatus::Scanning { done: 0, total: 0 }),
            unreadable: Mutex::new(Vec::new()),
            generation: AtomicU64::new(1),
        }
    }

    #[test]
    fn panicking_probe_counts_as_unreadable_and_scan_still_finishes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.flac"), b"x").unwrap();
        fs::write(dir.path().join("b.flac"), b"x").unwrap();
        let shared = shared_for_test(dir.path());
        let folders = [dir.path().to_path_buf()];
        // A panic escaping `scan_with` would fail the test; with the fix it is contained.
        scan_with(&shared, 1, &folders, 4, |_| panic!("decoder bug"));
        assert_eq!(
            *shared.status.lock().unwrap(),
            ScanStatus::Done { unreadable: 2 }
        );
        let cache = shared.cache.lock().unwrap();
        assert_eq!(cache.len(), 2);
        assert!(
            cache
                .values()
                .all(|f| f.entry == Entry::Unreadable(PANIC_REASON.to_owned()))
        );
    }

    fn named_track(path: PathBuf) -> ScannedTrack {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        ScannedTrack {
            compilation: false,
            modified_ns: 0,
            album_artist: Some("Artist".into()),
            artist: None,
            album: Some(name.clone()),
            title: Some(name),
            year: None,
            disc: None,
            number: Some(1),
            source: crate::state::support::testutil::pcm(44_100, 16),
            duration: Duration::from_secs(60),
            path,
        }
    }

    /// Scans three files with a probe slower than the publish interval; returns the
    /// album count and status the probe saw at the end of each file. (The worker runs
    /// one file ahead of publishing, so the start of a file is too early to look.)
    fn scan_seen(initial: Vec<ScannedTrack>) -> Vec<(usize, ScanStatus)> {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a", "b", "c"] {
            fs::write(dir.path().join(format!("{n}.flac")), b"x").unwrap();
        }
        let shared = shared_for_test(dir.path());
        *shared.index.write().unwrap() = Arc::new(LibraryIndex::build(initial));
        let folders = [dir.path().to_path_buf()];
        let seen = Mutex::new(Vec::new());
        // One worker: the test reads state after each file in order.
        scan_with(&shared, 1, &folders, 1, |path| {
            std::thread::sleep(Duration::from_millis(600));
            let albums = shared.index.read().unwrap().albums(None).len();
            seen.lock()
                .unwrap()
                .push((albums, shared.status.lock().unwrap().clone()));
            Probed::Track(Box::new(named_track(path)))
        });
        assert_eq!(shared.index.read().unwrap().albums(None).len(), 3);
        seen.into_inner().unwrap()
    }

    #[test]
    fn rescan_of_populated_library_publishes_only_at_the_end() {
        let old = ["a", "b", "c"]
            .iter()
            .map(|n| named_track(PathBuf::from(format!("/old/{n}.flac"))))
            .collect();
        let seen = scan_seen(old);
        assert!(seen.iter().all(|(n, _)| *n == 3), "{seen:?}");
        // Progress still advances.
        assert_eq!(seen[2].1, ScanStatus::Scanning { done: 2, total: 3 });
    }

    #[test]
    fn first_scan_publishes_partial_index() {
        let seen = scan_seen(Vec::new());
        assert_eq!(seen[2], (2, ScanStatus::Scanning { done: 2, total: 3 }));
    }

    /// Runs `scan_with` with the real probe; returns how many files it probed.
    fn scan_counting(shared: &Shared, folders: &[PathBuf]) -> usize {
        let probed = std::sync::atomic::AtomicUsize::new(0);
        scan_with(shared, 1, folders, 4, |path| {
            probed.fetch_add(1, Ordering::SeqCst);
            probe(path)
        });
        probed.into_inner()
    }

    fn one_album_folder(dir: &Path) -> (PathBuf, PathBuf) {
        let music = dir.join("m");
        let album = music.join("Al");
        fs::create_dir_all(&album).unwrap();
        let file = album.join("01.flac");
        fs::copy(fixture("flac_16_44.flac"), &file).unwrap();
        // Long enough ago that the cache trusts its stamp.
        set_modified(&file, SystemTime::now() - Duration::from_secs(3600));
        (music, file)
    }

    #[test]
    fn cached_index_is_shown_before_scanning() {
        let dir = tempfile::tempdir().unwrap();
        let (music, _) = one_album_folder(dir.path());
        let cfg = dir.path().join("cfg/library.toml");
        let lib = FsLibrary::open(cfg.clone());
        lib.add_folder(music);
        assert!(lib.wait_idle(WAIT));
        let scanned = lib.albums(None);
        drop(lib);
        let lib = FsLibrary::load_cached(cfg);
        let cached = lib.albums(None);
        assert_eq!(cached.len(), 1);
        assert_eq!(
            (cached[0].id, &cached[0].title),
            (scanned[0].id, &scanned[0].title)
        );
        assert_eq!(lib.scan_status(), ScanStatus::Done { unreadable: 0 });
    }

    #[test]
    fn albums_carry_the_file_date_after_a_scan_and_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let (music, file) = one_album_folder(dir.path());
        let modified = fs::metadata(&file).unwrap().modified().unwrap();
        let ns = modified.duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
        let cfg = dir.path().join("cfg/library.toml");
        let lib = FsLibrary::open(cfg.clone());
        lib.add_folder(music);
        assert!(lib.wait_idle(WAIT));
        assert_eq!(lib.recently_added(5)[0].added_ns, ns);
        drop(lib);
        let lib = FsLibrary::load_cached(cfg);
        assert_eq!(lib.recently_added(5)[0].added_ns, ns);
    }

    #[test]
    fn cache_entries_outside_folders_are_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let (music, _) = one_album_folder(dir.path());
        let cfg = dir.path().join("cfg/library.toml");
        let lib = FsLibrary::open(cfg.clone());
        lib.add_folder(music);
        assert!(lib.wait_idle(WAIT));
        drop(lib);
        fs::write(&cfg, "folders = [\"/elsewhere\"]\n").unwrap();
        assert!(FsLibrary::load_cached(cfg).albums(None).is_empty());
    }

    #[test]
    fn corrupt_cache_means_full_scan() {
        let dir = tempfile::tempdir().unwrap();
        let (music, _) = one_album_folder(dir.path());
        let cfg = dir.path().join("cfg/library.toml");
        fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        fs::write(cache::cache_path(&cfg), "not json").unwrap();
        let lib = FsLibrary::open(cfg);
        lib.add_folder(music);
        assert!(lib.wait_idle(WAIT));
        assert_eq!(lib.albums(None).len(), 1);
    }

    #[test]
    fn unchanged_file_is_not_probed() {
        let dir = tempfile::tempdir().unwrap();
        let (music, _) = one_album_folder(dir.path());
        let shared = shared_for_test(dir.path());
        assert_eq!(scan_counting(&shared, std::slice::from_ref(&music)), 1);
        assert_eq!(scan_counting(&shared, &[music]), 0);
        assert_eq!(shared.index.read().unwrap().albums(None).len(), 1);
    }

    fn set_modified(file: &Path, t: SystemTime) {
        File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    /// On FAT, modified times have 2 s steps: a same-size edit within the step of the scan
    /// would keep the stamp the scan saw.
    #[test]
    fn file_modified_just_before_the_scan_is_read_again_next_time() {
        let dir = tempfile::tempdir().unwrap();
        let (music, file) = one_album_folder(dir.path());
        set_modified(&file, SystemTime::now());
        let shared = shared_for_test(dir.path());
        assert_eq!(scan_counting(&shared, std::slice::from_ref(&music)), 1);
        assert_eq!(scan_counting(&shared, &[music]), 1);
    }

    #[test]
    fn modified_file_is_reprobed() {
        let dir = tempfile::tempdir().unwrap();
        let (music, file) = one_album_folder(dir.path());
        let shared = shared_for_test(dir.path());
        assert_eq!(scan_counting(&shared, std::slice::from_ref(&music)), 1);
        let later = fs::metadata(&file).unwrap().modified().unwrap() + Duration::from_secs(1);
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(scan_counting(&shared, &[music]), 1);
    }

    #[test]
    fn unstampable_unreadable_file_is_listed() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("m");
        fs::create_dir_all(&music).unwrap();
        let file = music.join("old.flac");
        fs::write(&file, b"x").unwrap();
        // Modified before 1970: no stamp, so it isn't cached.
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH - Duration::from_secs(1))
            .unwrap();
        assert_eq!(cache::stamp(&file), None);
        let lib = FsLibrary {
            shared: Arc::new(shared_for_test(dir.path())),
        };
        scan_with(&lib.shared, 1, &[music], 1, |_| {
            Probed::Unreadable("bad".into())
        });
        assert_eq!(lib.scan_status(), ScanStatus::Done { unreadable: 1 });
        assert_eq!(lib.unreadable(), [(file, "bad".to_owned())]);
    }

    #[test]
    fn progress_counts_cached_files_before_probing() {
        let dir = tempfile::tempdir().unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        for n in ["a", "b"] {
            let file = dir.path().join(format!("{n}.flac"));
            fs::write(&file, b"x").unwrap();
            set_modified(&file, old);
        }
        let shared = shared_for_test(dir.path());
        let folders = [dir.path().to_path_buf()];
        scan_with(&shared, 1, &folders, 1, |p| {
            Probed::Track(Box::new(named_track(p)))
        });
        fs::write(dir.path().join("c.flac"), b"x").unwrap();
        let seen = Mutex::new(None);
        scan_with(&shared, 1, &folders, 1, |p| {
            *seen.lock().unwrap() = Some(shared.status.lock().unwrap().clone());
            Probed::Track(Box::new(named_track(p)))
        });
        assert_eq!(
            seen.into_inner().unwrap(),
            Some(ScanStatus::Scanning { done: 2, total: 3 })
        );
    }

    #[test]
    fn deleted_file_disappears() {
        let dir = tempfile::tempdir().unwrap();
        let (music, file) = one_album_folder(dir.path());
        let shared = shared_for_test(dir.path());
        scan_counting(&shared, std::slice::from_ref(&music));
        fs::remove_file(file).unwrap();
        scan_counting(&shared, &[music]);
        assert!(shared.index.read().unwrap().albums(None).is_empty());
    }

    #[test]
    fn parallel_scan_matches_single_worker() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("m");
        fs::create_dir_all(&music).unwrap();
        for n in 0..40 {
            fs::write(music.join(format!("t{n:02}.flac")), b"x").unwrap();
        }
        let scan = |workers| {
            let shared = shared_for_test(&dir.path().join(format!("w{workers}")));
            scan_with(&shared, 1, std::slice::from_ref(&music), workers, |p| {
                Probed::Track(Box::new(named_track(p)))
            });
            let albums: Vec<_> = shared
                .index
                .read()
                .unwrap()
                .albums(None)
                .into_iter()
                .map(|a| (a.id, a.title))
                .collect();
            let status = shared.status.lock().unwrap().clone();
            (albums, status)
        };
        let one = scan(1);
        assert_eq!(one.0.len(), 40);
        assert_eq!(one, scan(8));
    }

    #[test]
    fn superseded_parallel_scan_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("m");
        fs::create_dir_all(&music).unwrap();
        for n in 0..20 {
            fs::write(music.join(format!("t{n:02}.flac")), b"x").unwrap();
        }
        let shared = shared_for_test(dir.path());
        scan_with(&shared, 1, std::slice::from_ref(&music), 4, |p| {
            shared.generation.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(50));
            Probed::Track(Box::new(named_track(p)))
        });
        // Superseded by the first probe: the progress from before it stays.
        assert_eq!(
            *shared.status.lock().unwrap(),
            ScanStatus::Scanning { done: 0, total: 20 }
        );
        assert!(shared.index.read().unwrap().is_empty());
        assert!(!shared.cache_file.exists());
    }

    #[test]
    fn non_audio_files_are_never_probed() {
        let dir = tempfile::tempdir().unwrap();
        let (music, _) = one_album_folder(dir.path());
        fs::write(music.join("Al/cover.jpg"), b"jpeg").unwrap();
        fs::write(music.join("Al/notes.txt"), b"hi").unwrap();
        let shared = shared_for_test(dir.path());
        assert_eq!(scan_counting(&shared, &[music]), 1);
    }

    /// Times a one-worker scan, a full parallel scan and a launch from the cache on a
    /// real library: `BP_SCAN_DIR=/path/to/music cargo test --release -p bp-gui --lib
    /// scan_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn scan_timing() {
        let music = PathBuf::from(std::env::var("BP_SCAN_DIR").expect("BP_SCAN_DIR"));
        let dir = tempfile::tempdir().unwrap();
        let shared = shared_for_test(&dir.path().join("one"));
        let t = Instant::now();
        if std::env::var_os("BP_SKIP_ONE").is_none() {
            scan_with(&shared, 1, std::slice::from_ref(&music), 1, probe);
        }
        println!("one worker: {:?}", t.elapsed());
        let cfg = dir.path().join("cfg/library.toml");
        let t = Instant::now();
        let lib = FsLibrary::open(cfg.clone());
        lib.add_folder(music);
        assert!(lib.wait_idle(Duration::from_secs(600)));
        println!("{} workers: {:?}", worker_count(), t.elapsed());
        let albums = lib.albums(None).len();
        drop(lib);
        let t = Instant::now();
        let lib = FsLibrary::open(cfg);
        println!(
            "cached index shown: {:?} ({} albums)",
            t.elapsed(),
            lib.albums(None).len()
        );
        assert!(lib.wait_idle(Duration::from_secs(600)));
        println!("rescan with cache: {:?} ({albums} albums)", t.elapsed());
        println!("status: {:?}", lib.scan_status());
    }

    #[test]
    fn unreadable_lists_reasons() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("m");
        fs::create_dir_all(&music).unwrap();
        fs::write(music.join("broken.flac"), b"").unwrap();
        fs::write(music.join("fake.m4a"), b"not audio").unwrap();
        let cfg = dir.path().join("cfg/library.toml");
        let lib = FsLibrary::open(cfg.clone());
        lib.add_folder(music.clone());
        assert!(lib.wait_idle(WAIT));
        let list = lib.unreadable();
        let paths: Vec<_> = list.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(paths, [music.join("broken.flac"), music.join("fake.m4a")]);
        assert!(
            list.iter().all(|(_, reason)| !reason.is_empty()),
            "{list:?}"
        );
        drop(lib);
        assert_eq!(FsLibrary::load_cached(cfg).unreadable(), list);
    }

    #[test]
    fn compilation_flag_is_read() {
        use lofty::config::WriteOptions;
        use lofty::tag::{Tag, TagType};
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("m");
        fs::create_dir_all(&music).unwrap();
        for (file, artist) in [("x.flac", "X"), ("y.flac", "Y")] {
            let f = music.join(file);
            fs::copy(fixture("flac_16_44.flac"), &f).unwrap();
            let mut tag = Tag::new(TagType::VorbisComments);
            tag.set_artist(artist.into());
            tag.set_album("Hits".into());
            tag.insert_text(ItemKey::FlagCompilation, "1".into());
            tag.save_to_path(&f, WriteOptions::default()).unwrap();
        }
        let lib = FsLibrary::open(dir.path().join("library.toml"));
        lib.add_folder(music);
        assert!(lib.wait_idle(WAIT));
        let albums = lib.albums(None);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].artist, "Various Artists");
    }

    #[test]
    fn dsf_compilation_flag_is_read() {
        let mut tag = id3::Tag::new();
        {
            use id3::TagLike;
            tag.set_artist("X");
            tag.set_album("Hits");
            tag.add_frame(id3::Frame::text("TCMP", "1"));
        }
        let mut id3 = Vec::new();
        tag.write_to(&mut id3, id3::Version::Id3v24).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("m");
        fs::create_dir_all(&music).unwrap();
        let bytes =
            bp_decode::testgen::dsf_bytes(2, 64, &[vec![0x69; 8192], vec![0x69; 8192]], Some(&id3));
        fs::write(music.join("01.dsf"), bytes).unwrap();
        let lib = FsLibrary::open(dir.path().join("library.toml"));
        lib.add_folder(music);
        assert!(lib.wait_idle(WAIT));
        assert_eq!(lib.albums(None)[0].artist, "Various Artists");
    }

    #[test]
    fn folder_art_is_found_in_the_album_folder() {
        let dir = tempfile::tempdir().unwrap();
        let disc = dir.path().join("m/Al/CD1");
        fs::create_dir_all(&disc).unwrap();
        fs::copy(fixture("flac_16_44.flac"), disc.join("01.flac")).unwrap();
        fs::write(dir.path().join("m/Al/cover.jpg"), b"parent art").unwrap();
        let lib = FsLibrary::open(dir.path().join("library.toml"));
        lib.add_folder(dir.path().join("m"));
        assert!(lib.wait_idle(WAIT));
        assert_eq!(
            lib.art(lib.albums(None)[0].id),
            Some(b"parent art".to_vec())
        );
    }

    fn tag_flac(path: &Path, cover: Option<PictureType>) {
        use lofty::config::WriteOptions;
        use lofty::picture::{MimeType, Picture};
        use lofty::tag::{Tag, TagType};
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::AlbumArtist, "Miles Davis".into());
        tag.set_artist("M. Davis".into());
        tag.set_album("Kind of Blue".into());
        tag.set_title("So What".into());
        tag.insert_text(ItemKey::RecordingDate, "1959".into());
        tag.set_disk(2);
        tag.set_track(3);
        if let Some(kind) = cover {
            tag.push_picture(
                Picture::unchecked(b"embedded".to_vec())
                    .pic_type(kind)
                    .mime_type(MimeType::Jpeg)
                    .build(),
            );
        }
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    #[test]
    fn tags_and_embedded_art_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("m/whatever");
        fs::create_dir_all(&album).unwrap();
        let f = album.join("x.flac");
        fs::copy(fixture("flac_24_96.flac"), &f).unwrap();
        tag_flac(&f, Some(PictureType::CoverFront));
        let lib = FsLibrary::open(dir.path().join("library.toml"));
        lib.add_folder(dir.path().join("m"));
        assert!(lib.wait_idle(WAIT));
        let albums = lib.albums(None);
        assert_eq!(albums.len(), 1);
        let a = &albums[0];
        assert_eq!(
            (a.artist.as_str(), a.title.as_str(), a.year),
            ("Miles Davis", "Kind of Blue", Some(1959))
        );
        let t = &lib.tracks(a.id)[0];
        assert_eq!((t.title.as_str(), t.disc, t.number), ("So What", 2, 3));
        assert_eq!(lib.art(a.id), Some(b"embedded".to_vec()));
    }

    #[test]
    fn a_picture_not_marked_front_cover_is_used_as_art() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("m/Al");
        fs::create_dir_all(&album).unwrap();
        let f = album.join("x.flac");
        fs::copy(fixture("flac_24_96.flac"), &f).unwrap();
        tag_flac(&f, Some(PictureType::Other));
        let lib = FsLibrary::open(dir.path().join("library.toml"));
        lib.add_folder(dir.path().join("m"));
        assert!(lib.wait_idle(WAIT));
        assert_eq!(lib.art(lib.albums(None)[0].id), Some(b"embedded".to_vec()));
    }

    #[test]
    fn dsf_id3_tags_and_art_are_read() {
        use id3::TagLike;
        let mut tag = id3::Tag::new();
        tag.set_album_artist("Stevie Wonder");
        tag.set_album("Songs in the Key of Life");
        tag.set_title("Pastime Paradise");
        tag.set_year(1976);
        tag.set_disc(1);
        tag.set_track(5);
        tag.add_frame(id3::frame::Picture {
            mime_type: "image/jpeg".into(),
            picture_type: id3::frame::PictureType::Other,
            description: String::new(),
            data: b"dsf cover".to_vec(),
        });
        let mut id3 = Vec::new();
        tag.write_to(&mut id3, id3::Version::Id3v24).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("m");
        fs::create_dir_all(&album).unwrap();
        let bytes =
            bp_decode::testgen::dsf_bytes(2, 64, &[vec![0x69; 8192], vec![0x69; 8192]], Some(&id3));
        fs::write(album.join("05.dsf"), bytes).unwrap();
        let lib = FsLibrary::open(dir.path().join("library.toml"));
        lib.add_folder(album);
        assert!(lib.wait_idle(WAIT));
        let a = &lib.albums(None)[0];
        assert_eq!(
            (a.artist.as_str(), a.title.as_str(), a.year),
            ("Stevie Wonder", "Songs in the Key of Life", Some(1976))
        );
        let t = &lib.tracks(a.id)[0];
        assert_eq!(
            (t.title.as_str(), t.disc, t.number),
            ("Pastime Paradise", 1, 5)
        );
        assert_eq!(lib.art(a.id), Some(b"dsf cover".to_vec()));
    }

    #[test]
    fn untagged_art_falls_back_to_none_without_cover_file() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("m/Al");
        fs::create_dir_all(&album).unwrap();
        fs::copy(fixture("flac_24_96.flac"), album.join("x.flac")).unwrap();
        let lib = FsLibrary::open(dir.path().join("library.toml"));
        lib.add_folder(dir.path().join("m"));
        assert!(lib.wait_idle(WAIT));
        assert_eq!(lib.art(lib.albums(None)[0].id), None);
    }

    #[test]
    fn back_to_back_rescans_and_add_folder_during_scan_end_done() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["one", "two"] {
            let d = dir.path().join(n).join("Album");
            fs::create_dir_all(&d).unwrap();
            fs::copy(fixture("flac_16_44.flac"), d.join("t.flac")).unwrap();
        }
        let lib = FsLibrary::open(dir.path().join("library.toml"));
        lib.add_folder(dir.path().join("one"));
        lib.rescan();
        lib.rescan();
        lib.add_folder(dir.path().join("two"));
        assert!(lib.wait_idle(WAIT));
        assert_eq!(lib.scan_status(), ScanStatus::Done { unreadable: 0 });
        assert_eq!(lib.albums(None).len(), 2);
    }
}

//! Background album-art loading: thumbnails, page art and blurred backdrops.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crossbeam_channel::Receiver;
use image::imageops::{self, FilterType};
use image::{DynamicImage, RgbaImage};

use crate::backend::{AlbumId, LibrarySource};

const THUMB_SIDE: u32 = 256;
const PAGE_SIDE: u32 = 1024;
const THUMB_CAPACITY: usize = 600;
const BACKDROP_SMALL_WIDTH: u32 = 64;
const BACKDROP_WIDTH: u32 = 512;
const BACKDROP_SIGMA: f32 = 6.0;
const BACKDROP_DIM: f32 = 0.35;
/// Side the cover is shrunk to before its colour is measured.
const COLOR_SIDE: u32 = 64;
const HUE_BUCKETS: usize = 12;
/// Share of measured pixels that must be coloured for the album to have a colour.
const MIN_COLORED: f32 = 0.05;

/// Colours taken from an album's cover: the two tints of the now-playing background.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlbumColors {
    /// Centre of the now-playing gradient.
    pub tint_hi: egui::Color32,
    /// Edges of the now-playing gradient.
    pub tint_lo: egui::Color32,
}

/// `(hue in degrees, saturation, lightness)`.
pub(crate) fn to_hsl(c: egui::Color32) -> (f32, f32, f32) {
    let [r, g, b] = [c.r(), c.g(), c.b()].map(|v| f32::from(v) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s.min(1.0), l)
}

pub(crate) fn from_hsl(h: f32, s: f32, l: f32) -> egui::Color32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let u = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgb(u(r), u(g), u(b))
}

/// The cover's dominant colour, or `None` for a grey, black or white cover.
pub fn album_colors(img: &RgbaImage) -> Option<AlbumColors> {
    let small;
    let img = if img.width() > COLOR_SIDE || img.height() > COLOR_SIDE {
        small = imageops::thumbnail(img, COLOR_SIDE, COLOR_SIDE);
        &small
    } else {
        img
    };
    // Per hue bucket: saturation weight, hue as a weighted vector, weighted s and l.
    let mut buckets = [(0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32); HUE_BUCKETS];
    let mut measured = 0usize;
    let mut colored = 0usize;
    for p in img.pixels() {
        if p[3] < 128 {
            continue;
        }
        measured += 1;
        let (h, s, l) = to_hsl(egui::Color32::from_rgb(p[0], p[1], p[2]));
        if s < 0.2 || !(0.12..=0.92).contains(&l) {
            continue;
        }
        colored += 1;
        let b = &mut buckets[(h / (360.0 / HUE_BUCKETS as f32)) as usize % HUE_BUCKETS];
        let rad = h.to_radians();
        b.0 += s;
        b.1 += s * rad.sin();
        b.2 += s * rad.cos();
        b.3 += s * s;
        b.4 += s * l;
    }
    if measured == 0 || (colored as f32) < MIN_COLORED * measured as f32 {
        return None;
    }
    let (w, sin, cos, ss, _) = buckets.into_iter().max_by(|a, b| a.0.total_cmp(&b.0))?;
    let h = sin.atan2(cos).to_degrees().rem_euclid(360.0);
    let s = ss / w;
    let tint_sat = s.min(0.7);
    Some(AlbumColors {
        tint_hi: from_hsl(h, tint_sat, 0.28),
        tint_lo: from_hsl(h, tint_sat, 0.10),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtKind {
    Thumb,
    Page,
    Backdrop,
}

#[derive(Clone)]
pub enum ArtState {
    Ready(egui::TextureHandle),
    Loading,
    Missing,
}

/// Decode and shrink to fit within `side` x `side`, keeping aspect. Never upscales.
fn fit(bytes: &[u8], side: u32) -> Option<RgbaImage> {
    let img = image::load_from_memory(bytes).ok()?;
    Some(fit_image(img, side))
}

fn fit_image(img: DynamicImage, side: u32) -> RgbaImage {
    if img.width() > side || img.height() > side {
        img.resize(side, side, FilterType::Lanczos3).to_rgba8()
    } else {
        img.to_rgba8()
    }
}

pub fn make_thumb(bytes: &[u8]) -> Option<RgbaImage> {
    fit(bytes, THUMB_SIDE)
}

pub fn make_backdrop(bytes: &[u8]) -> Option<RgbaImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let (w, h) = (img.width() as u64, img.height() as u64);
    if w == 0 || h == 0 {
        return None;
    }
    // Fit within 64x64 (64 wide for landscape/square), aspect kept.
    let side = BACKDROP_SMALL_WIDTH as u64;
    let (sw, sh) = if w >= h {
        (side, (h * side / w).max(1))
    } else {
        ((w * side / h).max(1), side)
    };
    let small = img
        .resize_exact(sw as u32, sh as u32, FilterType::Triangle)
        .to_rgba8();
    let mut blurred = imageops::fast_blur(&small, BACKDROP_SIGMA);
    for p in blurred.pixels_mut() {
        for c in &mut p.0[..3] {
            *c = (*c as f32 * BACKDROP_DIM).round() as u8;
        }
    }
    // Same scale factor on both axes: at most 512x512.
    let scale = (BACKDROP_WIDTH / BACKDROP_SMALL_WIDTH) as u64;
    Some(imageops::resize(
        &blurred,
        (sw * scale) as u32,
        (sh * scale) as u32,
        FilterType::Triangle,
    ))
}

struct Request {
    album: AlbumId,
    kind: ArtKind,
}

struct Done {
    album: AlbumId,
    kind: ArtKind,
    /// `None`: dropped unloaded, because no frame asked for it lately.
    image: Option<Option<RgbaImage>>,
    /// Measured from thumbnails only.
    colors: Option<AlbumColors>,
}

/// Work for the art workers. Page and backdrop art go first, in order; thumbnails
/// are served newest-first, and one not requested in the current or previous frame
/// (scrolled away) is dropped before it is decoded.
#[derive(Default)]
struct Queue {
    urgent: VecDeque<Request>,
    /// Newest last.
    thumbs: Vec<AlbumId>,
    /// The frame each queued thumbnail was last requested in.
    wanted: HashMap<AlbumId, u64>,
    /// The newest frame seen by `texture()`.
    frame: u64,
    closed: bool,
}

enum Job {
    Load(Request),
    Drop(AlbumId),
}

impl Queue {
    fn next(&mut self) -> Option<Job> {
        if let Some(req) = self.urgent.pop_front() {
            return Some(Job::Load(req));
        }
        let album = self.thumbs.pop()?;
        let last = self.wanted.remove(&album).unwrap_or(0);
        Some(if last + 1 >= self.frame {
            Job::Load(Request {
                album,
                kind: ArtKind::Thumb,
            })
        } else {
            Job::Drop(album)
        })
    }
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Shared {
    /// Blocks for the next job; `None` once the loader is dropped.
    fn take(&self) -> Option<Job> {
        let mut q = self.queue.lock().unwrap();
        loop {
            if q.closed {
                return None;
            }
            if let Some(job) = q.next() {
                return Some(job);
            }
            q = self.ready.wait(q).unwrap();
        }
    }
}

#[derive(Clone)]
enum Entry {
    Ready(egui::TextureHandle),
    Missing,
}

impl Entry {
    fn state(&self) -> ArtState {
        match self {
            Entry::Ready(h) => ArtState::Ready(h.clone()),
            Entry::Missing => ArtState::Missing,
        }
    }
}

pub struct ArtLoader {
    shared: Arc<Shared>,
    results: Receiver<Done>,
    pending: HashSet<(AlbumId, ArtKind)>,
    /// Thumbnails with their last-use tick (LRU).
    thumbs: HashMap<AlbumId, (Entry, u64)>,
    tick: u64,
    page: Option<(AlbumId, Entry)>,
    /// Cover colours of every album whose thumbnail has loaded.
    colors: HashMap<AlbumId, AlbumColors>,
    backdrop: Option<(AlbumId, Entry)>,
    /// Album whose Page / Backdrop art is currently wanted; other results are stale.
    wanted_page: Option<AlbumId>,
    wanted_backdrop: Option<AlbumId>,
    cache_dir: PathBuf,
}

impl ArtLoader {
    pub fn new(
        lib: Arc<dyn LibrarySource>,
        cache_dir: PathBuf,
        workers: usize,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let _ = std::fs::create_dir_all(&cache_dir);
        let shared = Arc::new(Shared::default());
        let (done_tx, done_rx) = crossbeam_channel::unbounded::<Done>();
        for _ in 0..workers.max(1) {
            let shared = shared.clone();
            let done_tx = done_tx.clone();
            let lib = lib.clone();
            let cache_dir = cache_dir.clone();
            let waker = waker.clone();
            thread::spawn(move || {
                // Ends when the loader is dropped.
                while let Some(job) = shared.take() {
                    let done = match job {
                        Job::Load(req) => {
                            // A panicking library or decoder counts as missing art; the
                            // worker lives on.
                            let image =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    load(&*lib, &cache_dir, req.album, req.kind)
                                }))
                                .unwrap_or(None);
                            let colors = match (&image, req.kind) {
                                (Some(img), ArtKind::Thumb) => album_colors(img),
                                _ => None,
                            };
                            Done {
                                album: req.album,
                                kind: req.kind,
                                image: Some(image),
                                colors,
                            }
                        }
                        Job::Drop(album) => Done {
                            album,
                            kind: ArtKind::Thumb,
                            image: None,
                            colors: None,
                        },
                    };
                    if done_tx.send(done).is_err() {
                        break;
                    }
                    waker();
                }
            });
        }
        Self {
            shared,
            results: done_rx,
            pending: HashSet::new(),
            thumbs: HashMap::new(),
            tick: 0,
            page: None,
            backdrop: None,
            colors: HashMap::new(),
            wanted_page: None,
            wanted_backdrop: None,
            cache_dir,
        }
    }

    pub fn texture(&mut self, ctx: &egui::Context, album: AlbumId, kind: ArtKind) -> ArtState {
        self.drain(ctx);
        self.tick += 1;
        match kind {
            ArtKind::Page => self.wanted_page = Some(album),
            ArtKind::Backdrop => self.wanted_backdrop = Some(album),
            ArtKind::Thumb => {}
        }
        let cached = match kind {
            ArtKind::Thumb => self.thumbs.get_mut(&album).map(|(e, t)| {
                *t = self.tick;
                e.state()
            }),
            ArtKind::Page => Self::single(&mut self.page, album),
            ArtKind::Backdrop => Self::single(&mut self.backdrop, album),
        };
        if let Some(state) = cached {
            return state;
        }
        let mut q = self.shared.queue.lock().unwrap();
        // Monotonic, whichever context the caller passes.
        q.frame = q.frame.max(ctx.cumulative_frame_nr());
        let frame = q.frame;
        if self.pending.insert((album, kind)) {
            match kind {
                ArtKind::Thumb => {
                    q.thumbs.push(album);
                    q.wanted.insert(album, frame);
                }
                ArtKind::Page | ArtKind::Backdrop => q.urgent.push_back(Request { album, kind }),
            }
            self.shared.ready.notify_one();
        } else if let Some(last) = q.wanted.get_mut(&album)
            && kind == ArtKind::Thumb
        {
            *last = frame; // still queued and still on screen
        }
        ArtState::Loading
    }

    /// The album's cover colours, once its thumbnail has loaded.
    pub fn colors(&self, album: AlbumId) -> Option<AlbumColors> {
        self.colors.get(&album).copied()
    }

    /// Forgets every `Missing` result so it is requested again: a finished scan may
    /// have found art for it.
    pub fn forget_missing(&mut self) {
        self.thumbs.retain(|_, (e, _)| !matches!(e, Entry::Missing));
        for slot in [&mut self.page, &mut self.backdrop] {
            if matches!(slot, Some((_, Entry::Missing))) {
                *slot = None;
            }
        }
    }

    /// Deletes, off the UI thread, the cached thumbnails of albums not in `keep`: album
    /// ids change when tags or folders do, which would leave old files forever.
    pub fn prune(&mut self, keep: HashSet<AlbumId>) {
        self.colors.retain(|a, _| keep.contains(a));
        let dir = self.cache_dir.clone();
        thread::spawn(move || prune_thumbs(&dir, &keep));
    }

    /// Drops the page art and backdrop textures.
    pub fn release_page(&mut self) {
        self.page = None;
        self.backdrop = None;
        self.wanted_page = None;
        self.wanted_backdrop = None;
    }

    /// Returns the slot's state if it holds `album`; a slot for another album is
    /// dropped so only one album is kept.
    fn single(slot: &mut Option<(AlbumId, Entry)>, album: AlbumId) -> Option<ArtState> {
        match slot {
            Some((a, e)) if *a == album => Some(e.state()),
            Some(_) => {
                *slot = None;
                None
            }
            None => None,
        }
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.results.try_recv() {
            self.pending.remove(&(done.album, done.kind));
            if let Some(c) = done.colors {
                self.colors.insert(done.album, c);
            }
            let Some(image) = done.image else {
                continue; // dropped unloaded; requested again if it comes back on screen
            };
            let wanted = match done.kind {
                ArtKind::Thumb => Some(done.album),
                ArtKind::Page => self.wanted_page,
                ArtKind::Backdrop => self.wanted_backdrop,
            };
            if wanted != Some(done.album) {
                continue; // stale: discard without uploading
            }
            let entry = match image {
                Some(img) => {
                    let size = [img.width() as usize, img.height() as usize];
                    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                    let name = format!("art-{:016x}-{:?}", done.album.0, done.kind);
                    Entry::Ready(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
                }
                None => Entry::Missing,
            };
            match done.kind {
                ArtKind::Thumb => {
                    self.thumbs.insert(done.album, (entry, self.tick));
                    self.evict();
                }
                ArtKind::Page => self.page = Some((done.album, entry)),
                ArtKind::Backdrop => self.backdrop = Some((done.album, entry)),
            }
        }
    }

    fn evict(&mut self) {
        while self.thumbs.len() > THUMB_CAPACITY {
            let Some(oldest) = self
                .thumbs
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| *k)
            else {
                break;
            };
            self.thumbs.remove(&oldest);
        }
    }
}

impl Drop for ArtLoader {
    fn drop(&mut self) {
        // Ends the workers.
        self.shared.queue.lock().unwrap().closed = true;
        self.shared.ready.notify_all();
    }
}

fn thumb_path(cache_dir: &Path, album: AlbumId) -> PathBuf {
    cache_dir.join(format!("{:016x}.png", album.0))
}

/// Deletes the thumbnails in `cache_dir` whose album is not in `keep`; other files stay.
fn prune_thumbs(cache_dir: &Path, keep: &HashSet<AlbumId>) {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let id = (path.extension().is_some_and(|e| e == "png"))
            .then(|| path.file_stem()?.to_str())
            .flatten()
            .filter(|stem| stem.len() == 16)
            .and_then(|stem| u64::from_str_radix(stem, 16).ok());
        if let Some(id) = id
            && !keep.contains(&AlbumId(id))
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn load(
    lib: &dyn LibrarySource,
    cache_dir: &Path,
    album: AlbumId,
    kind: ArtKind,
) -> Option<RgbaImage> {
    match kind {
        ArtKind::Thumb => {
            let path = thumb_path(cache_dir, album);
            if let Ok(bytes) = std::fs::read(&path)
                && let Ok(img) = image::load_from_memory(&bytes)
            {
                return Some(img.to_rgba8());
            }
            let thumb = make_thumb(&lib.art(album)?)?;
            // A failed cache write is not an error.
            let _ = thumb.save_with_format(&path, image::ImageFormat::Png);
            Some(thumb)
        }
        ArtKind::Page => fit(&lib.art(album)?, PAGE_SIDE),
        ArtKind::Backdrop => make_backdrop(&lib.art(album)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use crate::backend::index::MemLibrary;
    use crate::backend::{Album, ArtistId, FilterResult, ScanStatus, Track};

    const JPEG_HEADER_ONLY: [u8; 6] = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];

    fn encode(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        let img = RgbaImage::from_pixel(w, h, image::Rgba(px));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// MemLibrary wrapper that counts `art()` calls.
    struct CountingLib {
        inner: MemLibrary,
        calls: AtomicUsize,
        /// Albums in the order `art()` was called for them.
        order: std::sync::Mutex<Vec<AlbumId>>,
        slow: Option<AlbumId>,
        panics: Option<AlbumId>,
    }

    impl CountingLib {
        fn new() -> Arc<Self> {
            Self::with(None, None)
        }

        fn with(slow: Option<AlbumId>, panics: Option<AlbumId>) -> Arc<Self> {
            Arc::new(Self {
                inner: MemLibrary::new(vec![], vec![]),
                calls: AtomicUsize::new(0),
                order: std::sync::Mutex::new(Vec::new()),
                slow,
                panics,
            })
        }
    }

    impl LibrarySource for CountingLib {
        fn artists(&self) -> Vec<(ArtistId, String)> {
            self.inner.artists()
        }
        fn albums(&self, artist: Option<ArtistId>) -> Vec<Album> {
            self.inner.albums(artist)
        }
        fn recently_added(&self, limit: usize) -> Vec<Album> {
            self.inner.recently_added(limit)
        }
        fn album(&self, id: AlbumId) -> Option<Album> {
            self.inner.album(id)
        }
        fn tracks(&self, album: AlbumId) -> Vec<Track> {
            self.inner.tracks(album)
        }
        fn track(&self, id: crate::backend::TrackId) -> Option<Track> {
            self.inner.track(id)
        }
        fn art(&self, album: AlbumId) -> Option<Vec<u8>> {
            self.order.lock().unwrap().push(album);
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.panics == Some(album) {
                panic!("art() panic injected by test");
            }
            if self.slow == Some(album) {
                thread::sleep(Duration::from_millis(400));
            }
            self.inner.art(album)
        }
        fn filter(&self, query: &str) -> FilterResult {
            self.inner.filter(query)
        }
        fn folders(&self) -> Vec<PathBuf> {
            self.inner.folders()
        }
        fn add_folder(&self, path: PathBuf) {
            self.inner.add_folder(path)
        }
        fn remove_folder(&self, path: &Path) {
            self.inner.remove_folder(path)
        }
        fn rescan(&self) {
            self.inner.rescan()
        }
        fn scan_status(&self) -> ScanStatus {
            self.inner.scan_status()
        }
        fn unreadable(&self) -> Vec<(PathBuf, String)> {
            self.inner.unreadable()
        }
    }

    fn loader(lib: &Arc<CountingLib>, dir: &Path) -> ArtLoader {
        loader_n(lib, dir, 2)
    }

    fn loader_n(lib: &Arc<CountingLib>, dir: &Path, workers: usize) -> ArtLoader {
        ArtLoader::new(lib.clone(), dir.to_path_buf(), workers, Arc::new(|| {}))
    }

    /// Polls `texture()` until it leaves Loading, or panics after 5 s.
    fn wait(l: &mut ArtLoader, id: AlbumId, kind: ArtKind) -> ArtState {
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match l.texture(&ctx, id, kind) {
                ArtState::Loading => {
                    assert!(Instant::now() < deadline, "art never arrived");
                    thread::sleep(Duration::from_millis(5));
                }
                s => return s,
            }
        }
    }

    #[test]
    fn prune_removes_thumbnails_of_albums_no_longer_listed() {
        let dir = tempfile::tempdir().unwrap();
        for id in [1, 2] {
            std::fs::write(thumb_path(dir.path(), AlbumId(id)), b"png").unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), b"keep").unwrap();
        prune_thumbs(dir.path(), &HashSet::from([AlbumId(1)]));
        assert!(thumb_path(dir.path(), AlbumId(1)).exists());
        assert!(!thumb_path(dir.path(), AlbumId(2)).exists());
        assert!(dir.path().join("notes.txt").exists());
    }

    #[test]
    fn corrupt_art_yields_none() {
        assert!(make_thumb(&JPEG_HEADER_ONLY).is_none());
        assert!(make_backdrop(b"nope").is_none());
    }

    #[test]
    fn huge_art_thumbnail_is_256() {
        let t = make_thumb(&encode(6000, 3000, [10, 20, 30, 255])).unwrap();
        assert_eq!((t.width(), t.height()), (256, 128));
    }

    #[test]
    fn backdrop_is_dark() {
        let b = make_backdrop(&encode(300, 300, [255, 255, 255, 255])).unwrap();
        assert_eq!(b.width(), 512);
        let limit = (0.36 * 255.0) as u8;
        assert!(b.pixels().all(|p| p.0[..3].iter().all(|&c| c <= limit)));
    }

    #[test]
    fn disk_cache_hit_skips_library() {
        let dir = tempfile::tempdir().unwrap();
        let lib = CountingLib::new();
        let id = AlbumId(0xabc);
        lib.inner.set_art(id, encode(400, 400, [200, 50, 50, 255]));

        let mut first = loader(&lib, dir.path());
        assert!(matches!(
            wait(&mut first, id, ArtKind::Thumb),
            ArtState::Ready(_)
        ));
        assert_eq!(lib.calls.load(Ordering::SeqCst), 1);
        assert!(dir.path().join("0000000000000abc.png").exists());

        let mut second = loader(&lib, dir.path());
        assert!(matches!(
            wait(&mut second, id, ArtKind::Thumb),
            ArtState::Ready(_)
        ));
        assert_eq!(lib.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn missing_art_reports_missing() {
        let dir = tempfile::tempdir().unwrap();
        let lib = CountingLib::new();
        let mut l = loader(&lib, dir.path());
        let id = AlbumId(7);
        assert!(matches!(
            wait(&mut l, id, ArtKind::Thumb),
            ArtState::Missing
        ));
        // Missing is cached: no re-request.
        let ctx = egui::Context::default();
        assert!(matches!(
            l.texture(&ctx, id, ArtKind::Thumb),
            ArtState::Missing
        ));
        assert_eq!(lib.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn forget_missing_rerequests_missing_art() {
        let dir = tempfile::tempdir().unwrap();
        let lib = CountingLib::new();
        let mut l = loader(&lib, dir.path());
        let id = AlbumId(7);
        for kind in [ArtKind::Thumb, ArtKind::Page] {
            assert!(matches!(wait(&mut l, id, kind), ArtState::Missing));
        }
        assert_eq!(lib.calls.load(Ordering::SeqCst), 2);
        // A rescan found a cover.
        lib.inner.set_art(id, encode(50, 50, [1, 2, 3, 255]));
        l.forget_missing();
        for kind in [ArtKind::Thumb, ArtKind::Page] {
            assert!(matches!(wait(&mut l, id, kind), ArtState::Ready(_)));
        }
        assert_eq!(lib.calls.load(Ordering::SeqCst), 4);
    }

    /// Spins until `art()` has been called `n` times.
    fn wait_calls(lib: &CountingLib, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while lib.calls.load(Ordering::SeqCst) < n {
            assert!(Instant::now() < deadline, "art() never called");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn page_request_is_served_before_queued_thumbnails() {
        let dir = tempfile::tempdir().unwrap();
        let slow = AlbumId(1);
        let page = AlbumId(2);
        let lib = CountingLib::with(Some(slow), None);
        lib.inner.set_art(page, encode(50, 50, [1, 2, 3, 255]));
        let mut l = loader_n(&lib, dir.path(), 1);
        let ctx = egui::Context::default();
        l.texture(&ctx, slow, ArtKind::Thumb);
        wait_calls(&lib, 1); // the only worker is busy
        for id in 10..40 {
            l.texture(&ctx, AlbumId(id), ArtKind::Thumb);
        }
        assert!(matches!(
            wait(&mut l, page, ArtKind::Page),
            ArtState::Ready(_)
        ));
        assert_eq!(lib.order.lock().unwrap()[..2], [slow, page]);
    }

    #[test]
    fn thumbnails_are_served_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let slow = AlbumId(1);
        let lib = CountingLib::with(Some(slow), None);
        let mut l = loader_n(&lib, dir.path(), 1);
        let ctx = egui::Context::default();
        l.texture(&ctx, slow, ArtKind::Thumb);
        wait_calls(&lib, 1);
        for id in 10..13 {
            l.texture(&ctx, AlbumId(id), ArtKind::Thumb);
        }
        wait_calls(&lib, 4);
        assert_eq!(
            *lib.order.lock().unwrap(),
            [slow, AlbumId(12), AlbumId(11), AlbumId(10)]
        );
    }

    #[test]
    fn thumbnail_no_longer_requested_is_dropped_unloaded() {
        let dir = tempfile::tempdir().unwrap();
        let slow = AlbumId(1);
        let gone = AlbumId(2);
        let lib = CountingLib::with(Some(slow), None);
        let mut l = loader_n(&lib, dir.path(), 1);
        let ctx = egui::Context::default();
        l.texture(&ctx, slow, ArtKind::Thumb);
        wait_calls(&lib, 1);
        l.texture(&ctx, gone, ArtKind::Thumb);
        // Two frames pass in which only `slow` is still on screen.
        for _ in 0..2 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
            out.textures_delta.clear();
            l.texture(&ctx, slow, ArtKind::Thumb);
        }
        assert!(matches!(
            wait(&mut l, slow, ArtKind::Thumb),
            ArtState::Missing
        ));
        thread::sleep(Duration::from_millis(100));
        assert_eq!(*lib.order.lock().unwrap(), [slow], "dropped before loading");
        // Back on screen: requested again.
        assert!(matches!(
            wait(&mut l, gone, ArtKind::Thumb),
            ArtState::Missing
        ));
        assert_eq!(*lib.order.lock().unwrap(), [slow, gone]);
    }

    #[test]
    fn page_is_kept_for_one_album_only() {
        let dir = tempfile::tempdir().unwrap();
        let lib = CountingLib::new();
        lib.inner
            .set_art(AlbumId(1), encode(2000, 1000, [1, 2, 3, 255]));
        lib.inner
            .set_art(AlbumId(2), encode(10, 10, [1, 2, 3, 255]));
        let mut l = loader(&lib, dir.path());
        assert!(matches!(
            wait(&mut l, AlbumId(1), ArtKind::Page),
            ArtState::Ready(_)
        ));
        assert!(matches!(
            wait(&mut l, AlbumId(2), ArtKind::Page),
            ArtState::Ready(_)
        ));
        assert!(l.page.as_ref().is_some_and(|(a, _)| *a == AlbumId(2)));
        l.release_page();
        assert!(l.page.is_none() && l.backdrop.is_none());
    }

    #[test]
    fn late_page_result_for_old_album_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let (a1, a2) = (AlbumId(1), AlbumId(2));
        let lib = CountingLib::with(Some(a1), None);
        lib.inner.set_art(a1, encode(50, 50, [1, 2, 3, 255]));
        lib.inner.set_art(a2, encode(50, 50, [4, 5, 6, 255]));
        let mut l = loader(&lib, dir.path());
        let ctx = egui::Context::default();
        assert!(matches!(
            l.texture(&ctx, a1, ArtKind::Page),
            ArtState::Loading
        ));
        assert!(matches!(
            wait(&mut l, a2, ArtKind::Page),
            ArtState::Ready(_)
        ));
        // Let the slow A1 result arrive; it must not replace A2.
        thread::sleep(Duration::from_millis(700));
        assert!(matches!(
            l.texture(&ctx, a2, ArtKind::Page),
            ArtState::Ready(_)
        ));
        assert!(l.page.as_ref().is_some_and(|(a, _)| *a == a2));
    }

    #[test]
    fn result_after_release_page_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let a = AlbumId(1);
        let lib = CountingLib::with(Some(a), None);
        lib.inner.set_art(a, encode(50, 50, [1, 2, 3, 255]));
        let mut l = loader(&lib, dir.path());
        let ctx = egui::Context::default();
        l.texture(&ctx, a, ArtKind::Page);
        l.texture(&ctx, a, ArtKind::Backdrop);
        l.release_page();
        thread::sleep(Duration::from_millis(900));
        l.drain(&ctx);
        assert!(l.page.is_none() && l.backdrop.is_none());
    }

    #[test]
    fn panicking_library_yields_missing_and_worker_survives() {
        let dir = tempfile::tempdir().unwrap();
        let bad = AlbumId(1);
        let good = AlbumId(2);
        let lib = CountingLib::with(None, Some(bad));
        lib.inner.set_art(good, encode(50, 50, [1, 2, 3, 255]));
        let mut l = loader_n(&lib, dir.path(), 1);
        assert!(matches!(
            wait(&mut l, bad, ArtKind::Thumb),
            ArtState::Missing
        ));
        assert!(matches!(
            wait(&mut l, good, ArtKind::Thumb),
            ArtState::Ready(_)
        ));
    }

    #[test]
    fn very_tall_art_is_clamped() {
        let bytes = encode(10, 10000, [9, 9, 9, 255]);
        let t = make_thumb(&bytes).unwrap();
        assert!(t.width() <= 256 && t.height() <= 256);
        let b = make_backdrop(&bytes).unwrap();
        assert!(
            b.width() <= 512 && b.height() <= 512,
            "{}x{}",
            b.width(),
            b.height()
        );
    }

    fn hue_distance(a: f32, b: f32) -> f32 {
        let d = (a - b).abs() % 360.0;
        d.min(360.0 - d)
    }

    #[test]
    fn solid_red_gives_red_tints() {
        let img = RgbaImage::from_pixel(64, 64, image::Rgba([0xD7, 0x26, 0x3D, 255]));
        let c = album_colors(&img).expect("a colour");
        let (h, _, _) = to_hsl(c.tint_hi);
        assert!(hue_distance(h, 353.0) < 15.0, "hue {h}");
        let (_, _, hi) = to_hsl(c.tint_hi);
        let (_, _, lo) = to_hsl(c.tint_lo);
        assert!((hi - 0.28).abs() < 0.03, "tint_hi {hi}");
        assert!((lo - 0.10).abs() < 0.03, "tint_lo {lo}");
    }

    #[test]
    fn dominant_bucket_wins_over_grey() {
        let mut img = RgbaImage::from_pixel(10, 10, image::Rgba([128, 128, 128, 255]));
        for (i, p) in img.pixels_mut().enumerate() {
            if i >= 70 {
                *p = image::Rgba([40, 80, 200, 255]);
            }
        }
        let c = album_colors(&img).expect("a colour");
        let (h, _, _) = to_hsl(c.tint_hi);
        assert!(hue_distance(h, 225.0) < 15.0, "hue {h}");
    }

    #[test]
    fn greyscale_or_near_black_has_no_colour() {
        let grey = RgbaImage::from_fn(64, 64, |x, _| {
            let v = (x * 4) as u8;
            image::Rgba([v, v, v, 255])
        });
        assert_eq!(album_colors(&grey), None);
        let black = RgbaImage::from_pixel(64, 64, image::Rgba([5, 5, 5, 255]));
        assert_eq!(album_colors(&black), None);
    }

    #[test]
    fn colors_arrive_with_the_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        let lib = CountingLib::new();
        let red = AlbumId(1);
        let none = AlbumId(2);
        lib.inner
            .set_art(red, encode(32, 32, [0xD7, 0x26, 0x3D, 255]));
        let mut l = loader(&lib, dir.path());
        assert!(matches!(
            wait(&mut l, red, ArtKind::Thumb),
            ArtState::Ready(_)
        ));
        assert!(l.colors(red).is_some());
        assert!(matches!(
            wait(&mut l, none, ArtKind::Thumb),
            ArtState::Missing
        ));
        assert_eq!(l.colors(none), None);
    }
}

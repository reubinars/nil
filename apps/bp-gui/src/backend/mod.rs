//! Contracts between the GUI and its backends (library and player).

pub mod cache;
pub mod engine_player;
pub mod fs_library;
pub mod index;
pub mod mock_player;

use std::path::{Path, PathBuf};
use std::time::Duration;

use bp_core::{DeviceId, DeviceInfo, SourceFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtistId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AlbumId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u64);

#[derive(Debug, Clone)]
pub struct Album {
    pub id: AlbumId,
    pub artist_id: ArtistId,
    pub title: String,
    pub artist: String,
    pub year: Option<u16>,
    pub duration: Duration,
    pub format_summary: String,
    /// Newest modified time of the album's files, in ns since the epoch; 0 if unknown.
    pub added_ns: u64,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub id: TrackId,
    pub album: AlbumId,
    pub path: PathBuf,
    pub disc: u16,
    pub number: u16,
    pub title: String,
    pub duration: Duration,
    pub source: SourceFormat,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterResult {
    pub artists: Vec<ArtistId>,
    pub albums: Vec<AlbumId>,
    pub tracks: Vec<TrackId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScanStatus {
    Idle,
    Scanning { done: u64, total: u64 },
    Done { unreadable: u64 },
}

pub trait LibrarySource: Send + Sync {
    /// Natural sort.
    fn artists(&self) -> Vec<(ArtistId, String)>;
    /// `None` = all albums.
    fn albums(&self, artist: Option<ArtistId>) -> Vec<Album>;
    /// At most `limit` albums, newest file first.
    fn recently_added(&self, limit: usize) -> Vec<Album>;
    fn album(&self, id: AlbumId) -> Option<Album>;
    /// Disc, then track order.
    fn tracks(&self, album: AlbumId) -> Vec<Track>;
    fn track(&self, id: TrackId) -> Option<Track>;
    /// Encoded image; called off the UI thread.
    fn art(&self, album: AlbumId) -> Option<Vec<u8>>;
    fn filter(&self, query: &str) -> FilterResult;
    fn folders(&self) -> Vec<PathBuf>;
    fn add_folder(&self, path: PathBuf);
    fn remove_folder(&self, path: &Path);
    /// Returns immediately.
    fn rescan(&self);
    fn scan_status(&self) -> ScanStatus;
    /// Files the last scan couldn't read, with the reason; natural sort by path.
    fn unreadable(&self) -> Vec<(PathBuf, String)>;
    /// Bumped by every finished scan, including ones the UI didn't start: a change
    /// means the index may have changed.
    fn revision(&self) -> u64 {
        0
    }
    /// Called from any thread when a scan starts or finishes on its own.
    fn set_waker(&self, _waker: Box<dyn Fn() + Send + Sync>) {}
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueItem {
    pub path: PathBuf,
    pub source: SourceFormat,
    pub duration: Duration,
    pub title: String,
}

pub use bp_engine::DeviceCaps;

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerCommand {
    Play { items: Vec<QueueItem>, start: usize },
    Pause,
    Resume,
    Stop,
    Next,
    Previous,
    Seek { position: Duration },
    SelectDevice { id: DeviceId },
    Reprobe { id: DeviceId },
    RefreshDevices,
}

#[derive(Debug)]
pub enum PlayerEvent {
    Engine(bp_engine::Event),
    DevicesChanged,
}

pub trait PlayerHandle {
    fn send(&self, cmd: PlayerCommand);
    fn try_recv(&self) -> Option<PlayerEvent>;
    fn devices(&self) -> (Vec<DeviceInfo>, Option<DeviceId>);
    fn device_caps(&self, id: &DeviceId) -> Option<DeviceCaps>;
    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>);
    /// A copy of the audio being played, for the spectrum; refreshed on each call.
    fn analysis(&self) -> Option<bp_engine::AnalysisTap>;
}

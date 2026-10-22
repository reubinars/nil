//! The saved library index, `library-cache.json`: one entry per audio file with the size
//! and modified time it was read at, so a rescan only re-reads files that changed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::backend::index::ScannedTrack;

pub const CACHE_VERSION: u32 = 3;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Entry {
    Track(ScannedTrack),
    /// Why the file couldn't be read.
    Unreadable(String),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CachedFile {
    pub path: PathBuf,
    pub size: u64,
    /// [`UNTRUSTED`] if the file was modified too close to the scan to trust it.
    pub mtime_ns: u64,
    pub entry: Entry,
}

#[derive(Serialize, Deserialize)]
struct CacheFile<F> {
    version: u32,
    files: F,
}

pub fn cache_path(config_file: &Path) -> PathBuf {
    config_file.with_file_name("library-cache.json")
}

/// `None` when missing, unparsable or another version.
pub fn load(path: &Path) -> Option<Vec<CachedFile>> {
    let text = std::fs::read_to_string(path).ok()?;
    let cache: CacheFile<Vec<CachedFile>> = serde_json::from_str(&text).ok()?;
    (cache.version == CACHE_VERSION).then_some(cache.files)
}

/// Writes via a temporary file so a crash never leaves a half-written cache.
pub fn save(path: &Path, files: &[CachedFile]) -> std::io::Result<()> {
    let text = serde_json::to_string(&CacheFile {
        version: CACHE_VERSION,
        files,
    })
    .map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Each save has its own temporary file: a superseded scan can still be saving.
    static SAVES: AtomicU64 = AtomicU64::new(0);
    let n = SAVES.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}-{n}.tmp", std::process::id()));
    let saved = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if saved.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    saved
}

/// A modified time no file has: the entry is read again by the next scan.
pub const UNTRUSTED: u64 = u64::MAX;

/// How close to a scan's start a file's modified time must be for its stamp not to be
/// trusted: FAT records modified times in 2 s steps, so a same-size edit within that step
/// keeps the stamp the scan saw.
pub const RACY_WINDOW: Duration = Duration::from_secs(2);

/// `(size, mtime_ns)`; `None` if the metadata or modified time can't be read.
pub fn stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((meta.len(), u64::try_from(mtime.as_nanos()).ok()?))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::state::support::testutil::pcm;

    fn files() -> Vec<CachedFile> {
        let track = ScannedTrack {
            compilation: false,
            modified_ns: 0,
            path: PathBuf::from("/m/a.flac"),
            album_artist: Some("A".into()),
            artist: None,
            album: Some("Al".into()),
            title: Some("T".into()),
            year: Some(1999),
            disc: Some(1),
            number: Some(2),
            source: pcm(96_000, 24),
            duration: Duration::from_millis(61_500),
        };
        vec![
            CachedFile {
                path: track.path.clone(),
                size: 10,
                mtime_ns: 20,
                entry: Entry::Track(track),
            },
            CachedFile {
                path: PathBuf::from("/m/b.m4a"),
                size: 30,
                mtime_ns: 40,
                entry: Entry::Unreadable("x".into()),
            },
        ]
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/library-cache.json");
        save(&path, &files()).unwrap();
        assert_eq!(load(&path), Some(files()));
    }

    /// A superseded scan can still be saving when the next one saves.
    #[test]
    fn concurrent_saves_leave_one_whole_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library-cache.json");
        // Big enough that the writes interleave.
        let big = |n: u64| -> Vec<CachedFile> {
            (0..5_000)
                .map(|i| CachedFile {
                    path: PathBuf::from(format!("/m/{n}/{i}.flac")),
                    size: i,
                    mtime_ns: n,
                    entry: Entry::Unreadable("x".into()),
                })
                .collect()
        };
        let sets = [big(1), big(2)];
        for _ in 0..10 {
            let results: Vec<_> = std::thread::scope(|s| {
                let saves: Vec<_> = sets.iter().map(|f| s.spawn(|| save(&path, f))).collect();
                saves.into_iter().map(|h| h.join().unwrap()).collect()
            });
            for r in results {
                r.unwrap();
            }
            let loaded = load(&path).expect("a whole cache");
            assert!(sets.contains(&loaded));
        }
    }

    #[test]
    fn corrupt_or_wrong_version_loads_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library-cache.json");
        assert_eq!(load(&path), None);
        std::fs::write(&path, "{").unwrap();
        assert_eq!(load(&path), None);
        let other = format!(r#"{{"version": {}, "files": []}}"#, CACHE_VERSION + 1);
        std::fs::write(&path, other).unwrap();
        assert_eq!(load(&path), None);
    }

    #[test]
    fn version_2_cache_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library-cache.json");
        std::fs::write(&path, r#"{"version": 2, "files": []}"#).unwrap();
        assert_eq!(load(&path), None);
    }

    #[test]
    fn stamp_reads_size_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"abc").unwrap();
        let (size, mtime) = stamp(&path).unwrap();
        assert_eq!(size, 3);
        assert!(mtime > 0);
        assert_eq!(stamp(&dir.path().join("missing")), None);
    }
}

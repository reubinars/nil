//! Maps absolute file paths to library tracks, so plugins can name tracks by path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::backend::{AlbumId, LibrarySource, QueueItem};

#[derive(Debug, Clone)]
pub struct CatalogTrack {
    pub item: QueueItem,
    pub title: String,
    /// The album's artist: the library has no per-track artist.
    pub artist: String,
    pub album: String,
    pub album_id: AlbumId,
    pub duration_s: Option<f64>,
}

/// The library's tracks by path, as of one library revision.
pub struct Catalog {
    revision: u64,
    /// Library order (album, disc, number).
    tracks: Vec<CatalogTrack>,
    by_path: HashMap<String, usize>,
}

/// The key paths are compared by. Windows paths ignore case and slash direction.
pub fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        s.to_lowercase().replace('/', "\\")
    } else {
        s.into_owned()
    }
}

impl Catalog {
    pub fn build(lib: &dyn LibrarySource) -> Catalog {
        // Read the revision first: a scan finishing mid-build then triggers a rebuild.
        let revision = lib.revision();
        let mut tracks = Vec::new();
        let mut by_path = HashMap::new();
        for album in lib.albums(None) {
            for t in lib.tracks(album.id) {
                let secs = t.duration.as_secs_f64();
                by_path.insert(path_key(&t.path), tracks.len());
                tracks.push(CatalogTrack {
                    title: t.title.clone(),
                    artist: album.artist.clone(),
                    album: album.title.clone(),
                    album_id: album.id,
                    duration_s: (secs > 0.0).then_some(secs),
                    item: QueueItem {
                        path: t.path,
                        source: t.source,
                        duration: t.duration,
                        title: t.title,
                    },
                });
            }
        }
        Catalog {
            revision,
            tracks,
            by_path,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn get(&self, path: &Path) -> Option<&CatalogTrack> {
        self.by_path.get(&path_key(path)).map(|&i| &self.tracks[i])
    }

    pub fn tracks(&self) -> impl Iterator<Item = &CatalogTrack> {
        self.tracks.iter()
    }

    /// The queue items for `paths`, in order, or every unknown path in input order.
    pub fn resolve(&self, paths: &[PathBuf]) -> Result<Vec<QueueItem>, Vec<PathBuf>> {
        let mut items = Vec::with_capacity(paths.len());
        let mut unknown = Vec::new();
        for p in paths {
            match self.get(p) {
                Some(t) => items.push(t.item.clone()),
                None => unknown.push(p.clone()),
            }
        }
        if unknown.is_empty() {
            Ok(items)
        } else {
            Err(unknown)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bp_core::{Codec, SourceFormat, SourceKind};

    use super::*;
    use crate::backend::index::{MemLibrary, ScannedTrack};

    fn scanned(path: &str, album: &str, title: &str, n: u16, secs: u64) -> ScannedTrack {
        ScannedTrack {
            path: PathBuf::from(path),
            album_artist: Some("Ada".into()),
            artist: None,
            album: Some(album.into()),
            title: Some(title.into()),
            year: None,
            disc: Some(1),
            number: Some(n),
            compilation: false,
            modified_ns: 0,
            source: SourceFormat {
                codec: Codec::Flac,
                sample_rate: 44100,
                channels: 2,
                kind: SourceKind::Pcm {
                    bits: 16,
                    float: false,
                },
            },
            duration: Duration::from_secs(secs),
        }
    }

    fn lib() -> MemLibrary {
        let base = if cfg!(windows) {
            "E:\\Music\\A"
        } else {
            "/music/A"
        };
        let sep = if cfg!(windows) { "\\" } else { "/" };
        MemLibrary::new(
            vec![
                scanned(&format!("{base}{sep}01.flac"), "First", "One", 1, 90),
                scanned(&format!("{base}{sep}02.flac"), "First", "Two", 2, 0),
            ],
            vec![],
        )
    }

    fn p(name: &str) -> PathBuf {
        let sep = if cfg!(windows) { "\\" } else { "/" };
        let base = if cfg!(windows) {
            "E:\\Music\\A"
        } else {
            "/music/A"
        };
        PathBuf::from(format!("{base}{sep}{name}"))
    }

    #[test]
    fn get_returns_title_album_artist_and_duration() {
        let cat = Catalog::build(&lib());
        let t = cat.get(&p("01.flac")).expect("known path");
        assert_eq!(
            (t.title.as_str(), t.album.as_str(), t.artist.as_str()),
            ("One", "First", "Ada")
        );
        assert_eq!(t.duration_s, Some(90.0));
        assert_eq!(t.item.title, "One");
        assert_eq!(cat.get(&p("02.flac")).unwrap().duration_s, None);
        let order: Vec<_> = cat.tracks().map(|t| t.title.as_str()).collect();
        assert_eq!(order, ["One", "Two"]);
    }

    #[test]
    fn resolve_reports_every_unknown_path_in_order() {
        let cat = Catalog::build(&lib());
        let ok = cat.resolve(&[p("02.flac"), p("01.flac")]).unwrap();
        let titles: Vec<_> = ok.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["Two", "One"]);
        let err = cat
            .resolve(&[p("z.flac"), p("01.flac"), p("y.flac")])
            .unwrap_err();
        assert_eq!(err, [p("z.flac"), p("y.flac")]);
    }

    #[cfg(windows)]
    #[test]
    fn lookup_ignores_case_and_slashes_on_windows() {
        let cat = Catalog::build(&lib());
        let t = cat.get(Path::new("e:/music/a/01.FLAC")).expect("same file");
        assert_eq!(t.title, "One");
        assert!(cat.get(Path::new("E:\\MUSIC\\A\\02.flac")).is_some());
    }
}

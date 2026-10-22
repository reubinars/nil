//! Search results: artists, albums and tracks matching a query.

use crate::backend::{Album, LibrarySource, Track};
use crate::state::artists::ArtistTile;

/// Most tracks listed for one query.
pub const TRACK_LIMIT: usize = 50;

#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    pub query: String,
    pub artists: Vec<ArtistTile>,
    pub albums: Vec<Album>,
    /// Each matching track with its album.
    pub tracks: Vec<(Track, Album)>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.artists.is_empty() && self.albums.is_empty() && self.tracks.is_empty()
    }
}

/// What matches `query` by name or title; `tiles` supplies the artists' tiles.
pub fn search(lib: &dyn LibrarySource, tiles: &[ArtistTile], query: &str) -> SearchResults {
    let hits = lib.filter(query);
    SearchResults {
        query: query.to_owned(),
        artists: tiles
            .iter()
            .filter(|t| hits.artists.contains(&t.id))
            .cloned()
            .collect(),
        albums: hits.albums.iter().filter_map(|&id| lib.album(id)).collect(),
        tracks: hits
            .tracks
            .iter()
            .take(TRACK_LIMIT)
            .filter_map(|&id| {
                let t = lib.track(id)?;
                let album = lib.album(t.album)?;
                Some((t, album))
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::backend::index::{MemLibrary, ScannedTrack};
    use crate::state::artists::artist_tiles;
    use crate::state::support::testutil::pcm;

    fn st(artist: &str, album: &str, title: &str, n: u16) -> ScannedTrack {
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
            number: Some(n),
            source: pcm(44_100, 16),
            duration: Duration::from_secs(1),
        }
    }

    #[test]
    fn search_groups_hits() {
        let lib = MemLibrary::new(
            vec![
                st("Miles Davis", "Kind of Blue", "So What", 1),
                st("Miles Davis", "Kind of Blue", "Blue in Green", 3),
                st("Radiohead", "Kid A", "Idioteque", 8),
            ],
            vec![],
        );
        let tiles = artist_tiles(&lib);
        let r = search(&lib, &tiles, "blue");
        assert_eq!(r.query, "blue");
        assert!(r.artists.is_empty());
        let albums: Vec<_> = r.albums.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(albums, vec!["Kind of Blue"]);
        let tracks: Vec<_> = r
            .tracks
            .iter()
            .map(|(t, a)| (t.title.as_str(), a.title.as_str()))
            .collect();
        assert_eq!(tracks, vec![("Blue in Green", "Kind of Blue")]);
        let miles = search(&lib, &tiles, "miles");
        assert_eq!(miles.artists.len(), 1);
        assert_eq!(miles.artists[0].name, "Miles Davis");
    }

    #[test]
    fn tracks_are_capped() {
        let lib = MemLibrary::new(
            (1..=60)
                .map(|n| st("A", "Loops", &format!("Loop {n}"), n))
                .collect(),
            vec![],
        );
        let r = search(&lib, &artist_tiles(&lib), "loop ");
        assert_eq!(r.tracks.len(), TRACK_LIMIT);
    }
}

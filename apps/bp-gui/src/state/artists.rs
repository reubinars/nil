//! Artist tiles: each artist with an album count and a cover.

use std::collections::HashMap;

use crate::backend::{AlbumId, ArtistId, LibrarySource};

#[derive(Debug, Clone, PartialEq)]
pub struct ArtistTile {
    pub id: ArtistId,
    pub name: String,
    pub albums: usize,
    /// The artist's first album, whose art stands for the artist.
    pub cover: Option<AlbumId>,
}

/// One tile per artist, in `artists()` order; one pass over all albums.
pub fn artist_tiles(lib: &dyn LibrarySource) -> Vec<ArtistTile> {
    let mut by_artist: HashMap<ArtistId, (usize, AlbumId)> = HashMap::new();
    for album in lib.albums(None) {
        by_artist
            .entry(album.artist_id)
            .and_modify(|(n, _)| *n += 1)
            .or_insert((1, album.id));
    }
    lib.artists()
        .into_iter()
        .map(|(id, name)| {
            let found = by_artist.get(&id);
            ArtistTile {
                id,
                name,
                albums: found.map_or(0, |(n, _)| *n),
                cover: found.map(|(_, a)| *a),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::index::MemLibrary;
    use crate::backend::index::ScannedTrack;
    use crate::state::support::testutil::pcm;
    use std::path::PathBuf;
    use std::time::Duration;

    fn st(artist: &str, album: &str) -> ScannedTrack {
        ScannedTrack {
            compilation: false,
            modified_ns: 0,
            path: PathBuf::from(format!("/m/{artist}/{album}/t.flac")),
            album_artist: Some(artist.into()),
            artist: None,
            album: Some(album.into()),
            title: Some("t".into()),
            year: None,
            disc: None,
            number: Some(1),
            source: pcm(44_100, 16),
            duration: Duration::from_secs(1),
        }
    }

    fn lib() -> MemLibrary {
        MemLibrary::new(
            vec![
                st("Miles", "Kob"),
                st("Miles", "Sketches"),
                st("Coltrane", "Giant"),
            ],
            vec![],
        )
    }

    #[test]
    fn tiles_count_albums_and_take_the_first_cover() {
        let lib = lib();
        let tiles = artist_tiles(&lib);
        let albums = lib.albums(None);
        let first = |artist: &str| albums.iter().find(|a| a.artist == artist).unwrap().id;
        let names: Vec<_> = tiles.iter().map(|t| (t.name.as_str(), t.albums)).collect();
        assert_eq!(names, vec![("Coltrane", 1), ("Miles", 2)]);
        assert_eq!(tiles[1].cover, Some(first("Miles")));
        assert_eq!(tiles[0].cover, Some(first("Coltrane")));
    }

    #[test]
    fn reads_albums_once() {
        let lib = lib();
        artist_tiles(&lib);
        assert_eq!(lib.album_reads(), 1);
    }
}

//! `.m3u8` playlists: plain UTF-8 files other players can read, one per playlist.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use crate::catalog::{CatalogTrack, path_key};

/// One playlist line, holding only what the file says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_s: Option<u64>,
}

const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return true;
    }
    let digit = |p: &str| {
        stem.strip_prefix(p)
            .is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']))
    };
    digit("COM") || digit("LPT")
}

/// Turns a playlist name into one that is a valid file stem on Windows.
pub fn sanitize(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = replaced.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return "Playlist".to_string();
    }
    if is_reserved(trimmed) {
        // Windows reserves the part before the first dot, whatever follows it.
        let (head, tail) = trimmed.split_at(trimmed.find('.').unwrap_or(trimmed.len()));
        return format!("{head}_{tail}");
    }
    trimmed.to_string()
}

/// The file for an already-sanitised name.
/// Appended to a playlist's name while a case-only rename passes through a temp file.
const RENAMING: &str = ".renaming";

fn stored_path(dir: &Path, stored: &str) -> PathBuf {
    dir.join(format!("{stored}.m3u8"))
}

fn file_path(dir: &Path, name: &str) -> PathBuf {
    stored_path(dir, &sanitize(name))
}

/// Control characters would start new lines in the file.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The names of the playlists in `dir`, in natural order.
pub fn list(dir: &Path) -> io::Result<Vec<String>> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for ent in rd {
        let p = ent?.path();
        let is_m3u8 = p
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("m3u8"));
        if is_m3u8 && let Some(stem) = p.file_stem() {
            let stem = stem.to_string_lossy();
            // A temp file left by an interrupted rename is not a playlist.
            if !stem.ends_with(RENAMING) {
                names.push(stem.into_owned());
            }
        }
    }
    names.sort_by(|a, b| natord::compare(a, b));
    Ok(names)
}

/// Windows-style absolute paths count on every platform, so a library synced
/// from Windows still reads right elsewhere.
fn is_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    Path::new(s).is_absolute()
        || s.starts_with(['\\', '/'])
        || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

fn parse_extinf(rest: &str) -> (Option<u64>, Option<String>, Option<String>) {
    let (num, text) = rest.split_once(',').unwrap_or((rest, ""));
    let duration = num.trim().parse::<u64>().ok();
    let text = text.trim();
    if text.is_empty() {
        return (duration, None, None);
    }
    match text.split_once(" - ") {
        Some((artist, title)) => (duration, Some(title.to_string()), Some(artist.to_string())),
        None => (duration, Some(text.to_string()), None),
    }
}

/// Reads a playlist. Relative paths are taken from `dir`.
pub fn read(dir: &Path, name: &str) -> io::Result<Vec<Entry>> {
    let text = fs::read_to_string(file_path(dir, name))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut out = Vec::new();
    let mut pending: Option<(Option<u64>, Option<String>, Option<String>)> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            pending = Some(parse_extinf(rest));
        } else if line.starts_with('#') {
            continue;
        } else {
            let (duration_s, title, artist) = pending.take().unwrap_or((None, None, None));
            let path = if is_absolute(line) {
                PathBuf::from(line)
            } else {
                dir.join(line)
            };
            out.push(Entry {
                path,
                title,
                artist,
                duration_s,
            });
        }
    }
    Ok(out)
}

/// Whether a playlist of this name exists (after sanitising).
pub fn exists(dir: &Path, name: &str) -> bool {
    file_path(dir, name).is_file()
}

/// Writes a playlist atomically and returns the stored name.
pub fn save(dir: &Path, name: &str, tracks: &[Entry]) -> io::Result<String> {
    fs::create_dir_all(dir)?;
    let stored = sanitize(name);
    let target = stored_path(dir, &stored);
    let tmp = dir.join(format!("{stored}.m3u8.tmp"));
    let mut body = String::from("#EXTM3U\r\n");
    for t in tracks {
        let secs = t.duration_s.map_or("-1".to_string(), |s| s.to_string());
        let (artist, title) = (
            t.artist.as_deref().map(one_line),
            t.title.as_deref().map(one_line),
        );
        let text = match (&artist, &title) {
            (Some(a), Some(ti)) => format!("{a} - {ti}"),
            (None, Some(ti)) => ti.clone(),
            (Some(a), None) => a.clone(),
            (None, None) => String::new(),
        };
        body.push_str(&format!("#EXTINF:{secs},{text}\r\n"));
        body.push_str(&t.path.to_string_lossy());
        body.push_str("\r\n");
    }
    fs::write(&tmp, body)?;
    if let Err(e) = fs::rename(&tmp, &target) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(stored)
}

/// Renames a playlist; refuses to replace another one.
pub fn rename(dir: &Path, from: &str, to: &str) -> io::Result<String> {
    let new = sanitize(to);
    let src = file_path(dir, from);
    let dst = stored_path(dir, &new);
    if dst.exists() {
        // On a case-insensitive filesystem a case-only rename names the same file.
        let same = matches!((src.canonicalize(), dst.canonicalize()), (Ok(a), Ok(b)) if a == b);
        if !same {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                format!("a playlist named {new} already exists"),
            ));
        }
        if src.file_name() != dst.file_name() {
            // A direct rename may keep the old case, so go through a temp name.
            let tmp = stored_path(dir, &format!("{new}{RENAMING}"));
            fs::rename(&src, &tmp)?;
            fs::rename(&tmp, &dst)?;
        }
        return Ok(new);
    }
    fs::rename(src, dst)?;
    Ok(new)
}

pub fn delete(dir: &Path, name: &str) -> io::Result<()> {
    fs::remove_file(file_path(dir, name))
}

/// What `add` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddResult {
    pub added: usize,
    pub skipped: usize,
}

fn gone(name: &str) -> io::Error {
    io::Error::new(ErrorKind::NotFound, format!("{name} no longer exists"))
}

/// Reads a playlist for an edit; a missing one is an error, never recreated.
fn read_existing(dir: &Path, name: &str) -> io::Result<Vec<Entry>> {
    match read(dir, name) {
        Err(e) if e.kind() == ErrorKind::NotFound => Err(gone(name)),
        r => r,
    }
}

/// Whether the file still holds exactly what the view showed: same paths, same order.
fn same_rows(file: &[Entry], shown: &[Entry]) -> bool {
    file.len() == shown.len()
        && file
            .iter()
            .zip(shown)
            .all(|(a, b)| path_key(&a.path) == path_key(&b.path))
}

fn check(name: &str, file: &[Entry], shown: &[Entry]) -> io::Result<()> {
    if same_rows(file, shown) {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{name} changed on disk. Reopen it."
        )))
    }
}

fn bad_index(name: &str) -> io::Error {
    io::Error::new(
        ErrorKind::InvalidInput,
        format!("{name} changed on disk. Reopen it."),
    )
}

/// Adds `tracks` to the end, skipping paths already there and repeats within `tracks`.
pub fn add(dir: &Path, name: &str, tracks: &[Entry]) -> io::Result<AddResult> {
    let mut rows = read_existing(dir, name)?;
    let mut seen: HashSet<String> = rows.iter().map(|e| path_key(&e.path)).collect();
    let before = rows.len();
    for t in tracks {
        if seen.insert(path_key(&t.path)) {
            rows.push(t.clone());
        }
    }
    let added = rows.len() - before;
    if added > 0 {
        save(dir, name, &rows)?;
    }
    Ok(AddResult {
        added,
        skipped: tracks.len() - added,
    })
}

/// Removes the rows at `indices`; refuses unless the file still holds `shown`.
pub fn remove(dir: &Path, name: &str, shown: &[Entry], indices: &[usize]) -> io::Result<()> {
    let rows = read_existing(dir, name)?;
    check(name, &rows, shown)?;
    if indices.iter().any(|&i| i >= rows.len()) {
        return Err(bad_index(name));
    }
    let drop: HashSet<usize> = indices.iter().copied().collect();
    let kept: Vec<Entry> = rows
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !drop.contains(i))
        .map(|(_, e)| e)
        .collect();
    save(dir, name, &kept).map(|_| ())
}

/// The order `reorder` writes: the moved indices (kept in their order) go in front of the
/// row at `to` (`len` = the end). `to` is an index into the current file.
pub fn reordered(len: usize, indices: &[usize], to: usize) -> Vec<usize> {
    let moved: BTreeSet<usize> = indices.iter().copied().filter(|&i| i < len).collect();
    let to = to.min(len);
    let mut out: Vec<usize> = (0..to).filter(|i| !moved.contains(i)).collect();
    out.extend(moved.iter().copied());
    out.extend((to..len).filter(|i| !moved.contains(i)));
    out
}

/// Moves the rows at `indices`, keeping their order, in front of the row at `to`;
/// refuses unless the file still holds `shown`. Writes nothing if the order is unchanged.
pub fn reorder(
    dir: &Path,
    name: &str,
    shown: &[Entry],
    indices: &[usize],
    to: usize,
) -> io::Result<()> {
    let rows = read_existing(dir, name)?;
    check(name, &rows, shown)?;
    if indices.iter().any(|&i| i >= rows.len()) {
        return Err(bad_index(name));
    }
    let order = reordered(rows.len(), indices, to);
    if order.iter().enumerate().all(|(pos, &i)| pos == i) {
        return Ok(());
    }
    let out: Vec<Entry> = order.iter().map(|&i| rows[i].clone()).collect();
    save(dir, name, &out).map(|_| ())
}

/// Creates an empty playlist; refuses an existing name.
pub fn create(dir: &Path, name: &str) -> io::Result<String> {
    if exists(dir, name) {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            format!("a playlist named {} already exists", sanitize(name)),
        ));
    }
    save(dir, name, &[])
}

/// Undo: writes `before` only if the file still holds `expected`.
pub fn restore(dir: &Path, name: &str, expected: &[Entry], before: &[Entry]) -> io::Result<()> {
    let rows = read_existing(dir, name)?;
    if !same_rows(&rows, expected) {
        return Err(io::Error::other(format!(
            "{name} changed since. Can't undo."
        )));
    }
    save(dir, name, before).map(|_| ())
}

/// `name`, or the first free "name (2)", "name (3)", ...
pub fn free_name(dir: &Path, name: &str) -> String {
    if !exists(dir, name) {
        return name.to_string();
    }
    (2..)
        .map(|n| format!("{name} ({n})"))
        .find(|c| !exists(dir, c))
        .expect("an unbounded range")
}

/// The playlist line for a library track.
pub fn entry_for(t: &CatalogTrack) -> Entry {
    Entry {
        path: t.item.path.clone(),
        title: Some(t.title.clone()),
        artist: Some(t.artist.clone()),
        duration_s: t.duration_s.map(|s| s.round() as u64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, title: &str, artist: &str, secs: u64) -> Entry {
        Entry {
            path: PathBuf::from(path),
            title: Some(title.into()),
            artist: Some(artist.into()),
            duration_s: Some(secs),
        }
    }

    #[test]
    fn round_trip_keeps_japanese_names_and_order() {
        let d = tempfile::tempdir().unwrap();
        let e = vec![
            entry(
                r"E:\Music\宇多田ヒカル\01 First Love.flac",
                "First Love",
                "宇多田ヒカル",
                257,
            ),
            entry(r"E:\Music\b.flac", "B", "A - B", 3),
        ];
        assert_eq!(save(d.path(), "夜の曲", &e).unwrap(), "夜の曲");
        let got = read(d.path(), "夜の曲").unwrap();
        assert_eq!(got[0], e[0]);
        assert_eq!(got[1].path, e[1].path);
        let raw = std::fs::read(d.path().join("夜の曲.m3u8")).unwrap();
        assert!(raw.starts_with(b"#EXTM3U\r\n#EXTINF:257,"));
        assert!(!raw.starts_with(&[0xEF, 0xBB, 0xBF]), "no BOM written");
    }

    #[test]
    fn sanitize_follows_windows_rules() {
        assert_eq!(sanitize(r#"a<b>c:d"e/f\g|h?i*j"#), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitize("mix. . "), "mix");
        assert_eq!(sanitize("con"), "con_");
        assert_eq!(sanitize("COM1.mix"), "COM1_.mix");
        assert_eq!(sanitize("CON"), "CON_");
        assert_eq!(sanitize("\u{7}"), "_");
        assert_eq!(sanitize(""), "Playlist");
        assert_eq!(sanitize("COM10"), "COM10");
    }

    #[test]
    fn exists_uses_the_sanitised_name() {
        let d = tempfile::tempdir().unwrap();
        save(d.path(), "a_b", &[]).unwrap();
        assert!(exists(d.path(), "a/b"));
    }

    #[test]
    fn save_overwrites_atomically_and_leaves_no_tmp() {
        let d = tempfile::tempdir().unwrap();
        save(d.path(), "p", &[entry("/a.flac", "A", "X", 1)]).unwrap();
        save(d.path(), "p", &[entry("/b.flac", "B", "X", 2)]).unwrap();
        let got = read(d.path(), "p").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title.as_deref(), Some("B"));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn reads_foobar2000_style_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("f.m3u8"),
            "\u{feff}#EXTM3U\n#EXTGRP:x\nsub/a.flac\n",
        )
        .unwrap();
        assert_eq!(
            read(d.path(), "f").unwrap(),
            vec![Entry {
                path: d.path().join("sub/a.flac"),
                title: None,
                artist: None,
                duration_s: None
            }]
        );
    }

    #[test]
    fn extinf_edge_cases_and_none_duration() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("f.m3u8"),
            "#EXTM3U\r\n#EXTINF:-1,Just A Title\r\n/a.flac\r\n#EXTINF:x,B - C\r\n/b.flac\r\n",
        )
        .unwrap();
        std::fs::write(d.path().join("h.M3U8"), "").unwrap();
        assert_eq!(list(d.path()).unwrap(), vec!["f", "h"]);
        let got = read(d.path(), "f").unwrap();
        assert_eq!(got[0].duration_s, None);
        assert_eq!(got[0].title.as_deref(), Some("Just A Title"));
        assert_eq!(got[0].artist, None);
        assert_eq!(got[1].duration_s, None);
        assert_eq!(got[1].artist.as_deref(), Some("B"));
        let e = Entry {
            duration_s: None,
            ..got[1].clone()
        };
        save(d.path(), "g", &[e]).unwrap();
        let raw = std::fs::read_to_string(d.path().join("g.m3u8")).unwrap();
        assert!(raw.contains("#EXTINF:-1,B - C\r\n"));
    }

    #[test]
    fn list_skips_a_file_left_by_an_interrupted_rename() {
        let d = tempfile::tempdir().unwrap();
        save(d.path(), "Mix", &[]).unwrap();
        std::fs::write(d.path().join("mix.renaming.m3u8"), "#EXTM3U\n").unwrap();
        assert_eq!(list(d.path()).unwrap(), vec!["Mix".to_string()]);
    }

    #[test]
    fn list_is_natural_sorted_and_missing_dir_is_empty() {
        let d = tempfile::tempdir().unwrap();
        for n in ["p10", "p2", "p1"] {
            save(d.path(), n, &[]).unwrap();
        }
        std::fs::write(d.path().join("note.txt"), "").unwrap();
        assert_eq!(list(d.path()).unwrap(), vec!["p1", "p2", "p10"]);
        assert!(list(&d.path().join("nope")).unwrap().is_empty());
    }

    #[test]
    fn sanitize_is_idempotent() {
        for x in ["COM1.mix", "CON", "a/b", "con.", "mix. .", "", "x:y"] {
            assert_eq!(sanitize(&sanitize(x)), sanitize(x), "{x}");
        }
    }

    #[test]
    fn case_only_rename_works() {
        let d = tempfile::tempdir().unwrap();
        save(d.path(), "mix", &[]).unwrap();
        assert_eq!(rename(d.path(), "mix", "Mix").unwrap(), "Mix");
        assert_eq!(list(d.path()).unwrap(), vec!["Mix"]);
    }

    #[test]
    fn control_characters_cannot_inject_lines() {
        let d = tempfile::tempdir().unwrap();
        let e = entry("/a.flac", "T\r\n/evil.flac", "A\u{7}B", 1);
        save(d.path(), "p", &[e]).unwrap();
        let got = read(d.path(), "p").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, PathBuf::from("/a.flac"));
        assert_eq!(got[0].title.as_deref(), Some("T  /evil.flac"));
        assert_eq!(got[0].artist.as_deref(), Some("A B"));
    }

    #[test]
    fn missing_files_are_kept() {
        let d = tempfile::tempdir().unwrap();
        let e = vec![entry("/does/not/exist.flac", "T", "A", 9)];
        save(d.path(), "m", &e).unwrap();
        assert_eq!(read(d.path(), "m").unwrap(), e);
        assert_eq!(
            read(d.path(), "zzz").unwrap_err().kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn rename_refuses_an_existing_target() {
        let d = tempfile::tempdir().unwrap();
        save(d.path(), "a", &[]).unwrap();
        save(d.path(), "b", &[]).unwrap();
        assert_eq!(
            rename(d.path(), "a", "b").unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(rename(d.path(), "a", "c").unwrap(), "c");
        assert!(!exists(d.path(), "a") && exists(d.path(), "c"));
        delete(d.path(), "c").unwrap();
        assert!(!exists(d.path(), "c"));
    }

    /// On a case-sensitive filesystem "mix" and "Mix" are two files; a case-only
    /// rename onto the other must be refused and leave both untouched.
    #[cfg(not(windows))]
    #[test]
    fn case_only_rename_never_overwrites_a_variant() {
        let d = tempfile::tempdir().unwrap();
        save(d.path(), "mix", &[entry("/a.flac", "A", "X", 1)]).unwrap();
        save(d.path(), "Mix", &[entry("/b.flac", "B", "Y", 2)]).unwrap();
        let err = rename(d.path(), "mix", "Mix").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        let title = |n| read(d.path(), n).unwrap()[0].title.clone();
        assert_eq!(title("mix").as_deref(), Some("A"));
        assert_eq!(title("Mix").as_deref(), Some("B"));
    }

    fn e(p: &str) -> Entry {
        entry(p, p, "X", 1)
    }

    fn paths(d: &Path, n: &str) -> Vec<String> {
        read(d, n)
            .unwrap()
            .iter()
            .map(|x| x.path.to_string_lossy().into_owned())
            .collect()
    }

    fn put(d: &Path, n: &str, ps: &[&str]) -> Vec<Entry> {
        let v: Vec<Entry> = ps.iter().map(|p| e(p)).collect();
        save(d, n, &v).unwrap();
        v
    }

    #[test]
    fn add_appends_and_counts() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "Mix", &["/a"]);
        let r = add(d.path(), "Mix", &[e("/a"), e("/b"), e("/c")]).unwrap();
        assert_eq!(
            r,
            AddResult {
                added: 2,
                skipped: 1
            }
        );
        assert_eq!(paths(d.path(), "Mix"), ["/a", "/b", "/c"]);
    }

    #[test]
    fn add_skips_paths_already_there_by_path_key() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "Mix", &["/m/a.flac"]);
        let r = add(d.path(), "Mix", &[e("/m/a.flac")]).unwrap();
        assert_eq!(
            r,
            AddResult {
                added: 0,
                skipped: 1
            }
        );
        #[cfg(windows)]
        {
            put(d.path(), "W", &[r"C:\Music\a.flac"]);
            let r = add(d.path(), "W", &[e("c:/music/A.flac")]).unwrap();
            assert_eq!(r.added, 0);
        }
    }

    #[test]
    fn add_skips_duplicates_within_the_request() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "Mix", &[]);
        let r = add(d.path(), "Mix", &[e("/a"), e("/a")]).unwrap();
        assert_eq!(
            r,
            AddResult {
                added: 1,
                skipped: 1
            }
        );
        assert_eq!(paths(d.path(), "Mix"), ["/a"]);
    }

    #[test]
    fn add_refuses_a_missing_playlist() {
        let d = tempfile::tempdir().unwrap();
        let err = add(d.path(), "Mix", &[e("/a")]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert_eq!(err.to_string(), "Mix no longer exists");
        assert!(!exists(d.path(), "Mix"));
    }

    #[test]
    fn remove_by_index() {
        let d = tempfile::tempdir().unwrap();
        let s = put(d.path(), "Mix", &["/a", "/b", "/c"]);
        remove(d.path(), "Mix", &s, &[0, 2]).unwrap();
        assert_eq!(paths(d.path(), "Mix"), ["/b"]);
    }

    #[test]
    fn remove_one_of_a_duplicated_path() {
        let d = tempfile::tempdir().unwrap();
        let s = put(d.path(), "Mix", &["/a", "/b", "/a"]);
        remove(d.path(), "Mix", &s, &[2]).unwrap();
        assert_eq!(paths(d.path(), "Mix"), ["/a", "/b"]);
    }

    #[test]
    fn remove_leaves_an_unlisted_row_in_place() {
        let d = tempfile::tempdir().unwrap();
        let s = put(d.path(), "Mix", &["/a", "/hidden", "/c"]);
        remove(d.path(), "Mix", &s, &[0]).unwrap();
        assert_eq!(paths(d.path(), "Mix"), ["/hidden", "/c"]);
    }

    #[test]
    fn reordered_one_row() {
        assert_eq!(reordered(4, &[0], 3), [1, 2, 0, 3]);
        assert_eq!(reordered(4, &[3], 1), [0, 3, 1, 2]);
    }

    #[test]
    fn reordered_keeps_several_rows_in_order() {
        assert_eq!(reordered(5, &[1, 3], 4), [0, 2, 1, 3, 4]);
        assert_eq!(reordered(5, &[3, 4], 1), [0, 3, 4, 1, 2]);
    }

    #[test]
    fn reordered_to_the_start() {
        assert_eq!(reordered(4, &[2, 3], 0), [2, 3, 0, 1]);
    }

    #[test]
    fn reordered_to_the_end() {
        assert_eq!(reordered(4, &[0], 4), [1, 2, 3, 0]);
    }

    #[test]
    fn reordered_moving_onto_itself_is_identity() {
        assert_eq!(reordered(4, &[1], 1), [0, 1, 2, 3]);
        assert_eq!(reordered(4, &[1], 2), [0, 1, 2, 3]);
        assert_eq!(reordered(4, &[1, 2], 2), [0, 1, 2, 3]);
    }

    #[test]
    fn reorder_keeps_a_hidden_row_after_its_neighbour() {
        let d = tempfile::tempdir().unwrap();
        let s = put(d.path(), "Mix", &["/A", "/B", "/C"]);
        reorder(d.path(), "Mix", &s, &[2], 0).unwrap();
        assert_eq!(paths(d.path(), "Mix"), ["/C", "/A", "/B"]);
        let s = put(d.path(), "Mix", &["/A", "/B", "/C"]);
        reorder(d.path(), "Mix", &s, &[0, 1], 3).unwrap();
        assert_eq!(paths(d.path(), "Mix"), ["/C", "/A", "/B"]);
    }

    #[test]
    fn reorder_noop_does_not_write() {
        let d = tempfile::tempdir().unwrap();
        let s = put(d.path(), "Mix", &["/a", "/b", "/c"]);
        let file = d.path().join("Mix.m3u8");
        // Change the bytes without changing the paths: a write would normalise them back.
        let marker = std::fs::read_to_string(&file).unwrap() + "# marker\r\n";
        std::fs::write(&file, &marker).unwrap();
        reorder(d.path(), "Mix", &s, &[1], 1).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), marker);
        assert!(!d.path().join("Mix.m3u8.tmp").exists());
    }

    #[test]
    fn create_new() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(create(d.path(), "a/b").unwrap(), "a_b");
        assert_eq!(
            std::fs::read_to_string(d.path().join("a_b.m3u8")).unwrap(),
            "#EXTM3U\r\n"
        );
    }

    #[test]
    fn create_refuses_a_taken_name() {
        let d = tempfile::tempdir().unwrap();
        create(d.path(), "Mix").unwrap();
        assert_eq!(
            create(d.path(), "Mix").unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn restore_puts_the_old_rows_back() {
        let d = tempfile::tempdir().unwrap();
        let before = put(d.path(), "Mix", &["/a", "/b", "/c"]);
        remove(d.path(), "Mix", &before, &[1]).unwrap();
        let after = read(d.path(), "Mix").unwrap();
        restore(d.path(), "Mix", &after, &before).unwrap();
        assert_eq!(paths(d.path(), "Mix"), ["/a", "/b", "/c"]);
    }

    #[test]
    fn restore_refuses_when_the_file_changed() {
        let d = tempfile::tempdir().unwrap();
        let before = put(d.path(), "Mix", &["/a", "/b"]);
        let expected = vec![e("/a")];
        let err = restore(d.path(), "Mix", &expected, &before).unwrap_err();
        assert_eq!(err.to_string(), "Mix changed since. Can't undo.");
        assert_eq!(paths(d.path(), "Mix"), ["/a", "/b"]);
    }

    #[test]
    fn edit_refused_after_an_outside_reorder_of_the_same_length() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "Mix", &["/b", "/a"]);
        let shown = vec![e("/a"), e("/b")];
        let err = remove(d.path(), "Mix", &shown, &[0]).unwrap_err();
        assert_eq!(err.to_string(), "Mix changed on disk. Reopen it.");
        let err = reorder(d.path(), "Mix", &shown, &[0], 2).unwrap_err();
        assert_eq!(err.to_string(), "Mix changed on disk. Reopen it.");
        assert_eq!(paths(d.path(), "Mix"), ["/b", "/a"]);
    }

    #[test]
    fn failed_write_leaves_no_temp_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("Mix.m3u8")).unwrap();
        assert!(save(d.path(), "Mix", &[e("/a")]).is_err());
        let tmps = std::fs::read_dir(d.path())
            .unwrap()
            .filter(|f| {
                f.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "tmp")
            })
            .count();
        assert_eq!(tmps, 0);
    }

    #[test]
    fn free_name_counts_up() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(free_name(d.path(), "Mix"), "Mix");
        put(d.path(), "Mix", &[]);
        put(d.path(), "Mix (2)", &[]);
        assert_eq!(free_name(d.path(), "Mix"), "Mix (3)");
    }
}

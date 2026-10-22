//! Expands command-line inputs into a list of audio files.

use std::path::{Path, PathBuf};

/// Extensions (lower case) the player accepts when listing a folder.
const EXTENSIONS: [&str; 7] = ["flac", "wav", "aif", "aiff", "m4a", "dsf", "dff"];

/// Folders are listed non-recursively, filtered by extension and naturally sorted by file
/// name. Files are kept as given. Inputs are concatenated in order.
pub fn collect_tracks(inputs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for input in inputs {
        if input.is_dir() {
            out.extend(list_folder(input));
        } else {
            out.push(input.clone());
        }
    }
    out
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

fn list_folder(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_audio(p))
        .collect();
    files.sort_by(|a, b| {
        let name = |p: &PathBuf| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };
        natord::compare(&name(a), &name(b))
    });
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_tracks_filters_and_sorts_unicode() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "10 b.flac",
            "2 a.FLAC",
            "cover.jpg",
            "rip.log",
            "日本語 01.dsf",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let got = collect_tracks(&[dir.path().to_path_buf()]);
        let names: Vec<_> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["2 a.FLAC", "10 b.flac", "日本語 01.dsf"]);
    }

    #[test]
    fn explicit_files_keep_order_and_mix_with_folders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.wav"), b"x").unwrap();
        std::fs::write(dir.path().join("a.wav"), b"x").unwrap();
        let z = PathBuf::from("z.txt");
        let first = PathBuf::from("zz.flac");
        let got = collect_tracks(&[first.clone(), dir.path().to_path_buf(), z.clone()]);
        assert_eq!(
            got,
            [first, dir.path().join("a.wav"), dir.path().join("b.wav"), z]
        );
    }
}

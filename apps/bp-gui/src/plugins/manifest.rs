//! Plugin manifests (`plugins\<name>\plugin.toml`) and folder discovery.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::rpc::Permission;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub api_version: u32,
    pub command: Vec<String>,
    pub permissions: Vec<Permission>,
    /// Shown in the top bar and Settings instead of `name`.
    #[serde(default)]
    pub title: Option<String>,
}

/// The longest `title`, in characters.
const TITLE_MAX: usize = 24;

impl Manifest {
    /// What the owner sees: `title`, else `name`.
    pub fn label(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.name)
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Reads and validates `<folder>/plugin.toml`. The error is the reason only; the
/// page prefixes it with "invalid manifest: ".
pub fn load(folder: &Path) -> Result<Manifest, String> {
    let text = fs::read_to_string(folder.join("plugin.toml"))
        .map_err(|e| format!("cannot read plugin.toml: {e}"))?;
    let m: Manifest = toml::from_str(&text).map_err(|e| e.to_string())?;
    if !valid_name(&m.name) {
        return Err(format!(
            "name {:?} may only use letters, digits, . _ -",
            m.name
        ));
    }
    let folder_name = folder.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if m.name != folder_name {
        return Err(format!(
            "name {:?} must equal the folder name {folder_name:?}",
            m.name
        ));
    }
    if let Some(title) = &m.title
        && (title.trim().is_empty() || title.contains('\n') || title.chars().count() > TITLE_MAX)
    {
        return Err(format!(
            "title {title:?} must be one line of 1 to {TITLE_MAX} characters"
        ));
    }
    if m.command.is_empty() || m.command[0].is_empty() {
        return Err("command must not be empty".into());
    }
    Ok(m)
}

/// Every sub-folder of `plugins_dir`, sorted by name, with its manifest result.
/// A missing directory gives an empty list.
pub fn discover(plugins_dir: &Path) -> Vec<(String, Result<Manifest, String>)> {
    let Ok(rd) = fs::read_dir(plugins_dir) else {
        return Vec::new();
    };
    let mut folders: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    folders.sort();
    folders
        .into_iter()
        .map(|f| {
            let m = load(&plugins_dir.join(&f));
            (f, m)
        })
        .collect()
}

/// Program and arguments to spawn. A `command[0]` with a path separator or a
/// leading `.` is relative to the plugin folder; an absolute one is kept; a bare
/// name (e.g. `python`) is left for the OS to find on PATH.
pub fn program(folder: &Path, m: &Manifest) -> (PathBuf, Vec<String>) {
    let first = Path::new(&m.command[0]);
    let raw = &m.command[0];
    let pathlike = raw.contains(['/', '\\']) || raw.starts_with('.');
    let exe = if first.is_absolute() || !pathlike {
        first.to_path_buf()
    } else {
        folder.join(first)
    };
    (exe, m.command[1..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"
name = "nil-dj"                       # must equal the folder name
version = "0.2.0"
description = "AI DJ: playlists from a description, more like this, radio"
api_version = 1
command = ["app\\.venv\\Scripts\\python.exe", "-m", "nil_dj", "plugin"]   # run from the plugin folder
permissions = ["playback", "library", "playlists"]
"#;

    fn plugin(root: &Path, folder: &str, toml: &str) -> PathBuf {
        let d = root.join(folder);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("plugin.toml"), toml).unwrap();
        d
    }

    #[test]
    fn loads_the_spec_example() {
        let t = tempfile::tempdir().unwrap();
        let d = plugin(t.path(), "nil-dj", SPEC);
        let m = load(&d).unwrap();
        assert_eq!(m.name, "nil-dj");
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.api_version, 1);
        assert_eq!(m.command.len(), 4);
        assert_eq!(m.command[1], "-m");
        assert_eq!(
            m.permissions,
            vec![
                Permission::Playback,
                Permission::Library,
                Permission::Playlists
            ]
        );
    }

    #[test]
    fn title_is_optional() {
        let t = tempfile::tempdir().unwrap();
        let d = plugin(t.path(), "nil-dj", SPEC);
        assert_eq!(load(&d).unwrap().title, None);
        let d = plugin(t.path(), "nil-dj", &format!("title = \"DJ\"\n{SPEC}"));
        assert_eq!(load(&d).unwrap().title.as_deref(), Some("DJ"));
    }

    #[test]
    fn title_must_be_one_short_line() {
        let t = tempfile::tempdir().unwrap();
        for bad in [
            "\"\"",
            "\"  \"",
            "\"a\\nb\"",
            &format!("\"{}\"", "x".repeat(25)),
        ] {
            let d = plugin(t.path(), "nil-dj", &format!("title = {bad}\n{SPEC}"));
            let err = load(&d).unwrap_err();
            assert!(err.contains("title"), "{bad}: {err}");
        }
    }

    #[test]
    fn name_must_equal_folder() {
        let t = tempfile::tempdir().unwrap();
        let d = plugin(t.path(), "other", SPEC);
        let e = load(&d).unwrap_err();
        assert!(e.contains("folder name"), "{e}");
    }

    #[test]
    fn unknown_permission_is_invalid() {
        let t = tempfile::tempdir().unwrap();
        let d = plugin(
            t.path(),
            "nil-dj",
            &SPEC.replace("\"playlists\"", "\"root\""),
        );
        assert!(load(&d).unwrap_err().contains("root"));
    }

    #[test]
    fn bad_names_and_empty_command_are_invalid() {
        let t = tempfile::tempdir().unwrap();
        let d = plugin(t.path(), "a b", &SPEC.replace("nil-dj", "a b"));
        assert!(load(&d).unwrap_err().contains("letters"));
        let d = plugin(
            t.path(),
            "nil-dj",
            &SPEC.replace(
                r#"["app\\.venv\\Scripts\\python.exe", "-m", "nil_dj", "plugin"]"#,
                "[]",
            ),
        );
        assert!(load(&d).unwrap_err().contains("command"));
    }

    #[test]
    fn broken_toml_reports_the_parse_error_and_never_panics() {
        let t = tempfile::tempdir().unwrap();
        let d = plugin(t.path(), "nil-dj", "name = \"nil-dj\nversion = ");
        assert!(!load(&d).unwrap_err().is_empty());
        let empty = t.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(load(&empty).is_err());
    }

    #[test]
    fn discover_lists_valid_and_invalid_side_by_side() {
        let t = tempfile::tempdir().unwrap();
        plugin(t.path(), "nil-dj", SPEC);
        plugin(t.path(), "aaa", "nonsense");
        fs::write(t.path().join("stray.txt"), "x").unwrap();
        let got = discover(t.path());
        let names: Vec<_> = got.iter().map(|(f, _)| f.as_str()).collect();
        assert_eq!(names, ["aaa", "nil-dj"]);
        assert!(got[0].1.is_err());
        assert!(got[1].1.is_ok());
        assert!(discover(&t.path().join("missing")).is_empty());
    }

    #[test]
    fn relative_command_runs_from_the_plugin_folder() {
        let t = tempfile::tempdir().unwrap();
        let folder = t.path().join("p");
        let mut m = load(&plugin(t.path(), "nil-dj", SPEC)).unwrap();
        m.command = vec!["bin/tool".into(), "--x".into()];
        let (exe, args) = program(&folder, &m);
        assert_eq!(exe, folder.join("bin/tool"));
        assert_eq!(args, ["--x"]);

        let abs = t.path().join("tool");
        m.command = vec![abs.to_string_lossy().into_owned()];
        assert_eq!(program(&folder, &m).0, abs);
    }

    #[test]
    fn bare_command_is_left_for_path_lookup() {
        let t = tempfile::tempdir().unwrap();
        let mut m = load(&plugin(t.path(), "nil-dj", SPEC)).unwrap();
        m.command = vec!["python".into(), "hello.py".into()];
        let (exe, args) = program(&t.path().join("p"), &m);
        assert_eq!(exe, PathBuf::from("python"));
        assert_eq!(args, ["hello.py"]);
    }
}

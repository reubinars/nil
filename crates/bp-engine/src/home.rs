//! Where nil keeps its files: `<root>\data` for state and `<root>\plugins` for plugins.
//! The root is `NIL_HOME`, else the exe folder when it is writable, else a per-user folder.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::Layer;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The resolved root folder. Nothing here creates directories except the writability probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    pub root: PathBuf,
    /// Set when the exe folder was not writable and the per-user folder is used instead.
    pub fallback_reason: Option<String>,
    /// Replaces `<root>\data` for this run (set when migration failed and the old folder
    /// is still the live one).
    pub data_override: Option<PathBuf>,
}

impl Home {
    pub fn data_dir(&self) -> PathBuf {
        self.data_override
            .clone()
            .unwrap_or_else(|| self.root.join("data"))
    }

    pub fn thumbs_dir(&self) -> PathBuf {
        self.data_dir().join("thumbs")
    }

    pub fn playlists_dir(&self) -> PathBuf {
        self.data_dir().join("playlists")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir().join("logs")
    }

    pub fn plugins_dir(&self) -> PathBuf {
        self.root.join("plugins")
    }
}

/// Resolves the home from the environment and the running exe.
pub fn resolve_home() -> Home {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let (var, rel) = if cfg!(windows) {
        ("APPDATA", "")
    } else {
        ("HOME", ".config")
    };
    let fallback_base = std::env::var_os(var).map(|b| PathBuf::from(b).join(rel));
    let nil_home = std::env::var_os("NIL_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    resolve_from(nil_home, &exe_dir, fallback_base)
}

/// The pure core of [`resolve_home`]. Order: `nil_home`, then `exe_dir` if `data\` there
/// exists or can be created and a probe file written and deleted, then `fallback_base/nil`
/// (or `exe_dir` when there is no fallback base).
pub fn resolve_from(
    nil_home: Option<PathBuf>,
    exe_dir: &Path,
    fallback_base: Option<PathBuf>,
) -> Home {
    if let Some(root) = nil_home {
        return Home {
            root,
            fallback_reason: None,
            data_override: None,
        };
    }
    if is_writable(&exe_dir.join("data")) {
        return Home {
            root: exe_dir.to_path_buf(),
            fallback_reason: None,
            data_override: None,
        };
    }
    match fallback_base {
        Some(base) => {
            let root = base.join("nil");
            let reason = format!(
                "Data is stored in {} because {} isn't writable",
                root.display(),
                exe_dir.display()
            );
            Home {
                root,
                fallback_reason: Some(reason),
                data_override: None,
            }
        }
        None => Home {
            root: exe_dir.to_path_buf(),
            fallback_reason: None,
            data_override: None,
        },
    }
}

/// True when `dir` exists or can be created, and a file can be written in it and deleted.
fn is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write-probe");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Appends to `logs_dir/bp.log`. Nothing is ever logged to the terminal, which is in raw
/// mode during `bp play`. `app_target` (`bp_cli` or `bp_gui`) logs at INFO along with the
/// engine crates.
pub fn init_file_logging(logs_dir: &Path, app_target: &str) {
    let _ = std::fs::create_dir_all(logs_dir);
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs_dir.join("bp.log"))
    else {
        return;
    };
    // The `wasapi` crate logs every buffer write at trace via `log`; keep it (and anything
    // else unknown) at warn or above.
    let filter = Targets::new()
        .with_default(LevelFilter::WARN)
        .with_target(app_target, LevelFilter::INFO)
        .with_target("bp_engine", LevelFilter::INFO)
        .with_target("bp_output", LevelFilter::INFO)
        .with_target("bp_decode", LevelFilter::INFO);
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_filter(filter);
    let _ = tracing_subscriber::registry().with(layer).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Makes `data\` impossible to create: a regular file named `data` blocks
    /// `create_dir_all` on every OS.
    fn make_read_only(exe: &Path) {
        std::fs::write(exe.join("data"), b"").unwrap();
    }

    #[test]
    fn nil_home_wins() {
        let exe = tempfile::tempdir().unwrap();
        let h = resolve_from(Some("/x/nil".into()), exe.path(), Some("/appdata".into()));
        assert_eq!(h.root, PathBuf::from("/x/nil"));
        assert_eq!(h.fallback_reason, None);
    }

    #[test]
    fn writable_exe_folder_is_the_home() {
        let exe = tempfile::tempdir().unwrap();
        let h = resolve_from(None, exe.path(), Some("/appdata".into()));
        assert_eq!(h.root, exe.path());
        assert_eq!(h.fallback_reason, None);
        assert!(
            exe.path().join("data").is_dir(),
            "data\\ is created by the probe"
        );
        assert_eq!(
            std::fs::read_dir(exe.path().join("data")).unwrap().count(),
            0,
            "probe deleted"
        );
    }

    #[test]
    fn read_only_exe_folder_falls_back_with_a_reason() {
        let exe = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        make_read_only(exe.path());
        let h = resolve_from(None, exe.path(), Some(base.path().into()));
        assert_eq!(h.root, base.path().join("nil"));
        assert_eq!(
            h.fallback_reason.as_deref(),
            Some(
                format!(
                    "Data is stored in {} because {} isn't writable",
                    base.path().join("nil").display(),
                    exe.path().display()
                )
                .as_str()
            )
        );
    }

    #[test]
    fn no_fallback_base_keeps_the_exe_folder() {
        let exe = tempfile::tempdir().unwrap();
        make_read_only(exe.path());
        let h = resolve_from(None, exe.path(), None);
        assert_eq!(h.root, exe.path());
    }

    #[test]
    fn layout_is_derived_from_the_root() {
        let h = Home {
            root: "/n".into(),
            fallback_reason: None,
            data_override: None,
        };
        assert_eq!(h.data_dir(), PathBuf::from("/n/data"));
        assert_eq!(h.thumbs_dir(), PathBuf::from("/n/data/thumbs"));
        assert_eq!(h.playlists_dir(), PathBuf::from("/n/data/playlists"));
        assert_eq!(h.logs_dir(), PathBuf::from("/n/data/logs"));
        assert_eq!(h.plugins_dir(), PathBuf::from("/n/plugins"));
    }

    #[test]
    fn data_override_replaces_the_data_dir() {
        let h = Home {
            root: "/n".into(),
            fallback_reason: None,
            data_override: Some("/old".into()),
        };
        assert_eq!(h.data_dir(), PathBuf::from("/old"));
        assert_eq!(h.logs_dir(), PathBuf::from("/old/logs"));
        assert_eq!(h.plugins_dir(), PathBuf::from("/n/plugins"));
    }
}

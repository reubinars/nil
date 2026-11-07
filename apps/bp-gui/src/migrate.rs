//! One-time move of the old `bitperfect` folders into `<home>\data`.
//!
//! The copy goes into `data.partial\`, which is renamed to `data\` only when complete, so an
//! interruption never leaves a half-filled `data\`. The old folders are deleted last. If that
//! fails (a file in use), `data\.cleanup-pending` stays behind and the next start retries.

use std::io;
use std::path::{Path, PathBuf};

/// The folders the app used before it became portable.
#[derive(Debug, Clone)]
pub struct OldDirs {
    /// `%APPDATA%\bitperfect`: config, library cache and `bp.log`.
    pub roaming: PathBuf,
    /// `%LOCALAPPDATA%\bitperfect\thumbs`: regenerable thumbnails.
    pub thumbs: PathBuf,
}

impl OldDirs {
    /// Off Windows these are `~/.config/bitperfect` and `~/.cache/bitperfect/thumbs`. When the
    /// base variable is unset the paths are empty, which never exist.
    pub fn from_env() -> OldDirs {
        let (roaming_var, local_var, roaming_rel, local_rel) = if cfg!(windows) {
            ("APPDATA", "LOCALAPPDATA", "", "")
        } else {
            ("HOME", "HOME", ".config", ".cache")
        };
        let under = |var: &str, rel: &str| {
            std::env::var_os(var)
                .map(|b| PathBuf::from(b).join(rel).join("bitperfect"))
                .unwrap_or_default()
        };
        OldDirs {
            roaming: under(roaming_var, roaming_rel),
            thumbs: under(local_var, local_rel).join("thumbs"),
        }
    }

    /// The `bitperfect` folder under LOCALAPPDATA (the parent of `thumbs`).
    fn local(&self) -> Option<&Path> {
        self.thumbs.parent().filter(|p| !p.as_os_str().is_empty())
    }

    fn any_exists(&self) -> bool {
        self.roaming.exists() || self.local().is_some_and(Path::exists)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// Nothing to do: already migrated, or there is no old data.
    NotNeeded,
    Migrated,
    /// The copy failed; use `use_dir` (the old roaming folder) as the data dir for this run.
    CopyFailed {
        reason: String,
        use_dir: PathBuf,
    },
    /// `data\` is complete but the old folders could not all be deleted; retried next start.
    CleanupPending(String),
}

const MARKER: &str = ".cleanup-pending";

/// Migrates into `home`'s data folder. When the copy fails, `home` is pointed at the old
/// roaming folder for this run, so the user's settings are still the live ones.
pub fn migrate_home(home: &mut bp_engine::home::Home, old: &OldDirs) -> Migration {
    let migration = migrate(&home.data_dir(), old);
    if let Migration::CopyFailed { use_dir, .. } = &migration {
        home.data_override = Some(use_dir.clone());
    }
    migration
}

pub fn migrate(data_dir: &Path, old: &OldDirs) -> Migration {
    // `resolve_home`'s writability probe leaves an empty `data\` behind; that is not migrated data.
    if data_dir.exists() && !is_empty_dir(data_dir) {
        let marker = data_dir.join(MARKER);
        if !marker.exists() {
            return Migration::NotNeeded;
        }
        return match remove_old(old) {
            Ok(()) => {
                let _ = std::fs::remove_file(&marker);
                Migration::NotNeeded
            }
            Err(e) => Migration::CleanupPending(e),
        };
    }
    let partial = data_dir.with_file_name("data.partial");
    if partial.exists() {
        let _ = std::fs::remove_dir_all(&partial);
    }
    if !old.any_exists() {
        return Migration::NotNeeded;
    }
    if let Err(e) = build_partial(&partial, data_dir, old) {
        let _ = std::fs::remove_dir_all(&partial);
        return Migration::CopyFailed {
            reason: e.to_string(),
            use_dir: if old.roaming.as_os_str().is_empty() {
                data_dir.to_path_buf()
            } else {
                old.roaming.clone()
            },
        };
    }
    match remove_old(old) {
        Ok(()) => {
            let _ = std::fs::remove_file(data_dir.join(MARKER));
            Migration::Migrated
        }
        Err(e) => Migration::CleanupPending(e),
    }
}

/// Fills `partial` and renames it to `data_dir`. The caller deletes `partial` on error.
fn build_partial(partial: &Path, data_dir: &Path, old: &OldDirs) -> io::Result<()> {
    std::fs::create_dir_all(partial)?;
    if old.roaming.is_dir() {
        for entry in std::fs::read_dir(&old.roaming)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == "bp.log" {
                std::fs::create_dir_all(partial.join("logs"))?;
                std::fs::copy(entry.path(), partial.join("logs").join(&name))?;
            } else {
                copy_any(&entry.path(), &partial.join(&name))?;
            }
        }
    }
    // Thumbnails are regenerated, so a failure here is not worth aborting for.
    if old.thumbs.is_dir() && copy_dir(&old.thumbs, &partial.join("thumbs")).is_err() {
        let _ = std::fs::remove_dir_all(partial.join("thumbs"));
    }
    std::fs::write(partial.join(MARKER), b"")?;
    // Renaming onto an existing directory fails on Windows, so drop the empty one first.
    if data_dir.exists() {
        std::fs::remove_dir(data_dir)?;
    }
    std::fs::rename(partial, data_dir)
}

fn is_empty_dir(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none())
}

fn copy_any(from: &Path, to: &Path) -> io::Result<()> {
    if from.is_dir() {
        copy_dir(from, to)
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_any(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

/// Deletes both old folders, trying each even when the first fails.
fn remove_old(old: &OldDirs) -> Result<(), String> {
    let mut errors = Vec::new();
    for dir in [Some(old.roaming.as_path()), old.local()]
        .into_iter()
        .flatten()
    {
        if dir.exists()
            && let Err(e) = std::fs::remove_dir_all(dir)
        {
            errors.push(format!("{}: {e}", dir.display()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Rig {
        _tmp: tempfile::TempDir,
        data: PathBuf,
        old: OldDirs,
    }

    fn rig() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let old = OldDirs {
            roaming: p.join("roaming/bitperfect"),
            thumbs: p.join("local/bitperfect/thumbs"),
        };
        Rig {
            data: p.join("home/data"),
            old,
            _tmp: tmp,
        }
    }

    impl Rig {
        fn partial(&self) -> PathBuf {
            self.data.with_file_name("data.partial")
        }

        fn seed_roaming(&self) {
            fs::create_dir_all(&self.old.roaming).unwrap();
            fs::create_dir_all(self.data.parent().unwrap()).unwrap();
            fs::write(self.old.roaming.join("gui.toml"), "g").unwrap();
            fs::write(self.old.roaming.join("library-cache.json"), "{}").unwrap();
            fs::write(self.old.roaming.join("bp.log"), "log").unwrap();
        }

        fn seed_thumbs(&self) {
            fs::create_dir_all(&self.old.thumbs).unwrap();
            fs::write(self.old.thumbs.join("x.png"), "png").unwrap();
        }
    }

    #[test]
    fn no_old_folder_is_not_needed() {
        let r = rig();
        assert_eq!(migrate(&r.data, &r.old), Migration::NotNeeded);
        assert!(!r.data.exists());
    }

    #[test]
    fn roaming_only_moves_config_and_log() {
        let r = rig();
        r.seed_roaming();
        assert_eq!(migrate(&r.data, &r.old), Migration::Migrated);
        assert!(r.data.join("gui.toml").is_file());
        assert!(r.data.join("library-cache.json").is_file());
        assert_eq!(
            fs::read_to_string(r.data.join("logs/bp.log")).unwrap(),
            "log"
        );
        assert!(!r.data.join("bp.log").exists());
        assert!(!r.old.roaming.exists());
        assert!(!r.partial().exists());
    }

    #[test]
    fn both_folders_move_thumbs_too() {
        let r = rig();
        r.seed_roaming();
        r.seed_thumbs();
        assert_eq!(migrate(&r.data, &r.old), Migration::Migrated);
        assert!(r.data.join("thumbs/x.png").is_file());
        assert!(!r.old.thumbs.parent().unwrap().exists());
    }

    #[test]
    fn existing_data_dir_is_left_alone() {
        let r = rig();
        r.seed_roaming();
        fs::create_dir_all(&r.data).unwrap();
        fs::write(r.data.join("gui.toml"), "a").unwrap();
        assert_eq!(migrate(&r.data, &r.old), Migration::NotNeeded);
        assert!(r.old.roaming.join("gui.toml").is_file());
        assert_eq!(fs::read_to_string(r.data.join("gui.toml")).unwrap(), "a");
    }

    #[test]
    fn leftover_partial_is_replaced() {
        let r = rig();
        r.seed_roaming();
        fs::create_dir_all(r.partial()).unwrap();
        fs::write(r.partial().join("junk"), "j").unwrap();
        assert_eq!(migrate(&r.data, &r.old), Migration::Migrated);
        assert!(!r.data.join("junk").exists());
    }

    #[test]
    fn interrupted_after_copy_finishes_next_start() {
        let r = rig();
        r.seed_roaming();
        fs::create_dir_all(r.partial()).unwrap();
        fs::write(r.partial().join("gui.toml"), "g").unwrap();
        assert_eq!(migrate(&r.data, &r.old), Migration::Migrated);
        assert!(r.data.join("library-cache.json").is_file());
        assert!(!r.partial().exists());
        assert!(!r.old.roaming.exists());
    }

    #[test]
    fn empty_data_dir_from_the_probe_still_migrates() {
        let r = rig();
        r.seed_roaming();
        fs::create_dir_all(&r.data).unwrap();
        assert_eq!(migrate(&r.data, &r.old), Migration::Migrated);
        assert!(r.data.join("gui.toml").is_file());
        assert!(!r.old.roaming.exists());
    }

    #[test]
    fn migrates_after_resolve_home_probe_creates_data() {
        let r = rig();
        r.seed_roaming();
        let exe = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let home = bp_engine::home::resolve_from(None, exe.path(), Some(base.path().into()));
        assert!(home.data_dir().is_dir(), "the probe created data\\");
        assert_eq!(migrate(&home.data_dir(), &r.old), Migration::Migrated);
        assert!(home.data_dir().join("gui.toml").is_file());
        assert!(!home.data_dir().join(MARKER).exists());
    }

    #[test]
    fn thumbs_only_old_folder_migrates_with_an_empty_roaming_path() {
        let r = rig();
        r.seed_thumbs();
        let old = OldDirs {
            roaming: PathBuf::new(),
            thumbs: r.old.thumbs.clone(),
        };
        assert_eq!(migrate(&r.data, &old), Migration::Migrated);
        assert!(r.data.join("thumbs/x.png").is_file());
    }

    #[test]
    fn successful_migration_leaves_no_marker() {
        let r = rig();
        r.seed_roaming();
        assert_eq!(migrate(&r.data, &r.old), Migration::Migrated);
        assert!(!r.data.join(MARKER).exists());
    }

    #[cfg(unix)]
    #[test]
    fn failed_config_copy_keeps_the_old_folder() {
        use std::os::unix::fs::PermissionsExt;
        let r = rig();
        r.seed_roaming();
        let locked = r.old.roaming.join("library-cache.json");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&locked).is_ok() {
            return; // running as root: permissions are not enforced
        }
        let result = migrate(&r.data, &r.old);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            &result,
            Migration::CopyFailed { use_dir, .. } if *use_dir == r.old.roaming
        ));
        assert!(!r.data.exists());
        assert!(!r.partial().exists());
        assert!(r.old.roaming.join("gui.toml").is_file());
        assert!(locked.is_file());
    }

    #[cfg(windows)]
    #[test]
    fn failed_config_copy_keeps_the_old_folder() {
        use std::os::windows::fs::OpenOptionsExt;
        let r = rig();
        r.seed_roaming();
        let locked = r.old.roaming.join("library-cache.json");
        let _held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&locked)
            .unwrap();
        let result = migrate(&r.data, &r.old);
        assert!(matches!(
            &result,
            Migration::CopyFailed { use_dir, .. } if *use_dir == r.old.roaming
        ));
        assert!(!r.data.exists());
        assert!(!r.partial().exists());
        assert!(r.old.roaming.join("gui.toml").is_file());
    }

    /// After a failed copy the run reads and writes the old folder, which still holds
    /// everything.
    #[cfg(unix)]
    #[test]
    fn failed_copy_points_the_home_at_the_old_folder() {
        use std::os::unix::fs::PermissionsExt;
        let r = rig();
        r.seed_roaming();
        let locked = r.old.roaming.join("library-cache.json");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&locked).is_ok() {
            return; // running as root: permissions are not enforced
        }
        let mut home = bp_engine::home::Home {
            root: r.data.parent().unwrap().to_path_buf(),
            fallback_reason: None,
            data_override: None,
        };
        let result = migrate_home(&mut home, &r.old);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(result, Migration::CopyFailed { .. }));
        assert_eq!(home.data_dir(), r.old.roaming);
        assert!(home.data_dir().join("gui.toml").is_file());
        assert_eq!(home.plugins_dir(), r.data.with_file_name("plugins"));
    }

    #[cfg(windows)]
    #[test]
    fn failed_copy_points_the_home_at_the_old_folder() {
        use std::os::windows::fs::OpenOptionsExt;
        let r = rig();
        r.seed_roaming();
        let _held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(r.old.roaming.join("library-cache.json"))
            .unwrap();
        let mut home = bp_engine::home::Home {
            root: r.data.parent().unwrap().to_path_buf(),
            fallback_reason: None,
            data_override: None,
        };
        assert!(matches!(
            migrate_home(&mut home, &r.old),
            Migration::CopyFailed { .. }
        ));
        assert_eq!(home.data_dir(), r.old.roaming);
        assert!(home.data_dir().join("gui.toml").is_file());
    }

    #[test]
    fn successful_migration_keeps_the_home_data_dir() {
        let r = rig();
        r.seed_roaming();
        let mut home = bp_engine::home::Home {
            root: r.data.parent().unwrap().to_path_buf(),
            fallback_reason: None,
            data_override: None,
        };
        assert_eq!(migrate_home(&mut home, &r.old), Migration::Migrated);
        assert_eq!(home.data_override, None);
        assert!(home.data_dir().join("gui.toml").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn locked_old_folder_is_retried_next_start() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        let r = rig();
        r.seed_roaming();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(r.old.roaming.join("bp.log"))
            .unwrap();
        assert!(matches!(
            migrate(&r.data, &r.old),
            Migration::CleanupPending(_)
        ));
        assert!(r.data.join("gui.toml").is_file());
        drop(held);
        assert_eq!(migrate(&r.data, &r.old), Migration::NotNeeded);
        assert!(!r.old.roaming.exists());
        assert!(!r.data.join(MARKER).exists());
    }

    /// The marker from an earlier run whose deletion failed: this run retries and succeeds.
    #[cfg(unix)]
    #[test]
    fn pending_cleanup_is_retried_next_start() {
        let r = rig();
        r.seed_roaming();
        fs::create_dir_all(&r.data).unwrap();
        fs::write(r.data.join(MARKER), b"").unwrap();
        assert_eq!(migrate(&r.data, &r.old), Migration::NotNeeded);
        assert!(!r.old.roaming.exists());
        assert!(!r.data.join(MARKER).exists());
    }
}

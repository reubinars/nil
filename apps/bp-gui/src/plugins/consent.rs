//! Per-plugin consent, stored in `data\plugins.toml`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::manifest::Manifest;
use super::rpc::Permission;

/// A plugin's switch and the permissions the owner approved. New plugins are off.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Consent {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub granted: Vec<Permission>,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    plugins: BTreeMap<String, Consent>,
}

pub struct ConsentStore {
    path: PathBuf,
    file: File,
}

impl ConsentStore {
    /// A missing or broken file gives an empty store.
    pub fn load(path: &Path) -> ConsentStore {
        let file = fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        ConsentStore {
            path: path.to_path_buf(),
            file,
        }
    }

    pub fn get(&self, name: &str) -> Consent {
        self.file.plugins.get(name).cloned().unwrap_or_default()
    }

    /// Stores `c` and saves the file atomically (tmp, then rename). The
    /// in-memory store changes only if the save succeeds.
    pub fn set(&mut self, name: &str, c: Consent) -> io::Result<()> {
        let mut next = File {
            plugins: self.file.plugins.clone(),
        };
        next.plugins.insert(name.to_string(), c);
        let body = toml::to_string(&next).map_err(io::Error::other)?;
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("toml.tmp");
        let written = fs::write(&tmp, body).and_then(|()| fs::rename(&tmp, &self.path));
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        self.file = next;
        Ok(())
    }
}

/// True when the plugin is on and every declared permission has been granted.
pub fn may_run(m: &Manifest, c: &Consent) -> bool {
    c.enabled && m.permissions.iter().all(|p| c.granted.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(perms: Vec<Permission>) -> Manifest {
        Manifest {
            name: "nil-dj".into(),
            version: "1".into(),
            description: String::new(),
            api_version: 1,
            command: vec!["x".into()],
            permissions: perms,
            title: None,
        }
    }

    #[test]
    fn new_plugins_are_off() {
        let t = tempfile::tempdir().unwrap();
        let s = ConsentStore::load(&t.path().join("plugins.toml"));
        let c = s.get("nil-dj");
        assert!(!c.enabled && c.granted.is_empty());
        assert!(!may_run(&manifest(vec![]), &c));
    }

    #[test]
    fn broken_file_gives_an_empty_store() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("plugins.toml");
        fs::write(&p, "[plugins.nil-dj\nenabled = ").unwrap();
        assert_eq!(ConsentStore::load(&p).get("nil-dj"), Consent::default());
    }

    #[test]
    fn consent_round_trips() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("data").join("plugins.toml");
        let all = vec![
            Permission::Playback,
            Permission::Library,
            Permission::Playlists,
        ];
        let mut s = ConsentStore::load(&p);
        s.set(
            "nil-dj",
            Consent {
                enabled: true,
                granted: all.clone(),
            },
        )
        .unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.contains("[plugins.nil-dj]"), "{text}");
        assert!(text.contains("enabled = true"), "{text}");
        assert!(
            text.contains(r#"granted = ["playback", "library", "playlists"]"#),
            "{text}"
        );
        let back = ConsentStore::load(&p).get("nil-dj");
        assert!(back.enabled);
        assert_eq!(back.granted, all);
        assert_eq!(fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn set_keeps_other_plugins() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("plugins.toml");
        let on = |g: Vec<Permission>| Consent {
            enabled: true,
            granted: g,
        };
        let mut s = ConsentStore::load(&p);
        s.set("a", on(vec![Permission::Playback])).unwrap();
        s.set("b", on(vec![Permission::Library])).unwrap();
        let r = ConsentStore::load(&p);
        assert_eq!(r.get("a").granted, [Permission::Playback]);
        assert_eq!(r.get("b").granted, [Permission::Library]);
    }

    #[test]
    fn a_new_permission_switches_the_plugin_off() {
        let c = Consent {
            enabled: true,
            granted: vec![Permission::Playback],
        };
        assert!(may_run(&manifest(vec![Permission::Playback]), &c));
        assert!(!may_run(
            &manifest(vec![Permission::Playback, Permission::Library]),
            &c
        ));
    }
}

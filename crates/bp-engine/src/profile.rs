//! Per-device profile store persisted as TOML.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bp_core::{Capabilities, DeviceFormat, DeviceId, EngineError, OutputError, SampleEncoding};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbedFormat {
    pub rate: u32,
    pub channels: u16,
    pub encoding: SampleEncoding,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceProfile {
    pub name: String,
    #[serde(default)]
    pub probed: Vec<ProbedFormat>,
    #[serde(default)]
    pub dop_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rate: Option<u32>,
    /// Formats turned off by hand; kept across probes.
    #[serde(default)]
    pub disabled: Vec<ProbedFormat>,
    /// Formats the device refused to open although probed; cleared by the next probe.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_disabled: Vec<ProbedFormat>,
}

impl DeviceProfile {
    /// Probed formats minus disabled and auto-disabled ones, dropping rates above
    /// `max_rate`.
    pub fn capabilities(&self) -> Capabilities {
        let formats = self
            .probed
            .iter()
            .filter(|p| !self.disabled.contains(p) && !self.auto_disabled.contains(p))
            .filter(|p| self.max_rate.is_none_or(|m| p.rate <= m))
            .map(|p| (p.rate, p.channels, p.encoding))
            .collect();
        Capabilities { formats }
    }
}

/// A fresh probe result merged into the saved profile: name and probed formats are replaced
/// and `auto_disabled` is cleared; `dop_enabled`, `max_rate` and `disabled` survive (stale
/// `disabled` entries are harmless, `capabilities()` just subtracts them). A probe that found
/// no format is an error.
pub fn merge_probe(
    existing: Option<&DeviceProfile>,
    name: String,
    caps: &Capabilities,
) -> Result<DeviceProfile, OutputError> {
    if caps.formats.is_empty() {
        return Err(OutputError::NoFormats);
    }
    let mut profile = existing.cloned().unwrap_or_default();
    profile.name = name;
    profile.auto_disabled.clear();
    profile.probed = caps
        .formats
        .iter()
        .map(|&(rate, channels, encoding)| ProbedFormat {
            rate,
            channels,
            encoding,
        })
        .collect();
    Ok(profile)
}

#[derive(Debug)]
pub struct ProfileStore {
    path: PathBuf,
    devices: BTreeMap<String, DeviceProfile>,
    last_device: Option<DeviceId>,
}

#[derive(Serialize, Deserialize, Default)]
struct FileModel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_device: Option<DeviceId>,
    #[serde(default)]
    devices: BTreeMap<String, DeviceProfile>,
}

fn profile_err(what: &str, path: &Path, e: impl std::fmt::Display) -> EngineError {
    EngineError::Profile(format!("{what} {}: {e}", path.display()))
}

impl ProfileStore {
    /// Loads the store; a missing file yields an empty store.
    pub fn load(path: &Path) -> Result<Self, EngineError> {
        let model = match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str::<FileModel>(&text).map_err(|e| profile_err("parsing", path, e))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileModel::default(),
            Err(e) => return Err(profile_err("reading", path, e)),
        };
        Ok(Self {
            path: path.to_path_buf(),
            devices: model.devices,
            last_device: model.last_device,
        })
    }

    /// An empty store that saves to `path`, without reading it.
    pub fn empty(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            devices: BTreeMap::new(),
            last_device: None,
        }
    }

    /// The device used most recently by the CLI, if any.
    pub fn last_device(&self) -> Option<&DeviceId> {
        self.last_device.as_ref()
    }

    pub fn set_last_device(&mut self, id: Option<DeviceId>) {
        self.last_device = id;
    }

    pub fn get(&self, id: &DeviceId) -> Option<&DeviceProfile> {
        self.devices.get(&id.0)
    }

    pub fn upsert(&mut self, id: &DeviceId, profile: DeviceProfile) {
        self.devices.insert(id.0.clone(), profile);
    }

    /// Marks a format the device refused as unusable until the next probe; a no-op for
    /// unknown devices.
    pub fn disable_format(&mut self, id: &DeviceId, f: &DeviceFormat) {
        if let Some(p) = self.devices.get_mut(&id.0) {
            let pf = ProbedFormat {
                rate: f.sample_rate,
                channels: f.channels,
                encoding: f.encoding,
            };
            if !p.auto_disabled.contains(&pf) {
                p.auto_disabled.push(pf);
            }
        }
    }

    /// Writes `<path>.tmp` then renames it over `<path>`.
    pub fn save(&self) -> Result<(), EngineError> {
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .map_err(|e| profile_err("creating dir for", &self.path, e))?;
        }
        let text = toml::to_string_pretty(&FileModel {
            last_device: self.last_device.clone(),
            devices: self.devices.clone(),
        })
        .map_err(|e| profile_err("serializing", &self.path, e))?;
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, text).map_err(|e| profile_err("writing", &tmp, e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| profile_err("renaming to", &self.path, e))
    }
}

/// Directory holding `devices.toml` and `bp.log`.
pub fn default_dir() -> PathBuf {
    let (var, rel) = if cfg!(windows) {
        ("APPDATA", "")
    } else {
        ("HOME", ".config")
    };
    match std::env::var_os(var) {
        Some(base) => PathBuf::from(base).join(rel).join("bitperfect"),
        None => PathBuf::from("."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_store_has_no_profiles_and_remembers_its_path() {
        let path = Path::new("somewhere/devices.toml");
        let store = ProfileStore::empty(path);
        assert!(store.devices.is_empty());
        assert_eq!(store.last_device(), None);
        assert_eq!(store.path, path);
    }

    const I24_IN_32: SampleEncoding = SampleEncoding::Int {
        container: 32,
        valid: 24,
    };
    const I16: SampleEncoding = SampleEncoding::Int {
        container: 16,
        valid: 16,
    };

    fn pf(rate: u32, encoding: SampleEncoding) -> ProbedFormat {
        ProbedFormat {
            rate,
            channels: 2,
            encoding,
        }
    }

    fn id() -> DeviceId {
        DeviceId("{0.0.0.00000000}.{8f1c2a3e-1111-2222-3333-444455556666}".to_string())
    }

    fn profile() -> DeviceProfile {
        DeviceProfile {
            name: "USB DAC".into(),
            probed: vec![
                pf(44_100, I16),
                pf(96_000, I24_IN_32),
                pf(192_000, I24_IN_32),
            ],
            dop_enabled: true,
            max_rate: Some(192_000),
            disabled: vec![pf(44_100, I16)],
            auto_disabled: vec![pf(192_000, I24_IN_32)],
        }
    }

    #[test]
    fn roundtrip_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("devices.toml");
        let mut store = ProfileStore::load(&path).unwrap();
        store.upsert(&id(), profile());
        store.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("encoding = \"int32/24\""), "{text}");
        assert!(!dir.path().join("sub").join("devices.toml.tmp").exists());
        let loaded = ProfileStore::load(&path).unwrap();
        assert_eq!(loaded.get(&id()), Some(&profile()));
    }

    #[test]
    fn last_device_roundtrip_and_missing_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.toml");
        let mut store = ProfileStore::load(&path).unwrap();
        assert_eq!(store.last_device(), None);
        store.upsert(&id(), profile());
        store.save().unwrap();
        // Old file without the key loads as None.
        assert_eq!(ProfileStore::load(&path).unwrap().last_device(), None);
        store.set_last_device(Some(id()));
        store.save().unwrap();
        let loaded = ProfileStore::load(&path).unwrap();
        assert_eq!(loaded.last_device(), Some(&id()));
        assert_eq!(loaded.get(&id()), Some(&profile()));
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::load(&dir.path().join("none.toml")).unwrap();
        assert!(store.get(&id()).is_none());
    }

    #[test]
    fn max_rate_override_filters() {
        let mut p = profile();
        p.disabled.clear();
        p.auto_disabled.clear();
        p.max_rate = Some(96_000);
        let caps = p.capabilities();
        assert!(caps.supports(&DeviceFormat {
            sample_rate: 96_000,
            channels: 2,
            encoding: I24_IN_32
        }));
        assert!(!caps.supports(&DeviceFormat {
            sample_rate: 192_000,
            channels: 2,
            encoding: I24_IN_32
        }));
        assert!(caps.supports(&DeviceFormat {
            sample_rate: 44_100,
            channels: 2,
            encoding: I16
        }));
    }

    #[test]
    fn refused_formats_are_auto_disabled_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProfileStore::load(&dir.path().join("d.toml")).unwrap();
        let mut p = profile();
        p.disabled.clear();
        p.auto_disabled.clear();
        p.max_rate = None;
        store.upsert(&id(), p);
        let f = DeviceFormat {
            sample_rate: 96_000,
            channels: 2,
            encoding: I24_IN_32,
        };
        assert!(store.get(&id()).unwrap().capabilities().supports(&f));
        store.disable_format(&id(), &f);
        store.disable_format(&id(), &f);
        let p = store.get(&id()).unwrap();
        assert_eq!(p.auto_disabled.len(), 1);
        assert!(p.disabled.is_empty(), "the user's list is left alone");
        assert!(!p.capabilities().supports(&f));
        // unknown device: no-op
        store.disable_format(&DeviceId("other".into()), &f);
        assert!(store.get(&DeviceId("other".into())).is_none());
    }

    #[test]
    fn merge_probe_keeps_settings_and_replaces_probe_data() {
        let existing = profile();
        let mut caps = Capabilities::default();
        caps.formats.insert((48_000, 2, I16));
        let merged = merge_probe(Some(&existing), "new".into(), &caps).unwrap();
        assert_eq!(merged.name, "new");
        assert_eq!(merged.probed, vec![pf(48_000, I16)]);
        assert!(merged.dop_enabled);
        assert_eq!(merged.max_rate, Some(192_000));
        assert_eq!(merged.disabled, existing.disabled);
        // A fresh probe is the truth: formats refused before get another chance.
        assert!(merged.auto_disabled.is_empty());

        let fresh = merge_probe(None, "n".into(), &caps).unwrap();
        assert!(!fresh.dop_enabled && fresh.max_rate.is_none() && fresh.disabled.is_empty());
    }

    #[test]
    fn merge_probe_rejects_empty_probe() {
        assert!(matches!(
            merge_probe(Some(&profile()), "n".into(), &Capabilities::default()),
            Err(OutputError::NoFormats)
        ));
    }

    #[test]
    fn corrupt_file_is_profile_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.toml");
        std::fs::write(&path, "this is [not valid toml").unwrap();
        assert!(matches!(
            ProfileStore::load(&path),
            Err(EngineError::Profile(_))
        ));
    }
}

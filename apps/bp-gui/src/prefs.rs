//! UI preferences persisted in `gui.toml`.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Unknown keys (such as the old `tile_size` and `rail_hidden`) are ignored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Last selected output device id.
    pub device: Option<String>,
}

impl Prefs {
    /// A missing or unreadable file gives the defaults.
    pub fn load(path: &Path) -> Prefs {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Writes via a temporary file so a crash never leaves a half-written file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

use std::path::PathBuf;

/// Directory holding `gui.toml`, `library.toml` and the engine's `devices.toml`.
/// Does not create it.
pub fn config_dir() -> PathBuf {
    bp_engine::profile::default_dir()
}

/// Thumbnail cache directory. Does not create it.
pub fn thumbs_dir() -> PathBuf {
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(base).join("bitperfect").join("thumbs");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".cache").join("bitperfect").join("thumbs")
}

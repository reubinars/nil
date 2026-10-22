//! The nil music player. Release builds on Windows run without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::Path;
use std::sync::Arc;

use bp_engine::profile::ProfileStore;
use bp_gui::app::{BpApp, Deps};
use bp_gui::backend::PlayerHandle;
use bp_gui::backend::fs_library::FsLibrary;
use bp_gui::backend::mock_player::{MockPlayer, SystemClock, demo_devices};
use bp_gui::paths::{config_dir, thumbs_dir};

/// The real engine on Windows, unless `--mock` asks for the demo player.
fn make_player(clock: &Arc<SystemClock>) -> Box<dyn PlayerHandle> {
    #[cfg(windows)]
    if !std::env::args().any(|a| a == "--mock") {
        return real_player();
    }
    Box::new(MockPlayer::new(demo_devices(), clock.clone()))
}

#[cfg(windows)]
fn real_player() -> Box<dyn PlayerHandle> {
    use bp_engine::profile::default_dir;
    use bp_engine::{Engine, EngineConfig};
    use bp_gui::backend::engine_player::EnginePlayer;
    use bp_output::WasapiBackend;

    let path = default_dir().join("devices.toml");
    let profiles = load_profiles(&path);
    let engine = Engine::start(
        Box::new(WasapiBackend::new()),
        EngineConfig {
            device: None,
            profiles,
        },
    );
    Box::new(EnginePlayer::new(engine))
}

/// Loads the device profiles. An unreadable file is moved to `<name>.bad` and
/// the app starts with an empty store; this never fails.
#[cfg_attr(not(windows), allow(dead_code))]
fn load_profiles(path: &Path) -> ProfileStore {
    match ProfileStore::load(path) {
        Ok(store) => store,
        Err(e) => {
            let bad = path.with_extension("toml.bad");
            eprintln!(
                "bp-gui: {} is unreadable ({e}); moving it to {}",
                path.display(),
                bad.display()
            );
            if let Err(e) = std::fs::rename(path, &bad) {
                eprintln!("bp-gui: cannot move it aside: {e}; starting with no profiles");
                return ProfileStore::empty(path);
            }
            ProfileStore::load(path).unwrap_or_else(|e| {
                eprintln!("bp-gui: starting with no profiles: {e}");
                ProfileStore::empty(path)
            })
        }
    }
}

fn main() -> eframe::Result {
    let config = config_dir();
    let thumbs = thumbs_dir();
    for dir in [&config, &thumbs] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("bp-gui: cannot create {}: {e}", dir.display());
        }
    }

    // The player and the app must read the same clock.
    let clock = Arc::new(SystemClock::new());
    let player = make_player(&clock);
    let library = Arc::new(FsLibrary::open(config.join("library.toml")));
    let prefs_path = config.join("gui.toml");

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("nil")
            .with_icon(std::sync::Arc::new(bp_gui::icon::app_icon()))
            // The top bar draws the title bar: window buttons, move and resize.
            .with_decorations(false),
        ..Default::default()
    };
    eframe::run_native(
        "bitperfect",
        options,
        Box::new(move |cc| {
            let deps = Deps {
                player,
                library,
                clock,
                prefs_path: Some(prefs_path),
                thumbs_dir: thumbs,
                pick_folder: Box::new(|| rfd::FileDialog::new().pick_folder()),
            };
            Ok(Box::new(BpApp::new(&cc.egui_ctx, deps)))
        }),
    )
}

#[cfg(test)]
mod tests {
    use bp_core::DeviceId;

    use super::*;

    #[test]
    fn corrupt_profiles_are_moved_aside_and_an_empty_store_returned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.toml");
        std::fs::write(&path, "this is [not toml").unwrap();
        let store = load_profiles(&path);
        assert_eq!(store.last_device(), None);
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("devices.toml.bad")).unwrap(),
            "this is [not toml"
        );
    }

    #[test]
    fn the_store_from_corrupt_profiles_saves_to_the_original_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.toml");
        std::fs::write(&path, "this is [not toml").unwrap();
        let mut store = load_profiles(&path);
        store.set_last_device(Some(DeviceId("dac".into())));
        store.save().unwrap();
        let reloaded = ProfileStore::load(&path).unwrap();
        assert_eq!(reloaded.last_device(), Some(&DeviceId("dac".into())));
    }

    /// Unix only: a read-only directory blocks the rename there, not on Windows.
    #[cfg(unix)]
    #[test]
    fn corrupt_profiles_that_cannot_be_moved_give_an_empty_store_and_stay() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.toml");
        std::fs::write(&path, "this is [not toml").unwrap();
        let mode = |m| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(dir.path(), mode(0o555)).unwrap();
        let store = load_profiles(&path);
        std::fs::set_permissions(dir.path(), mode(0o755)).unwrap();
        assert_eq!(store.last_device(), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "this is [not toml");
        assert!(!dir.path().join("devices.toml.bad").exists());
    }

    #[test]
    fn an_unreadable_path_gives_an_empty_store_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be: reading it fails.
        let path = dir.path().join("devices.toml");
        std::fs::create_dir(&path).unwrap();
        let store = load_profiles(&path);
        assert_eq!(store.last_device(), None);
    }

    #[test]
    fn a_missing_file_is_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = load_profiles(&dir.path().join("devices.toml"));
        assert_eq!(store.last_device(), None);
    }
}

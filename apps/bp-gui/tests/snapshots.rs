//! Pixel snapshots of the main views. Baselines are generated on Windows:
//! `UPDATE_SNAPSHOTS=1 cargo test -p bp-gui --features snapshots --test snapshots`.
//! On Windows add `-- --test-threads=1`: harnesses running in parallel crash in the
//! GPU stack.
#![cfg(feature = "snapshots")]

mod common;

use std::path::PathBuf;

use bp_core::EngineError;
use bp_gui::backend::{LibrarySource, PlayerEvent};
use common::*;
use egui::Role;
use egui_kittest::kittest::Queryable;

#[test]
fn grid() {
    let mut f = library();
    f.harness.run();
    f.harness.get_by_role_and_label(Role::Tab, "Albums").click();
    f.harness.run();
    f.harness.snapshot("grid");
}

#[test]
fn home() {
    let mut f = library();
    f.harness.run();
    f.harness.snapshot("home");
}

#[test]
fn grid_under_the_bar() {
    let many: Vec<_> = (1..=24)
        .map(|i| st(&format!("Artist {i:02}"), &format!("Album {i:02}"), "t", 1))
        .collect();
    let mut f = fixture_with(many, vec![PathBuf::from("/music")], None);
    f.harness.run();
    f.open_album("Album 01");
    f.double_click_track("t");
    f.advance(30.0);
    // The mock writes audio as its clock moves.
    f.advance(0.2);
    f.harness.get_by_label("Back").click();
    f.harness.run();
    f.harness.snapshot("grid_under_the_bar");
}

#[test]
fn album_page() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.harness.snapshot("album_page");
}

#[test]
fn context_menu() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.right_click_track("So What");
    f.harness.snapshot("context_menu");
}

#[test]
fn settings() {
    let mut f = library();
    f.harness.get_by_label("Settings").click();
    f.harness.run();
    f.harness.snapshot("settings");
}

#[test]
fn device_list() {
    let mut f = library();
    f.harness
        .get_by_role_and_label(Role::Button, "DAC A")
        .click();
    f.harness.run();
    f.harness.snapshot("device_list");
}

#[test]
fn playing_bar() {
    let mut f = library();
    f.open_album("Kid A");
    f.double_click_track("Everything in Its Right Place");
    f.advance(80.0);
    // The mock writes audio as its clock moves.
    f.advance(0.2);
    f.harness.snapshot("playing_bar");
}

#[test]
fn now_playing() {
    let mut f = library();
    let kob = f
        .lib
        .albums(None)
        .into_iter()
        .find(|a| a.title == "Kind of Blue")
        .unwrap()
        .id;
    let img = image::RgbaImage::from_fn(64, 64, |x, y| {
        image::Rgba([0xD7 - (x + y) as u8 / 2, 0x26, 0x3D, 255])
    });
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png).unwrap();
    f.lib.set_art(kob, png.into_inner());
    // A rescan picks the cover up.
    f.lib
        .set_scan_status(bp_gui::backend::ScanStatus::Scanning { done: 0, total: 1 });
    f.harness.run();
    f.lib
        .set_scan_status(bp_gui::backend::ScanStatus::Done { unreadable: 0 });
    f.harness.run();
    f.open_album("Kind of Blue");
    f.double_click_track("Freddie Freeloader");
    f.advance(50.0);
    // The mock writes audio as its clock moves.
    f.advance(0.2);
    f.harness
        .get_by_role_and_label(Role::Button, "Open now playing")
        .click();
    for _ in 0..50 {
        f.harness.run();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    f.harness.snapshot("now_playing");
}

#[test]
fn fatal_error_bar() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("Blue in Green");
    f.advance(0.4);
    f.player.inject(PlayerEvent::Engine(bp_engine::Event::Error(
        EngineError::NoDevice,
    )));
    f.harness.run();
    f.harness.snapshot("fatal_error_bar");
}

#[test]
fn first_run() {
    let mut f = fixture(Vec::<PathBuf>::new(), None);
    f.harness.run();
    f.harness.snapshot("first_run");
}

#[test]
fn playlists_page() {
    let mut f = library();
    let files: Vec<_> = ["a", "b"]
        .iter()
        .map(|n| {
            let p = f.dir.path().join(format!("{n}.flac"));
            std::fs::write(&p, b"").unwrap();
            bp_gui::playlists::Entry {
                path: p,
                title: Some(format!("Track {n}")),
                artist: None,
                duration_s: Some(60),
            }
        })
        .collect();
    let dir = f.dir.path().join("playlists");
    for name in ["Late night", "Road trip", "夜のドライブ"] {
        bp_gui::playlists::save(&dir, name, &files).unwrap();
    }
    f.harness
        .get_by_role_and_label(Role::Tab, "Playlists")
        .click();
    f.harness.run();
    f.harness.snapshot("playlists_page");
}

#[test]
fn sample_panel() {
    let mut f = library();
    let panel = bp_gui::plugins::panel::parse(&serde_json::json!({"elements": [
        {"type": "heading", "text": "Mood mix"},
        {"type": "text", "text": "Describe what you want to hear.", "muted": true},
        {"type": "input", "id": "q", "placeholder": "Ask for a mood", "submit_label": "Go"},
        {"type": "row", "children": [
            {"type": "button", "id": "b", "label": "Surprise me"},
            {"type": "toggle", "id": "t", "label": "Keep it quiet", "value": true},
        ]},
        {"type": "progress", "value": 0.4, "label": "Choosing tracks"},
        // One track from the library (its album's placeholder art) and one from outside.
        {"type": "tracks", "label": "Mood mix · 2 tracks", "items": [
            {"path": "/music/Miles Davis/Kind of Blue/So What.flac", "title": "So What", "artist": "Miles Davis", "duration_s": 545},
            {"path": "/music/b.flac", "title": "Song B", "artist": "Band", "duration_s": 187},
        ]},
    ]}))
    .unwrap();
    f.harness.state_mut().set_panel("dj".into(), panel);
    f.harness.run();
    f.harness.get_by_role_and_label(Role::Tab, "dj").click();
    f.harness.run();
    f.harness.snapshot("sample_panel");
}

#[test]
fn plugins_page() {
    use bp_gui::plugins::consent::Consent;
    use bp_gui::plugins::host::{PluginInfo, Status};
    use bp_gui::plugins::manifest::Manifest;
    use bp_gui::plugins::rpc::Permission;

    let all = vec![
        Permission::Playback,
        Permission::Library,
        Permission::Playlists,
    ];
    let manifest = |name: &str, description: &str| Manifest {
        name: name.into(),
        version: "0.3.1".into(),
        description: description.into(),
        api_version: 1,
        command: vec!["python".into(), "main.py".into()],
        permissions: all.clone(),
        title: None,
    };
    let on = Consent {
        enabled: true,
        granted: all.clone(),
    };
    let mut f = fixture_sized(tracks(), egui::vec2(1280.0, 1700.0), false);
    f.harness.state_mut().set_plugin_infos(vec![
        PluginInfo {
            folder: "nil-dj".into(),
            manifest: Some(manifest("nil-dj", "Builds playlists from a mood")),
            consent: on.clone(),
            status: Status::Running,
            stderr_tail: Vec::new(),
        },
        PluginInfo {
            folder: "scrobbler".into(),
            manifest: Some(manifest("scrobbler", "Reports what you play")),
            consent: on,
            status: Status::Crashed {
                reason: "exited with code 1".into(),
            },
            stderr_tail: vec![
                "Traceback (most recent call last):".into(),
                "  File \"main.py\", line 12, in <module>".into(),
                "ConnectionError: no network".into(),
            ],
        },
        PluginInfo {
            folder: "broken".into(),
            manifest: None,
            consent: Consent::default(),
            status: Status::InvalidManifest("missing field `command`".into()),
            stderr_tail: Vec::new(),
        },
    ]);
    let settings = bp_gui::plugins::panel::parse(&serde_json::json!({"elements": [
        {"type": "text", "text": "2,890 tracks indexed"},
        {"type": "progress", "value": 0.4, "label": "Indexing new tracks"},
        {"type": "row", "children": [
            {"type": "button", "id": "index", "label": "Update index"},
        ]},
    ]}))
    .unwrap();
    f.harness
        .state_mut()
        .set_settings_panel("nil-dj".into(), Some(settings));
    f.harness.get_by_label("Settings").click();
    f.harness.run();
    f.harness.snapshot("plugins_page");
}

//! The `hello` example plugin against the real host, when Python is installed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bp_core::{Codec, SourceFormat, SourceKind};
use bp_gui::backend::index::{MemLibrary, ScannedTrack};
use bp_gui::catalog::Catalog;
use bp_gui::plugin_bridge::{BridgeCtx, answer};
use bp_gui::plugins::consent::{Consent, ConsentStore};
use bp_gui::plugins::host::{HostEvent, PluginHost, Timings};
use bp_gui::plugins::panel::{Element, Panel};
use bp_gui::plugins::rpc::Permission;
use bp_gui::state::now_playing::NowPlaying;

fn python_available() -> bool {
    Command::new("python")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_file() {
            fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

fn track(i: usize) -> ScannedTrack {
    ScannedTrack {
        path: PathBuf::from(format!("/music/Album/{i:02} 夜.flac")),
        album_artist: Some("Ada".into()),
        artist: None,
        album: Some("Album".into()),
        title: Some(format!("曲 {i}")),
        year: None,
        disc: Some(1),
        number: Some(i as u16),
        compilation: false,
        modified_ns: 0,
        source: SourceFormat {
            codec: Codec::Flac,
            sample_rate: 44100,
            channels: 2,
            kind: SourceKind::Pcm {
                bits: 16,
                float: false,
            },
        },
        duration: Duration::from_secs(60),
    }
}

/// Answers the plugin's calls and returns the next panel it sets.
fn next_panel(host: &PluginHost, catalog: &Catalog, playlists: &Path) -> Panel {
    let np = NowPlaying::default();
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        match host.try_recv() {
            Some(HostEvent::Call { plugin, id, call }) => {
                let ctx = BridgeCtx {
                    catalog,
                    np: &np,
                    folders: vec![PathBuf::from("/music")],
                    playlists_dir: playlists,
                    device_ready: true,
                };
                host.reply(&plugin, id, answer(call, &ctx).0);
            }
            Some(HostEvent::Panel {
                panel: Some(panel), ..
            }) => return panel,
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("no panel within 20 s");
}

fn heading(p: &Panel) -> String {
    p.elements
        .iter()
        .find_map(|e| match e {
            Element::Heading { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("a heading")
}

fn track_count(p: &Panel) -> usize {
    p.elements
        .iter()
        .find_map(|e| match e {
            Element::Tracks { items, .. } => Some(items.len()),
            _ => None,
        })
        .expect("a tracks list")
}

#[test]
fn hello_plugin_end_to_end() {
    if !python_available() {
        eprintln!("skipping hello_plugin_end_to_end: `python --version` failed");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/hello");
    copy_dir(&example, &plugins.join("hello"));

    let consent_path = tmp.path().join("data").join("plugins.toml");
    ConsentStore::load(&consent_path)
        .set(
            "hello",
            Consent {
                enabled: true,
                granted: vec![Permission::Library, Permission::Playback],
            },
        )
        .unwrap();

    let lib = MemLibrary::new((1..=15).map(track).collect(), vec![PathBuf::from("/music")]);
    let catalog = Catalog::build(&lib);
    let playlists = tmp.path().join("data").join("playlists");
    let host = PluginHost::start(
        plugins,
        consent_path,
        tmp.path().join("data").join("logs"),
        Timings::default(),
        Arc::new(|| {}),
    );

    let first = next_panel(&host, &catalog, &playlists);
    assert_eq!(heading(&first), "Hello from nil");
    assert_eq!(track_count(&first), 0);

    host.notify("hello", "panel.event", serde_json::json!({"id": "shuffle"}));
    let shuffled = next_panel(&host, &catalog, &playlists);
    assert_eq!(track_count(&shuffled), 10);
    // Non-ASCII titles and paths survive both directions.
    for e in &shuffled.elements {
        if let Element::Tracks { items, .. } = e {
            assert!(
                items
                    .iter()
                    .all(|t| t.title.starts_with("曲 ") && t.path.to_string_lossy().contains('夜'))
            );
        }
    }

    host.notify(
        "hello",
        "panel.event",
        serde_json::json!({"id": "name", "value": "Ada"}),
    );
    let greeted = next_panel(&host, &catalog, &playlists);
    assert_eq!(heading(&greeted), "Hello, Ada");

    host.shutdown_all();
}

//! Fixtures shared by the UI and snapshot tests.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bp_core::{
    Capabilities, Codec, DeviceId, PROBE_ENCODINGS, PROBE_RATES, SampleEncoding, SourceFormat,
    SourceKind,
};
use bp_engine::profile::{DeviceProfile, ProbedFormat, ProfileStore};
use bp_engine::{Engine, EngineConfig};
use bp_gui::app::{BpApp, Deps};
use bp_gui::backend::engine_player::EnginePlayer;
use bp_gui::backend::index::{MemLibrary, ScannedTrack};
use bp_gui::backend::mock_player::{
    ManualClock, MockDevice, MockPlayer, SystemClock, demo_devices,
};
use bp_gui::backend::{LibrarySource, PlayerCommand, QueueItem};
use bp_gui::prefs::Prefs;
use bp_output::{CaptureBackend, CaptureLog};
use egui::{Modifiers, PointerButton, Role};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tempfile::TempDir;

pub const CD: SourceFormat = SourceFormat {
    codec: Codec::Flac,
    sample_rate: 44100,
    channels: 2,
    kind: SourceKind::Pcm {
        bits: 16,
        float: false,
    },
};

pub const HIRES: SourceFormat = SourceFormat {
    codec: Codec::Flac,
    sample_rate: 192_000,
    channels: 2,
    kind: SourceKind::Pcm {
        bits: 24,
        float: false,
    },
};

pub fn dsd(multiple: u16) -> SourceFormat {
    SourceFormat {
        codec: Codec::Dsf,
        sample_rate: 2_822_400 * u32::from(multiple) / 64,
        channels: 2,
        kind: SourceKind::Dsd { multiple },
    }
}

pub fn st(artist: &str, album: &str, title: &str, n: u16) -> ScannedTrack {
    stf(artist, album, title, n, CD)
}

pub fn stf(artist: &str, album: &str, title: &str, n: u16, source: SourceFormat) -> ScannedTrack {
    ScannedTrack {
        compilation: false,
        modified_ns: 0,
        path: PathBuf::from(format!("/music/{artist}/{album}/{title}.flac")),
        album_artist: Some(artist.into()),
        artist: None,
        album: Some(album.into()),
        title: Some(title.into()),
        year: None,
        disc: None,
        number: Some(n),
        source,
        duration: Duration::from_secs(300),
    }
}

pub const KIND_OF_BLUE: [&str; 5] = [
    "So What",
    "Freddie Freeloader",
    "Blue in Green",
    "All Blues",
    "Flamenco Sketches",
];

pub fn tracks() -> Vec<ScannedTrack> {
    let mut v: Vec<_> = KIND_OF_BLUE
        .iter()
        .zip(1..)
        .map(|(t, n)| st("Miles Davis", "Kind of Blue", t, n))
        .collect();
    v.push(stf(
        "Radiohead",
        "Kid A",
        "Everything in Its Right Place",
        1,
        HIRES,
    ));
    v.push(stf("Test Artist", "Test Tones", "Tone 256", 1, dsd(256)));
    v.push(stf("Test Artist", "Test Tones", "Tone 128", 2, dsd(128)));
    v
}

pub struct Fixture {
    pub harness: Harness<'static, BpApp>,
    pub lib: Arc<MemLibrary>,
    pub player: MockPlayer,
    pub clock: Arc<ManualClock>,
    pub dir: TempDir,
}

impl Fixture {
    pub fn prefs_path(&self) -> PathBuf {
        self.dir.path().join("gui.toml")
    }

    pub fn last_sent(&self) -> Option<PlayerCommand> {
        self.player.sent().pop()
    }

    /// Advances the player clock and lets the app catch up.
    pub fn advance(&mut self, secs: f64) {
        self.clock.advance(Duration::from_secs_f64(secs));
        self.harness.run();
    }

    pub fn open_album(&mut self, title: &str) {
        self.harness
            .get_by_role_and_label(Role::Button, title)
            .click();
        self.harness.run();
    }

    /// Two clicks inside one frame: kittest steps 0.25 s per queued event, which
    /// is longer than egui's double-click window.
    pub fn double_click_track(&mut self, title: &str) {
        let pos = self
            .harness
            .get_by_role_and_label(Role::ListItem, title)
            .rect()
            .center();
        let events = &mut self.harness.input_mut().events;
        events.push(egui::Event::PointerMoved(pos));
        for _ in 0..2 {
            for pressed in [true, false] {
                events.push(egui::Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::default(),
                });
            }
        }
        self.harness.run();
    }

    /// Hovers a node long enough for its tooltip to show.
    pub fn hover_long(&mut self, role: Role, label: &str) {
        self.harness.get_by_role_and_label(role, label).hover();
        self.harness.run_steps(4);
    }
}

pub fn fixture_with(
    tracks: Vec<ScannedTrack>,
    folders: Vec<PathBuf>,
    picked: Option<PathBuf>,
) -> Fixture {
    fixture_full(tracks, demo_devices(), folders, picked)
}

pub fn fixture_full(
    tracks: Vec<ScannedTrack>,
    devices: Vec<MockDevice>,
    folders: Vec<PathBuf>,
    picked: Option<PathBuf>,
) -> Fixture {
    fixture_prefs(tracks, devices, folders, picked, None)
}

/// `prefs`, if given, is written to `gui.toml` before the app starts.
pub fn fixture_prefs(
    tracks: Vec<ScannedTrack>,
    devices: Vec<MockDevice>,
    folders: Vec<PathBuf>,
    picked: Option<PathBuf>,
    prefs: Option<Prefs>,
) -> Fixture {
    build(
        tracks,
        devices,
        folders,
        picked,
        prefs,
        egui::vec2(1280.0, 800.0),
        false,
    )
}

/// A library window of `size`; `tofu` draws boxes for glyphs no font has (CJK on a
/// machine without CJK fonts) instead of failing the test.
pub fn fixture_sized(tracks: Vec<ScannedTrack>, size: egui::Vec2, tofu: bool) -> Fixture {
    build(
        tracks,
        demo_devices(),
        vec![PathBuf::from("/music")],
        None,
        None,
        size,
        tofu,
    )
}

pub fn build(
    tracks: Vec<ScannedTrack>,
    devices: Vec<MockDevice>,
    folders: Vec<PathBuf>,
    picked: Option<PathBuf>,
    prefs: Option<Prefs>,
    size: egui::Vec2,
    tofu: bool,
) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    if let Some(p) = prefs {
        p.save(&dir.path().join("gui.toml")).unwrap();
    }
    let lib = Arc::new(MemLibrary::new(tracks, folders));
    let clock = Arc::new(ManualClock::new());
    let player = MockPlayer::new(devices, clock.clone());
    let deps = Deps {
        player: Box::new(player.clone()),
        library: lib.clone(),
        clock: clock.clone(),
        prefs_path: Some(dir.path().join("gui.toml")),
        thumbs_dir: dir.path().join("thumbs"),
        pick_folder: Box::new(move || picked.clone()),
    };
    // Art results arrive from worker threads and each asks for a repaint.
    let mut builder = Harness::builder().with_size(size).with_max_steps(64);
    if tofu {
        builder = builder.allow_missing_glyphs();
    }
    let harness = builder.build_eframe(move |cc| BpApp::new(&cc.egui_ctx, deps));
    Fixture {
        harness,
        lib,
        player,
        clock,
        dir,
    }
}

pub fn fixture(folders: Vec<PathBuf>, picked: Option<PathBuf>) -> Fixture {
    fixture_with(tracks(), folders, picked)
}

pub fn library() -> Fixture {
    fixture(vec![PathBuf::from("/music")], None)
}

pub fn album_items(f: &Fixture, title: &str) -> Vec<QueueItem> {
    let album = f
        .lib
        .albums(None)
        .into_iter()
        .find(|a| a.title == title)
        .unwrap();
    f.lib
        .tracks(album.id)
        .into_iter()
        .map(|t| QueueItem {
            path: t.path,
            source: t.source,
            duration: t.duration,
            title: t.title,
        })
        .collect()
}

pub const CTRL: Modifiers = Modifiers {
    alt: false,
    ctrl: true,
    shift: false,
    mac_cmd: false,
    command: true,
};

/// A GUI wired to the real engine on a capture backend.
pub struct EngineFixture {
    pub harness: Harness<'static, BpApp>,
    pub log: CaptureLog,
    pub dir: TempDir,
}

impl EngineFixture {
    /// Runs frames until `pred` holds on the app, or panics after 3 s.
    pub fn wait_until(&mut self, what: &str, pred: impl Fn(&BpApp) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            self.harness.run_steps(2);
            if pred(self.harness.state()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "timed out waiting for {what}: {:?}",
            self.harness.state().now_playing()
        );
    }
}

fn dac_a() -> Capabilities {
    let mut c = Capabilities::default();
    for rate in PROBE_RATES.into_iter().filter(|r| *r <= 384_000) {
        for e in PROBE_ENCODINGS {
            if e != SampleEncoding::Float32 {
                c.formats.insert((rate, 2, e));
            }
        }
    }
    c
}

/// A 16-bit stereo 44.1 kHz WAV of `secs` seconds of silence. The capture
/// backend drains faster than real time, so tests use long (sparse) files to
/// keep a track playing while they look at the app.
fn write_wav(path: &std::path::Path, secs: u32) {
    use std::io::Write;
    let data_len = 44_100 * 4 * secs;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&44_100u32.to_le_bytes());
    b.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(&b).unwrap();
    f.set_len(b.len() as u64 + u64::from(data_len)).unwrap();
}

const SECS: u32 = 600;

/// Album "Real Album" by "Real Artist" with tracks "First" and "Second"
/// (600 s WAVs; sparse files, so cheap to create. They are long because the capture
/// backend drains faster than real time and the app must still be mid-track),
/// played by the real engine on a capture DAC with a stored profile.
pub fn engine_fixture() -> EngineFixture {
    let dir = tempfile::tempdir().unwrap();
    let source = SourceFormat {
        codec: Codec::Wav,
        sample_rate: 44_100,
        channels: 2,
        kind: SourceKind::Pcm {
            bits: 16,
            float: false,
        },
    };
    let tracks: Vec<ScannedTrack> = ["First", "Second"]
        .iter()
        .zip(1..)
        .map(|(title, n)| {
            let path = dir.path().join(format!("{title}.wav"));
            write_wav(&path, SECS);
            ScannedTrack {
                compilation: false,
                modified_ns: 0,
                path,
                album_artist: Some("Real Artist".into()),
                artist: None,
                album: Some("Real Album".into()),
                title: Some((*title).into()),
                year: None,
                disc: None,
                number: Some(n),
                source,
                duration: Duration::from_secs(u64::from(SECS)),
            }
        })
        .collect();

    let caps = dac_a();
    let (backend, log) = CaptureBackend::new(caps.clone());
    let mut profiles = ProfileStore::load(&dir.path().join("devices.toml")).unwrap();
    profiles.upsert(
        &DeviceId("capture".into()),
        DeviceProfile {
            name: "Capture".into(),
            probed: caps
                .formats
                .iter()
                .map(|&(rate, channels, encoding)| ProbedFormat {
                    rate,
                    channels,
                    encoding,
                })
                .collect(),
            dop_enabled: false,
            max_rate: None,
            disabled: Vec::new(),
            auto_disabled: Vec::new(),
        },
    );
    let engine = Engine::start(
        Box::new(backend),
        EngineConfig {
            device: None,
            profiles,
        },
    );

    let lib = Arc::new(MemLibrary::new(tracks, vec![dir.path().to_path_buf()]));
    let deps = Deps {
        player: Box::new(EnginePlayer::new(engine)),
        library: lib,
        clock: Arc::new(SystemClock::new()),
        prefs_path: Some(dir.path().join("gui.toml")),
        thumbs_dir: dir.path().join("thumbs"),
        pick_folder: Box::new(|| None),
    };
    let harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_max_steps(64)
        .build_eframe(move |cc| BpApp::new(&cc.egui_ctx, deps));
    EngineFixture { harness, log, dir }
}

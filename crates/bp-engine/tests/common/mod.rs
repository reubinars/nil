//! Helpers shared by the engine integration tests.
#![allow(dead_code)] // each test binary uses a subset

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bp_core::{Capabilities, DeviceFormat, DeviceId, PROBE_ENCODINGS, PROBE_RATES, SampleEncoding};
use bp_engine::profile::{DeviceProfile, ProbedFormat, ProfileStore};
use bp_engine::{Command, Engine, EngineConfig, Event};
use bp_output::{CaptureBackend, CaptureLog};

pub const TIMEOUT: Duration = Duration::from_secs(10);

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../bp-decode/tests/fixtures/"
    ))
    .join(name)
}

pub fn wav_data_chunk(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap();
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        if &bytes[at..at + 4] == b"data" {
            return bytes[at + 8..at + 8 + size].to_vec();
        }
        at += 8 + size + (size & 1);
    }
    panic!("no data chunk");
}

pub fn data(name: &str) -> Vec<u8> {
    wav_data_chunk(&fixture(name))
}

pub fn enc(s: &str) -> SampleEncoding {
    s.parse().unwrap()
}

pub fn fmt(rate: u32, e: &str) -> DeviceFormat {
    DeviceFormat {
        sample_rate: rate,
        channels: 2,
        encoding: enc(e),
    }
}

pub fn only(list: &[(u32, &str)]) -> Capabilities {
    let mut c = Capabilities::default();
    for &(rate, e) in list {
        c.formats.insert((rate, 2, enc(e)));
    }
    c
}

/// Every probe encoding except Float32 at every probe rate ≤ `max`, stereo.
pub fn dac_up_to(max: u32) -> Capabilities {
    let mut c = Capabilities::default();
    for rate in PROBE_RATES.into_iter().filter(|r| *r <= max) {
        for e in PROBE_ENCODINGS {
            if e != SampleEncoding::Float32 {
                c.formats.insert((rate, 2, e));
            }
        }
    }
    c
}

pub fn dac_a() -> Capabilities {
    dac_up_to(384_000)
}

pub fn dac_b() -> Capabilities {
    dac_up_to(192_000)
}

pub fn capture_id() -> DeviceId {
    DeviceId("capture".into())
}

pub fn profile_for(caps: &Capabilities, dop_enabled: bool) -> DeviceProfile {
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
        dop_enabled,
        max_rate: None,
        disabled: Vec::new(),
        auto_disabled: Vec::new(),
    }
}

pub struct Rig {
    pub engine: Engine,
    pub log: CaptureLog,
    pub profile_path: PathBuf,
    pub _dir: tempfile::TempDir,
}

/// Starts an engine on a capture backend. `profile`: `Some(dop)` stores a profile matching
/// `caps` for the capture device; `None` leaves the store empty.
pub fn rig_with(
    backend: CaptureBackend,
    log: CaptureLog,
    caps: &Capabilities,
    profile: Option<bool>,
) -> Rig {
    rig_with_profile(backend, log, profile.map(|dop| profile_for(caps, dop)))
}

/// Starts an engine on a capture backend with `profile` (if any) stored for the capture
/// device.
pub fn rig_with_profile(
    backend: CaptureBackend,
    log: CaptureLog,
    profile: Option<DeviceProfile>,
) -> Rig {
    let profiles = profile.map(|p| (capture_id(), p)).into_iter().collect();
    rig_with_profiles(backend, log, profiles)
}

/// Starts an engine on a capture backend with `profiles` stored.
pub fn rig_with_profiles(
    backend: CaptureBackend,
    log: CaptureLog,
    stored: Vec<(DeviceId, DeviceProfile)>,
) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let profile_path = dir.path().join("devices.toml");
    let mut profiles = ProfileStore::load(&profile_path).unwrap();
    for (id, p) in stored {
        profiles.upsert(&id, p);
    }
    let engine = Engine::start(
        Box::new(backend),
        EngineConfig {
            device: None,
            profiles,
        },
    );
    Rig {
        engine,
        log,
        profile_path,
        _dir: dir,
    }
}

pub fn rig(caps: Capabilities) -> Rig {
    let (backend, log) = CaptureBackend::new(caps.clone());
    rig_with(backend, log, &caps, Some(false))
}

pub fn play(rig: &Rig, tracks: Vec<PathBuf>, start: usize) {
    rig.engine.send(Command::Play { tracks, start });
}

/// Collects events until `pred` matches one (included in the result); panics on timeout.
pub fn wait_for(engine: &Engine, timeout: Duration, pred: impl Fn(&Event) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match engine.events().recv_timeout(left) {
            Ok(e) => {
                let hit = pred(&e);
                seen.push(e);
                if hit {
                    return seen;
                }
            }
            Err(_) => panic!("timed out waiting for event; seen: {seen:#?}"),
        }
    }
}

pub fn wait_finished(rig: &Rig) -> Vec<Event> {
    wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::QueueFinished | Event::Error(_))
    })
}

pub fn started(events: &[Event]) -> Vec<usize> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TrackStarted { index, .. } => Some(*index),
            _ => None,
        })
        .collect()
}

/// The per-track events in the order they were emitted.
pub fn track_events(events: &[Event]) -> Vec<(&'static str, usize)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TrackStarted { index, .. } => Some(("started", *index)),
            Event::TrackSkipped { index, .. } => Some(("skipped", *index)),
            Event::TrackError { index, .. } => Some(("error", *index)),
            _ => None,
        })
        .collect()
}

pub fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cond()
}

/// Asserts that no event matching `pred` arrives within `window`.
pub fn assert_quiet(engine: &Engine, window: Duration, pred: impl Fn(&Event) -> bool) {
    let deadline = Instant::now() + window;
    while let Ok(e) = engine
        .events()
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        assert!(!pred(&e), "unexpected event {e:?}");
    }
}

/// A long queue: decoding is slower than the capture backend consumes, so it is still
/// playing when the test acts.
pub fn long_queue() -> Vec<PathBuf> {
    vec![fixture("flac_24_192.flac"); 40]
}

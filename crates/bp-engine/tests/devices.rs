mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bp_core::{Capabilities, DeviceFormat, DeviceId, DeviceInfo, OutputError};
use bp_engine::profile::ProfileStore;
use bp_engine::{Command, Engine, EngineConfig, Event};
use bp_output::{CaptureBackend, DevicesChanged, Feed, OutputBackend, Stream};
use crossbeam_channel::{Receiver, Sender};

use common::*;

const WAV_A: &str = "wav_16_44_a.wav";

fn other_id() -> DeviceId {
    DeviceId("other".into())
}

#[test]
fn device_list_is_published_at_start() {
    let rig = rig(dac_a());
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    let (devices, selected) = rig.engine.devices();
    assert_eq!(
        devices.iter().map(|d| &d.id).collect::<Vec<_>>(),
        [&capture_id()]
    );
    assert_eq!(selected, Some(capture_id()));
    let caps = rig.engine.capabilities(&capture_id()).unwrap();
    assert_eq!((caps.caps, caps.dop_enabled), (dac_a(), false));
}

#[test]
fn unprobed_device_has_no_capabilities_until_played() {
    let (backend, log) = CaptureBackend::new(dac_a());
    let rig = rig_with(backend, log, &dac_a(), None);
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    assert_eq!(rig.engine.capabilities(&capture_id()), None);
    play(&rig, vec![fixture(WAV_A)], 0);
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    assert!(rig.engine.capabilities(&capture_id()).is_some());
}

#[test]
fn reprobe_replaces_probe_data_and_keeps_settings() {
    // Saved profile says DAC B with DoP on; the backend now probes as DAC A.
    let (backend, log) = CaptureBackend::new(dac_a());
    let rig = rig_with_profile(backend, log, Some(profile_for(&dac_b(), true)));
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    rig.engine.send(Command::Reprobe { id: capture_id() });
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    let caps = rig.engine.capabilities(&capture_id()).unwrap();
    assert_eq!((caps.caps, caps.dop_enabled), (dac_a(), true));
    let saved = ProfileStore::load(&rig.profile_path).unwrap();
    assert_eq!(saved.get(&capture_id()).unwrap().capabilities(), dac_a());
}

#[test]
fn reprobe_of_another_device_leaves_playback_alone() {
    let (backend, log) = CaptureBackend::new(dac_a());
    let backend = backend.with_devices([other_id()]);
    let rig = rig_with_profiles(
        backend,
        log,
        vec![
            (capture_id(), profile_for(&dac_a(), false)),
            (other_id(), profile_for(&dac_b(), false)),
        ],
    );
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    play(&rig, long_queue(), 0);
    wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    rig.engine.send(Command::Reprobe { id: other_id() });
    let events = wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    assert!(
        !events.iter().any(|e| matches!(e, Event::Error(_))),
        "{events:?}"
    );
    wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::Position { .. })
    });
    assert_quiet(&rig.engine, Duration::from_millis(50), |e| {
        matches!(e, Event::Error(_) | Event::QueueFinished)
    });
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    assert!(!streams[0].closed);
    let caps = rig.engine.capabilities(&other_id()).unwrap();
    assert_eq!(caps.caps, dac_a());
}

/// Delegates to a capture backend, but `devices()` takes a while once armed.
struct SlowList {
    inner: CaptureBackend,
    armed: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}

impl OutputBackend for SlowList {
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError> {
        if self.armed.load(Ordering::SeqCst) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
        }
        self.inner.devices()
    }
    fn default_device(&self) -> Result<DeviceId, OutputError> {
        self.inner.default_device()
    }
    fn probe(&self, dev: &DeviceId, channels: u16) -> Result<Capabilities, OutputError> {
        self.inner.probe(dev, channels)
    }
    fn open(
        &self,
        dev: &DeviceId,
        fmt: &DeviceFormat,
        feed: Feed,
    ) -> Result<Box<dyn Stream>, OutputError> {
        self.inner.open(dev, fmt, feed)
    }
}

#[test]
fn devices_does_not_wait_for_device_enumeration() {
    let (inner, _log) = CaptureBackend::new(dac_a());
    let armed = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = SlowList {
        inner,
        armed: armed.clone(),
        calls: calls.clone(),
    };
    let dir = tempfile::tempdir().unwrap();
    let mut profiles = ProfileStore::load(&dir.path().join("devices.toml")).unwrap();
    profiles.upsert(&capture_id(), profile_for(&dac_b(), false));
    let engine = Engine::start(
        Box::new(backend),
        EngineConfig {
            device: None,
            profiles,
        },
    );
    wait_for(&engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));

    armed.store(true, Ordering::SeqCst);
    engine.send(Command::Reprobe { id: capture_id() });
    assert!(wait_until(|| calls.load(Ordering::SeqCst) >= 1));
    // Keep reading while the control thread enumerates (the relist is the second slow call).
    let end = Instant::now() + Duration::from_millis(500);
    let mut slowest = Duration::ZERO;
    while Instant::now() < end {
        let t = Instant::now();
        let _ = engine.devices();
        let _ = engine.capabilities(&capture_id());
        slowest = slowest.max(t.elapsed());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        slowest < Duration::from_millis(50),
        "read blocked for {slowest:?}"
    );
    wait_for(&engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
}

#[test]
fn reprobe_of_the_playing_device_resumes_where_it_was() {
    let rig = rig(dac_a());
    play(&rig, long_queue(), 0);
    let mut before = wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    // Paused, the track playing is known exactly and the pause state must survive.
    rig.engine.send(Command::Pause);
    before.extend(wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::Paused)
    }));
    let index = *started(&before).last().expect("a track started");

    rig.engine.send(Command::Reprobe { id: capture_id() });
    let events = wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Error(_) | Event::QueueFinished)),
        "{events:#?}"
    );
    assert!(
        wait_until(|| rig.log.streams().len() == 2),
        "a second stream opened"
    );
    assert!(rig.log.streams()[0].closed);
    assert_quiet(&rig.engine, Duration::from_millis(100), |e| {
        matches!(e, Event::TrackStarted { .. } | Event::Error(_))
    });

    rig.engine.send(Command::Resume);
    let events = wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    assert_eq!(started(&events), [index], "{events:#?}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Error(_) | Event::QueueFinished)),
        "{events:#?}"
    );
    assert!(!rig.log.streams()[1].closed);
}

#[test]
fn reprobe_of_the_playing_device_while_playing_continues() {
    let rig = rig(dac_a());
    play(&rig, long_queue(), 0);
    wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    rig.engine.send(Command::Reprobe { id: capture_id() });
    let events = wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    assert!(!events.iter().any(|e| matches!(e, Event::Error(_))));
    let events = wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Error(_) | Event::QueueFinished)),
        "{events:#?}"
    );
    assert_eq!(rig.log.streams().len(), 2);
    assert!(rig.log.streams()[0].closed);
}

/// Delegates to a capture backend; once a stream opens, the capture device counts as
/// unplugged: it is no longer listed and `other` becomes the default.
struct Unplug {
    inner: CaptureBackend,
    unplugged: Arc<AtomicBool>,
}

impl OutputBackend for Unplug {
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError> {
        let gone = self.unplugged.load(Ordering::SeqCst);
        let info = |id: DeviceId, is_default| DeviceInfo {
            name: id.0.clone(),
            id,
            is_default,
        };
        let mut list = vec![info(other_id(), gone)];
        if !gone {
            list.insert(0, info(capture_id(), true));
        }
        Ok(list)
    }
    fn default_device(&self) -> Result<DeviceId, OutputError> {
        let gone = self.unplugged.load(Ordering::SeqCst);
        Ok(if gone { other_id() } else { capture_id() })
    }
    fn probe(&self, dev: &DeviceId, channels: u16) -> Result<Capabilities, OutputError> {
        self.inner.probe(dev, channels)
    }
    fn open(
        &self,
        dev: &DeviceId,
        fmt: &DeviceFormat,
        feed: Feed,
    ) -> Result<Box<dyn Stream>, OutputError> {
        let stream = self.inner.open(dev, fmt, feed);
        self.unplugged.store(true, Ordering::SeqCst);
        stream
    }
}

#[test]
fn unplugging_the_playing_device_republishes_the_list() {
    let (mut inner, _log) = CaptureBackend::new(dac_a());
    inner.fail_stream_after(1000, OutputError::Invalidated);
    let backend = Unplug {
        inner,
        unplugged: Arc::new(AtomicBool::new(false)),
    };
    let dir = tempfile::tempdir().unwrap();
    let mut profiles = ProfileStore::load(&dir.path().join("devices.toml")).unwrap();
    profiles.upsert(&capture_id(), profile_for(&dac_a(), false));
    let engine = Engine::start(
        Box::new(backend),
        EngineConfig {
            device: None,
            profiles,
        },
    );
    wait_for(&engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    engine.send(Command::SelectDevice { id: capture_id() });
    engine.send(Command::Play {
        tracks: vec![fixture(WAV_A)],
        start: 0,
    });
    wait_for(&engine, TIMEOUT, |e| matches!(e, Event::Error(_)));
    wait_for(&engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    let (devices, selected) = engine.devices();
    assert_eq!(
        devices.iter().map(|d| &d.id).collect::<Vec<_>>(),
        [&other_id()]
    );
    assert_eq!(selected, Some(other_id()));
}

#[test]
fn refresh_devices_drops_an_unplugged_idle_device() {
    let (inner, _log) = CaptureBackend::new(dac_a());
    let unplugged = Arc::new(AtomicBool::new(false));
    let backend = Unplug {
        inner,
        unplugged: unplugged.clone(),
    };
    let dir = tempfile::tempdir().unwrap();
    let profiles = ProfileStore::load(&dir.path().join("devices.toml")).unwrap();
    let engine = Engine::start(
        Box::new(backend),
        EngineConfig {
            device: Some(capture_id()),
            profiles,
        },
    );
    wait_for(&engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    assert_eq!(engine.devices().1, Some(capture_id()));
    unplugged.store(true, Ordering::SeqCst);
    engine.send(Command::RefreshDevices);
    wait_for(&engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    let (devices, selected) = engine.devices();
    assert_eq!(
        devices.iter().map(|d| &d.id).collect::<Vec<_>>(),
        [&other_id()]
    );
    assert_eq!(selected, Some(other_id()));
}

/// Delegates to a capture backend, holding the second `open` until the test releases it:
/// the first stream has drained by then and the next one has not started.
struct HoldSecondOpen {
    inner: CaptureBackend,
    opens: AtomicUsize,
    reached: Sender<()>,
    release: Mutex<Receiver<()>>,
}

impl OutputBackend for HoldSecondOpen {
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError> {
        self.inner.devices()
    }
    fn default_device(&self) -> Result<DeviceId, OutputError> {
        self.inner.default_device()
    }
    fn probe(&self, dev: &DeviceId, channels: u16) -> Result<Capabilities, OutputError> {
        self.inner.probe(dev, channels)
    }
    fn open(
        &self,
        dev: &DeviceId,
        fmt: &DeviceFormat,
        feed: Feed,
    ) -> Result<Box<dyn Stream>, OutputError> {
        if self.opens.fetch_add(1, Ordering::SeqCst) == 1 {
            self.reached.send(()).unwrap();
            self.release.lock().unwrap().recv_timeout(TIMEOUT).unwrap();
        }
        self.inner.open(dev, fmt, feed)
    }
}

#[test]
fn switching_device_between_streams_continues_with_the_next_track() {
    let (inner, log) = CaptureBackend::new(dac_a());
    let (reached_tx, reached_rx) = crossbeam_channel::bounded(1);
    let (release_tx, release_rx) = crossbeam_channel::bounded(1);
    let backend = HoldSecondOpen {
        inner: inner.with_devices([other_id()]),
        opens: AtomicUsize::new(0),
        reached: reached_tx,
        release: Mutex::new(release_rx),
    };
    let dir = tempfile::tempdir().unwrap();
    let mut profiles = ProfileStore::load(&dir.path().join("devices.toml")).unwrap();
    profiles.upsert(&capture_id(), profile_for(&dac_a(), false));
    let engine = Engine::start(
        Box::new(backend),
        EngineConfig {
            device: None,
            profiles,
        },
    );
    engine.send(Command::Play {
        tracks: vec![fixture(WAV_A), fixture("wav_24_96.wav")],
        start: 0,
    });
    reached_rx.recv_timeout(TIMEOUT).unwrap();
    engine.send(Command::SelectDevice { id: other_id() });
    release_tx.send(()).unwrap();

    let events = wait_for(&engine, TIMEOUT, |e| {
        matches!(e, Event::QueueFinished | Event::Error(_))
    });
    assert_eq!(started(&events), [0, 1], "{events:#?}");
    let streams = log.streams();
    let last = streams.last().unwrap();
    assert_eq!(last.device, other_id());
    assert!(last.bytes == data("wav_24_96.wav"), "track 0 replayed");
}

/// An `Unplug` that reports changes like a hot-plug listener: the engine's `changed`
/// callback is kept for the test to call, and device listings are counted. Once `hold` is
/// set, the next listing signals `reached` and waits for `release`.
struct HotPlug {
    inner: Unplug,
    changed: Arc<Mutex<Option<DevicesChanged>>>,
    lists: Arc<AtomicUsize>,
    unwatched: Arc<AtomicBool>,
    hold: Arc<AtomicBool>,
    reached: Sender<()>,
    release: Receiver<()>,
}

/// Dropping it marks the watch as ended.
struct WatchGuard(Arc<AtomicBool>);

impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl OutputBackend for HotPlug {
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError> {
        self.lists.fetch_add(1, Ordering::SeqCst);
        if self.hold.swap(false, Ordering::SeqCst) {
            self.reached.send(()).unwrap();
            self.release.recv().unwrap();
        }
        self.inner.devices()
    }
    fn default_device(&self) -> Result<DeviceId, OutputError> {
        self.inner.default_device()
    }
    fn probe(&self, dev: &DeviceId, channels: u16) -> Result<Capabilities, OutputError> {
        self.inner.probe(dev, channels)
    }
    fn open(
        &self,
        dev: &DeviceId,
        fmt: &DeviceFormat,
        feed: Feed,
    ) -> Result<Box<dyn Stream>, OutputError> {
        self.inner.open(dev, fmt, feed)
    }
    fn watch_devices(&self, changed: DevicesChanged) -> Option<Box<dyn std::any::Any>> {
        *self.changed.lock().unwrap() = Some(changed);
        Some(Box::new(WatchGuard(self.unwatched.clone())))
    }
}

#[test]
fn device_change_notifications_relist_the_devices_once_per_burst() {
    let (inner, _log) = CaptureBackend::new(dac_a());
    let unplugged = Arc::new(AtomicBool::new(false));
    let changed = Arc::new(Mutex::new(None));
    let lists = Arc::new(AtomicUsize::new(0));
    let unwatched = Arc::new(AtomicBool::new(false));
    let hold = Arc::new(AtomicBool::new(false));
    let (reached_tx, reached) = crossbeam_channel::bounded(1);
    let (release, release_rx) = crossbeam_channel::bounded(1);
    let backend = HotPlug {
        inner: Unplug {
            inner,
            unplugged: unplugged.clone(),
        },
        changed: changed.clone(),
        lists: lists.clone(),
        unwatched: unwatched.clone(),
        hold: hold.clone(),
        reached: reached_tx,
        release: release_rx,
    };
    let dir = tempfile::tempdir().unwrap();
    let profiles = ProfileStore::load(&dir.path().join("devices.toml")).unwrap();
    let engine = Engine::start(
        Box::new(backend),
        EngineConfig {
            device: Some(capture_id()),
            profiles,
        },
    );
    wait_for(&engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    assert_eq!(engine.devices().1, Some(capture_id()));
    let before = lists.load(Ordering::SeqCst);

    // One unplug: Windows reports it through several callbacks in a row. The first one
    // starts a relist, held while the rest arrive.
    unplugged.store(true, Ordering::SeqCst);
    hold.store(true, Ordering::SeqCst);
    {
        let changed = changed.lock().unwrap();
        let changed = changed.as_ref().expect("the engine watches devices");
        changed();
        reached.recv_timeout(TIMEOUT).expect("a relist started");
        for _ in 0..4 {
            changed();
        }
    }
    release.send(()).unwrap();
    wait_for(&engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    let (devices, selected) = engine.devices();
    assert_eq!(
        devices.iter().map(|d| &d.id).collect::<Vec<_>>(),
        [&other_id()]
    );
    assert_eq!(selected, Some(other_id()));
    // The four held back are one more relist, then nothing.
    wait_for(&engine, TIMEOUT, |e| matches!(e, Event::DevicesChanged));
    assert_quiet(&engine, Duration::from_millis(100), |e| {
        matches!(e, Event::DevicesChanged)
    });
    assert_eq!(lists.load(Ordering::SeqCst) - before, 2);

    assert!(!unwatched.load(Ordering::SeqCst));
    drop(engine);
    assert!(
        unwatched.load(Ordering::SeqCst),
        "the watch ends with the engine"
    );
}

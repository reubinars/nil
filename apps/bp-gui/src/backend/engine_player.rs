//! `PlayerHandle` over the real engine.

use std::sync::{Arc, Mutex};

use bp_core::{DeviceId, DeviceInfo};
use bp_engine::{Command, Engine, Event};
use crossbeam_channel::{Receiver, unbounded};

use super::{DeviceCaps, PlayerCommand, PlayerEvent, PlayerHandle};

type Waker = Arc<dyn Fn() + Send + Sync>;

/// The real engine behind the GUI's player interface.
///
/// A forwarder thread is the only reader of the engine's event channel (crossbeam
/// receivers are multi-consumer, so a second reader would steal events). It moves
/// each event into this adapter's own channel and wakes the UI. It exits when the
/// engine, and so the engine's channel, is dropped.
pub struct EnginePlayer {
    engine: Engine,
    events: Receiver<PlayerEvent>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl EnginePlayer {
    pub fn new(engine: Engine) -> Self {
        let (tx, events) = unbounded();
        let waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let source = engine.events().clone();
        let forwarder_waker = waker.clone();
        std::thread::Builder::new()
            .name("bp-gui-events".into())
            .spawn(move || {
                while let Ok(event) = source.recv() {
                    let event = match event {
                        Event::DevicesChanged => PlayerEvent::DevicesChanged,
                        other => PlayerEvent::Engine(other),
                    };
                    if tx.send(event).is_err() {
                        break;
                    }
                    let waker = forwarder_waker.lock().unwrap().clone();
                    if let Some(waker) = waker {
                        waker();
                    }
                }
            })
            .expect("spawn event forwarder");
        EnginePlayer {
            engine,
            events,
            waker,
        }
    }
}

impl PlayerHandle for EnginePlayer {
    fn analysis(&self) -> Option<bp_engine::AnalysisTap> {
        Some(self.engine.analysis())
    }

    fn send(&self, cmd: PlayerCommand) {
        let cmd = match cmd {
            PlayerCommand::Play { items, start } => Command::Play {
                tracks: items.into_iter().map(|i| i.path).collect(),
                start,
            },
            PlayerCommand::Pause => Command::Pause,
            PlayerCommand::Resume => Command::Resume,
            PlayerCommand::Stop => Command::Stop,
            PlayerCommand::Next => Command::Next,
            PlayerCommand::Previous => Command::Previous,
            PlayerCommand::Seek { position } => Command::Seek { position },
            PlayerCommand::SelectDevice { id } => Command::SelectDevice { id },
            PlayerCommand::Reprobe { id } => Command::Reprobe { id },
            PlayerCommand::RefreshDevices => Command::RefreshDevices,
        };
        self.engine.send(cmd);
    }

    fn try_recv(&self) -> Option<PlayerEvent> {
        self.events.try_recv().ok()
    }

    fn devices(&self) -> (Vec<DeviceInfo>, Option<DeviceId>) {
        self.engine.devices()
    }

    fn device_caps(&self, id: &DeviceId) -> Option<DeviceCaps> {
        self.engine.capabilities(id)
    }

    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        *self.waker.lock().unwrap() = Some(Arc::from(waker));
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use bp_core::{Capabilities, DeviceId, PROBE_ENCODINGS, PROBE_RATES, SampleEncoding};
    use bp_core::{Codec, SourceFormat, SourceKind};
    use bp_engine::profile::{DeviceProfile, ProbedFormat, ProfileStore};
    use bp_engine::{Engine, EngineConfig, Event};
    use bp_output::CaptureBackend;

    use super::*;
    use crate::backend::{PlayerCommand, PlayerEvent, PlayerHandle, QueueItem};

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

    fn capture() -> DeviceId {
        DeviceId("capture".into())
    }

    /// A 16-bit stereo 44.1 kHz WAV of `secs` seconds of silence.
    fn write_wav(path: &Path, secs: u32) {
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
        b.resize(b.len() + data_len as usize, 0);
        std::fs::write(path, b).unwrap();
    }

    fn item(path: &Path) -> QueueItem {
        QueueItem {
            path: path.to_path_buf(),
            source: SourceFormat {
                codec: Codec::Wav,
                sample_rate: 44_100,
                channels: 2,
                kind: SourceKind::Pcm {
                    bits: 16,
                    float: false,
                },
            },
            duration: Duration::from_secs(1),
            title: "t".into(),
        }
    }

    struct Rig {
        player: EnginePlayer,
        wav: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        write_wav(&wav, 1);
        let caps = dac_a();
        let (backend, _log) = CaptureBackend::new(caps.clone());
        let mut profiles = ProfileStore::load(&dir.path().join("devices.toml")).unwrap();
        profiles.upsert(
            &capture(),
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
        Rig {
            player: EnginePlayer::new(engine),
            wav,
            _dir: dir,
        }
    }

    /// Polls `try_recv` for up to 2 s, collecting events until `pred` matches one.
    fn wait_for(player: &EnginePlayer, pred: impl Fn(&PlayerEvent) -> bool) -> Vec<PlayerEvent> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            match player.try_recv() {
                Some(e) => {
                    let hit = pred(&e);
                    seen.push(e);
                    if hit {
                        return seen;
                    }
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        panic!("timed out; seen: {seen:?}");
    }

    #[test]
    fn play_maps_items_to_paths_and_events_come_back() {
        let r = rig();
        r.player.send(PlayerCommand::Play {
            items: vec![item(&r.wav)],
            start: 0,
        });
        wait_for(&r.player, |e| {
            matches!(e, PlayerEvent::Engine(Event::TrackStarted { index: 0, .. }))
        });
    }

    #[test]
    fn engine_devices_changed_becomes_player_devices_changed() {
        let r = rig();
        let seen = wait_for(&r.player, |e| matches!(e, PlayerEvent::DevicesChanged));
        assert!(
            seen.iter()
                .all(|e| !matches!(e, PlayerEvent::Engine(Event::DevicesChanged)))
        );
    }

    #[test]
    fn waker_runs_for_each_forwarded_event() {
        let r = rig();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        r.player.set_waker(Box::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        r.player.send(PlayerCommand::Play {
            items: vec![item(&r.wav)],
            start: 0,
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while count.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(count.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn device_caps_reads_saved_profile() {
        let r = rig();
        wait_for(&r.player, |e| matches!(e, PlayerEvent::DevicesChanged));
        assert_eq!(
            r.player.device_caps(&capture()),
            Some(DeviceCaps {
                caps: dac_a(),
                dop_enabled: false
            })
        );
    }

    #[test]
    fn dropping_the_player_does_not_hang() {
        let r = rig();
        let items = vec![item(&r.wav); 40];
        r.player.send(PlayerCommand::Play { items, start: 0 });
        wait_for(&r.player, |e| {
            matches!(e, PlayerEvent::Engine(Event::TrackStarted { .. }))
        });
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(r.player);
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(2)).expect("drop hung");
    }
}

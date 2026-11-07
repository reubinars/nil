//! Playback engine: queue, gapless transitions, strict format policy.

pub mod analysis;
mod control;
mod decoder;
pub mod home;
pub mod negotiate;
pub mod profile;
mod queue;
mod timeline;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use bp_core::{
    Capabilities, DecodeError, DeviceId, DeviceInfo, EngineError, PlaybackPath, UnsupportedReason,
};
use bp_output::OutputBackend;
use crossbeam_channel::{Receiver, Sender};

pub use crate::analysis::{AnalysisTap, Window};
use crate::profile::ProfileStore;

pub struct EngineConfig {
    /// `None` = the backend's default device.
    pub device: Option<DeviceId>,
    pub profiles: ProfileStore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Plays `tracks[start..]` in order, then stops.
    Play {
        tracks: Vec<PathBuf>,
        start: usize,
    },
    /// Extends the current track list. Gapless across the boundary when the format matches,
    /// exactly as within a `Play` list. When stopped or finished, behaves like `Play` from
    /// the first appended track. Unsupported-on-device tracks are skipped and reported as
    /// today.
    Append {
        tracks: Vec<PathBuf>,
    },
    /// Puts `tracks` after the current track, before the rest of the list, unless the decoder
    /// has already started the next one (see below). Gapless into the first inserted track when
    /// the format allows, exactly as `Append`. When stopped or finished, behaves like `Play`
    /// from the first inserted track.
    ///
    /// The decoder runs up to 2 s ahead: in the last ~2 s of a track it has started the next
    /// one, and the tracks go after that one. Several inserts in a row keep their order, each
    /// after the tracks of the one before, until the decoder moves past those.
    Insert {
        tracks: Vec<PathBuf>,
    },
    /// Replaces the tracks from `from` on with `tracks`, if they still are `expected` and the
    /// decoder has not taken any of them. Never restarts playback: the current track, its
    /// position and the stream are untouched.
    Rearrange {
        from: usize,
        expected: Vec<PathBuf>,
        tracks: Vec<PathBuf>,
    },
    Pause,
    Resume,
    Stop,
    Next,
    /// Restarts the current track if more than 3 s of it has played, else goes back to the
    /// previous track; on the first track it restarts it.
    Previous,
    /// Within the current track.
    Seek {
        position: Duration,
    },
    /// Playing on the device already: nothing changes. While playing, continues the current
    /// track from its position on the new device; stopped, applies from the next `Play`.
    /// A device that is not listed stops playback, republishes the list
    /// (`DevicesChanged`) and reports `Error(DeviceNotFound)`.
    SelectDevice {
        id: DeviceId,
    },
    /// Probes the device again and replaces its saved probe data, keeping its settings.
    /// Playback on that device stops first (probing needs exclusive access) and, after a
    /// successful probe, resumes at the same position, keeping the pause state; other
    /// devices are untouched. A failed probe leaves playback stopped and arrives as
    /// `Event::Error`.
    Reprobe {
        id: DeviceId,
    },
    /// Turns DSD over PCM on or off for the device, saves it and publishes the devices
    /// (`DevicesChanged`). Applies from the next start of playback (`Play`, a skip or a
    /// seek); the stream in progress is untouched. Ignored for a device never probed.
    SetDop {
        id: DeviceId,
        enabled: bool,
    },
    /// Lists the devices again and publishes them (`DevicesChanged`). A selected device
    /// that is no longer listed falls back to the system default.
    RefreshDevices,
    Shutdown,
}

/// A device's saved capabilities and settings.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCaps {
    pub caps: Capabilities,
    pub dop_enabled: bool,
}

#[derive(Debug)]
pub enum Event {
    TrackStarted {
        index: usize,
        path: PathBuf,
        playback: PlaybackPath,
        duration: Option<Duration>,
    },
    /// About 4 times per second.
    Position {
        index: usize,
        position: Duration,
    },
    TrackSkipped {
        index: usize,
        path: PathBuf,
        reason: UnsupportedReason,
    },
    /// `Insert` put `count` tracks into the playing queue at index `at`; the tracks from
    /// `at` on moved down by `count`. Not sent when the insert started playback.
    Inserted {
        at: usize,
        count: usize,
    },
    /// `Rearrange` was applied: the tracks from `from` on are its `tracks`.
    Rearranged {
        from: usize,
    },
    /// `Rearrange` was not applied; the queue is unchanged.
    RearrangeRefused {
        from: usize,
        reason: RearrangeRefusal,
    },
    Paused,
    Resumed,
    QueueFinished,
    /// Cumulative count, emitted when it changes.
    Underrun {
        total: u64,
    },
    /// Non-fatal: that track ends and the next one plays.
    TrackError {
        index: usize,
        path: PathBuf,
        error: DecodeError,
    },
    /// The device list, the selected device or a device's saved capabilities changed; read
    /// them with `Engine::devices` and `Engine::capabilities`.
    DevicesChanged,
    /// Fatal: playback stops and the device is released.
    Error(EngineError),
}

/// Why a `Rearrange` was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RearrangeRefusal {
    /// The decoder has already taken track `from` (or one after it).
    Started,
    /// The tracks from `from` on are not `expected`.
    Changed,
    /// Nothing is playing.
    Stopped,
}

pub struct Engine {
    cmd_tx: Sender<Command>,
    evt_rx: Receiver<Event>,
    control: Option<JoinHandle<()>>,
    view: Arc<RwLock<control::DeviceView>>,
    analysis: AnalysisTap,
}

impl Engine {
    /// Starts the control thread; it owns `backend` and the profile store.
    pub fn start(backend: Box<dyn OutputBackend>, cfg: EngineConfig) -> Engine {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (evt_tx, evt_rx) = crossbeam_channel::unbounded();
        let view = Arc::default();
        let analysis = AnalysisTap::default();
        let control = control::Control::new(
            backend,
            cfg.profiles,
            cfg.device,
            evt_tx,
            Arc::clone(&view),
            analysis.clone(),
        );
        let control = std::thread::Builder::new()
            .name("bp-control".into())
            .spawn(move || control.run(cmd_rx))
            .expect("spawn control thread");
        Engine {
            cmd_tx,
            evt_rx,
            control: Some(control),
            view,
            analysis,
        }
    }

    /// A display-only copy of the audio being played, for a spectrum.
    pub fn analysis(&self) -> AnalysisTap {
        self.analysis.clone()
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// The listed devices and the selected one (the chosen device, else the default). Reads a
    /// published snapshot: never waits for the control thread. Empty until the first
    /// `DevicesChanged`.
    pub fn devices(&self) -> (Vec<DeviceInfo>, Option<DeviceId>) {
        let v = self.view.read().unwrap_or_else(|e| e.into_inner());
        (v.devices.clone(), v.selected.clone())
    }

    /// The saved capabilities of a listed device; `None` if it has no probe data yet.
    pub fn capabilities(&self, id: &DeviceId) -> Option<DeviceCaps> {
        let v = self.view.read().unwrap_or_else(|e| e.into_inner());
        v.caps.get(&id.0).cloned()
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.evt_rx
    }
}

impl Drop for Engine {
    /// Shuts the engine down, releasing the device.
    fn drop(&mut self) {
        self.send(Command::Shutdown);
        if let Some(h) = self.control.take() {
            let _ = h.join();
        }
    }
}

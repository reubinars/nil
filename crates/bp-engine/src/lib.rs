//! Playback engine: queue, gapless transitions, strict format policy.

pub mod analysis;
mod control;
mod decoder;
pub mod negotiate;
pub mod profile;
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

//! Device selection and the per-device profile store.

use std::collections::BTreeMap;

use bp_core::{Capabilities, DeviceId, DeviceInfo, OutputError};
use tracing::{info, warn};

use super::Control;
use crate::profile::{DeviceProfile, merge_probe};
use crate::{DeviceCaps, Event};

/// What `Engine::devices` and `Engine::capabilities` read; rebuilt by the control thread.
#[derive(Default)]
pub(crate) struct DeviceView {
    pub(crate) devices: Vec<DeviceInfo>,
    pub(crate) selected: Option<DeviceId>,
    pub(crate) caps: BTreeMap<String, DeviceCaps>,
}

impl Control {
    /// Enumerates the devices, logging a failure.
    pub(super) fn list_devices(&self) -> Option<Vec<DeviceInfo>> {
        self.backend
            .devices()
            .inspect_err(|e| warn!(%e, "could not list devices"))
            .ok()
    }

    /// Publishes `devices` (the old list if `None`), the selection and the saved
    /// capabilities, and emits `DevicesChanged`.
    pub(super) fn publish_devices(&mut self, devices: Option<Vec<DeviceInfo>>) {
        let devices = devices.unwrap_or_else(|| {
            self.view
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .devices
                .clone()
        });
        let selected = self
            .device
            .clone()
            .or_else(|| devices.iter().find(|d| d.is_default).map(|d| d.id.clone()));
        let caps = devices
            .iter()
            .filter_map(|d| {
                let p = self.profiles.get(&d.id).filter(|p| !p.probed.is_empty())?;
                let caps = DeviceCaps {
                    caps: p.capabilities(),
                    dop_enabled: p.dop_enabled,
                };
                Some((d.id.0.clone(), caps))
            })
            .collect();
        // No device I/O under the lock: readers never wait on the backend.
        *self.view.write().unwrap_or_else(|e| e.into_inner()) = DeviceView {
            devices,
            selected,
            caps,
        };
        self.emit(Event::DevicesChanged);
    }

    /// Lists the devices again and publishes them. A selected device that is no longer
    /// listed (unplugged) falls back to the system default.
    pub(super) fn relist_devices(&mut self) {
        let Some(devices) = self.list_devices() else {
            return;
        };
        if let Some(id) = &self.device
            && !devices.iter().any(|d| d.id == *id)
        {
            info!(device = %id.0, "device no longer listed");
            self.device = None;
        }
        self.publish_devices(Some(devices));
    }

    /// Probes `id` again, replacing its probe data and keeping its settings. Playback on
    /// `id` stops first, since probing needs the device to itself, and after a successful
    /// probe resumes where it was, keeping the pause state.
    pub(super) fn reprobe(&mut self, id: &DeviceId) {
        let interrupted = if self.play.as_ref().is_some_and(|p| p.device == *id) {
            let (index, pos) = self.playing_position().expect("playing");
            self.stop_keeping_queue()
                .map(|(tracks, paused)| (tracks, index, pos, paused))
        } else {
            None
        };
        let result = self.backend.probe(id, 2).and_then(|caps| {
            let devices = self.list_devices();
            self.store_probe(id, &caps, devices.as_deref().unwrap_or_default())?;
            Ok(devices)
        });
        match result {
            Ok(devices) => {
                self.publish_devices(devices);
                if let Some((tracks, index, pos, paused)) = interrupted {
                    self.play_from(tracks, index, pos, paused);
                }
            }
            Err(e) => self.emit(Event::Error(e.into())),
        }
    }

    /// Plays on `id` from now on: playback in progress continues from where it is on the
    /// new device. Nothing happens if `id` is the device in use. A device that is not
    /// listed (any more) stops playback, and the list is published again.
    pub(super) fn select_device(&mut self, id: DeviceId) {
        let current = self
            .play
            .as_ref()
            .map_or(self.device.as_ref(), |p| Some(&p.device));
        if current == Some(&id) {
            return;
        }
        let devices = match self.backend.devices() {
            Ok(devices) => devices,
            Err(e) => {
                self.stop();
                return self.emit(Event::Error(e.into()));
            }
        };
        if !devices.iter().any(|d| d.id == id) {
            warn!(device = %id.0, "selected device not found");
            self.stop();
            self.publish_devices(Some(devices));
            return self.emit(Event::Error(OutputError::DeviceNotFound.into()));
        }
        info!(device = %id.0, "device selected");
        self.device = Some(id);
        if let Some((index, pos)) = self.playing_position() {
            self.restart(index, pos);
        }
        self.publish_devices(None);
    }

    /// The device's saved profile, probing and saving one if there is none or it has no
    /// probe data (its settings are kept).
    pub(super) fn profile(&mut self, device: &DeviceId) -> Result<DeviceProfile, OutputError> {
        if let Some(p) = self.profiles.get(device).filter(|p| !p.probed.is_empty()) {
            return Ok(p.clone());
        }
        let caps = self.backend.probe(device, 2)?;
        let devices = self.list_devices();
        let profile = self.store_probe(device, &caps, devices.as_deref().unwrap_or_default())?;
        self.publish_devices(None);
        Ok(profile)
    }

    /// Merges a probe of `id` into its saved profile, keeping its settings, and saves the
    /// store. The device's name comes from `devices`.
    fn store_probe(
        &mut self,
        id: &DeviceId,
        caps: &Capabilities,
        devices: &[DeviceInfo],
    ) -> Result<DeviceProfile, OutputError> {
        let name = devices
            .iter()
            .find(|d| d.id == *id)
            .map_or_else(|| id.0.clone(), |d| d.name.clone());
        let profile = merge_probe(self.profiles.get(id), name, caps)?;
        info!(device = %id.0, formats = profile.probed.len(), "probed device");
        self.profiles.upsert(id, profile.clone());
        self.save_profiles();
        Ok(profile)
    }

    fn save_profiles(&self) {
        if let Err(e) = self.profiles.save() {
            warn!(%e, "could not save device profiles");
        }
    }
}

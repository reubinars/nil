//! WASAPI exclusive-mode backend (Windows).
//!
//! Everything that needs COM is `#[cfg(windows)]`. The COM-free logic (HRESULT table, format
//! parameters, probe matrix) lives in submodules that compile and are unit-tested on every OS.
//!
//! Every backend method calls `initialize_mta()` first (idempotent per thread; its result is
//! ignored: `S_FALSE` means "already initialized", and on a thread that is already an STA COM
//! still works). COM is never uninitialized, so COM objects can't outlive their apartment.

mod hresult;
mod params;
pub(crate) mod render;
#[cfg(windows)]
mod stream;

#[cfg(windows)]
use std::any::Any;
#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
use ::wasapi::{
    DeviceEnumerator, DeviceEventCallbacks, Direction, Role, SampleType, ShareMode, WaveFormat,
    initialize_mta,
};
#[cfg(windows)]
use bp_core::{Capabilities, DeviceFormat, DeviceId, DeviceInfo, OutputError};
#[cfg(windows)]
use tracing::warn;

#[cfg(windows)]
use self::hresult::map_wasapi;
#[cfg(windows)]
use self::params::{SampleKind, probe_with, wave_params};
#[cfg(windows)]
use crate::{DevicesChanged, Feed, OutputBackend, Stream};

/// The real Windows output: WASAPI render endpoints in exclusive mode.
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct WasapiBackend;

#[cfg(windows)]
impl WasapiBackend {
    pub fn new() -> WasapiBackend {
        WasapiBackend
    }
}

/// The `WAVEFORMATEXTENSIBLE` for `fmt` (never a plain `WAVEFORMATEX`).
#[cfg(windows)]
fn wave_format(fmt: &DeviceFormat) -> WaveFormat {
    let (store, valid, kind) = wave_params(fmt.encoding);
    let kind = match kind {
        SampleKind::Int => SampleType::Int,
        SampleKind::Float => SampleType::Float,
    };
    let rate = fmt.sample_rate as usize;
    WaveFormat::new(store, valid, &kind, rate, usize::from(fmt.channels), None)
}

#[cfg(windows)]
fn enumerator() -> Result<DeviceEnumerator, OutputError> {
    let _ = initialize_mta();
    DeviceEnumerator::new().map_err(map_wasapi)
}

/// The render endpoint with this ID (`DeviceNotFound` if unknown).
#[cfg(windows)]
pub(crate) fn render_device(dev: &DeviceId) -> Result<::wasapi::Device, OutputError> {
    let device = enumerator()?.get_device(&dev.0).map_err(map_wasapi)?;
    if device.get_direction() != Direction::Render {
        return Err(OutputError::DeviceNotFound);
    }
    Ok(device)
}

#[cfg(windows)]
impl OutputBackend for WasapiBackend {
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError> {
        let enumerator = enumerator()?;
        // No default endpoint (E_NOTFOUND) is not an error here: nothing is marked default.
        let default = enumerator
            .get_default_device(&Direction::Render)
            .and_then(|d| d.get_id())
            .ok();
        let collection = enumerator
            .get_device_collection(&Direction::Render)
            .map_err(map_wasapi)?;
        let mut devices = Vec::new();
        for device in &collection {
            let device = device.map_err(map_wasapi)?;
            let id = device.get_id().map_err(map_wasapi)?;
            let name = device.get_friendlyname().map_err(map_wasapi)?;
            devices.push(DeviceInfo {
                is_default: default.as_ref() == Some(&id),
                id: DeviceId(id),
                name,
            });
        }
        Ok(devices)
    }

    fn default_device(&self) -> Result<DeviceId, OutputError> {
        let device = enumerator()?
            .get_default_device(&Direction::Render)
            .map_err(map_wasapi)?;
        device.get_id().map(DeviceId).map_err(map_wasapi)
    }

    fn probe(&self, dev: &DeviceId, channels: u16) -> Result<Capabilities, OutputError> {
        let client = render_device(dev)?.get_iaudioclient().map_err(map_wasapi)?;
        probe_with(channels, |f| {
            client
                .is_supported(&wave_format(f), &ShareMode::Exclusive)
                .map(|_| ())
                .map_err(map_wasapi)
        })
    }

    fn open(
        &self,
        dev: &DeviceId,
        fmt: &DeviceFormat,
        feed: Feed,
    ) -> Result<Box<dyn Stream>, OutputError> {
        Ok(Box::new(stream::open(dev, fmt, feed)?))
    }

    fn watch_devices(&self, changed: DevicesChanged) -> Option<Box<dyn Any>> {
        // The callbacks run on a Windows audio thread: they only pass the news on, and
        // never call back into the enumerator.
        let changed: Arc<dyn Fn() + Send + Sync> = Arc::from(changed);
        let mut callbacks = DeviceEventCallbacks::new();
        let c = Arc::clone(&changed);
        callbacks.set_device_added_callback(move |_| c());
        let c = Arc::clone(&changed);
        callbacks.set_device_removed_callback(move |_| c());
        let c = Arc::clone(&changed);
        callbacks.set_device_state_callback(move |_, _| c());
        // `devices` marks the console default as the default.
        callbacks.set_default_device_callback(move |direction, role, _| {
            if direction == Direction::Render && role == Role::Console {
                changed();
            }
        });
        let registration = enumerator()
            .and_then(|e| {
                e.register_notification_callback(callbacks)
                    .map_err(map_wasapi)
            })
            .inspect_err(|e| warn!(%e, "could not watch devices"))
            .ok()?;
        Some(Box::new(registration))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn watch_devices_registers_until_dropped() {
        let watch = WasapiBackend::new().watch_devices(Box::new(|| {}));
        assert!(watch.is_some());
        drop(watch);
    }

    /// Hardware: prints the exclusive-mode matrix of the default render device.
    /// `cargo test -p bp-output -- --ignored probe_default_device_prints_matrix --nocapture`
    #[test]
    #[ignore = "needs a Windows audio device"]
    fn probe_default_device_prints_matrix() {
        let backend = WasapiBackend::new();
        let devices = backend.devices().unwrap();
        for d in &devices {
            println!(
                "{} {:?} {}",
                if d.is_default { "*" } else { " " },
                d.name,
                d.id.0
            );
        }
        let dev = backend.default_device().unwrap();
        assert!(devices.iter().any(|d| d.id == dev && d.is_default));
        let caps = backend.probe(&dev, 2).unwrap();
        for (rate, channels, enc) in &caps.formats {
            println!("{rate:>7} Hz  {channels} ch  {enc}");
        }
        println!("max rate: {:?}", caps.max_rate(2));
        assert!(
            !caps.formats.is_empty(),
            "the default device accepts nothing"
        );
    }
}

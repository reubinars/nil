//! Output backend abstraction, ring-buffer `Feed`, silence fill, and the capture test backend.

pub mod capture;
// Pure helpers are only used by the Windows backend (and by tests elsewhere).
#[cfg_attr(not(windows), allow(dead_code))]
mod wasapi;

use std::any::Any;

use bp_core::{
    Capabilities, DOP_MARKERS, DSD_IDLE, DeviceFormat, DeviceId, DeviceInfo, OutputError,
};

#[cfg(windows)]
pub use self::wasapi::WasapiBackend;
pub use capture::{CaptureBackend, CaptureLog, CapturedStream};

/// What to emit when the ring runs dry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Silence {
    Pcm,
    Dop { container: u8 },
}

/// Fills `out` (a whole number of frames of `frame_bytes`) with silence.
pub fn fill_silence(s: &Silence, frame_bytes: usize, last_frame: Option<&[u8]>, out: &mut [u8]) {
    match *s {
        Silence::Pcm => out.fill(0),
        Silence::Dop { container } => {
            let cb = usize::from(container) / 8;
            let [m0, m1] = DOP_MARKERS;
            // Marker of the first silence frame: the opposite of the last frame's.
            let mut marker = match last_frame.and_then(|f| f.get(cb - 1)) {
                Some(&m) if m == m0 => m1,
                _ => m0,
            };
            for frame in out.chunks_exact_mut(frame_bytes) {
                for sample in frame.chunks_exact_mut(cb) {
                    sample.fill(0);
                    sample[cb - 1] = marker;
                    sample[cb - 2] = DSD_IDLE;
                    sample[cb - 3] = DSD_IDLE;
                }
                marker = if marker == m0 { m1 } else { m0 };
            }
        }
    }
}

/// The consumer side of the engine's ring plus the info a render thread needs.
pub struct Feed {
    pub consumer: rtrb::Consumer<u8>,
    pub frame_bytes: usize,
    pub silence: Silence,
}

/// What `OutputBackend::watch_devices` calls on a device change.
pub type DevicesChanged = Box<dyn Fn() + Send + Sync>;

pub trait OutputBackend: Send {
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError>;
    fn default_device(&self) -> Result<DeviceId, OutputError>;
    fn probe(&self, dev: &DeviceId, channels: u16) -> Result<Capabilities, OutputError>;
    fn open(
        &self,
        dev: &DeviceId,
        fmt: &DeviceFormat,
        feed: Feed,
    ) -> Result<Box<dyn Stream>, OutputError>;
    /// Calls `changed` whenever a device is added, removed or changes state, or the
    /// default device changes, until the returned value is dropped. `changed` may run on
    /// any thread and must return quickly. `None`: no notifications.
    fn watch_devices(&self, changed: DevicesChanged) -> Option<Box<dyn Any>> {
        let _ = changed;
        None
    }
}

pub trait Stream: Send {
    fn start(&mut self) -> Result<(), OutputError>;
    fn pause(&mut self) -> Result<(), OutputError>;
    /// Frames popped from the ring; inserted silence is never counted.
    fn frames_played(&self) -> u64;
    fn underruns(&self) -> u64;
    fn take_error(&self) -> Option<OutputError>;
    fn close(self: Box<Self>);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dop_silence(
        container: u8,
        frame_bytes: usize,
        last: Option<&[u8]>,
        frames: usize,
    ) -> Vec<u8> {
        let mut out = vec![0xAA; frame_bytes * frames];
        fill_silence(&Silence::Dop { container }, frame_bytes, last, &mut out);
        out
    }

    #[test]
    fn dop_silence_continues_markers() {
        // 32-bit container, 2 channels, last marker 0x05.
        let last = [0x00, 0x12, 0x34, 0x05, 0x00, 0x56, 0x78, 0x05];
        let out = dop_silence(32, 8, Some(&last), 3);
        assert_eq!(
            out,
            [
                0x00, 0x69, 0x69, 0xFA, 0x00, 0x69, 0x69, 0xFA, //
                0x00, 0x69, 0x69, 0x05, 0x00, 0x69, 0x69, 0x05, //
                0x00, 0x69, 0x69, 0xFA, 0x00, 0x69, 0x69, 0xFA,
            ]
        );
        // 24-bit container, 2 channels, last marker 0xFA.
        let last = [0x11, 0x22, 0xFA, 0x33, 0x44, 0xFA];
        let out = dop_silence(24, 6, Some(&last), 2);
        assert_eq!(
            out,
            [
                0x69, 0x69, 0x05, 0x69, 0x69, 0x05, //
                0x69, 0x69, 0xFA, 0x69, 0x69, 0xFA,
            ]
        );
        // No last frame: starts at 0x05.
        let out = dop_silence(32, 8, None, 2);
        assert_eq!(out[3], 0x05);
        assert_eq!(out[11], 0xFA);
    }

    #[test]
    fn pcm_silence_is_zero() {
        let mut out = vec![0xFF; 24];
        fill_silence(&Silence::Pcm, 6, Some(&[1, 2, 3, 4, 5, 6]), &mut out);
        assert!(out.iter().all(|&b| b == 0));
    }
}

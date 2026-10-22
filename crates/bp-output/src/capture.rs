//! Capture backend: records exactly what would have been sent to the DAC.
//!
//! By default a stream copies frames out of the ring as fast as they come. A
//! [`CaptureBackend::paced`] one runs the WASAPI backend's render step instead, one device
//! buffer per period in real time, so its silence, underruns and DoP markers are recorded.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bp_core::{Capabilities, DeviceFormat, DeviceId, DeviceInfo, OutputError};

use crate::wasapi::render::Renderer;
use crate::{Feed, OutputBackend, Stream};

const PAUSED: u8 = 0;
const RUNNING: u8 = 1;
const STOP: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedStream {
    /// The device the stream was opened on.
    pub device: DeviceId,
    pub format: DeviceFormat,
    /// Frames taken from the ring.
    pub bytes: Vec<u8>,
    /// Everything the render step wrote, silence included; empty unless paced.
    pub rendered: Vec<u8>,
    pub closed: bool,
}

/// Shared view of everything the capture backend's streams have been sent.
#[derive(Clone)]
pub struct CaptureLog {
    streams: Arc<Mutex<Vec<CapturedStream>>>,
}

impl CaptureLog {
    /// Snapshot of all streams, in open order.
    pub fn streams(&self) -> Vec<CapturedStream> {
        self.streams.lock().unwrap().clone()
    }
}

pub struct CaptureBackend {
    caps: Capabilities,
    log: CaptureLog,
    rejected: Vec<DeviceFormat>,
    /// One-shot: applies to the next stream opened.
    fail_after: Mutex<Option<(u64, OutputError)>>,
    /// Devices listed after the default "capture" one; all share `caps`.
    extra_devices: Vec<DeviceId>,
    /// Render step period, if paced.
    pace: Option<Duration>,
}

impl CaptureBackend {
    pub fn new(caps: Capabilities) -> (CaptureBackend, CaptureLog) {
        let log = CaptureLog {
            streams: Arc::new(Mutex::new(Vec::new())),
        };
        let backend = CaptureBackend {
            caps,
            log: log.clone(),
            rejected: Vec::new(),
            fail_after: Mutex::new(None),
            extra_devices: Vec::new(),
            pace: None,
        };
        (backend, log)
    }

    /// `open` fails with `UnsupportedFormat` for this format.
    pub fn reject_on_open(&mut self, fmt: DeviceFormat) {
        self.rejected.push(fmt);
    }

    /// The next opened stream stops consuming after `frames` frames and then reports `err`.
    pub fn fail_stream_after(&mut self, frames: u64, err: OutputError) {
        *self.fail_after.get_mut().unwrap() = Some((frames, err));
    }

    /// Also lists these devices (not default; same capabilities as "capture").
    pub fn with_devices(mut self, ids: impl IntoIterator<Item = DeviceId>) -> CaptureBackend {
        self.extra_devices.extend(ids);
        self
    }

    /// Streams run the real render step: every `period`, one buffer of that length, padded
    /// with silence when the ring runs short. `fail_stream_after` does not apply.
    pub fn paced(mut self, period: Duration) -> CaptureBackend {
        self.pace = Some(period);
        self
    }

    fn device_id() -> DeviceId {
        DeviceId("capture".to_owned())
    }
}

impl OutputBackend for CaptureBackend {
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError> {
        let mut devices = vec![DeviceInfo {
            id: Self::device_id(),
            name: "Capture".to_owned(),
            is_default: true,
        }];
        devices.extend(self.extra_devices.iter().map(|id| DeviceInfo {
            id: id.clone(),
            name: id.0.clone(),
            is_default: false,
        }));
        Ok(devices)
    }

    fn default_device(&self) -> Result<DeviceId, OutputError> {
        Ok(Self::device_id())
    }

    fn probe(&self, _dev: &DeviceId, _channels: u16) -> Result<Capabilities, OutputError> {
        Ok(self.caps.clone())
    }

    fn open(
        &self,
        dev: &DeviceId,
        fmt: &DeviceFormat,
        feed: Feed,
    ) -> Result<Box<dyn Stream>, OutputError> {
        if self.rejected.contains(fmt)
            || (self.caps.has_channels(fmt.channels) && !self.caps.supports(fmt))
        {
            return Err(OutputError::UnsupportedFormat);
        }
        let fail_after = self.fail_after.lock().unwrap().take();
        let index = {
            let mut streams = self.log.streams.lock().unwrap();
            streams.push(CapturedStream {
                device: dev.clone(),
                format: *fmt,
                bytes: Vec::new(),
                rendered: Vec::new(),
                closed: false,
            });
            streams.len() - 1
        };
        let shared = Arc::new(Shared {
            control: AtomicU8::new(PAUSED),
            frames_played: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let thread = {
            let shared = Arc::clone(&shared);
            let log = Arc::clone(&self.log.streams);
            let pace = self.pace.map(|p| (p, fmt.sample_rate));
            thread::spawn(move || match pace {
                Some((period, rate)) => render_paced(feed, &shared, &log, index, period, rate),
                None => render(feed, &shared, &log, index, fail_after),
            })
        };
        Ok(Box::new(CaptureStream {
            shared,
            log: Arc::clone(&self.log.streams),
            index,
            thread: Some(thread),
        }))
    }
}

struct Shared {
    control: AtomicU8,
    frames_played: AtomicU64,
    underruns: AtomicU64,
    error: Mutex<Option<OutputError>>,
}

/// The capture "render thread": pops whole frames while running, never inserts silence.
fn render(
    mut feed: Feed,
    shared: &Shared,
    log: &Mutex<Vec<CapturedStream>>,
    index: usize,
    mut fail_after: Option<(u64, OutputError)>,
) {
    loop {
        let control = shared.control.load(Ordering::Acquire);
        if control == STOP {
            return;
        }
        if control == RUNNING {
            let played = shared.frames_played.load(Ordering::Relaxed);
            let mut frames = feed.consumer.slots() / feed.frame_bytes;
            if let Some((limit, _)) = &fail_after {
                frames = frames.min((limit - played) as usize);
                if played >= *limit {
                    // Limit reached: report the error once, then stop consuming.
                    let (_, err) = fail_after.take().unwrap();
                    *shared.error.lock().unwrap() = Some(err);
                    while shared.control.load(Ordering::Acquire) != STOP {
                        thread::sleep(Duration::from_millis(1));
                    }
                    return;
                }
            }
            if frames > 0 {
                let chunk = feed.consumer.read_chunk(frames * feed.frame_bytes).unwrap();
                let (a, b) = chunk.as_slices();
                {
                    let mut streams = log.lock().unwrap();
                    streams[index].bytes.extend_from_slice(a);
                    streams[index].bytes.extend_from_slice(b);
                }
                chunk.commit_all();
                shared
                    .frames_played
                    .fetch_add(frames as u64, Ordering::Release);
                continue;
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// The paced "render thread": one `period` buffer per `period` through [`Renderer::fill`],
/// like the WASAPI event loop.
fn render_paced(
    feed: Feed,
    shared: &Shared,
    log: &Mutex<Vec<CapturedStream>>,
    index: usize,
    period: Duration,
    rate: u32,
) {
    let mut renderer = Renderer::new(feed);
    let fb = renderer.frame_bytes();
    let frames = ((u128::from(rate) * period.as_nanos() / 1_000_000_000) as usize).max(1);
    let mut buf = vec![0; frames * fb];
    loop {
        match shared.control.load(Ordering::Acquire) {
            STOP => return,
            RUNNING => {
                let fill = renderer.fill(&mut buf);
                let at = renderer.popped_at();
                {
                    let mut streams = log.lock().unwrap();
                    let s = &mut streams[index];
                    s.bytes
                        .extend_from_slice(&buf[at * fb..(at + fill.popped) * fb]);
                    s.rendered.extend_from_slice(&buf);
                }
                shared
                    .frames_played
                    .fetch_add(fill.popped as u64, Ordering::Release);
                shared
                    .underruns
                    .fetch_add(fill.underrun as u64, Ordering::Release);
            }
            _ => {}
        }
        thread::sleep(period);
    }
}

struct CaptureStream {
    shared: Arc<Shared>,
    log: Arc<Mutex<Vec<CapturedStream>>>,
    index: usize,
    thread: Option<JoinHandle<()>>,
}

impl CaptureStream {
    fn stop_and_join(&mut self) {
        self.shared.control.store(STOP, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Stream for CaptureStream {
    fn start(&mut self) -> Result<(), OutputError> {
        self.shared.control.store(RUNNING, Ordering::Release);
        Ok(())
    }

    fn pause(&mut self) -> Result<(), OutputError> {
        self.shared.control.store(PAUSED, Ordering::Release);
        Ok(())
    }

    fn frames_played(&self) -> u64 {
        self.shared.frames_played.load(Ordering::Acquire)
    }

    fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Acquire)
    }

    fn take_error(&self) -> Option<OutputError> {
        self.shared.error.lock().unwrap().take()
    }

    fn close(mut self: Box<Self>) {
        self.stop_and_join();
        self.log.lock().unwrap()[self.index].closed = true;
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use bp_core::SampleEncoding;
    use rtrb::RingBuffer;

    use super::*;
    use crate::Silence;

    const ENC: SampleEncoding = SampleEncoding::Int {
        container: 24,
        valid: 24,
    };
    const FMT: DeviceFormat = DeviceFormat {
        sample_rate: 44_100,
        channels: 2,
        encoding: ENC,
    };

    fn caps() -> Capabilities {
        let mut c = Capabilities::default();
        c.formats.insert((44_100, 2, ENC));
        c
    }

    fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        cond()
    }

    fn open(
        backend: &CaptureBackend,
        cap_bytes: usize,
    ) -> Result<(rtrb::Producer<u8>, Box<dyn Stream>), OutputError> {
        let (prod, cons) = RingBuffer::new(cap_bytes);
        let feed = Feed {
            consumer: cons,
            frame_bytes: FMT.bytes_per_frame(),
            silence: Silence::Pcm,
        };
        let id = DeviceId("capture".into());
        backend.open(&id, &FMT, feed).map(|s| (prod, s))
    }

    fn push(prod: &mut rtrb::Producer<u8>, data: &[u8]) {
        for &b in data {
            prod.push(b).unwrap();
        }
    }

    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    #[test]
    fn capture_records_bytes_in_order() {
        let (backend, log) = CaptureBackend::new(caps());
        let (mut prod, mut stream) = open(&backend, 6000).unwrap();
        let data = pattern(6000);
        push(&mut prod, &data);
        stream.start().unwrap();
        assert!(wait_until(|| stream.frames_played() == 1000));
        assert_eq!(stream.underruns(), 0);
        stream.close();
        let streams = log.streams();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].format, FMT);
        assert_eq!(streams[0].bytes, data);
        assert!(streams[0].closed);
    }

    #[test]
    fn capture_paused_does_not_drain() {
        let (backend, log) = CaptureBackend::new(caps());
        let (mut prod, mut stream) = open(&backend, 660).unwrap();
        push(&mut prod, &pattern(600));
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(stream.frames_played(), 0, "not started yet");
        stream.start().unwrap();
        assert!(wait_until(|| stream.frames_played() == 100));
        stream.pause().unwrap();
        push(&mut prod, &pattern(60));
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(stream.frames_played(), 100);
        assert_eq!(prod.slots(), 600, "paused stream leaves the ring alone");
        stream.start().unwrap();
        assert!(wait_until(|| stream.frames_played() == 110));
        stream.close();
        assert_eq!(log.streams()[0].bytes.len(), 660);
    }

    #[test]
    fn devices_and_probe() {
        let (backend, _log) = CaptureBackend::new(caps());
        let devs = backend.devices().unwrap();
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].id, DeviceId("capture".into()));
        assert_eq!(devs[0].name, "Capture");
        assert!(devs[0].is_default);
        assert_eq!(
            backend.default_device().unwrap(),
            DeviceId("capture".into())
        );
        assert_eq!(backend.probe(&devs[0].id, 2).unwrap(), caps());
    }

    #[test]
    fn extra_devices_are_listed_and_recorded() {
        let extra = DeviceId("dac".into());
        let (backend, log) = CaptureBackend::new(caps());
        let backend = backend.with_devices([extra.clone()]);
        let devs = backend.devices().unwrap();
        assert_eq!(devs.len(), 2);
        assert_eq!(devs[1].id, extra);
        assert!(!devs[1].is_default);
        let (_p, c) = RingBuffer::<u8>::new(12);
        let feed = Feed {
            consumer: c,
            frame_bytes: 6,
            silence: Silence::Pcm,
        };
        backend.open(&extra, &FMT, feed).unwrap().close();
        assert_eq!(log.streams()[0].device, extra);
    }

    #[test]
    fn open_rejects_unsupported_and_listed_formats() {
        let (mut backend, _log) = CaptureBackend::new(caps());
        let id = DeviceId("capture".into());
        // Same channel count but unsupported rate.
        let bad = DeviceFormat {
            sample_rate: 96_000,
            ..FMT
        };
        let (_p, c) = RingBuffer::<u8>::new(16);
        let feed = Feed {
            consumer: c,
            frame_bytes: 6,
            silence: Silence::Pcm,
        };
        assert!(matches!(
            backend.open(&id, &bad, feed),
            Err(OutputError::UnsupportedFormat)
        ));
        // Channel count absent from caps: accepted.
        let other = DeviceFormat { channels: 6, ..FMT };
        let (_p, c) = RingBuffer::<u8>::new(36);
        let feed = Feed {
            consumer: c,
            frame_bytes: 18,
            silence: Silence::Pcm,
        };
        assert!(backend.open(&id, &other, feed).is_ok());
        // Explicit reject.
        backend.reject_on_open(FMT);
        assert!(matches!(
            open(&backend, 12),
            Err(OutputError::UnsupportedFormat)
        ));
    }

    #[test]
    fn fail_stream_after_stops_and_reports_once() {
        let (mut backend, log) = CaptureBackend::new(caps());
        backend.fail_stream_after(10, OutputError::Invalidated);
        let (mut prod, mut stream) = open(&backend, 600).unwrap();
        push(&mut prod, &pattern(600));
        stream.start().unwrap();
        assert!(wait_until(|| stream.frames_played() == 10));
        let mut err = None;
        assert!(wait_until(|| {
            err = stream.take_error();
            err.is_some()
        }));
        assert!(matches!(err, Some(OutputError::Invalidated)));
        assert!(stream.take_error().is_none());
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(stream.frames_played(), 10);
        stream.close();
        assert_eq!(log.streams()[0].bytes.len(), 60);
    }

    #[test]
    fn streams_snapshot_in_open_order() {
        let (backend, log) = CaptureBackend::new(caps());
        let (_p1, s1) = open(&backend, 12).unwrap();
        let (_p2, s2) = open(&backend, 12).unwrap();
        s1.close();
        let snap = log.streams();
        assert_eq!(snap.len(), 2);
        assert!(snap[0].closed);
        assert!(!snap[1].closed);
        s2.close();
        assert!(!snap[1].closed, "snapshot is a clone");
        assert!(log.streams()[1].closed);
    }
}

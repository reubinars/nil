//! Exclusive event-driven render stream (Windows), spec §6.2.
//!
//! `open` spawns the `"bp render"` thread, which does all COM work itself (no COM object ever
//! crosses threads) and reports the `Initialize` result over a one-shot channel before `open`
//! returns. The control thread then drives it through one atomic (`PAUSED` / `RUNNING` /
//! `CLOSE`) and reads the counters; the error slot is written only right before the render
//! thread exits.
//!
//! Render thread:
//! - Steady loop: wait for the device event (timeout → `Stalled`, see [`event_timeout_ms`]),
//!   fill one whole buffer with [`Renderer::fill`], `write_to_device`, publish the counters.
//!   No allocation, lock or log; the buffer is allocated before the loop.
//! - Pre-fill: on the first `Start`, one buffer is filled from the feed (silence if it is
//!   empty) and written before `start_stream`.
//! - Pause: for DoP, two more buffers of DoP silence (markers continuing from the last frame)
//!   are written before `stop_stream`, so the DAC does not drop out of DSD with a pop; for
//!   PCM, `stop_stream` right away (the device keeps its buffer). Resume: `start_stream`.
//! - Close while playing: the last written buffer may not have played yet (the engine closes
//!   once every ring frame was *popped*). So the thread waits for the next event (that buffer
//!   is now playing), writes one buffer of silence, and waits for one more event (it finished)
//!   before `stop_stream`. This keeps the end of an album from being clipped, and costs about
//!   two periods on a flush (Stop/seek), where the tail is at most one already-written buffer.
//! - While paused (or before the first Start) the thread parks; the control side unparks it.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ::wasapi::{
    AudioClient, AudioRenderClient, Direction, Handle, StreamMode, WasapiError, initialize_mta,
};
use bp_core::{DeviceFormat, DeviceId, OutputError};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::Audio::AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED;
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
};
use windows::core::w;

use super::hresult::{classify_timeout, codes, map_wasapi};
use super::params::{aligned_period_hns, event_timeout_ms};
use super::render::Renderer;
use super::{render_device, wave_format};
use crate::{Feed, Stream};

const PAUSED: u8 = 0;
const RUNNING: u8 = 1;
const CLOSE: u8 = 2;

/// How long a paused render thread sleeps between control checks (it is also unparked).
const IDLE: Duration = Duration::from_millis(5);

struct Shared {
    control: AtomicU8,
    frames_played: AtomicU64,
    underruns: AtomicU64,
    /// Written once, right before the render thread exits.
    error: Mutex<Option<OutputError>>,
}

/// The render thread's answer once the device is initialized.
struct Ready {
    /// HRESULT of a failed MMCSS registration (playback continues at normal priority).
    mmcss_failed: Option<i32>,
}

pub(crate) struct WasapiStream {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

/// Opens `dev` exclusively in `fmt`; returns once `Initialize` succeeded or failed.
pub(crate) fn open(
    dev: &DeviceId,
    fmt: &DeviceFormat,
    feed: Feed,
) -> Result<WasapiStream, OutputError> {
    debug_assert_eq!(feed.frame_bytes, fmt.bytes_per_frame());
    let shared = Arc::new(Shared {
        control: AtomicU8::new(PAUSED),
        frames_played: AtomicU64::new(0),
        underruns: AtomicU64::new(0),
        error: Mutex::new(None),
    });
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let thread = {
        let shared = Arc::clone(&shared);
        let dev = dev.clone();
        let fmt = *fmt;
        thread::Builder::new()
            .name("bp render".into())
            .spawn(move || render_thread(&dev, &fmt, feed, &shared, &ready_tx))
            .map_err(|_| OutputError::Os(codes::E_FAIL))?
    };
    match ready_rx.recv() {
        Ok(Ok(ready)) => {
            // The "status channel" for the MMCSS failure: logged once, on the control thread.
            if let Some(code) = ready.mmcss_failed {
                tracing::warn!(
                    code = format_args!("{code:#010x}"),
                    "could not register the render thread with MMCSS \"Pro Audio\"; \
                     playing at normal priority"
                );
            }
            Ok(WasapiStream {
                shared,
                thread: Some(thread),
            })
        }
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        // The thread died (panicked) before reporting.
        Err(_) => {
            let _ = thread.join();
            Err(OutputError::Os(codes::E_FAIL))
        }
    }
}

impl WasapiStream {
    fn set(&self, control: u8) {
        self.shared.control.store(control, Ordering::Release);
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
    }

    fn close_and_join(&mut self) {
        self.set(CLOSE);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Stream for WasapiStream {
    fn start(&mut self) -> Result<(), OutputError> {
        self.set(RUNNING);
        Ok(())
    }

    fn pause(&mut self) -> Result<(), OutputError> {
        self.set(PAUSED);
        Ok(())
    }

    fn frames_played(&self) -> u64 {
        self.shared.frames_played.load(Ordering::Acquire)
    }

    fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Acquire)
    }

    fn take_error(&self) -> Option<OutputError> {
        self.shared
            .error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn close(mut self: Box<Self>) {
        self.close_and_join();
    }
}

impl Drop for WasapiStream {
    fn drop(&mut self) {
        self.close_and_join();
    }
}

/// MMCSS registration of the current thread, reverted on drop.
struct Mmcss(HANDLE);

impl Mmcss {
    fn register() -> Result<Mmcss, i32> {
        let mut task_index = 0u32;
        // SAFETY: a static NUL-terminated wide string and a valid out pointer.
        unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) }
            .map(Mmcss)
            .map_err(|e| e.code().0)
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        // SAFETY: the handle came from AvSetMmThreadCharacteristicsW on this thread.
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.0) };
    }
}

/// Stores `Os(E_FAIL)` in the error slot if the render thread unwinds without having stored
/// an error.
struct PanicGuard<'a>(&'a Shared);

impl Drop for PanicGuard<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            let mut slot = self.0.error.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.is_none() {
                *slot = Some(OutputError::Os(codes::E_FAIL));
            }
        }
    }
}

fn render_thread(
    dev: &DeviceId,
    fmt: &DeviceFormat,
    feed: Feed,
    shared: &Shared,
    ready: &SyncSender<Result<Ready, OutputError>>,
) {
    // Dropped last: a panic anywhere below still leaves an error for the control thread,
    // which would otherwise wait forever on a frozen `frames_played`.
    let _panic_guard = PanicGuard(shared);
    let _ = initialize_mta();
    // Declared first so it is reverted after the COM objects below are released.
    let mmcss = Mmcss::register();
    let device = match Device::init(dev, fmt) {
        Ok(d) => d,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut renderer = Renderer::new(feed);
    let mut buf = vec![0u8; device.buffer_frames * renderer.frame_bytes()];
    let _ = ready.send(Ok(Ready {
        mmcss_failed: mmcss.as_ref().err().copied(),
    }));
    if let Err(e) = device.run(&mut renderer, &mut buf, shared) {
        let _ = device.client.stop_stream();
        *shared.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
    }
}

/// An initialized exclusive event-driven render client.
struct Device {
    client: AudioClient,
    render: AudioRenderClient,
    event: Handle,
    buffer_frames: usize,
    timeout_ms: u32,
}

impl Device {
    /// Spec §6.2 steps 1–4.
    fn init(dev: &DeviceId, fmt: &DeviceFormat) -> Result<Device, OutputError> {
        let device = render_device(dev)?;
        let wave = wave_format(fmt);
        let mut client = device.get_iaudioclient().map_err(map_wasapi)?;
        let (default_period, _min) = client.get_device_period().map_err(map_wasapi)?;
        let mut period = default_period;
        match client.initialize_client(&wave, &Direction::Render, &events_exclusive(period)) {
            Ok(()) => {}
            Err(WasapiError::Windows(e)) if e.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => {
                // The aligned period from the buffer size this attempt reports, then a fresh
                // client initialized with *that* period (not the old one).
                let frames = client.get_buffer_size().map_err(map_wasapi)?;
                period = aligned_period_hns(frames, fmt.sample_rate);
                drop(client);
                client = device.get_iaudioclient().map_err(map_wasapi)?;
                client
                    .initialize_client(&wave, &Direction::Render, &events_exclusive(period))
                    .map_err(map_wasapi)?;
            }
            Err(e) => return Err(map_wasapi(e)),
        }
        let event = client.set_get_eventhandle().map_err(map_wasapi)?;
        let render = client.get_audiorenderclient().map_err(map_wasapi)?;
        let buffer_frames = client.get_buffer_size().map_err(map_wasapi)? as usize;
        Ok(Device {
            client,
            render,
            event,
            buffer_frames,
            timeout_ms: event_timeout_ms(period),
        })
    }

    fn run(&self, r: &mut Renderer, buf: &mut [u8], shared: &Shared) -> Result<(), OutputError> {
        let mut running = false;
        let mut primed = false;
        loop {
            match shared.control.load(Ordering::Acquire) {
                CLOSE => {
                    if running {
                        self.drain(r, buf);
                    }
                    let _ = self.client.stop_stream();
                    return Ok(());
                }
                PAUSED => {
                    if running {
                        if r.is_dop() {
                            for _ in 0..2 {
                                self.wait()?;
                                r.fill_silence(buf);
                                self.write(buf)?;
                            }
                        }
                        self.client.stop_stream().map_err(map_wasapi)?;
                        running = false;
                    }
                    thread::park_timeout(IDLE);
                    continue;
                }
                _ => {
                    if !running {
                        if !primed {
                            self.fill_and_write(r, buf, shared)?;
                            primed = true;
                        }
                        self.client.start_stream().map_err(map_wasapi)?;
                        running = true;
                    }
                }
            }
            self.wait()?;
            self.fill_and_write(r, buf, shared)?;
        }
    }

    /// Waits for the device event. On timeout (error path only) the client is queried, so an
    /// unplugged endpoint, which typically just stops signalling, reports `Invalidated`
    /// rather than `Stalled`.
    fn wait(&self) -> Result<(), OutputError> {
        self.event.wait_for_event(self.timeout_ms).map_err(|_| {
            classify_timeout(
                self.client
                    .get_current_padding()
                    .map(|_| ())
                    .map_err(map_wasapi),
            )
        })
    }

    fn write(&self, buf: &[u8]) -> Result<(), OutputError> {
        self.render
            .write_to_device(self.buffer_frames, buf, None)
            .map_err(map_wasapi)
    }

    fn fill_and_write(
        &self,
        r: &mut Renderer,
        buf: &mut [u8],
        shared: &Shared,
    ) -> Result<(), OutputError> {
        let fill = r.fill(buf);
        self.write(buf)?;
        shared
            .frames_played
            .fetch_add(fill.popped as u64, Ordering::Release);
        if fill.underrun > 0 {
            shared
                .underruns
                .fetch_add(fill.underrun as u64, Ordering::Release);
        }
        Ok(())
    }

    /// Lets the last written buffer play out (best effort: errors just end the drain).
    fn drain(&self, r: &mut Renderer, buf: &mut [u8]) {
        if self.wait().is_ok() {
            r.fill_silence(buf);
            if self.write(buf).is_ok() {
                let _ = self.wait();
            }
        }
    }
}

fn events_exclusive(period_hns: i64) -> StreamMode {
    StreamMode::EventsExclusive { period_hns }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use bp_core::DeviceFormat;
    use rtrb::RingBuffer;

    use crate::{Feed, OutputBackend, Silence, WasapiBackend};

    /// Hardware: `cargo test -p bp-output -- --ignored plays_one_second --nocapture`
    #[test]
    #[ignore = "needs a Windows audio device"]
    fn plays_one_second_of_silence_exclusive() {
        let backend = WasapiBackend::new();
        let dev = backend.default_device().unwrap();
        let caps = backend.probe(&dev, 2).unwrap();
        // Lowest probed rate first (the set is ordered by rate).
        let &(sample_rate, channels, encoding) =
            caps.formats.iter().next().expect("no probed format");
        let fmt = DeviceFormat {
            sample_rate,
            channels,
            encoding,
        };
        println!("playing 1 s of silence as {fmt:?}");
        let frame_bytes = fmt.bytes_per_frame();
        let rate = u64::from(sample_rate);
        let (mut prod, consumer) = RingBuffer::new(sample_rate as usize * frame_bytes * 2);
        let feed = Feed {
            consumer,
            frame_bytes,
            silence: Silence::Pcm,
        };
        let mut stream = backend.open(&dev, &fmt, feed).unwrap();
        for _ in 0..sample_rate as usize * frame_bytes {
            prod.push(0).unwrap();
        }
        // End of stream, as the decoder does: the dry ring afterwards is not an underrun.
        drop(prod);
        let t0 = Instant::now();
        stream.start().unwrap();
        while stream.frames_played() < rate && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let elapsed = t0.elapsed();
        let played = stream.frames_played();
        let underruns = stream.underruns();
        let err = stream.take_error();
        stream.close();
        println!("played {played} frames in {elapsed:?}, underruns {underruns}, error {err:?}");
        assert!(err.is_none(), "stream error: {err:?}");
        assert_eq!(played, rate, "all of the 1 s was popped within 2 s");
        assert_eq!(underruns, 0);
    }
}

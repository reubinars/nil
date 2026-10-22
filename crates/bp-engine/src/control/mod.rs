//! Control thread: owns the backend, the profile store and the stream lifecycle; maps the
//! decoder's timeline onto `frames_played` to emit events.

use std::any::Any;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bp_core::{DeviceId, EngineError, OutputError, PathKind, PlaybackPath, SampleEncoding};
use bp_output::{Feed, OutputBackend, Silence};
use crossbeam_channel::{Receiver, Sender, bounded, never, select, tick};
use rtrb::RingBuffer;
use tracing::{info, warn};

use crate::analysis::AnalysisTap;
use crate::decoder::{self, DecoderMsg, Job, StreamReply};
use crate::profile::ProfileStore;
use crate::timeline::{Mark, Output, Segment, close_drained, report_progress, schedule};
use crate::{Command, Event};

mod devices;

pub(crate) use devices::DeviceView;

const TICK: Duration = Duration::from_millis(10);
/// `Previous` restarts the track when more than this much of it has played.
const RESTART_AFTER: Duration = Duration::from_secs(3);

pub(crate) struct Control {
    backend: Box<dyn OutputBackend>,
    profiles: ProfileStore,
    device: Option<DeviceId>,
    events: Sender<Event>,
    view: Arc<RwLock<DeviceView>>,
    play: Option<Playback>,
    /// Told where playback is, for the spectrum.
    analysis: AnalysisTap,
}

/// One `Play` command's worth of state.
struct Playback {
    device: DeviceId,
    dec_rx: Option<Receiver<DecoderMsg>>,
    reply_tx: Option<Sender<StreamReply>>,
    cancel: Arc<AtomicBool>,
    decoder: Option<JoinHandle<()>>,
    stream: Option<Output>,
    /// The track a requested stream is for and its candidates, opened once the current
    /// one has drained.
    pending: Option<(usize, Vec<PlaybackPath>)>,
    done: bool,
    /// Underruns of all streams of this playback.
    underruns: u64,
    /// The queue, for `Next` and `Seek`.
    tracks: Vec<PathBuf>,
    /// The track being played (the last `TrackStarted`), or the first one to play.
    index: usize,
    /// Where in track `index` the first stream starts; zero once it has drained.
    start_pos: Duration,
    /// Paused by the user; a stream opened meanwhile waits for `Resume`.
    paused: bool,
}

impl Playback {
    fn drained(&self) -> bool {
        self.stream.as_ref().is_none_or(Output::drained)
    }
}

impl Control {
    pub(crate) fn new(
        backend: Box<dyn OutputBackend>,
        profiles: ProfileStore,
        device: Option<DeviceId>,
        events: Sender<Event>,
        view: Arc<RwLock<DeviceView>>,
        analysis: AnalysisTap,
    ) -> Control {
        Control {
            backend,
            profiles,
            device,
            events,
            view,
            play: None,
            analysis,
        }
    }

    pub(crate) fn run(mut self, commands: Receiver<Command>) {
        // Watching before the first listing: a change in between is not missed.
        // One slot: a burst of notifications (a plug-in fires several) is one relist.
        let (changed_tx, changed_rx) = bounded(1);
        let watch = self.backend.watch_devices(Box::new(move || {
            let _ = changed_tx.try_send(());
        }));
        // Without a watch the sender is gone and `changed_rx` would always be ready.
        let changed_rx = if watch.is_some() { changed_rx } else { never() };
        let devices = self.list_devices();
        self.publish_devices(devices);
        let ticker = tick(TICK);
        loop {
            let dec_rx = self
                .play
                .as_ref()
                .and_then(|p| p.dec_rx.clone())
                .unwrap_or_else(never);
            select! {
                recv(commands) -> cmd => {
                    let Ok(cmd) = cmd else {
                        self.stop();
                        return;
                    };
                    if !self.handle(cmd, &commands) {
                        return;
                    }
                }
                recv(dec_rx) -> msg => match msg {
                    Ok(msg) => self.decoder_msg(msg),
                    Err(_) => self.decoder_gone(),
                },
                recv(changed_rx) -> _ => self.relist_devices(),
                recv(ticker) -> _ => {}
            }
            self.update();
        }
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    /// Runs `cmd`, folding the `Next`/`Previous`/`Seek` commands already queued behind a jump into it.
    /// Returns false after `Shutdown`.
    fn handle(&mut self, cmd: Command, commands: &Receiver<Command>) -> bool {
        let mut cmd = Some(cmd);
        while let Some(c) = cmd.take() {
            match c {
                Command::Shutdown => {
                    self.stop();
                    return false;
                }
                Command::Next | Command::Previous | Command::Seek { .. } => {
                    cmd = self.jump(c, commands)
                }
                c => self.command(c),
            }
        }
        true
    }

    fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Play { tracks, start } => {
                self.stop();
                self.start_playback(tracks, start, Duration::ZERO);
            }
            Command::Stop => self.stop(),
            Command::Pause => self.pause(),
            Command::Resume => self.resume(),
            Command::Next | Command::Previous | Command::Seek { .. } => {
                self.jump(cmd, &never());
            }
            Command::SelectDevice { id } => self.select_device(id),
            Command::Reprobe { id } => self.reprobe(&id),
            Command::RefreshDevices => self.relist_devices(),
            Command::Shutdown => unreachable!("handled by handle"),
        }
    }

    /// Restarts once at where `first` and the `Next`/`Previous`/`Seek` commands queued right behind it
    /// land, so a burst of them (a dragged seek bar, a held key) reopens the device once
    /// rather than once per command. Returns the first other queued command. Ignored when
    /// nothing is playing.
    fn jump(&mut self, first: Command, commands: &Receiver<Command>) -> Option<Command> {
        let (index, pos) = self.playing_position()?;
        let (index, pos, rest) = fold_jumps(index, pos, first, || commands.try_recv().ok());
        self.restart(index, pos);
        rest
    }

    /// The track actually playing and how far into it, after emitting the marks reached
    /// since the last tick. Before the stream has started that is where it will start.
    fn playing_position(&mut self) -> Option<(usize, Duration)> {
        let play = self.play.as_mut()?;
        catch_up(play, &self.events);
        let started = play.stream.as_ref().filter(|o| o.started);
        Some(
            started
                .and_then(Output::position)
                .unwrap_or((play.index, play.start_pos)),
        )
    }

    fn pause(&mut self) {
        let Some(play) = self.play.as_mut().filter(|p| !p.paused) else {
            return;
        };
        if let Some(out) = play.stream.as_mut().filter(|o| o.started)
            && let Err(e) = out.stream.pause()
        {
            return self.fail(e.into());
        }
        // Emit what was reached up to the pause: ticks skip progress while paused.
        catch_up(play, &self.events);
        play.paused = true;
        self.emit(Event::Paused);
    }

    fn resume(&mut self) {
        let Some(play) = self.play.as_mut().filter(|p| p.paused) else {
            return;
        };
        play.paused = false;
        if let Some(out) = play.stream.as_mut().filter(|o| o.started || o.primed) {
            if let Err(e) = out.stream.start() {
                return self.fail(e.into());
            }
            out.started = true;
        }
        self.emit(Event::Resumed);
    }

    /// Restarts the pipeline at `(start, pos)` on a fresh stream, keeping the pause state.
    /// Past the end of the queue it finishes.
    fn restart(&mut self, start: usize, pos: Duration) {
        if let Some((tracks, paused)) = self.stop_keeping_queue() {
            self.play_from(tracks, start, pos, paused);
        }
    }

    /// Stops playback, returning its queue and pause state.
    fn stop_keeping_queue(&mut self) -> Option<(Vec<PathBuf>, bool)> {
        let mut play = self.play.take()?;
        let tracks = std::mem::take(&mut play.tracks);
        let paused = play.paused;
        stop_playback(play);
        Some((tracks, paused))
    }

    /// Starts `tracks` at `(start, pos)`, paused or not. Past the end of the queue it
    /// finishes.
    fn play_from(&mut self, tracks: Vec<PathBuf>, start: usize, pos: Duration, paused: bool) {
        if start >= tracks.len() {
            info!("queue finished");
            return self.emit(Event::QueueFinished);
        }
        self.start_playback(tracks, start, pos);
        if let Some(play) = &mut self.play {
            play.paused = paused;
        }
    }

    fn start_playback(&mut self, tracks: Vec<PathBuf>, start: usize, start_pos: Duration) {
        let device = match self.device.clone() {
            Some(d) => d,
            None => match self.backend.default_device() {
                Ok(d) => d,
                Err(e) => return self.emit(Event::Error(e.into())),
            },
        };
        let profile = match self.profile(&device) {
            Ok(p) => p,
            Err(e) => return self.emit(Event::Error(e.into())),
        };
        info!(device = %device.0, tracks = tracks.len(), start, ?start_pos, "play");
        let job = Job {
            tracks: tracks.clone(),
            start,
            start_pos,
            caps: profile.capabilities(),
            dop: profile.dop_enabled,
            analysis: self.analysis.clone(),
        };
        let (ctl_tx, dec_rx) = crossbeam_channel::unbounded();
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let decoder = {
            let cancel = Arc::clone(&cancel);
            thread::Builder::new()
                .name("bp-decoder".into())
                .spawn(move || decoder::run(job, ctl_tx, reply_rx, cancel))
                .expect("spawn decoder thread")
        };
        let mut play = Playback::new(device, dec_rx, reply_tx, cancel, decoder);
        play.tracks = tracks;
        play.index = start;
        play.start_pos = start_pos;
        self.play = Some(play);
    }

    fn decoder_msg(&mut self, msg: DecoderMsg) {
        let Some(play) = &mut self.play else { return };
        match msg {
            DecoderMsg::Segment {
                index,
                path,
                playback,
                duration,
                start_frame,
                start_pos,
            } => {
                if let Some(out) = &mut play.stream {
                    let seg = Segment {
                        index,
                        path,
                        playback,
                        duration,
                        start_pos,
                    };
                    out.timeline.push_back((start_frame, Mark::Track(seg)));
                }
            }
            DecoderMsg::Skipped {
                index,
                path,
                reason,
                at_frame,
            } => {
                let e = Event::TrackSkipped {
                    index,
                    path,
                    reason,
                };
                schedule(play.stream.as_mut(), at_frame, e, &self.events);
            }
            DecoderMsg::TrackError {
                index,
                path,
                error,
                at_frame,
            } => {
                let e = Event::TrackError { index, path, error };
                schedule(play.stream.as_mut(), at_frame, e, &self.events);
            }
            DecoderMsg::NeedStream { index, candidates } => {
                play.pending = Some((index, candidates))
            }
            DecoderMsg::StreamEnd { end_frame } => {
                if let Some(out) = &mut play.stream {
                    out.end_frame = Some(end_frame);
                }
            }
            DecoderMsg::Primed => {
                if let Some(out) = play.stream.as_mut().filter(|o| !o.started) {
                    out.primed = true;
                    if !play.paused {
                        match out.stream.start() {
                            Ok(()) => out.started = true,
                            Err(e) => self.fail(e.into()),
                        }
                    }
                }
            }
            DecoderMsg::Done => play.done = true,
        }
    }

    /// The decoder's channel closed. After `Done` that is normal; otherwise the decoder
    /// died (panicked) and playback cannot continue.
    fn decoder_gone(&mut self) {
        let Some(play) = &mut self.play else { return };
        play.dec_rx = None;
        if play.done {
            return;
        }
        let play = self.play.take().expect("playback");
        let reason = teardown(play).unwrap_or_else(|| "exited unexpectedly".to_owned());
        warn!(%reason, "decoder thread failed");
        self.emit(Event::Error(EngineError::DecoderFailed(reason)));
    }

    /// Runs after every wake-up: errors, events, stream changes, end of queue.
    fn update(&mut self) {
        let Some(play) = &mut self.play else {
            self.analysis.clear();
            return;
        };
        match &play.stream {
            // Not started yet counts as paused: nothing is heard.
            Some(out) => self.analysis.played(
                out.stream.frames_played(),
                Instant::now(),
                play.paused || !out.started,
            ),
            None => self.analysis.clear(),
        }
        if let Some(out) = &mut play.stream {
            if let Some(e) = out.stream.take_error() {
                return self.fail(e.into());
            }
            let underruns = out.stream.underruns();
            if underruns != out.underruns {
                play.underruns += underruns.saturating_sub(out.underruns);
                out.underruns = underruns;
                let _ = self.events.send(Event::Underrun {
                    total: play.underruns,
                });
            }
            if out.started && !play.paused {
                report_progress(out, &self.events);
            }
            play.index = out.current_index().unwrap_or(play.index);
        }
        if !play.drained() {
            return;
        }
        if let Some((index, candidates)) = play.pending.take() {
            if let Some(out) = play.stream.take() {
                close_drained(out, &self.events);
                // Until the next stream starts, that is where playback is: at the
                // beginning of the track it is for.
                play.index = index;
                play.start_pos = Duration::ZERO;
            }
            if let Err(e) = self.open_stream(candidates) {
                self.fail(e.into());
            }
        } else if play.done {
            if let Some(out) = play.stream.take() {
                close_drained(out, &self.events);
            }
            self.stop();
            info!("queue finished");
            self.emit(Event::QueueFinished);
        }
    }

    /// Opens the first candidate the device accepts and hands its ring to the decoder.
    fn open_stream(&mut self, candidates: Vec<PlaybackPath>) -> Result<(), OutputError> {
        let Some(play) = &mut self.play else {
            return Ok(());
        };
        for path in candidates {
            let frame_bytes = path.device.bytes_per_frame();
            let capacity = path.device.sample_rate as usize * frame_bytes * 2;
            let (producer, consumer) = RingBuffer::new(capacity);
            let feed = Feed {
                consumer,
                frame_bytes,
                silence: silence_for(&path),
            };
            match self.backend.open(&play.device, &path.device, feed) {
                Ok(stream) => {
                    info!(?path, "stream opened");
                    play.stream = Some(Output::new(stream));
                    reply(play, StreamReply::Ready { producer, path });
                    return Ok(());
                }
                Err(OutputError::UnsupportedFormat) => {
                    warn!(?path.device, "device rejected a probed format; disabling it");
                    self.profiles.disable_format(&play.device, &path.device);
                    if let Err(e) = self.profiles.save() {
                        warn!(%e, "could not save device profiles");
                    }
                }
                Err(e) => return Err(e),
            }
        }
        reply(play, StreamReply::NoneFit);
        Ok(())
    }

    /// Fatal error: stop and report. A device that went away (unplugged) also gets the
    /// device list published again.
    fn fail(&mut self, e: EngineError) {
        warn!(%e, "playback failed");
        self.stop();
        let gone = matches!(
            e,
            EngineError::Output(OutputError::Invalidated | OutputError::DeviceNotFound)
        );
        self.emit(Event::Error(e));
        if gone {
            self.relist_devices();
        }
    }

    /// Cancels the decoder, flushes and closes the stream.
    fn stop(&mut self) {
        if let Some(play) = self.play.take() {
            stop_playback(play);
        }
    }
}

impl Playback {
    fn new(
        device: DeviceId,
        dec_rx: Receiver<DecoderMsg>,
        reply_tx: Sender<StreamReply>,
        cancel: Arc<AtomicBool>,
        decoder: JoinHandle<()>,
    ) -> Playback {
        Playback {
            device,
            dec_rx: Some(dec_rx),
            reply_tx: Some(reply_tx),
            cancel,
            decoder: Some(decoder),
            stream: None,
            pending: None,
            done: false,
            underruns: 0,
            tracks: Vec::new(),
            index: 0,
            start_pos: Duration::ZERO,
            paused: false,
        }
    }
}

/// Emits the marks playback has reached since the last tick and updates `play.index`.
fn catch_up(play: &mut Playback, events: &Sender<Event>) {
    if let Some(out) = play.stream.as_mut().filter(|o| o.started) {
        report_progress(out, events);
        play.index = out.current_index().unwrap_or(play.index);
    }
}

/// Where a run of `Next`/`Previous`/`Seek` commands starting with `first` lands from
/// position `pos` in track `index`: a `Next` goes to the start of the following track, a
/// `Previous` to the start of this track if more than `RESTART_AFTER` of it has played
/// (or it is the first) else of the one before, a `Seek` to a position in the track
/// reached so far. Takes commands from `more` until one is none of these, and returns it.
fn fold_jumps(
    mut index: usize,
    mut pos: Duration,
    first: Command,
    mut more: impl FnMut() -> Option<Command>,
) -> (usize, Duration, Option<Command>) {
    let mut cmd = Some(first);
    while let Some(c) = cmd.take() {
        match c {
            Command::Next => {
                index += 1;
                pos = Duration::ZERO;
            }
            Command::Previous => {
                if pos <= RESTART_AFTER {
                    index = index.saturating_sub(1);
                }
                pos = Duration::ZERO;
            }
            Command::Seek { position } => pos = position,
            other => return (index, pos, Some(other)),
        }
        cmd = more();
    }
    (index, pos, None)
}

fn stop_playback(play: Playback) {
    if let Some(panic) = teardown(play) {
        warn!(%panic, "decoder thread panicked");
    }
}

/// Cancels and joins the decoder, then closes the stream. Returns the decoder's panic
/// message if it panicked.
fn teardown(mut play: Playback) -> Option<String> {
    play.cancel.store(true, Ordering::Release);
    drop(play.reply_tx.take());
    let panic = play
        .decoder
        .take()
        .and_then(|h| h.join().err())
        .map(|p| panic_message(p.as_ref()));
    if let Some(out) = play.stream.take() {
        out.stream.close();
    }
    panic
}

fn panic_message(p: &(dyn Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panicked".to_owned())
}

fn reply(play: &Playback, r: StreamReply) {
    if let Some(tx) = &play.reply_tx {
        let _ = tx.send(r);
    }
}

/// Silence the render thread inserts for a stream on `path`.
fn silence_for(path: &PlaybackPath) -> Silence {
    match (path.kind, path.device.encoding) {
        (PathKind::Dop, SampleEncoding::Int { container, .. }) => Silence::Dop { container },
        _ => Silence::Pcm,
    }
}

#[cfg(test)]
mod tests {
    use bp_core::{Capabilities, Codec, DeviceFormat, SampleEncoding, SourceFormat, SourceKind};
    use bp_output::CaptureBackend;
    use crossbeam_channel::{bounded, unbounded};

    use super::*;

    /// A control with a started stream whose timeline holds two tracks, both already
    /// reached by `frames_played` (0) but not yet reported, as right after a gapless
    /// boundary between two ticks.
    fn boundary_not_yet_reported() -> (Control, Receiver<Event>) {
        let encoding = SampleEncoding::Int {
            container: 16,
            valid: 16,
        };
        let fmt = DeviceFormat {
            sample_rate: 44_100,
            channels: 2,
            encoding,
        };
        let mut caps = Capabilities::default();
        caps.formats.insert((44_100, 2, encoding));
        let (backend, _log) = CaptureBackend::new(caps);
        let dir = tempfile::tempdir().unwrap();
        let profiles = ProfileStore::load(&dir.path().join("devices.toml")).unwrap();
        let (evt_tx, evt_rx) = unbounded();
        let mut control = Control::new(
            Box::new(backend),
            profiles,
            None,
            evt_tx,
            Arc::default(),
            AnalysisTap::default(),
        );
        let device = DeviceId("capture".into());
        let (_producer, consumer) = RingBuffer::new(4_000);
        let feed = Feed {
            consumer,
            frame_bytes: 4,
            silence: Silence::Pcm,
        };
        let stream = control.backend.open(&device, &fmt, feed).unwrap();
        let (_ctl_tx, dec_rx) = unbounded();
        let (reply_tx, _reply_rx) = bounded(1);
        let decoder = thread::spawn(|| {});
        let cancel = Arc::new(AtomicBool::new(false));
        let mut play = Playback::new(device, dec_rx, reply_tx, cancel, decoder);
        play.tracks = vec![PathBuf::from("t0"), PathBuf::from("t1")];
        let playback = PlaybackPath {
            source: SourceFormat {
                codec: Codec::Wav,
                sample_rate: 44_100,
                channels: 2,
                kind: SourceKind::Pcm {
                    bits: 16,
                    float: false,
                },
            },
            device: fmt,
            kind: PathKind::Native,
        };
        let mut out = Output::new(stream);
        out.started = true;
        for index in 0..2 {
            let seg = Segment {
                index,
                path: play.tracks[index].clone(),
                playback,
                duration: None,
                start_pos: Duration::ZERO,
            };
            out.timeline.push_back((0, Mark::Track(seg)));
        }
        play.stream = Some(out);
        control.play = Some(play);
        (control, evt_rx)
    }

    /// The events emitted so far, except `Position` and `DevicesChanged`.
    fn drain(rx: &Receiver<Event>) -> Vec<Event> {
        rx.try_iter()
            .filter(|e| !matches!(e, Event::Position { .. } | Event::DevicesChanged))
            .collect()
    }

    #[test]
    fn pause_reports_progress_reached_before_it() {
        let (mut control, rx) = boundary_not_yet_reported();
        control.command(Command::Pause);
        let events = drain(&rx);
        assert!(
            matches!(
                events[..],
                [
                    Event::TrackStarted { index: 0, .. },
                    Event::TrackStarted { index: 1, .. },
                    Event::Paused
                ]
            ),
            "{events:?}"
        );
        assert_eq!(control.play.as_ref().unwrap().index, 1);
        control.stop();
    }

    #[test]
    fn next_advances_from_the_track_actually_playing() {
        let (mut control, rx) = boundary_not_yet_reported();
        // Track 1 (the last) is playing, so Next finishes the queue rather than
        // restarting at track 1.
        control.command(Command::Next);
        let events = drain(&rx);
        assert!(
            matches!(
                events[..],
                [
                    Event::TrackStarted { index: 0, .. },
                    Event::TrackStarted { index: 1, .. },
                    Event::QueueFinished
                ]
            ),
            "{events:?}"
        );
        assert!(control.play.is_none());
    }

    #[test]
    fn a_run_of_jumps_folds_into_one_target() {
        let ms = Duration::from_millis;
        let mut queue = vec![
            Command::Seek { position: ms(100) },
            Command::Seek { position: ms(250) },
            Command::Pause,
            Command::Next,
        ]
        .into_iter();
        let (index, pos, rest) = fold_jumps(3, ms(0), Command::Next, || queue.next());
        assert_eq!((index, pos), (4, ms(250)));
        assert!(matches!(rest, Some(Command::Pause)), "{rest:?}");
        assert!(
            matches!(queue.next(), Some(Command::Next)),
            "stops at the first other"
        );

        // A Next after a Seek starts the following track from its beginning.
        let mut queue = vec![Command::Next, Command::Next].into_iter();
        let seek = Command::Seek { position: ms(500) };
        let (index, pos, rest) = fold_jumps(0, ms(0), seek, || queue.next());
        assert_eq!((index, pos, rest.is_none()), (2, Duration::ZERO, true));
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn previous_past_three_seconds_restarts_the_track() {
        let (i, p, _) = fold_jumps(2, ms(3_001), Command::Previous, || None);
        assert_eq!((i, p), (2, Duration::ZERO));
    }

    #[test]
    fn previous_within_three_seconds_goes_back() {
        let (i, _, _) = fold_jumps(2, ms(3_000), Command::Previous, || None);
        assert_eq!(i, 1);
    }

    #[test]
    fn previous_on_the_first_track_restarts_it() {
        let (i, p, _) = fold_jumps(0, ms(500), Command::Previous, || None);
        assert_eq!((i, p), (0, Duration::ZERO));
    }

    #[test]
    fn seek_then_previous_uses_the_seek_position() {
        let mut more = vec![Command::Previous].into_iter();
        let seek = Command::Seek {
            position: ms(10_000),
        };
        let (i, p, _) = fold_jumps(2, ms(0), seek, || more.next());
        assert_eq!((i, p), (2, Duration::ZERO));
    }

    #[test]
    fn queued_jumps_restart_once_and_the_next_command_still_runs() {
        let (mut control, rx) = boundary_not_yet_reported();
        let (cmd_tx, cmd_rx) = unbounded();
        for ms in [100, 200, 300] {
            let position = Duration::from_millis(ms);
            cmd_tx.send(Command::Seek { position }).unwrap();
        }
        cmd_tx.send(Command::Pause).unwrap();
        let first = Command::Seek {
            position: Duration::ZERO,
        };
        assert!(control.handle(first, &cmd_rx));
        assert!(cmd_rx.is_empty(), "the queued commands were consumed");
        let events = drain(&rx);
        assert!(
            matches!(
                events[..],
                [
                    Event::TrackStarted { index: 0, .. },
                    Event::TrackStarted { index: 1, .. },
                    Event::Paused
                ]
            ),
            "{events:?}"
        );
        assert!(
            control
                .play
                .as_ref()
                .is_some_and(|p| p.paused && p.index == 1)
        );
        assert!(!control.handle(Command::Shutdown, &cmd_rx));
        assert!(control.play.is_none(), "Shutdown stops playback");
    }

    #[test]
    fn decoder_panic_is_fatal_and_releases_device() {
        let encoding = SampleEncoding::Int {
            container: 16,
            valid: 16,
        };
        let fmt = DeviceFormat {
            sample_rate: 44_100,
            channels: 2,
            encoding,
        };
        let mut caps = Capabilities::default();
        caps.formats.insert((44_100, 2, encoding));
        let (backend, log) = CaptureBackend::new(caps);
        let dir = tempfile::tempdir().unwrap();
        let profiles = ProfileStore::load(&dir.path().join("devices.toml")).unwrap();
        let (evt_tx, evt_rx) = unbounded();
        let mut control = Control::new(
            Box::new(backend),
            profiles,
            None,
            evt_tx,
            Arc::default(),
            AnalysisTap::default(),
        );

        // Mid-playback: a stream is open when the decoder dies without sending `Done`.
        let device = DeviceId("capture".into());
        let (_producer, consumer) = RingBuffer::new(4_000);
        let feed = Feed {
            consumer,
            frame_bytes: 4,
            silence: Silence::Pcm,
        };
        let stream = control.backend.open(&device, &fmt, feed).unwrap();
        let (ctl_tx, dec_rx) = unbounded();
        let (reply_tx, _reply_rx) = bounded(1);
        let decoder = thread::spawn(move || {
            let _ctl = ctl_tx;
            panic!("boom");
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let mut play = Playback::new(device, dec_rx, reply_tx, cancel, decoder);
        play.stream = Some(Output::new(stream));
        control.play = Some(play);

        let (cmd_tx, cmd_rx) = unbounded();
        let handle = thread::spawn(move || control.run(cmd_rx));
        let e = evt_rx
            .iter()
            .find(|e| !matches!(e, Event::DevicesChanged))
            .unwrap();
        assert!(
            matches!(&e, Event::Error(EngineError::DecoderFailed(m)) if m == "boom"),
            "{e:?}"
        );
        assert!(log.streams()[0].closed, "device released");
        cmd_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
        assert!(evt_rx.try_recv().is_err(), "no further events");
    }
}

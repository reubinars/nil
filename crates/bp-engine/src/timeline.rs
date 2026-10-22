//! Event timeline of an open stream: marks the decoder reported, emitted once `frames_played`
//! reaches them, plus `Position` for the track being played.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bp_core::PlaybackPath;
use bp_output::Stream;
use crossbeam_channel::Sender;

use crate::Event;

const POSITION_EVERY: Duration = Duration::from_millis(250);

pub(crate) struct Segment {
    pub index: usize,
    pub path: PathBuf,
    pub playback: PlaybackPath,
    pub duration: Option<Duration>,
    pub start_pos: Duration,
}

/// Something to emit once playback reaches a frame.
pub(crate) enum Mark {
    Track(Segment),
    Event(Event),
}

/// The track being played, for `Position`.
struct Current {
    index: usize,
    start_frame: u64,
    start_pos: Duration,
    rate: u32,
}

/// An open stream and its timeline.
pub(crate) struct Output {
    pub stream: Box<dyn Stream>,
    pub started: bool,
    /// The decoder reported enough data to start.
    pub primed: bool,
    /// Marks not yet reached, in decoder order (non-decreasing frames).
    pub timeline: VecDeque<(u64, Mark)>,
    current: Option<Current>,
    pub end_frame: Option<u64>,
    /// Underruns of this stream already counted.
    pub underruns: u64,
    last_position: Option<Instant>,
}

impl Output {
    pub(crate) fn new(stream: Box<dyn Stream>) -> Output {
        Output {
            stream,
            started: false,
            primed: false,
            timeline: VecDeque::new(),
            current: None,
            end_frame: None,
            underruns: 0,
            last_position: None,
        }
    }

    /// The track being played, once one has started.
    pub(crate) fn current_index(&self) -> Option<usize> {
        self.current.as_ref().map(|c| c.index)
    }

    /// The track being played and how far into it, as the next `Position` would say.
    pub(crate) fn position(&self) -> Option<(usize, Duration)> {
        let cur = self.current.as_ref()?;
        let played = self.stream.frames_played();
        let pos = cur.start_pos + frames_to_duration(played - cur.start_frame, cur.rate);
        Some((cur.index, pos))
    }

    pub(crate) fn drained(&self) -> bool {
        self.end_frame
            .is_some_and(|end| self.stream.frames_played() >= end)
    }
}

/// Queues `e` on the current stream's timeline at `at_frame`. With no stream open on the
/// decoder side everything before it has already been emitted, so it is emitted now.
pub(crate) fn schedule(
    stream: Option<&mut Output>,
    at_frame: Option<u64>,
    e: Event,
    events: &Sender<Event>,
) {
    match (at_frame, stream) {
        (Some(frame), Some(out)) => out.timeline.push_back((frame, Mark::Event(e))),
        _ => {
            let _ = events.send(e);
        }
    }
}

/// Closes a drained stream, first emitting the marks reached since the last report.
pub(crate) fn close_drained(mut out: Output, events: &Sender<Event>) {
    report_progress(&mut out, events);
    out.stream.close();
}

/// Emits every timeline mark playback has reached, then `Position`.
pub(crate) fn report_progress(out: &mut Output, events: &Sender<Event>) {
    let played = out.stream.frames_played();
    while out.timeline.front().is_some_and(|(f, _)| *f <= played) {
        let (frame, mark) = out.timeline.pop_front().expect("front checked");
        let e = match mark {
            Mark::Event(e) => e,
            Mark::Track(seg) => {
                out.current = Some(Current {
                    index: seg.index,
                    start_frame: frame,
                    start_pos: seg.start_pos,
                    rate: seg.playback.device.sample_rate,
                });
                out.last_position = None;
                Event::TrackStarted {
                    index: seg.index,
                    path: seg.path,
                    playback: seg.playback,
                    duration: seg.duration,
                }
            }
        };
        let _ = events.send(e);
    }
    let Some((index, position)) = out.position() else {
        return;
    };
    if out
        .last_position
        .is_some_and(|t| t.elapsed() < POSITION_EVERY)
    {
        return;
    }
    out.last_position = Some(Instant::now());
    let _ = events.send(Event::Position { index, position });
}

fn frames_to_duration(frames: u64, rate: u32) -> Duration {
    let nanos = u128::from(frames) * 1_000_000_000 / u128::from(rate);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

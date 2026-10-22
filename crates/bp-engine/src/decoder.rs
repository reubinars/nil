//! Decoder thread: opens tracks in order, packs their samples into device bytes and pushes
//! whole frames into the ring of the current stream. It runs ahead of playback by up to one
//! ring (2 s); the control thread maps the reported segments onto `frames_played`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use bp_core::{
    Capabilities, DecodeError, DeviceFormat, PathKind, PlaybackPath, SourceKind, UnsupportedReason,
};
use bp_decode::{Block, Packer, Source};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use rtrb::Producer;
use tracing::{debug, info, warn};

use crate::analysis::{AnalysisTap, analysis_rate, downmix};
use crate::negotiate::candidates;

/// How often a decoder waiting for a stream checks `cancel`.
const REPLY_POLL: Duration = Duration::from_millis(10);

/// Decoder → control.
pub(crate) enum DecoderMsg {
    /// Track `index` starts at `start_frame` of the current stream.
    Segment {
        index: usize,
        path: PathBuf,
        playback: PlaybackPath,
        duration: Option<Duration>,
        start_frame: u64,
        start_pos: Duration,
    },
    /// `at_frame`: frames in the current stream when the decoder got there (`None`: no
    /// stream open), so the event can be emitted when playback reaches it.
    Skipped {
        index: usize,
        path: PathBuf,
        reason: UnsupportedReason,
        at_frame: Option<u64>,
    },
    TrackError {
        index: usize,
        path: PathBuf,
        error: DecodeError,
        at_frame: Option<u64>,
    },
    /// The current stream (if any) has ended; open one of these, best first, for track
    /// `index`.
    NeedStream {
        index: usize,
        candidates: Vec<PlaybackPath>,
    },
    /// No more frames go into the current stream; it holds `end_frame` frames in total.
    StreamEnd { end_frame: u64 },
    /// The current stream holds enough data to start.
    Primed,
    /// End of the queue.
    Done,
}

/// Control → decoder, answering `NeedStream`.
pub(crate) enum StreamReply {
    Ready {
        producer: Producer<u8>,
        path: PlaybackPath,
    },
    NoneFit,
}

/// What the decoder plays and with which device capabilities.
pub(crate) struct Job {
    pub tracks: Vec<PathBuf>,
    pub start: usize,
    /// Position within `tracks[start]`.
    pub start_pos: Duration,
    pub caps: Capabilities,
    pub dop: bool,
    /// Receives a mono copy of every block, for the spectrum.
    pub analysis: AnalysisTap,
}

/// The thread stops because playback was cancelled or the control thread went away.
struct Cancelled;

/// The ring of the stream currently fed.
struct Out {
    format: DeviceFormat,
    /// The stream carries DoP: its silence and pause tail are DoP, so it never takes PCM
    /// (or the reverse) even when the device format is the same.
    dop: bool,
    producer: Producer<u8>,
    frame_bytes: usize,
    pushed_bytes: usize,
    prime_at: usize,
    primed: bool,
}

struct Decoder {
    ctl: Sender<DecoderMsg>,
    replies: Receiver<StreamReply>,
    cancel: Arc<AtomicBool>,
    caps: Capabilities,
    dop: bool,
    out: Option<Out>,
    /// Packer of the last track, with its path; kept across tracks when compatible so DoP
    /// markers stay continuous.
    packer: Option<(PlaybackPath, Packer)>,
    buf: Vec<u8>,
    analysis: AnalysisTap,
    /// Reused downmix buffer.
    mono: Vec<f32>,
}

pub(crate) fn run(
    job: Job,
    ctl: Sender<DecoderMsg>,
    replies: Receiver<StreamReply>,
    cancel: Arc<AtomicBool>,
) {
    let mut d = Decoder {
        ctl,
        replies,
        cancel,
        caps: job.caps,
        dop: job.dop,
        out: None,
        packer: None,
        buf: Vec::new(),
        analysis: job.analysis,
        mono: Vec::new(),
    };
    if d.run(job.tracks, job.start, job.start_pos).is_err() {
        debug!("decoder cancelled");
    }
}

impl Decoder {
    fn run(
        &mut self,
        tracks: Vec<PathBuf>,
        start: usize,
        start_pos: Duration,
    ) -> Result<(), Cancelled> {
        for (index, path) in tracks.into_iter().enumerate().skip(start) {
            self.check_cancel()?;
            let pos = if index == start {
                start_pos
            } else {
                Duration::ZERO
            };
            self.track(index, path, pos)?;
        }
        self.end_stream()?;
        self.send(DecoderMsg::Done)
    }

    fn track(&mut self, index: usize, path: PathBuf, pos: Duration) -> Result<(), Cancelled> {
        let mut src = match bp_decode::open(&path) {
            Ok(s) => s,
            Err(DecodeError::UnknownFormat) => {
                return self.skip(index, path, UnsupportedReason::UnknownFormat);
            }
            Err(DecodeError::DstUnsupported) => {
                return self.skip(index, path, UnsupportedReason::DstCompressed);
            }
            Err(error) => return self.track_error(index, path, error),
        };
        let (start_pos, skip) = if pos.is_zero() {
            (pos, 0)
        } else {
            match seek(src.as_mut(), pos) {
                Ok(Some(at)) => at,
                // Nothing left of this track: like `Next`.
                Ok(None) => return Ok(()),
                Err(error) => return self.track_error(index, path, error),
            }
        };
        let format = src.format();
        let is_dop = matches!(format.kind, SourceKind::Dsd { .. });
        let current = self
            .out
            .as_ref()
            .filter(|o| o.dop == is_dop)
            .map(|o| o.format);
        let cands = match candidates(&format, &self.caps, self.dop, current.as_ref()) {
            Ok(c) if !c.is_empty() => c,
            Ok(_) => return self.skip(index, path, UnsupportedReason::FormatNotAccepted),
            Err(reason) => return self.skip(index, path, reason),
        };

        let playback = if current == Some(cands[0].device) {
            cands[0]
        } else {
            self.end_stream()?;
            self.send(DecoderMsg::NeedStream {
                index,
                candidates: cands,
            })?;
            match self.reply()? {
                StreamReply::Ready { producer, path } => {
                    self.analysis
                        .begin(path.device.sample_rate, analysis_rate(&path.source));
                    let frame_bytes = path.device.bytes_per_frame();
                    let prime_at = producer.buffer().capacity() / 4;
                    self.out = Some(Out {
                        format: path.device,
                        dop: path.kind == PathKind::Dop,
                        producer,
                        frame_bytes,
                        pushed_bytes: 0,
                        prime_at,
                        primed: false,
                    });
                    path
                }
                StreamReply::NoneFit => {
                    let reason = if self.caps.has_channels(format.channels) {
                        UnsupportedReason::FormatNotAccepted
                    } else {
                        UnsupportedReason::ChannelsNotAccepted {
                            channels: format.channels,
                        }
                    };
                    return self.skip(index, path, reason);
                }
            }
        };
        self.select_packer(&playback)?;

        info!(index, path = %path.display(), ?playback, "decoding");
        let segment = DecoderMsg::Segment {
            index,
            path: path.clone(),
            playback,
            duration: src.duration(),
            start_frame: self.pushed_frames().expect("stream open"),
            start_pos,
        };
        // After a seek the segment is reported with its first block: a track with nothing
        // left after the seek point plays nothing, like `Next`.
        let segment = if pos.is_zero() {
            self.send(segment)?;
            None
        } else {
            Some(segment)
        };
        self.decode(src.as_mut(), index, path, segment, skip)
    }

    /// Decodes the rest of `src`, dropping its first `skip` frames.
    fn decode(
        &mut self,
        src: &mut dyn Source,
        index: usize,
        path: PathBuf,
        mut segment: Option<DecoderMsg>,
        mut skip: u64,
    ) -> Result<(), Cancelled> {
        let channels = usize::from(src.format().channels);
        let bits = match src.format().kind {
            SourceKind::Pcm { bits, .. } => u32::from(bits),
            SourceKind::Dsd { .. } => 1,
        };
        loop {
            self.check_cancel()?;
            let block = match src.next_block() {
                Ok(Some(block)) => trim(block, &mut skip, channels),
                Ok(None) => return Ok(()),
                Err(error) => return self.track_error(index, path, error),
            };
            if is_empty(&block) {
                continue;
            }
            if let Some(seg) = segment.take() {
                // Nothing pushed since `start_frame` was taken, so it is still exact.
                self.send(seg)?;
            }
            self.mono.clear();
            downmix(&block, channels, bits, &mut self.mono);
            self.analysis.write(&self.mono);
            let (_, packer) = self.packer.as_mut().expect("packer");
            packer.pack(block, &mut self.buf);
            self.push()?;
        }
    }

    /// Waits for the control thread's answer to `NeedStream`, observing `cancel`.
    fn reply(&self) -> Result<StreamReply, Cancelled> {
        loop {
            match self.replies.recv_timeout(REPLY_POLL) {
                Ok(r) => return Ok(r),
                Err(RecvTimeoutError::Timeout) => self.check_cancel()?,
                Err(RecvTimeoutError::Disconnected) => return Err(Cancelled),
            }
        }
    }

    /// Keeps the current packer when the new path packs identically, else flushes it.
    fn select_packer(&mut self, path: &PlaybackPath) -> Result<(), Cancelled> {
        let same = |p: &PlaybackPath| {
            p.device == path.device && p.kind == path.kind && p.source.kind == path.source.kind
        };
        if self.packer.as_ref().is_some_and(|(p, _)| same(p)) {
            return Ok(());
        }
        if let Some((_, mut old)) = self.packer.take() {
            old.finish(&mut self.buf);
            self.push()?;
        }
        self.packer = Some((*path, Packer::new(path)));
        Ok(())
    }

    /// Flushes the packer into the current stream and reports its end.
    fn end_stream(&mut self) -> Result<(), Cancelled> {
        if let Some((_, mut packer)) = self.packer.take() {
            packer.finish(&mut self.buf);
        }
        if self.out.is_none() {
            self.buf.clear();
            return Ok(());
        }
        self.push()?;
        let out = self.out.take().expect("stream open");
        if !out.primed {
            self.send(DecoderMsg::Primed)?;
        }
        self.send(DecoderMsg::StreamEnd {
            end_frame: (out.pushed_bytes / out.frame_bytes) as u64,
        })
    }

    /// Pushes `buf` (whole frames) into the ring, waiting while it is full.
    fn push(&mut self) -> Result<(), Cancelled> {
        let mut done = 0;
        while done < self.buf.len() {
            let out = self.out.as_mut().expect("stream open");
            let free = out.producer.slots() / out.frame_bytes * out.frame_bytes;
            if free == 0 {
                self.check_cancel()?;
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            let n = free.min(self.buf.len() - done);
            out.producer
                .push_entire_slice(&self.buf[done..done + n])
                .expect("free slots checked");
            done += n;
            out.pushed_bytes += n;
            if !out.primed && out.pushed_bytes >= out.prime_at {
                out.primed = true;
                self.send(DecoderMsg::Primed)?;
            }
        }
        self.buf.clear();
        Ok(())
    }

    fn skip(
        &mut self,
        index: usize,
        path: PathBuf,
        reason: UnsupportedReason,
    ) -> Result<(), Cancelled> {
        info!(index, path = %path.display(), %reason, "skipping track");
        self.send(DecoderMsg::Skipped {
            index,
            path,
            reason,
            at_frame: self.pushed_frames(),
        })
    }

    fn track_error(
        &mut self,
        index: usize,
        path: PathBuf,
        error: DecodeError,
    ) -> Result<(), Cancelled> {
        warn!(index, path = %path.display(), %error, "track error");
        self.send(DecoderMsg::TrackError {
            index,
            path,
            error,
            at_frame: self.pushed_frames(),
        })
    }

    /// Frames pushed into the current stream so far.
    fn pushed_frames(&self) -> Option<u64> {
        self.out
            .as_ref()
            .map(|o| (o.pushed_bytes / o.frame_bytes) as u64)
    }

    fn send(&self, msg: DecoderMsg) -> Result<(), Cancelled> {
        self.ctl.send(msg).map_err(|_| Cancelled)
    }

    fn check_cancel(&self) -> Result<(), Cancelled> {
        if self.cancel.load(Ordering::Acquire) {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Seeks `src` to `pos`. Returns the exact start position and the number of frames to drop
/// first (a PCM source may land earlier, on a packet boundary), or `None` when nothing of
/// the track is left.
fn seek(src: &mut dyn Source, pos: Duration) -> Result<Option<(Duration, u64)>, DecodeError> {
    let duration = src.duration();
    let past_end = |p: Duration| duration.is_some_and(|d| p >= d);
    if past_end(pos) {
        return Ok(None);
    }
    let reached = src.seek(pos)?;
    if past_end(reached) {
        return Ok(None);
    }
    let format = src.format();
    if reached >= pos || !matches!(format.kind, SourceKind::Pcm { .. }) {
        return Ok(Some((reached, 0)));
    }
    const NANOS: u128 = 1_000_000_000;
    let rate = u128::from(format.sample_rate);
    let target = pos.as_nanos() * rate / NANOS;
    let from = (reached.as_nanos() * rate + NANOS / 2) / NANOS;
    let start = Duration::from_nanos(u64::try_from(target * NANOS / rate).unwrap_or(u64::MAX));
    Ok(Some((
        start,
        u64::try_from(target.saturating_sub(from)).unwrap_or(0),
    )))
}

/// Drops up to `*skip` leading frames of a PCM block.
fn trim<'a>(block: Block<'a>, skip: &mut u64, channels: usize) -> Block<'a> {
    fn cut<'a, T>(samples: &'a [T], skip: &mut u64, channels: usize) -> &'a [T] {
        let frames = usize::try_from(*skip)
            .unwrap_or(usize::MAX)
            .min(samples.len() / channels);
        *skip -= frames as u64;
        &samples[frames * channels..]
    }
    if *skip == 0 {
        return block;
    }
    match block {
        Block::Pcm(s) => Block::Pcm(cut(s, skip, channels)),
        Block::Float(s) => Block::Float(cut(s, skip, channels)),
        dsd @ Block::Dsd(_) => dsd,
    }
}

fn is_empty(block: &Block<'_>) -> bool {
    match block {
        Block::Pcm(s) => s.is_empty(),
        Block::Float(s) => s.is_empty(),
        Block::Dsd(s) => s.is_empty(),
    }
}

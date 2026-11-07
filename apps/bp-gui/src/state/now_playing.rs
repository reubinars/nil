//! Now-playing state, reduced from player commands and events.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use bp_core::{PathKind, PlaybackPath};
use bp_engine::Event;

use crate::backend::{PlayerCommand, PlayerEvent, QueueItem};

/// How long a skip notice stays up.
pub(crate) const NOTICE_TIME: Duration = Duration::from_secs(4);
/// `Previous` past this position restarts the current track.
const RESTART_AFTER: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Default, PartialEq)]
pub enum Badge {
    #[default]
    Idle,
    /// A command is (re)starting the stream.
    Opening,
    BitPerfect,
    /// A lossy source: what reaches the DAC is decoded, not the original samples.
    NotBitPerfect {
        cause: String,
    },
}

#[derive(Debug, Default)]
pub struct NowPlaying {
    pub queue: Vec<QueueItem>,
    pub index: Option<usize>,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub path: Option<PlaybackPath>,
    pub paused: bool,
    pub badge: Badge,
    pub underruns: u64,
    /// Text and its expiry on the player clock.
    pub notice: Option<(String, Duration)>,
    pub fatal: Option<String>,
    pub track_errors: HashMap<PathBuf, String>,
    /// Inserts sent while playing, waiting for the engine to say where they went. `None`
    /// once a Play or Stop made the answer stale, as for `pending_rearranges`. Dropped
    /// when playback ends: the engine answers an insert only while it is playing.
    pending_inserts: VecDeque<Option<Vec<QueueItem>>>,
    /// Rearranges sent, one per engine answer: `(from, rows replaced, items)`. `None` once
    /// a Play or Stop made the answer stale: it still pops its own entry but changes
    /// nothing.
    pending_rearranges: VecDeque<Option<(usize, usize, Vec<QueueItem>)>>,
}

impl NowPlaying {
    /// Records a command as it is sent to the player.
    pub fn on_command(&mut self, cmd: &PlayerCommand) {
        match cmd {
            PlayerCommand::Play { items, start } => {
                self.void_pending();
                self.queue = items.clone();
                self.index = Some(*start);
                self.position = Duration::ZERO;
                self.duration = items.get(*start).map(|i| i.duration);
                self.paused = false;
                self.opening();
            }
            PlayerCommand::Append { items } => {
                // After a fatal error the player has no playback, so it starts afresh.
                if self.index.is_some() && self.fatal.is_none() {
                    self.queue.extend(items.iter().cloned());
                } else if !items.is_empty() {
                    // Nothing playing: the player starts them from the first, as for Play.
                    self.queue = items.clone();
                    self.index = Some(0);
                    self.position = Duration::ZERO;
                    self.duration = items.first().map(|i| i.duration);
                    self.paused = false;
                    self.opening();
                }
            }
            PlayerCommand::Insert { items } => {
                if self.index.is_some() && self.fatal.is_none() {
                    if !items.is_empty() {
                        self.pending_inserts.push_back(Some(items.clone()));
                    }
                } else if !items.is_empty() {
                    // Not cleared: inserts a Stop made stale are still answered, in order.
                    self.void_pending();
                    self.queue = items.clone();
                    self.index = Some(0);
                    self.position = Duration::ZERO;
                    self.duration = items.first().map(|i| i.duration);
                    self.paused = false;
                    self.opening();
                }
            }
            // The engine's answer decides what happens to the queue.
            PlayerCommand::Rearrange {
                from,
                expected,
                items,
            } => {
                self.pending_rearranges
                    .push_back(Some((*from, expected.len(), items.clone())));
            }
            // These restart a stream; with nothing playing the player ignores them.
            PlayerCommand::Next => {
                if self.index.is_some() {
                    self.opening();
                }
            }
            // A stopped player (after a fatal error) only switches devices; Retry stays.
            // Re-probe is only offered for the selected device, which the player reopens
            // after probing.
            PlayerCommand::SelectDevice { .. } | PlayerCommand::Reprobe { .. } => {
                if self.index.is_some() && self.fatal.is_none() {
                    self.badge = Badge::Opening;
                }
            }
            PlayerCommand::Previous => {
                if self.index.is_some() {
                    if self.position > RESTART_AFTER {
                        self.position = Duration::ZERO;
                    }
                    self.opening();
                }
            }
            PlayerCommand::Seek { position } => {
                if self.index.is_some() {
                    self.position = *position;
                    self.opening();
                }
            }
            PlayerCommand::Stop => {
                self.void_pending();
                self.badge = Badge::Idle;
                self.index = None;
                self.paused = false;
            }
            // The stream in progress is untouched by these.
            PlayerCommand::Pause
            | PlayerCommand::Resume
            | PlayerCommand::RefreshDevices
            | PlayerCommand::SetDop { .. } => {}
        }
    }

    /// The queue the pending inserts and rearranges were for is gone: their answers will
    /// change nothing.
    fn void_pending(&mut self) {
        for entry in &mut self.pending_inserts {
            *entry = None;
        }
        for entry in &mut self.pending_rearranges {
            *entry = None;
        }
    }

    /// Playback ended: inserts not answered yet never will be, and its rearranges are stale.
    fn end_pending(&mut self) {
        self.pending_inserts.clear();
        for entry in &mut self.pending_rearranges {
            *entry = None;
        }
    }

    /// True if the engine's next `Inserted` answers a live insert: one that still splices.
    pub fn insert_is_live(&self) -> bool {
        matches!(self.pending_inserts.front(), Some(Some(_)))
    }

    /// True while a live insert or rearrange awaits the engine's answer.
    pub fn settling(&self) -> bool {
        self.pending_inserts.iter().any(Option::is_some)
            || self.pending_rearranges.iter().any(Option::is_some)
    }

    fn opening(&mut self) {
        self.badge = Badge::Opening;
        self.fatal = None;
    }

    /// Applies a player event received at `now` on the player clock.
    pub fn apply(&mut self, ev: &PlayerEvent, now: Duration) {
        let PlayerEvent::Engine(ev) = ev else {
            return;
        };
        match ev {
            // Idempotent: the engine re-emits this for the same track after a seek.
            Event::TrackStarted {
                index,
                playback,
                duration,
                ..
            } => {
                if self.index != Some(*index) {
                    self.position = Duration::ZERO;
                }
                self.index = Some(*index);
                self.path = Some(*playback);
                self.duration = *duration;
                self.paused = false;
                self.badge = if playback.source.codec.is_lossy() {
                    Badge::NotBitPerfect {
                        cause: format!(
                            "{} is lossy: decoded and rounded to 24-bit",
                            playback.source.codec
                        ),
                    }
                } else {
                    match playback.kind {
                        PathKind::Native | PathKind::Padded { .. } | PathKind::Dop => {
                            Badge::BitPerfect
                        }
                    }
                };
            }
            Event::Position { index, position } => {
                if self.index == Some(*index) {
                    self.position = *position;
                }
            }
            Event::TrackSkipped {
                index,
                path,
                reason,
            } => {
                let title = match self.queue.get(*index) {
                    Some(item) => item.title.clone(),
                    None => path.display().to_string(),
                };
                self.notice = Some((format!("Skipped: {title} — {reason}"), now + NOTICE_TIME));
            }
            Event::Paused => self.paused = true,
            Event::Resumed => self.paused = false,
            Event::QueueFinished => {
                self.end_pending();
                self.badge = Badge::Idle;
                self.index = None;
                self.paused = false;
            }
            Event::Inserted { at, .. } => {
                if let Some(Some(items)) = self.pending_inserts.pop_front() {
                    let at = (*at).min(self.queue.len());
                    self.queue.splice(at..at, items);
                }
            }
            Event::DevicesChanged => {}
            Event::Rearranged { .. } => {
                // Only the rows the engine checked: tracks appended since stay after them.
                if let Some(Some((from, len, items))) = self.pending_rearranges.pop_front() {
                    let end = (from + len).min(self.queue.len());
                    let from = from.min(end);
                    self.queue.splice(from..end, items);
                }
            }
            Event::RearrangeRefused { .. } => {
                self.pending_rearranges.pop_front();
            }
            Event::Underrun { total } => self.underruns = *total,
            Event::TrackError { path, error, .. } => {
                self.track_errors.insert(path.clone(), error.to_string());
            }
            Event::Error(e) => {
                self.end_pending();
                self.fatal = Some(e.to_string());
                self.badge = Badge::Idle;
                self.paused = false;
            }
        }
    }

    /// The notice, unless it has expired by `now`.
    pub fn notice_text(&self, now: Duration) -> Option<&str> {
        self.notice
            .as_ref()
            .filter(|(_, expiry)| now < *expiry)
            .map(|(text, _)| text.as_str())
    }

    pub fn is_playing(&self) -> bool {
        self.index.is_some() && !self.paused && self.fatal.is_none()
    }

    /// Replays the queue from the current track after a fatal error.
    pub fn retry_command(&self) -> Option<PlayerCommand> {
        self.fatal.as_ref()?;
        Some(PlayerCommand::Play {
            items: self.queue.clone(),
            start: self.index.unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use bp_core::{DeviceFormat, EngineError, SampleEncoding, UnsupportedReason};

    use super::*;
    use crate::state::support::testutil::pcm;

    fn t(secs: f64) -> Duration {
        Duration::from_secs_f64(secs)
    }

    fn items(n: usize) -> Vec<QueueItem> {
        (0..n)
            .map(|i| QueueItem {
                path: PathBuf::from(format!("/music/{i}.flac")),
                source: pcm(44_100, 16),
                duration: Duration::from_secs(120),
                title: if i == 0 { "x".into() } else { format!("t{i}") },
            })
            .collect()
    }

    fn play(n: usize, start: usize) -> PlayerCommand {
        PlayerCommand::Play {
            items: items(n),
            start,
        }
    }

    #[test]
    fn append_extends_queue_without_moving_index() {
        let mut np = NowPlaying::default();
        np.on_command(&play(1, 0));
        np.on_command(&PlayerCommand::Append { items: items(2) });
        assert_eq!(np.queue.len(), 3);
        assert_eq!(np.index, Some(0));
    }

    #[test]
    fn append_when_idle_starts_the_queue() {
        let mut np = NowPlaying::default();
        np.on_command(&PlayerCommand::Append { items: items(2) });
        assert_eq!(np.queue.len(), 2);
        assert_eq!(np.index, Some(0));
        assert_eq!(np.badge, Badge::Opening);
    }

    #[test]
    fn append_after_a_fatal_error_replaces_the_queue() {
        let mut np = NowPlaying::default();
        np.on_command(&play(3, 1));
        np.apply(&engine(Event::Error(EngineError::NoDevice)), t(1.0));
        assert!(np.fatal.is_some());
        np.on_command(&PlayerCommand::Append { items: items(2) });
        assert_eq!(np.queue.len(), 2);
        assert_eq!(np.index, Some(0));
        assert!(np.fatal.is_none());
    }

    #[test]
    fn append_after_queue_finished_replaces_the_queue() {
        let mut np = NowPlaying::default();
        np.on_command(&play(3, 0));
        np.apply(&engine(Event::QueueFinished), t(2.0));
        np.on_command(&PlayerCommand::Append { items: items(2) });
        assert_eq!(np.queue.len(), 2);
        assert_eq!(np.index, Some(0));
    }

    #[test]
    fn append_of_nothing_changes_nothing() {
        let mut np = NowPlaying::default();
        np.on_command(&PlayerCommand::Append { items: vec![] });
        assert!(np.queue.is_empty() && np.index.is_none());
        np.on_command(&play(2, 1));
        np.on_command(&PlayerCommand::Append { items: vec![] });
        assert_eq!(np.queue.len(), 2);
        assert_eq!(np.index, Some(1));
    }

    #[test]
    fn insert_splices_at_the_reported_index() {
        let mut np = NowPlaying::default();
        np.on_command(&play(3, 0));
        let mut x = items(1);
        x[0].title = "x".into();
        np.on_command(&PlayerCommand::Insert { items: x });
        assert_eq!(np.queue.len(), 3);
        np.apply(&engine(Event::Inserted { at: 2, count: 1 }), t(1.0));
        assert_eq!(np.queue.len(), 4);
        assert_eq!(np.queue[2].title, "x");
        assert_eq!(np.queue[3].title, "t2");
    }

    #[test]
    fn insert_when_stopped_starts_a_fresh_queue() {
        let mut np = NowPlaying::default();
        np.on_command(&PlayerCommand::Insert { items: items(2) });
        assert_eq!(np.queue.len(), 2);
        assert_eq!(np.index, Some(0));
        assert_eq!(np.badge, Badge::Opening);
    }

    #[test]
    fn play_between_insert_and_inserted_drops_the_pending_insert() {
        let mut np = NowPlaying::default();
        np.on_command(&play(3, 0));
        np.on_command(&PlayerCommand::Insert { items: items(1) });
        np.on_command(&play(2, 0));
        np.apply(&engine(Event::Inserted { at: 1, count: 1 }), t(1.0));
        assert_eq!(np.queue.len(), 2);
    }

    #[test]
    fn stale_inserted_after_play_does_not_move_a_newer_insert() {
        let mut np = NowPlaying::default();
        np.on_command(&play(3, 0));
        let mut a = items(1);
        a[0].title = "a".into();
        np.on_command(&PlayerCommand::Insert { items: a });
        let mut r = items(3);
        for (i, it) in r.iter_mut().enumerate() {
            it.title = format!("r{i}");
        }
        np.on_command(&PlayerCommand::Play { items: r, start: 0 });
        let mut b = items(1);
        b[0].title = "b".into();
        np.on_command(&PlayerCommand::Insert { items: b });
        // The answer to the insert sent before the Play.
        np.apply(&engine(Event::Inserted { at: 1, count: 1 }), t(1.0));
        assert_eq!(titles(&np), ["r0", "r1", "r2"]);
        assert!(np.settling());
        np.apply(&engine(Event::Inserted { at: 2, count: 1 }), t(1.0));
        assert_eq!(titles(&np), ["r0", "r1", "b", "r2"]);
        assert!(!np.settling());
    }

    fn titled(prefix: &str, n: usize) -> Vec<QueueItem> {
        let mut v = items(n);
        for (i, it) in v.iter_mut().enumerate() {
            it.title = format!("{prefix}{i}");
        }
        v
    }

    #[test]
    fn inserts_unanswered_when_the_queue_finishes_are_dropped() {
        let mut np = NowPlaying::default();
        np.on_command(&play(3, 0));
        np.on_command(&PlayerCommand::Insert { items: items(1) });
        // The engine had finished: it plays that insert afresh and never answers it.
        np.apply(&engine(Event::QueueFinished), t(1.0));
        assert!(!np.settling());
        np.on_command(&PlayerCommand::Play {
            items: titled("r", 3),
            start: 0,
        });
        np.on_command(&PlayerCommand::Insert {
            items: titled("b", 1),
        });
        np.apply(&engine(Event::Inserted { at: 1, count: 1 }), t(2.0));
        assert_eq!(titles(&np), ["r0", "b0", "r1", "r2"]);
        assert!(!np.settling());
    }

    #[test]
    fn inserts_unanswered_at_a_fatal_error_are_dropped() {
        let mut np = NowPlaying::default();
        np.on_command(&play(3, 0));
        np.on_command(&PlayerCommand::Insert { items: items(1) });
        np.apply(&engine(Event::Error(EngineError::NoDevice)), t(1.0));
        assert!(!np.settling());
        np.on_command(&PlayerCommand::Play {
            items: titled("r", 3),
            start: 0,
        });
        np.on_command(&PlayerCommand::Insert {
            items: titled("b", 1),
        });
        np.apply(&engine(Event::Inserted { at: 1, count: 1 }), t(2.0));
        assert_eq!(titles(&np), ["r0", "b0", "r1", "r2"]);
    }

    #[test]
    fn insert_after_stop_keeps_the_stale_answers_in_line() {
        let mut np = NowPlaying::default();
        np.on_command(&play(3, 1));
        np.on_command(&PlayerCommand::Insert { items: items(1) });
        np.on_command(&PlayerCommand::Stop);
        np.on_command(&PlayerCommand::Insert {
            items: titled("b", 2),
        });
        np.on_command(&PlayerCommand::Insert {
            items: titled("c", 1),
        });
        // The engine answered the first insert before it stopped.
        np.apply(&engine(Event::Inserted { at: 2, count: 1 }), t(1.0));
        assert_eq!(titles(&np), ["b0", "b1"]);
        assert!(np.settling());
        np.apply(&engine(Event::Inserted { at: 1, count: 1 }), t(1.0));
        assert_eq!(titles(&np), ["b0", "c0", "b1"]);
        assert!(!np.settling());
    }

    #[test]
    fn inserted_with_nothing_pending_is_ignored() {
        let mut np = NowPlaying::default();
        np.on_command(&play(2, 0));
        np.apply(&engine(Event::Inserted { at: 1, count: 1 }), t(1.0));
        assert_eq!(np.queue.len(), 2);
    }

    fn titles(np: &NowPlaying) -> Vec<String> {
        np.queue.iter().map(|i| i.title.clone()).collect()
    }

    fn rearrange(from: usize, expected: Vec<QueueItem>, items: Vec<QueueItem>) -> PlayerCommand {
        PlayerCommand::Rearrange {
            from,
            expected,
            items,
        }
    }

    #[test]
    fn rearranged_splices_from_from() {
        let mut np = NowPlaying::default();
        np.on_command(&play(4, 0));
        let q = np.queue.clone();
        np.on_command(&rearrange(1, q[1..].to_vec(), vec![q[3].clone()]));
        assert_eq!(titles(&np), ["x", "t1", "t2", "t3"]);
        np.apply(&engine(Event::Rearranged { from: 1 }), t(1.0));
        assert_eq!(titles(&np), ["x", "t3"]);
    }

    #[test]
    fn rearranged_keeps_tracks_appended_after_it_was_sent() {
        let mut np = NowPlaying::default();
        np.on_command(&play(4, 0));
        let q = np.queue.clone();
        np.on_command(&rearrange(1, q[1..].to_vec(), vec![q[3].clone()]));
        let mut y = items(1);
        y[0].title = "y".into();
        np.on_command(&PlayerCommand::Append { items: y });
        np.apply(&engine(Event::Rearranged { from: 1 }), t(1.0));
        assert_eq!(titles(&np), ["x", "t3", "y"]);
    }

    #[test]
    fn refused_leaves_the_queue() {
        let mut np = NowPlaying::default();
        np.on_command(&play(4, 0));
        let q = np.queue.clone();
        np.on_command(&rearrange(1, q[1..].to_vec(), vec![q[3].clone()]));
        np.apply(
            &engine(Event::RearrangeRefused {
                from: 1,
                reason: bp_engine::RearrangeRefusal::Changed,
            }),
            t(1.0),
        );
        assert_eq!(titles(&np), ["x", "t1", "t2", "t3"]);
        assert!(!np.settling());
    }

    #[test]
    fn inserts_and_rearranges_follow_their_events() {
        let mut np = NowPlaying::default();
        np.on_command(&play(4, 0));
        let q = np.queue.clone();
        let mut x = items(1);
        x[0].title = "y".into();
        np.on_command(&rearrange(1, q[1..].to_vec(), vec![q[3].clone()]));
        np.on_command(&PlayerCommand::Insert { items: x });
        np.apply(&engine(Event::Rearranged { from: 1 }), t(1.0));
        assert_eq!(titles(&np), ["x", "t3"]);
        assert!(np.settling());
        np.apply(&engine(Event::Inserted { at: 1, count: 1 }), t(1.0));
        assert_eq!(titles(&np), ["x", "y", "t3"]);
    }

    #[test]
    fn stale_rearranged_after_play_does_not_touch_the_new_queue() {
        let mut np = NowPlaying::default();
        np.on_command(&play(4, 0));
        let q = np.queue.clone();
        np.on_command(&rearrange(1, q[1..].to_vec(), vec![q[3].clone()]));
        let mut r = items(3);
        for (i, it) in r.iter_mut().enumerate() {
            it.title = format!("r{i}");
        }
        np.on_command(&PlayerCommand::Play {
            items: r.clone(),
            start: 0,
        });
        np.on_command(&rearrange(1, r[1..].to_vec(), vec![r[2].clone()]));
        np.apply(&engine(Event::Rearranged { from: 1 }), t(1.0));
        assert_eq!(titles(&np), ["r0", "r1", "r2"]);
        np.apply(&engine(Event::Rearranged { from: 1 }), t(1.0));
        assert_eq!(titles(&np), ["r0", "r2"]);
    }

    #[test]
    fn settling_while_anything_is_pending() {
        let mut np = NowPlaying::default();
        np.on_command(&play(3, 0));
        assert!(!np.settling());
        np.on_command(&PlayerCommand::Insert { items: items(1) });
        assert!(np.settling());
        np.apply(&engine(Event::Inserted { at: 1, count: 1 }), t(1.0));
        assert!(!np.settling());
        let q = np.queue.clone();
        np.on_command(&rearrange(1, q[1..].to_vec(), vec![]));
        assert!(np.settling());
        np.on_command(&PlayerCommand::Stop);
        assert!(!np.settling());
    }

    fn engine(ev: Event) -> PlayerEvent {
        PlayerEvent::Engine(ev)
    }

    fn started(index: usize, kind: PathKind) -> PlayerEvent {
        engine(Event::TrackStarted {
            index,
            path: PathBuf::from(format!("/music/{index}.flac")),
            playback: PlaybackPath {
                source: pcm(44_100, 16),
                device: DeviceFormat {
                    sample_rate: 44_100,
                    channels: 2,
                    encoding: SampleEncoding::Int {
                        container: 24,
                        valid: 24,
                    },
                },
                kind,
            },
            duration: Some(Duration::from_secs(120)),
        })
    }

    fn position(index: usize, secs: f64) -> PlayerEvent {
        engine(Event::Position {
            index,
            position: t(secs),
        })
    }

    #[test]
    fn badge_sequence() {
        let mut np = NowPlaying::default();
        np.on_command(&play(2, 0));
        assert_eq!(np.badge, Badge::Opening);
        np.apply(&started(0, PathKind::Padded { pad_bits: 8 }), t(0.3));
        assert_eq!(np.badge, Badge::BitPerfect);
        np.apply(&engine(Event::QueueFinished), t(2.0));
        assert_eq!(np.badge, Badge::Idle);
        assert_eq!(np.index, None);
    }

    #[test]
    fn lossy_track_is_not_bit_perfect() {
        let mut np = NowPlaying::default();
        np.on_command(&play(1, 0));
        let mut ev = started(0, PathKind::Native);
        if let PlayerEvent::Engine(Event::TrackStarted { playback, .. }) = &mut ev {
            playback.source.codec = bp_core::Codec::Aac;
        }
        np.apply(&ev, t(0.3));
        assert_eq!(
            np.badge,
            Badge::NotBitPerfect {
                cause: "AAC is lossy: decoded and rounded to 24-bit".into()
            }
        );
    }

    #[test]
    fn dop_and_native_are_bit_perfect() {
        for kind in [PathKind::Native, PathKind::Dop] {
            let mut np = NowPlaying::default();
            np.on_command(&play(1, 0));
            np.apply(&started(0, kind), t(0.3));
            assert_eq!(np.badge, Badge::BitPerfect);
            assert!(np.is_playing());
        }
    }

    #[test]
    fn stop_goes_idle() {
        let mut np = NowPlaying::default();
        np.on_command(&play(2, 0));
        np.apply(&started(0, PathKind::Native), t(0.3));
        np.on_command(&PlayerCommand::Stop);
        assert_eq!(np.badge, Badge::Idle);
        assert_eq!(np.index, None);
        assert!(!np.is_playing());
    }

    #[test]
    fn skip_notice_expires_after_4s() {
        let mut np = NowPlaying::default();
        np.on_command(&play(2, 0));
        let reason = UnsupportedReason::DopDisabled;
        np.apply(
            &engine(Event::TrackSkipped {
                index: 0,
                path: PathBuf::from("/music/0.flac"),
                reason: reason.clone(),
            }),
            t(10.0),
        );
        let expected = format!("Skipped: x — {reason}");
        assert_eq!(np.notice_text(t(13.9)), Some(expected.as_str()));
        assert_eq!(np.notice_text(t(14.0)), None);
    }

    #[test]
    fn fatal_error_then_retry() {
        let mut np = NowPlaying::default();
        np.on_command(&play(3, 0));
        np.apply(&started(1, PathKind::Native), t(0.3));
        np.apply(&engine(Event::Error(EngineError::NoDevice)), t(1.0));
        assert_eq!(np.fatal.as_deref(), Some("no audio device available"));
        assert_eq!(np.badge, Badge::Idle);
        assert!(!np.is_playing());
        assert_eq!(
            np.retry_command(),
            Some(PlayerCommand::Play {
                items: items(3),
                start: 1
            })
        );
        np.on_command(&np.retry_command().unwrap());
        assert_eq!(np.fatal, None);
        assert_eq!(np.retry_command(), None);
    }

    #[test]
    fn select_device_after_fatal_keeps_retry() {
        let mut np = NowPlaying::default();
        np.on_command(&play(3, 0));
        np.apply(&started(1, PathKind::Native), t(0.3));
        np.apply(&engine(Event::Error(EngineError::NoDevice)), t(1.0));
        np.on_command(&PlayerCommand::SelectDevice {
            id: bp_core::DeviceId("dac-b".into()),
        });
        assert_eq!(np.badge, Badge::Idle);
        assert!(np.fatal.is_some());
        assert!(np.retry_command().is_some());
    }

    #[test]
    fn track_errors_and_underruns_are_recorded() {
        let mut np = NowPlaying::default();
        np.on_command(&play(2, 0));
        np.apply(
            &engine(Event::TrackError {
                index: 0,
                path: PathBuf::from("/music/0.flac"),
                error: bp_core::DecodeError::Corrupt("bad frame".into()),
            }),
            t(1.0),
        );
        np.apply(&engine(Event::Underrun { total: 4 }), t(1.0));
        assert_eq!(
            np.track_errors
                .get(&PathBuf::from("/music/0.flac"))
                .map(String::as_str),
            Some("corrupt file: bad frame")
        );
        assert_eq!(np.underruns, 4);
    }

    #[test]
    fn pause_and_resume() {
        let mut np = NowPlaying::default();
        np.on_command(&play(1, 0));
        np.apply(&started(0, PathKind::Native), t(0.3));
        np.apply(&engine(Event::Paused), t(1.0));
        assert!(np.paused);
        assert!(!np.is_playing());
        np.apply(&engine(Event::Resumed), t(2.0));
        assert!(!np.paused);
    }

    #[test]
    fn stale_position_is_ignored() {
        let mut np = NowPlaying::default();
        np.on_command(&play(2, 0));
        np.apply(&started(0, PathKind::Native), t(0.3));
        np.apply(&position(0, 1.0), t(1.3));
        np.apply(&started(1, PathKind::Native), t(2.0));
        assert_eq!(np.position, Duration::ZERO);
        np.apply(&position(0, 1.25), t(2.1));
        assert_eq!(np.position, Duration::ZERO);
        np.apply(&position(1, 0.25), t(2.25));
        assert_eq!(np.position, t(0.25));
    }

    #[test]
    fn seek_restart_does_not_reset_position() {
        let mut np = NowPlaying::default();
        np.on_command(&play(2, 0));
        np.apply(&started(0, PathKind::Native), t(0.3));
        np.apply(&position(0, 5.0), t(5.3));
        np.on_command(&PlayerCommand::Seek { position: t(60.0) });
        assert_eq!(np.position, t(60.0));
        assert_eq!(np.badge, Badge::Opening);
        np.apply(&started(0, PathKind::Native), t(5.6));
        assert_eq!(np.position, t(60.0));
        assert_eq!(np.badge, Badge::BitPerfect);
    }

    #[test]
    fn previous_past_3s_restarts_at_zero() {
        let mut np = NowPlaying::default();
        np.on_command(&play(2, 0));
        np.apply(&started(1, PathKind::Native), t(0.3));
        np.apply(&position(1, 4.0), t(4.3));
        np.on_command(&PlayerCommand::Previous);
        assert_eq!(np.position, Duration::ZERO);
        np.apply(&started(1, PathKind::Native), t(4.6));
        assert_eq!(np.position, Duration::ZERO);

        np.apply(&position(1, 2.0), t(6.6));
        np.on_command(&PlayerCommand::Previous);
        assert_eq!(np.position, t(2.0));
    }

    #[test]
    fn reprobe_while_playing_shows_opening_until_the_track_restarts() {
        let reprobe = PlayerCommand::Reprobe {
            id: bp_core::DeviceId("dac-a".into()),
        };
        let mut np = NowPlaying::default();
        np.on_command(&play(2, 0));
        np.apply(&started(0, PathKind::Native), t(0.3));
        np.apply(&position(0, 1.0), t(1.3));
        np.on_command(&reprobe);
        assert_eq!(np.badge, Badge::Opening);
        np.apply(&started(0, PathKind::Native), t(1.6));
        assert_eq!(np.badge, Badge::BitPerfect);
        assert_eq!(np.position, t(1.0));

        // Idle or stopped by a fatal error: nothing restarts.
        let mut idle = NowPlaying::default();
        idle.on_command(&reprobe);
        assert_eq!(idle.badge, Badge::Idle);
        np.apply(&engine(Event::Error(EngineError::NoDevice)), t(2.0));
        np.on_command(&reprobe);
        assert_eq!(np.badge, Badge::Idle);
    }

    #[test]
    fn set_dop_while_playing_leaves_the_stream_alone() {
        let mut np = NowPlaying::default();
        np.on_command(&play(2, 0));
        np.apply(&started(0, PathKind::Dop), t(0.3));
        np.on_command(&PlayerCommand::SetDop {
            id: bp_core::DeviceId("dac-a".into()),
            enabled: false,
        });
        assert_eq!(np.badge, Badge::BitPerfect);
    }

    #[test]
    fn commands_while_idle_do_not_show_opening() {
        let mut np = NowPlaying::default();
        np.on_command(&PlayerCommand::SelectDevice {
            id: bp_core::DeviceId("dac-b".into()),
        });
        np.on_command(&PlayerCommand::Next);
        assert_eq!(np.badge, Badge::Idle);
    }
}

//! Now-playing state, reduced from player commands and events.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use bp_core::{PathKind, PlaybackPath};
use bp_engine::Event;

use crate::backend::{PlayerCommand, PlayerEvent, QueueItem};

/// How long a skip notice stays up.
const NOTICE_TIME: Duration = Duration::from_secs(4);
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
}

impl NowPlaying {
    /// Records a command as it is sent to the player.
    pub fn on_command(&mut self, cmd: &PlayerCommand) {
        match cmd {
            PlayerCommand::Play { items, start } => {
                self.queue = items.clone();
                self.index = Some(*start);
                self.position = Duration::ZERO;
                self.duration = items.get(*start).map(|i| i.duration);
                self.paused = false;
                self.opening();
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
                self.badge = Badge::Idle;
                self.index = None;
                self.paused = false;
            }
            PlayerCommand::Pause | PlayerCommand::Resume | PlayerCommand::RefreshDevices => {}
        }
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
                self.badge = Badge::Idle;
                self.index = None;
                self.paused = false;
            }
            Event::DevicesChanged => {}
            Event::Underrun { total } => self.underruns = *total,
            Event::TrackError { path, error, .. } => {
                self.track_errors.insert(path.clone(), error.to_string());
            }
            Event::Error(e) => {
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
    fn commands_while_idle_do_not_show_opening() {
        let mut np = NowPlaying::default();
        np.on_command(&PlayerCommand::SelectDevice {
            id: bp_core::DeviceId("dac-b".into()),
        });
        np.on_command(&PlayerCommand::Next);
        assert_eq!(np.badge, Badge::Idle);
    }
}

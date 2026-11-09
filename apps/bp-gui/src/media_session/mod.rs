//! nil's entry in Windows' media session (SMTC): what is playing goes out, so the
//! volume flyout, media keys and apps that follow the current session see it; their
//! play, pause, next, previous and seek come back as player commands.
//!
//! This module is the platform-free part. `smtc` (Windows only) applies the updates and
//! sends the button presses.

use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};

use crate::backend::PlayerCommand;
use crate::state::now_playing::NowPlaying;

#[cfg(windows)]
pub mod smtc;

/// How far the position may drift from where Windows would put it before it is sent
/// again. Windows extrapolates the position itself while playing.
const DRIFT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Playing,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The album's cached thumbnail, once it is on disk.
    pub art: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeline {
    pub position: Duration,
    pub duration: Duration,
}

/// What nil shows Windows in one frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub status: Status,
    pub track: Option<Track>,
    pub timeline: Option<Timeline>,
}

/// One change for Windows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    Track(Option<Track>),
    Timeline(Timeline),
    Status(Status),
}

/// A press from Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Play,
    Pause,
    Next,
    Previous,
    Seek(Duration),
}

/// The queue entry that is playing or paused; none after a fatal error.
fn active(np: &NowPlaying) -> Option<usize> {
    np.index.filter(|_| np.fatal.is_none())
}

pub fn status(np: &NowPlaying) -> Status {
    match (active(np), np.paused) {
        (None, _) => Status::Stopped,
        (Some(_), true) => Status::Paused,
        (Some(_), false) => Status::Playing,
    }
}

/// The current track's position and length; `None` when stopped.
pub fn timeline(np: &NowPlaying) -> Option<Timeline> {
    let item = np.queue.get(active(np)?)?;
    let duration = np.duration.unwrap_or(item.duration);
    Some(Timeline {
        position: np.position.min(duration),
        duration,
    })
}

/// The command a press asks for, if it means anything now. Play only resumes and
/// Pause only pauses: Windows sends whichever one its play/pause toggle stands for.
pub fn command(button: Button, np: &NowPlaying) -> Option<PlayerCommand> {
    let status = status(np);
    match button {
        Button::Play => (status == Status::Paused).then_some(PlayerCommand::Resume),
        Button::Pause => (status == Status::Playing).then_some(PlayerCommand::Pause),
        _ if status == Status::Stopped => None,
        Button::Next => Some(PlayerCommand::Next),
        Button::Previous => Some(PlayerCommand::Previous),
        Button::Seek(to) => {
            let duration = timeline(np)?.duration;
            Some(PlayerCommand::Seek {
                position: to.min(duration),
            })
        }
    }
}

/// Turns each frame's snapshot into only the updates Windows needs.
#[derive(Default)]
pub struct Publisher {
    status: Option<Status>,
    track: Option<Option<Track>>,
    /// The timeline last sent, the clock time it was sent at and whether it was playing.
    timeline: Option<(Timeline, Duration, bool)>,
}

impl Publisher {
    /// `now` is on the player clock, the one `NowPlaying` positions are on.
    pub fn update(&mut self, snap: Snapshot, now: Duration) -> Vec<Update> {
        let mut out = Vec::new();
        let track_moved = self.track.as_ref() != Some(&snap.track);
        let status_moved = self.status != Some(snap.status);
        if track_moved {
            out.push(Update::Track(snap.track.clone()));
            self.track = Some(snap.track);
        }
        match snap.timeline {
            Some(t) if track_moved || status_moved || self.drifted(t, now) => {
                out.push(Update::Timeline(t));
                self.timeline = Some((t, now, snap.status == Status::Playing));
            }
            Some(_) => {}
            None => self.timeline = None,
        }
        if status_moved {
            out.push(Update::Status(snap.status));
            self.status = Some(snap.status);
        }
        out
    }

    /// Whether `t` is not where Windows' estimate from the last one would be.
    fn drifted(&self, t: Timeline, now: Duration) -> bool {
        let Some((sent, at, playing)) = self.timeline else {
            return true;
        };
        if t.duration != sent.duration {
            return true;
        }
        let expected = if playing {
            (sent.position + now.saturating_sub(at)).min(sent.duration)
        } else {
            sent.position
        };
        t.position.abs_diff(expected) > DRIFT
    }
}

/// The app's end of the session: snapshots go out as updates, presses come in.
pub struct MediaSession {
    publisher: Publisher,
    updates: Sender<Update>,
    buttons: Receiver<Button>,
}

impl MediaSession {
    pub fn new(updates: Sender<Update>, buttons: Receiver<Button>) -> Self {
        MediaSession {
            publisher: Publisher::default(),
            updates,
            buttons,
        }
    }

    pub fn publish(&mut self, snap: Snapshot, now: Duration) {
        for update in self.publisher.update(snap, now) {
            // The other end is gone only if the session failed; nothing to do then.
            let _ = self.updates.send(update);
        }
    }

    /// The presses since the last call.
    pub fn presses(&self) -> Vec<Button> {
        self.buttons.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use bp_core::{Codec, SourceFormat, SourceKind};

    use super::*;
    use crate::backend::QueueItem;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn item(title: &str) -> QueueItem {
        QueueItem {
            path: PathBuf::from(format!("/music/{title}.flac")),
            source: SourceFormat {
                codec: Codec::Flac,
                sample_rate: 44100,
                channels: 2,
                kind: SourceKind::Pcm {
                    bits: 16,
                    float: false,
                },
            },
            duration: secs(200),
            title: title.into(),
        }
    }

    /// Two tracks queued, the first at `position`.
    fn np(paused: bool, position: u64) -> NowPlaying {
        let mut np = NowPlaying::default();
        np.queue = vec![item("a"), item("b")];
        np.index = Some(0);
        np.position = secs(position);
        np.paused = paused;
        np
    }

    fn track(title: &str, art: Option<&str>) -> Track {
        Track {
            title: title.into(),
            artist: "Ada".into(),
            album: "First".into(),
            art: art.map(PathBuf::from),
        }
    }

    fn snap(status: Status, title: &str, position: u64) -> Snapshot {
        Snapshot {
            status,
            track: Some(track(title, None)),
            timeline: Some(Timeline {
                position: secs(position),
                duration: secs(200),
            }),
        }
    }

    fn stopped() -> Snapshot {
        Snapshot {
            status: Status::Stopped,
            track: None,
            timeline: None,
        }
    }

    #[test]
    fn status_follows_the_player() {
        assert_eq!(status(&NowPlaying::default()), Status::Stopped);
        assert_eq!(status(&np(false, 0)), Status::Playing);
        assert_eq!(status(&np(true, 0)), Status::Paused);
        let mut failed = np(false, 0);
        failed.fatal = Some("gone".into());
        assert_eq!(status(&failed), Status::Stopped);
    }

    #[test]
    fn timeline_uses_the_engine_duration_when_known() {
        assert_eq!(timeline(&NowPlaying::default()), None);
        assert_eq!(
            timeline(&np(false, 30)),
            Some(Timeline {
                position: secs(30),
                duration: secs(200)
            })
        );
        let mut measured = np(false, 195);
        measured.duration = Some(secs(190));
        assert_eq!(
            timeline(&measured),
            Some(Timeline {
                position: secs(190),
                duration: secs(190)
            })
        );
    }

    #[test]
    fn play_resumes_only_when_paused() {
        assert_eq!(
            command(Button::Play, &np(true, 0)),
            Some(PlayerCommand::Resume)
        );
        assert_eq!(command(Button::Play, &np(false, 0)), None);
        assert_eq!(command(Button::Play, &NowPlaying::default()), None);
    }

    #[test]
    fn pause_pauses_only_when_playing() {
        assert_eq!(
            command(Button::Pause, &np(false, 0)),
            Some(PlayerCommand::Pause)
        );
        assert_eq!(command(Button::Pause, &np(true, 0)), None);
        assert_eq!(command(Button::Pause, &NowPlaying::default()), None);
    }

    #[test]
    fn skips_and_seeks_need_a_track() {
        let playing = np(false, 0);
        assert_eq!(command(Button::Next, &playing), Some(PlayerCommand::Next));
        assert_eq!(
            command(Button::Previous, &np(true, 0)),
            Some(PlayerCommand::Previous)
        );
        assert_eq!(
            command(Button::Seek(secs(50)), &playing),
            Some(PlayerCommand::Seek { position: secs(50) })
        );
        let idle = NowPlaying::default();
        for b in [Button::Next, Button::Previous, Button::Seek(secs(5))] {
            assert_eq!(command(b, &idle), None, "{b:?}");
        }
    }

    #[test]
    fn a_seek_past_the_end_stops_at_the_end() {
        assert_eq!(
            command(Button::Seek(secs(900)), &np(false, 0)),
            Some(PlayerCommand::Seek {
                position: secs(200)
            })
        );
    }

    #[test]
    fn the_first_snapshot_sends_everything() {
        let mut p = Publisher::default();
        let s = snap(Status::Playing, "a", 0);
        assert_eq!(
            p.update(s.clone(), secs(0)),
            vec![
                Update::Track(s.track.clone()),
                Update::Timeline(s.timeline.unwrap()),
                Update::Status(Status::Playing),
            ]
        );
    }

    #[test]
    fn playing_on_sends_nothing() {
        let mut p = Publisher::default();
        p.update(snap(Status::Playing, "a", 10), secs(100));
        assert_eq!(p.update(snap(Status::Playing, "a", 10), secs(100)), vec![]);
        assert_eq!(p.update(snap(Status::Playing, "a", 40), secs(130)), vec![]);
    }

    #[test]
    fn a_seek_sends_the_timeline() {
        let mut p = Publisher::default();
        p.update(snap(Status::Playing, "a", 10), secs(100));
        let back = snap(Status::Playing, "a", 5);
        assert_eq!(
            p.update(back.clone(), secs(101)),
            vec![Update::Timeline(back.timeline.unwrap())]
        );
        let paused_jump = snap(Status::Paused, "a", 60);
        p.update(snap(Status::Paused, "a", 5), secs(101));
        assert_eq!(
            p.update(paused_jump.clone(), secs(150)),
            vec![Update::Timeline(paused_jump.timeline.unwrap())]
        );
    }

    #[test]
    fn a_pause_sends_the_status_and_where_it_stopped() {
        let mut p = Publisher::default();
        p.update(snap(Status::Playing, "a", 10), secs(100));
        let paused = snap(Status::Paused, "a", 20);
        assert_eq!(
            p.update(paused.clone(), secs(110)),
            vec![
                Update::Timeline(paused.timeline.unwrap()),
                Update::Status(Status::Paused),
            ]
        );
        // Paused time does not move the position Windows holds.
        assert_eq!(p.update(paused, secs(500)), vec![]);
    }

    #[test]
    fn a_new_track_sends_the_track_and_timeline() {
        let mut p = Publisher::default();
        p.update(snap(Status::Playing, "a", 199), secs(100));
        let next = snap(Status::Playing, "b", 0);
        assert_eq!(
            p.update(next.clone(), secs(101)),
            vec![
                Update::Track(next.track.clone()),
                Update::Timeline(next.timeline.unwrap()),
            ]
        );
    }

    #[test]
    fn art_that_arrives_later_resends_the_track() {
        let mut p = Publisher::default();
        p.update(snap(Status::Playing, "a", 0), secs(0));
        let with_art = Snapshot {
            track: Some(track("a", Some("/thumbs/1.png"))),
            ..snap(Status::Playing, "a", 0)
        };
        assert_eq!(
            p.update(with_art.clone(), secs(0)),
            vec![
                Update::Track(with_art.track.clone()),
                Update::Timeline(with_art.timeline.unwrap()),
            ]
        );
    }

    #[test]
    fn stopping_clears_the_track() {
        let mut p = Publisher::default();
        p.update(snap(Status::Playing, "a", 0), secs(0));
        assert_eq!(
            p.update(stopped(), secs(1)),
            vec![Update::Track(None), Update::Status(Status::Stopped)]
        );
        assert_eq!(p.update(stopped(), secs(2)), vec![]);
        // Playing again sends the timeline even at the same position.
        let again = snap(Status::Playing, "a", 0);
        assert_eq!(p.update(again, secs(3)).len(), 3);
    }

    #[test]
    fn the_session_sends_updates_and_hands_over_presses() {
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (button_tx, button_rx) = crossbeam_channel::unbounded();
        let mut session = MediaSession::new(update_tx, button_rx);
        session.publish(snap(Status::Paused, "a", 0), secs(0));
        assert_eq!(update_rx.try_iter().count(), 3);
        button_tx.send(Button::Next).unwrap();
        button_tx.send(Button::Play).unwrap();
        assert_eq!(session.presses(), vec![Button::Next, Button::Play]);
        assert_eq!(session.presses(), vec![]);
    }
}

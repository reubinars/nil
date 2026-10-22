mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bp_core::{DeviceId, EngineError, OutputError};
use bp_decode::Block;
use bp_engine::profile::ProfileStore;
use bp_engine::{Command, Event};
use bp_output::{CaptureBackend, CapturedStream};

use common::*;

const WAV_A: &str = "wav_16_44_a.wav";
const WAV_B: &str = "wav_16_44_b.wav";

fn last_stream(rig: &Rig) -> CapturedStream {
    rig.log.streams().pop().expect("a stream was opened")
}

fn total_bytes(rig: &Rig) -> usize {
    rig.log.streams().iter().map(|s| s.bytes.len()).sum()
}

/// Plays `tracks` but pauses before the first stream starts, so the test controls exactly
/// what is played.
fn play_paused(rig: &Rig, tracks: Vec<PathBuf>) {
    play(rig, tracks, 0);
    rig.engine.send(Command::Pause);
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::Paused));
}

/// The index of the last `TrackStarted` in `events`.
fn last_started(events: &[Event]) -> Option<usize> {
    started(events).last().copied()
}

#[test]
fn pause_stops_capture_growth() {
    let rig = rig(dac_a());
    play(&rig, long_queue(), 0);
    wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    rig.engine.send(Command::Pause);
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::Paused));
    // Let the capture render thread finish the chunk it may be copying.
    std::thread::sleep(Duration::from_millis(20));
    let paused_at = total_bytes(&rig);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(total_bytes(&rig), paused_at, "bytes grew while paused");
    assert!(!last_stream(&rig).closed, "pause keeps the stream open");

    rig.engine.send(Command::Resume);
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::Resumed));
    assert!(wait_until(|| total_bytes(&rig) > paused_at), "no growth");
    assert_eq!(rig.log.streams().len(), 1, "resume reuses the stream");

    // Not applicable: Resume while playing, Pause/Resume with nothing playing.
    rig.engine.send(Command::Resume);
    rig.engine.send(Command::Stop);
    assert!(wait_until(|| last_stream(&rig).closed));
    rig.engine.send(Command::Pause);
    rig.engine.send(Command::Resume);
    assert_quiet(&rig.engine, Duration::from_millis(200), |e| {
        matches!(e, Event::Paused | Event::Resumed)
    });
}

#[test]
fn next_skips_rest_of_track() {
    let rig = rig(dac_a());
    play(&rig, long_queue(), 0);
    wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    assert!(wait_until(|| last_stream(&rig).bytes.len() >= 4096));
    rig.engine.send(Command::Pause);
    rig.engine.send(Command::Next);
    rig.engine.send(Command::Resume);
    // Every TrackStarted before `Resumed` came from the first stream; the last one is the
    // track `Next` skipped.
    let before = wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::Resumed));
    let skipped = last_started(&before).unwrap_or(0);
    let after = wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    assert_eq!(last_started(&after), Some(skipped + 1), "{after:#?}");
    assert!(wait_until(|| last_stream(&rig).bytes.len() >= 4096));
    rig.engine.send(Command::Stop);
    assert!(wait_until(|| last_stream(&rig).closed));

    let streams = rig.log.streams();
    assert_eq!(streams.len(), 2, "Next restarts on a fresh stream");
    assert!(streams[0].closed);
    // Both streams start at the beginning of a track (all tracks are the same file).
    let n = streams[0].bytes.len().min(streams[1].bytes.len());
    assert!(n > 0);
    assert_eq!(streams[1].bytes[..n], streams[0].bytes[..n]);
}

#[test]
fn next_on_last_track_finishes() {
    let rig = rig(only(&[(44_100, "int16/16")]));
    play_paused(&rig, vec![fixture(WAV_A)]);
    rig.engine.send(Command::Next);
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert!(started(&events).is_empty(), "{events:#?}");
    assert!(
        rig.log
            .streams()
            .iter()
            .all(|s| s.closed && s.bytes.is_empty())
    );
    // Nothing playing: Next and Seek are ignored.
    rig.engine.send(Command::Next);
    rig.engine.send(Command::Seek {
        position: Duration::from_millis(100),
    });
    assert_quiet(&rig.engine, Duration::from_millis(200), |_| true);
}

#[test]
fn seek_restarts_at_position() {
    // WAV: exact.
    let rig = rig(only(&[(44_100, "int16/16")]));
    play_paused(&rig, vec![fixture(WAV_A), fixture(WAV_B)]);
    let position = Duration::from_millis(250);
    rig.engine.send(Command::Seek { position });
    rig.engine.send(Command::Resume);
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert_eq!(started(&events), [0, 1]);
    let first_pos = events.iter().find_map(|e| match e {
        Event::Position { index: 0, position } => Some(*position),
        _ => None,
    });
    assert!(
        first_pos.is_some_and(|p| p >= position),
        "Position is absolute in the track: {first_pos:?}"
    );
    let skip = 11_025 * 4; // 0.25 s × 44.1 kHz × 4 bytes
    let mut expected = data(WAV_A)[skip..].to_vec();
    expected.extend(data(WAV_B));
    assert_eq!(last_stream(&rig).bytes, expected);

    // FLAC: also exact (the decoder drops the frames before the target).
    let rig = common::rig(only(&[(44_100, "int16/16")]));
    let flac = fixture("flac_16_44.flac");
    play_paused(&rig, vec![flac.clone()]);
    rig.engine.send(Command::Seek { position });
    rig.engine.send(Command::Resume);
    wait_finished(&rig);
    let reference = decode_i16_le(&flac);
    let got = last_stream(&rig).bytes;
    assert!(!got.is_empty());
    let start = reference.len() - got.len();
    assert_eq!(reference[start..], got[..], "captured bytes are a suffix");
    assert_eq!(start / 4, 11_025, "first captured frame");
}

/// The whole track as interleaved little-endian 16-bit samples.
fn decode_i16_le(path: &std::path::Path) -> Vec<u8> {
    let mut src = bp_decode::open(path).unwrap();
    let mut out = Vec::new();
    while let Some(block) = src.next_block().unwrap() {
        let Block::Pcm(samples) = block else {
            panic!("not PCM")
        };
        out.extend(samples.iter().flat_map(|&s| (s as i16).to_le_bytes()));
    }
    out
}

#[test]
fn seek_past_end_advances() {
    let rig = rig(only(&[(44_100, "int16/16")]));
    play_paused(&rig, vec![fixture(WAV_A), fixture(WAV_B)]);
    rig.engine.send(Command::Seek {
        position: Duration::from_secs(10),
    });
    rig.engine.send(Command::Resume);
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert_eq!(started(&events), [1]);
    assert_eq!(last_stream(&rig).bytes, data(WAV_B));

    // On the last track it finishes the queue.
    play_paused(&rig, vec![fixture(WAV_A)]);
    rig.engine.send(Command::Seek {
        position: Duration::from_secs(10),
    });
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert!(started(&events).is_empty(), "{events:#?}");
    assert!(rig.log.streams().iter().all(|s| s.closed));
}

#[test]
fn select_device_switches_backend_device() {
    let caps = dac_a();
    let dac = DeviceId("dac2".into());
    let (backend, log) = CaptureBackend::new(caps.clone());
    let rig = rig_with(backend.with_devices([dac.clone()]), log, &caps, Some(false));

    // Selecting a device moves playback to it without finishing the queue.
    play(&rig, long_queue(), 0);
    wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    rig.engine.send(Command::SelectDevice { id: dac.clone() });
    wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    assert_eq!(rig.log.streams()[0].device, capture_id());
    assert!(rig.log.streams()[0].closed);
    assert_eq!(last_stream(&rig).device, dac);
    rig.engine.send(Command::Stop);
    assert!(wait_until(|| last_stream(&rig).closed));
    assert_quiet(&rig.engine, Duration::from_millis(100), |e| {
        matches!(e, Event::QueueFinished | Event::Error(_))
    });

    // Next Play goes to the new device, probing and saving its profile.
    play(&rig, vec![fixture(WAV_A)], 0);
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    let stream = last_stream(&rig);
    assert_eq!(stream.device, dac);
    assert_eq!(stream.bytes, data(WAV_A));
    let store = ProfileStore::load(&rig.profile_path).unwrap();
    assert_eq!(store.get(&dac).expect("profile saved").capabilities(), caps);

    // Unknown device: error, previous device kept.
    rig.engine.send(Command::SelectDevice {
        id: DeviceId("nope".into()),
    });
    let events = wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::Error(_)));
    assert!(
        matches!(
            events.last(),
            Some(Event::Error(EngineError::Output(
                OutputError::DeviceNotFound
            )))
        ),
        "{events:#?}"
    );
    play(&rig, vec![fixture(WAV_A)], 0);
    wait_finished(&rig);
    assert_eq!(last_stream(&rig).device, dac);
}

#[test]
fn previous_before_any_progress_plays_the_previous_track() {
    let rig = rig(dac_a());
    // Start at track 1, paused before its stream starts: no position is known yet.
    play(&rig, vec![fixture(WAV_A), fixture(WAV_B)], 1);
    rig.engine.send(Command::Pause);
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::Paused));
    rig.engine.send(Command::Previous);
    rig.engine.send(Command::Resume);
    let evs = wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    assert_eq!(last_started(&evs), Some(0));
}

#[test]
fn select_device_while_playing_continues_on_the_new_device() {
    let (backend, log) = CaptureBackend::new(dac_a());
    let other = DeviceId("other".into());
    let backend = backend.with_devices([other.clone()]);
    let profiles = vec![
        (capture_id(), profile_for(&dac_a(), false)),
        (other.clone(), profile_for(&dac_a(), false)),
    ];
    let rig = rig_with_profiles(backend, log, profiles);
    play(&rig, long_queue(), 0);
    wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    rig.engine.send(Command::SelectDevice { id: other.clone() });
    let evs = wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    assert!(
        !evs.iter()
            .any(|e| matches!(e, Event::QueueFinished | Event::Error(_)))
    );
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 2, "one stream per device");
    assert_eq!(streams[1].device, other);
}

#[test]
fn select_device_to_the_current_one_does_not_restart() {
    let rig = rig(dac_a());
    play(&rig, long_queue(), 0);
    let evs = wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    let mut latest = last_started(&evs).expect("a track started");
    rig.engine.send(Command::SelectDevice { id: capture_id() });
    // The queue plays on gaplessly; a restart would report a track again.
    let end = Instant::now() + Duration::from_millis(100);
    while let Ok(e) = rig
        .engine
        .events()
        .recv_timeout(end.saturating_duration_since(Instant::now()))
    {
        match e {
            Event::TrackStarted { index, .. } if index > latest => latest = index,
            Event::TrackStarted { .. } | Event::Error(_) => panic!("unexpected event {e:?}"),
            _ => {}
        }
    }
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    assert!(!streams[0].closed);
}

/// A rig with a second device, "other", both with a saved profile.
fn two_device_rig() -> (Rig, DeviceId) {
    let (backend, log) = CaptureBackend::new(dac_a());
    let other = DeviceId("other".into());
    let backend = backend.with_devices([other.clone()]);
    let profiles = vec![
        (capture_id(), profile_for(&dac_a(), false)),
        (other.clone(), profile_for(&dac_a(), false)),
    ];
    (rig_with_profiles(backend, log, profiles), other)
}

#[test]
fn switching_devices_twice_while_paused_keeps_the_position() {
    let (rig, other) = two_device_rig();
    // Paused 300 ms into track 1. (A paused Seek gets there exactly: the 500 ms tracks
    // play too fast to pause at a reported position.)
    play(&rig, long_queue(), 1);
    rig.engine.send(Command::Pause);
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::Paused));
    let paused_at = Duration::from_millis(300);
    rig.engine.send(Command::Seek {
        position: paused_at,
    });
    // Each switch leaves the new stream opened (primed) but not started.
    rig.engine.send(Command::SelectDevice { id: other.clone() });
    assert!(wait_until(|| rig
        .log
        .streams()
        .last()
        .is_some_and(|s| s.device == other)));
    rig.engine.send(Command::SelectDevice { id: capture_id() });
    rig.engine.send(Command::Resume);
    let evs = wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::Position { .. })
    });
    assert_eq!(started(&evs), [1], "{evs:#?}");
    let Some(&Event::Position { index, position }) = evs.last() else {
        unreachable!()
    };
    assert_eq!(index, 1);
    assert!(position >= paused_at, "restarted at {position:?}");
    assert_eq!(last_stream(&rig).device, capture_id());
}

#[test]
fn previous_after_a_paused_seek_restarts_the_track() {
    let rig = rig(dac_a());
    play(&rig, long_queue(), 1);
    rig.engine.send(Command::Pause);
    wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::Paused));
    rig.engine.send(Command::Seek {
        position: Duration::from_secs(10),
    });
    // A command in between keeps Previous from folding into the Seek: it runs against
    // the stream the Seek opened, which has not started.
    rig.engine.send(Command::Pause);
    rig.engine.send(Command::Previous);
    rig.engine.send(Command::Resume);
    let evs = wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    assert_eq!(last_started(&evs), Some(1), "{evs:#?}");
}

#[test]
fn select_device_of_an_unlisted_device_stops_and_relists() {
    let rig = rig(dac_a());
    play(&rig, long_queue(), 0);
    wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    rig.engine.send(Command::SelectDevice {
        id: DeviceId("gone".into()),
    });
    let evs = wait_for(&rig.engine, TIMEOUT, |e| matches!(e, Event::Error(_)));
    assert!(
        matches!(
            evs.last(),
            Some(Event::Error(EngineError::Output(
                OutputError::DeviceNotFound
            )))
        ),
        "{evs:#?}"
    );
    assert!(
        evs.iter().any(|e| matches!(e, Event::DevicesChanged)),
        "{evs:#?}"
    );
    assert!(rig.log.streams()[0].closed, "device released");
    assert_quiet(&rig.engine, Duration::from_millis(100), |e| {
        matches!(e, Event::Position { .. })
    });
}

/// Deterministic pseudo-random numbers (LCG), so failures are reproducible.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % below
    }
}

#[test]
fn command_spam_never_deadlocks() {
    const SEED: u64 = 0x5eed_2026_1018;
    const ROUNDS: u64 = 10;
    let opened: Vec<usize> = (0..ROUNDS).map(|r| spam_round(SEED + r)).collect();
    // Baseline: 1 stream per round (the setup Play). Spam commands must open streams too.
    assert!(
        opened.iter().sum::<usize>() >= 3 * ROUNDS as usize,
        "streams opened per round: {opened:?} (seed {SEED:#x})"
    );
}

/// One engine: playing, then 4 threads × 50 random commands, then `Shutdown`. Returns the
/// number of streams opened.
fn spam_round(seed: u64) -> usize {
    // 16/44.1 next to 24/96 (and 24/192): playing across them reopens the stream.
    let tracks = vec![
        fixture(WAV_A),
        fixture("wav_24_96.wav"),
        fixture(WAV_B),
        fixture("flac_24_192.flac"),
        fixture("wav_16_44_tiny.wav"),
        fixture("flac_16_44.flac"),
    ];
    let Rig {
        engine, log, _dir, ..
    } = rig(dac_a());
    // A stream is running when the spam starts.
    engine.send(Command::Play {
        tracks: long_queue(),
        start: 0,
    });
    wait_for(&engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    std::thread::scope(|s| {
        for t in 0..4 {
            let (engine, tracks) = (&engine, &tracks);
            s.spawn(move || {
                let mut rng = Lcg(seed.wrapping_mul(31).wrapping_add(t));
                for i in 0..50 {
                    let cmd = match rng.next(6) {
                        0 => Command::Play {
                            tracks: tracks.clone(),
                            start: rng.next(tracks.len() as u64) as usize,
                        },
                        1 => Command::Next,
                        2 => Command::Seek {
                            position: Duration::from_millis(rng.next(700)),
                        },
                        3 => Command::Stop,
                        4 => Command::Pause,
                        _ => Command::Resume,
                    };
                    engine.send(cmd);
                    // Short seeded pauses let decoders reach NeedStream / Primed and
                    // streams open, start and reopen between commands.
                    if i % 5 == 4 {
                        std::thread::sleep(Duration::from_millis(rng.next(20)));
                    }
                }
            });
        }
    });
    engine.send(Command::Shutdown);
    let shutdown = Instant::now();
    let (joined_tx, joined_rx) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        drop(engine); // joins the control thread
        let _ = joined_tx.send(());
    });
    let joined = joined_rx.recv_timeout(Duration::from_secs(1)).is_ok();
    let streams = log.streams();
    assert!(
        joined,
        "engine did not shut down within 1 s ({} streams opened, seed {seed:#x})",
        streams.len()
    );
    assert!(
        streams.iter().all(|s| s.closed),
        "{} of {} streams left open after {:?} (seed {seed:#x})",
        streams.iter().filter(|s| !s.closed).count(),
        streams.len(),
        shutdown.elapsed()
    );
    streams.len()
}

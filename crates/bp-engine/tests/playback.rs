mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use bp_core::{Capabilities, DOP_MARKERS, EngineError, OutputError, PathKind, UnsupportedReason};
use bp_decode::testgen::dsf_bytes;
use bp_engine::profile::{ProbedFormat, ProfileStore};
use bp_engine::{Command, Event};
use bp_output::CaptureBackend;
use md5::{Digest, Md5};

use common::*;

/// Little-endian 16-bit samples left-justified into `container` bytes.
fn pad16(bytes: &[u8], container: usize) -> Vec<u8> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .flat_map(|s| {
            let mut v = vec![0u8; container - 2];
            v.extend_from_slice(s);
            v
        })
        .collect()
}

/// Little-endian 24-bit samples left-justified into 32 bits.
fn pad24_32(bytes: &[u8]) -> Vec<u8> {
    bytes
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|s| [0, s[0], s[1], s[2]])
        .collect()
}

fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut s = seed;
    (0..len)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 24) as u8
        })
        .collect()
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn same_format_album_is_bit_perfect_and_gapless() {
    let rig = rig(only(&[(44_100, "int16/16")]));
    play(
        &rig,
        vec![fixture("wav_16_44_a.wav"), fixture("wav_16_44_b.wav")],
        0,
    );
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert_eq!(started(&events), vec![0, 1]);
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].format, fmt(44_100, "int16/16"));
    let mut want = data("wav_16_44_a.wav");
    want.extend(data("wav_16_44_b.wav"));
    assert!(
        streams[0].bytes == want,
        "captured bytes differ from the sources"
    );
    assert!(streams[0].closed);
}

#[test]
fn padded_album_matches_shifted_samples() {
    let rig = rig(only(&[(44_100, "int32/32")]));
    play(
        &rig,
        vec![fixture("wav_16_44_a.wav"), fixture("wav_16_44_b.wav")],
        0,
    );
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    for e in &events {
        if let Event::TrackStarted { playback, .. } = e {
            assert_eq!(playback.kind, PathKind::Padded { pad_bits: 16 });
        }
    }
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].format, fmt(44_100, "int32/32"));
    let mut src = data("wav_16_44_a.wav");
    src.extend(data("wav_16_44_b.wav"));
    assert!(streams[0].bytes == pad16(&src, 4));
}

#[test]
fn flac_through_engine_matches_streaminfo_md5() {
    let rig = rig(only(&[(96_000, "int32/24")]));
    let path = fixture("flac_24_96.flac");
    play(&rig, vec![path.clone()], 0);
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    let mut md5 = Md5::new();
    for c in streams[0].bytes.as_chunks::<4>().0 {
        let s = i32::from_le_bytes(*c) >> 8;
        md5.update(&s.to_le_bytes()[..3]);
    }
    let got: [u8; 16] = md5.finalize().into();
    let file = std::fs::read(&path).unwrap();
    let want: [u8; 16] = file[26..42].try_into().unwrap();
    assert_eq!(got, want);
}

#[test]
fn start_index_plays_to_end_then_stops() {
    let rig = rig(only(&[(44_100, "int16/16")]));
    play(
        &rig,
        vec![
            fixture("wav_16_44_a.wav"),
            fixture("wav_16_44_b.wav"),
            fixture("wav_16_44_tiny.wav"),
        ],
        1,
    );
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert_eq!(started(&events), vec![1, 2]);
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    let mut want = data("wav_16_44_b.wav");
    want.extend(data("wav_16_44_tiny.wav"));
    assert!(streams[0].bytes == want, "track 0 must not be captured");
}

#[test]
fn format_change_reopens_stream() {
    let rig = rig(dac_a());
    play(
        &rig,
        vec![fixture("wav_16_44_a.wav"), fixture("wav_24_96.wav")],
        0,
    );
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert_eq!(started(&events), vec![0, 1]);
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].format, fmt(44_100, "int16/16"));
    assert_eq!(streams[1].format, fmt(96_000, "int24/24"));
    assert!(streams[0].bytes == data("wav_16_44_a.wav"));
    assert!(streams[1].bytes == data("wav_24_96.wav"));
    assert!(streams.iter().all(|s| s.closed));
}

#[test]
fn prefer_keeps_stream_open() {
    let rig = rig(only(&[(44_100, "int16/16"), (44_100, "int24/24")]));
    play(
        &rig,
        vec![fixture("wav_24_44.wav"), fixture("wav_16_44_a.wav")],
        0,
    );
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    let kinds: Vec<(usize, PathKind)> = events
        .iter()
        .filter_map(|e| match e {
            Event::TrackStarted {
                index, playback, ..
            } => Some((*index, playback.kind)),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec![(0, PathKind::Native), (1, PathKind::Padded { pad_bits: 8 })]
    );
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].format, fmt(44_100, "int24/24"));
    let mut want = data("wav_24_44.wav");
    want.extend(pad16(&data("wav_16_44_a.wav"), 3));
    assert!(streams[0].bytes == want);
}

#[test]
fn corrupt_track_reports_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let wav = std::fs::read(fixture("wav_16_44_a.wav")).unwrap();
    let mut cut = wav[..wav.len() / 2].to_vec();
    cut[4..8].copy_from_slice(&0x7fff_0000u32.to_le_bytes());
    let corrupt = write_file(dir.path(), "corrupt.wav", &cut);

    let rig = rig(only(&[(44_100, "int16/16")]));
    play(
        &rig,
        vec![
            fixture("wav_16_44_a.wav"),
            corrupt,
            fixture("wav_16_44_b.wav"),
        ],
        0,
    );
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert_eq!(
        track_events(&events),
        vec![("started", 0), ("started", 1), ("error", 1), ("started", 2)],
        "{events:#?}"
    );
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    let a = data("wav_16_44_a.wav");
    let b = data("wav_16_44_b.wav");
    let got = &streams[0].bytes;
    assert!(got.len() >= a.len() + b.len());
    assert!(got.starts_with(&a));
    assert!(got.ends_with(&b));
    // What played of the corrupt track is a prefix of its audio.
    let middle = &got[a.len()..got.len() - b.len()];
    assert!(
        !middle.is_empty(),
        "frames decoded before the error must play"
    );
    assert!(a.starts_with(middle));
}

#[test]
fn skip_and_error_events_follow_playback_order() {
    let dir = tempfile::tempdir().unwrap();
    let unknown = write_file(dir.path(), "unknown.bin", &pattern(4096, 99));
    let rig = rig(only(&[(44_100, "int16/16")]));
    // A short first track: playback starts only once the decoder is past the skip.
    play(
        &rig,
        vec![
            fixture("wav_16_44_tiny.wav"),
            unknown,
            fixture("wav_16_44_b.wav"),
        ],
        0,
    );
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert_eq!(
        track_events(&events),
        vec![("started", 0), ("skipped", 1), ("started", 2)],
        "{events:#?}"
    );
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    let mut want = data("wav_16_44_tiny.wav");
    want.extend(data("wav_16_44_b.wav"));
    assert!(streams[0].bytes == want);
}

#[test]
fn output_error_stops_and_reports() {
    let caps = only(&[(44_100, "int16/16")]);
    let (mut backend, log) = CaptureBackend::new(caps.clone());
    backend.fail_stream_after(1000, OutputError::Invalidated);
    let rig = rig_with(backend, log, &caps, Some(false));
    play(
        &rig,
        vec![fixture("wav_16_44_a.wav"), fixture("wav_16_44_b.wav")],
        0,
    );
    let events = wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::Error(_) | Event::QueueFinished)
    });
    assert!(
        matches!(
            events.last(),
            Some(Event::Error(EngineError::Output(OutputError::Invalidated)))
        ),
        "{events:#?}"
    );
    assert!(wait_until(|| rig.log.streams()[0].closed));
    assert_quiet(&rig.engine, Duration::from_millis(200), |e| {
        matches!(e, Event::QueueFinished)
    });
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].bytes.len(), 1000 * 4);
}

#[test]
fn unsupported_track_skipped_rest_plays() {
    let dir = tempfile::tempdir().unwrap();
    let c = vec![pattern(8192, 1), pattern(8192, 2)];
    let dsd256 = write_file(dir.path(), "dsd256.dsf", &dsf_bytes(2, 256, &c, None));

    let caps = dac_b();
    let (backend, log) = CaptureBackend::new(caps.clone());
    let rig = rig_with(backend, log, &caps, Some(true));
    play(
        &rig,
        vec![
            fixture("wav_16_44_a.wav"),
            dsd256,
            fixture("wav_16_44_b.wav"),
        ],
        0,
    );
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::TrackSkipped {
                index: 1,
                reason: UnsupportedReason::DsdRateExceedsDevice {
                    multiple: 256,
                    max_multiple: 64
                },
                ..
            }
        )),
        "{events:#?}"
    );
    assert_eq!(started(&events), vec![0, 2]);
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    let mut want = data("wav_16_44_a.wav");
    want.extend(data("wav_16_44_b.wav"));
    assert!(streams[0].bytes == want);
}

#[test]
fn dsd64_plays_as_dop_with_continuous_markers() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = vec![pattern(6002, 10), pattern(6002, 11)];
    let t1 = vec![pattern(5000, 12), pattern(5000, 13)];
    let p0 = write_file(dir.path(), "t0.dsf", &dsf_bytes(2, 64, &t0, None));
    let p1 = write_file(dir.path(), "t1.dsf", &dsf_bytes(2, 64, &t1, None));

    let caps = dac_b();
    let (backend, log) = CaptureBackend::new(caps.clone());
    let rig = rig_with(backend, log, &caps, Some(true));
    play(&rig, vec![p0, p1], 0);
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert_eq!(started(&events), vec![0, 1]);
    for e in &events {
        if let Event::TrackStarted { playback, .. } = e {
            assert_eq!(playback.kind, PathKind::Dop);
        }
    }
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].format, fmt(176_400, "int32/24"));
    let bytes = &streams[0].bytes;
    assert_eq!(bytes.len(), (6002 + 5000) / 2 * 8);
    let mut ch = [Vec::new(), Vec::new()];
    for (i, frame) in bytes.as_chunks::<8>().0.iter().enumerate() {
        for (c, s) in frame.as_chunks::<4>().0.iter().enumerate() {
            assert_eq!(s[0], 0, "frame {i}");
            assert_eq!(s[3], DOP_MARKERS[i % 2], "marker of frame {i}");
            ch[c].push(s[2]);
            ch[c].push(s[1]);
        }
    }
    for c in 0..2 {
        let mut want = t0[c].clone();
        want.extend(&t1[c]);
        assert!(ch[c] == want, "channel {c} payload differs");
    }
}

/// Asserts `bytes` is `chans` DoP-packed in `container`-byte samples (payload in the top
/// 24 bits, markers alternating from 0x05).
fn assert_dop(bytes: &[u8], container: usize, chans: &[Vec<u8>]) {
    let frame = container * chans.len();
    assert_eq!(bytes.len(), chans[0].len() / 2 * frame);
    let mut got = vec![Vec::new(); chans.len()];
    for (i, f) in bytes.chunks_exact(frame).enumerate() {
        for (c, s) in f.chunks_exact(container).enumerate() {
            assert!(s[..container - 3].iter().all(|&b| b == 0), "frame {i}");
            assert_eq!(s[container - 1], DOP_MARKERS[i % 2], "marker of frame {i}");
            got[c].push(s[container - 2]);
            got[c].push(s[container - 3]);
        }
    }
    for (c, want) in chans.iter().enumerate() {
        assert!(got[c] == *want, "channel {c} payload differs");
    }
}

/// The real render step's buffer period, paced in real time like a device.
const PERIOD: Duration = Duration::from_millis(10);

/// The frame-aligned offset where `want` sits in `rendered` with only zero bytes around
/// it, if any.
fn find_between_silence(rendered: &[u8], want: &[u8], frame: usize) -> Option<usize> {
    let end = rendered.len().checked_sub(want.len())?;
    (0..=end).step_by(frame).find(|&at| {
        rendered[at..at + want.len()] == *want
            && rendered[..at].iter().all(|&b| b == 0)
            && rendered[at + want.len()..].iter().all(|&b| b == 0)
    })
}

#[test]
fn paced_album_renders_gapless_without_underruns() {
    let caps = only(&[(44_100, "int16/16")]);
    let (backend, log) = CaptureBackend::new(caps.clone());
    let rig = rig_with(backend.paced(PERIOD), log, &caps, Some(false));
    play(
        &rig,
        vec![fixture("wav_16_44_a.wav"), fixture("wav_16_44_b.wav")],
        0,
    );
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, Event::Underrun { .. })),
        "{events:#?}"
    );
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    let mut want = data("wav_16_44_a.wav");
    want.extend(data("wav_16_44_b.wav"));
    assert!(
        streams[0].bytes == want,
        "popped bytes differ from the sources"
    );
    assert!(
        find_between_silence(&streams[0].rendered, &want, 4).is_some(),
        "the rendered output is not silence, both tracks back to back, silence"
    );
}

/// The closing silence continues the markers from the data. (The pad after silence that
/// precedes data is covered by the render step's unit tests: here the ring is primed.)
#[test]
fn paced_dop_keeps_markers_alternating_through_silence() {
    let dir = tempfile::tempdir().unwrap();
    // An odd frame count: silence restarting the markers would repeat the last one.
    let dsd = vec![pattern(4002, 40), pattern(4002, 41)];
    let dsf = write_file(dir.path(), "t.dsf", &dsf_bytes(2, 64, &dsd, None));
    let caps = only(&[(176_400, "int32/24")]);
    let (backend, log) = CaptureBackend::new(caps.clone());
    let rig = rig_with(backend.paced(PERIOD), log, &caps, Some(true));
    play(&rig, vec![dsf], 0);
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    assert_dop(&streams[0].bytes, 4, &dsd);
    let rendered = &streams[0].rendered;
    assert!(
        rendered.len() > streams[0].bytes.len(),
        "silence was rendered"
    );
    let markers: Vec<u8> = rendered.as_chunks::<8>().0.iter().map(|f| f[3]).collect();
    for (i, pair) in markers.windows(2).enumerate() {
        assert!(
            DOP_MARKERS.contains(&pair[0]) && pair[0] != pair[1],
            "marker run broken at frame {i}: {:#04x} then {:#04x}",
            pair[0],
            pair[1]
        );
    }
}

/// DSD64 over DoP and 24/176.4 PCM share a device format (176.4k Int 32/24); they must still
/// get separate streams, since the stream's silence and pause behavior depend on DoP-ness.
/// A 16-bit stereo WAV of a sine at `freq` Hz, amplitude `amp`, lasting `secs`.
fn sine_wav(rate: u32, freq: f32, amp: f32, secs: f32) -> Vec<u8> {
    let frames = (rate as f32 * secs) as u32;
    let mut data = Vec::with_capacity(frames as usize * 4);
    for i in 0..frames {
        let v =
            (amp * (std::f32::consts::TAU * freq * i as f32 / rate as f32).sin() * 32_767.0) as i16;
        data.extend(v.to_le_bytes());
        data.extend(v.to_le_bytes());
    }
    let mut wav = Vec::new();
    wav.extend(b"RIFF");
    wav.extend((36 + data.len() as u32).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(rate.to_le_bytes());
    wav.extend((rate * 4).to_le_bytes());
    wav.extend(4u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend((data.len() as u32).to_le_bytes());
    wav.extend(data);
    wav
}

#[test]
fn analysis_tap_follows_playback() {
    let dir = tempfile::tempdir().unwrap();
    let wav = write_file(dir.path(), "sine.wav", &sine_wav(44_100, 1_000.0, 0.5, 2.0));
    let caps = only(&[(44_100, "int16/16")]);
    let (backend, log) = CaptureBackend::new(caps.clone());
    let rig = rig_with(backend.paced(PERIOD), log, &caps, Some(false));
    let tap = rig.engine.analysis();
    play(&rig, vec![wav], 0);
    wait_for(&rig.engine, Duration::from_secs(5), |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    let mut window = None;
    assert!(
        wait_until(|| {
            window = tap.window(1024, std::time::Instant::now());
            window.is_some()
        }),
        "no analysis window while playing"
    );
    let w = window.unwrap();
    assert_eq!(w.rate, 44_100);
    let rms = (w.samples.iter().map(|s| s * s).sum::<f32>() / w.samples.len() as f32).sqrt();
    let want = 0.5 / 2f32.sqrt();
    assert!((rms - want).abs() < want * 0.1, "rms {rms}");

    rig.engine.send(Command::Pause);
    wait_for(&rig.engine, Duration::from_secs(5), |e| {
        matches!(e, Event::Paused)
    });
    assert_eq!(tap.window(1024, std::time::Instant::now()), None, "paused");
    rig.engine.send(Command::Resume);
    let events = wait_finished(&rig);
    assert!(matches!(events.last(), Some(Event::QueueFinished)));
    assert!(
        wait_until(|| tap.window(1024, std::time::Instant::now()).is_none()),
        "a window after the queue finished"
    );
}

#[test]
fn dop_and_pcm_never_share_a_stream() {
    let dir = tempfile::tempdir().unwrap();
    let dsd = vec![pattern(6000, 20), pattern(6000, 21)];
    let dsf = write_file(dir.path(), "t.dsf", &dsf_bytes(2, 64, &dsd, None));
    let wav = fixture("wav_24_176.wav");
    let caps = only(&[(176_400, "int32/24")]);
    for dsd_first in [true, false] {
        let (backend, log) = CaptureBackend::new(caps.clone());
        let rig = rig_with(backend, log, &caps, Some(true));
        let tracks = if dsd_first {
            vec![dsf.clone(), wav.clone()]
        } else {
            vec![wav.clone(), dsf.clone()]
        };
        play(&rig, tracks, 0);
        let events = wait_finished(&rig);
        assert!(
            matches!(events.last(), Some(Event::QueueFinished)),
            "{events:#?}"
        );
        assert_eq!(started(&events), vec![0, 1]);
        let streams = rig.log.streams();
        assert_eq!(streams.len(), 2, "dsd_first={dsd_first}");
        assert!(streams.iter().all(|s| s.format == fmt(176_400, "int32/24")));
        let (dop, pcm) = if dsd_first {
            (&streams[0], &streams[1])
        } else {
            (&streams[1], &streams[0])
        };
        assert_dop(&dop.bytes, 4, &dsd);
        assert!(pcm.bytes == pad24_32(&data("wav_24_176.wav")));
    }
}

/// A device that only takes 32/32 still gets DoP: the word in the top 24 bits, low byte zero.
#[test]
fn dsd64_plays_as_dop_in_32_32() {
    let dir = tempfile::tempdir().unwrap();
    let dsd = vec![pattern(4000, 30), pattern(4000, 31)];
    let dsf = write_file(dir.path(), "t.dsf", &dsf_bytes(2, 64, &dsd, None));
    let caps = only(&[(176_400, "int32/32")]);
    let (backend, log) = CaptureBackend::new(caps.clone());
    let rig = rig_with(backend, log, &caps, Some(true));
    play(&rig, vec![dsf], 0);
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].format, fmt(176_400, "int32/32"));
    assert_dop(&streams[0].bytes, 4, &dsd);
}

#[test]
fn open_rejection_falls_back_and_corrects_profile() {
    let caps = only(&[(96_000, "int24/24"), (96_000, "int32/24")]);
    let (mut backend, log) = CaptureBackend::new(caps.clone());
    backend.reject_on_open(fmt(96_000, "int24/24"));
    let rig = rig_with(backend, log, &caps, Some(false));
    play(&rig, vec![fixture("wav_24_96.wav")], 0);
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].format, fmt(96_000, "int32/24"));
    assert!(streams[0].bytes == pad24_32(&data("wav_24_96.wav")));
    let store = ProfileStore::load(&rig.profile_path).unwrap();
    let profile = store.get(&capture_id()).expect("profile saved");
    assert!(profile.disabled.is_empty());
    assert_eq!(
        profile.auto_disabled,
        vec![ProbedFormat {
            rate: 96_000,
            channels: 2,
            encoding: enc("int24/24"),
        }]
    );
}

#[test]
fn tiny_track_between_tracks_is_gapless() {
    let rig = rig(only(&[(44_100, "int16/16")]));
    play(
        &rig,
        vec![
            fixture("wav_16_44_a.wav"),
            fixture("wav_16_44_tiny.wav"),
            fixture("wav_16_44_b.wav"),
        ],
        0,
    );
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    assert_eq!(started(&events), vec![0, 1, 2]);
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    let mut want = data("wav_16_44_a.wav");
    want.extend(data("wav_16_44_tiny.wav"));
    want.extend(data("wav_16_44_b.wav"));
    assert!(streams[0].bytes == want);
}

#[test]
fn stop_releases_device() {
    const TRACKS: usize = 40;
    let rig = rig(dac_a());
    // Long enough that the queue cannot finish before Stop arrives.
    play(&rig, vec![fixture("flac_24_192.flac"); TRACKS], 0);
    wait_for(&rig.engine, TIMEOUT, |e| {
        matches!(e, Event::TrackStarted { .. })
    });
    rig.engine.send(Command::Stop);
    assert!(wait_until(|| rig.log.streams()[0].closed));
    let streams = rig.log.streams();
    assert_eq!(streams.len(), 1);
    let format = fmt(192_000, "int24/24");
    assert_eq!(streams[0].format, format);
    let queue_bytes = TRACKS * 96_000 * format.bytes_per_frame();
    let captured = streams[0].bytes.len();
    assert!(
        captured < queue_bytes / 4,
        "closed after {captured} of {queue_bytes} bytes: Stop did not cut the queue short"
    );
    assert_quiet(&rig.engine, Duration::from_millis(200), |e| {
        matches!(e, Event::QueueFinished | Event::Error(_))
    });
}

#[test]
fn missing_profile_triggers_probe_and_save() {
    let caps = dac_b();
    let (backend, log) = CaptureBackend::new(caps.clone());
    let rig = rig_with(backend, log, &caps, None);
    assert!(!rig.profile_path.exists());
    play(&rig, vec![fixture("wav_16_44_a.wav")], 0);
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    let store = ProfileStore::load(&rig.profile_path).unwrap();
    let profile = store.get(&capture_id()).expect("profile saved");
    assert_eq!(profile.capabilities(), caps);
    assert!(!profile.dop_enabled);
}

/// A profile without probe data (e.g. `bp dop on` before `bp probe`) is probed like a
/// missing one; the user's settings survive.
#[test]
fn empty_probed_profile_is_reprobed_keeping_settings() {
    let caps = dac_b();
    let (backend, log) = CaptureBackend::new(caps.clone());
    let mut profile = profile_for(&Capabilities::default(), true);
    profile.max_rate = Some(96_000);
    let rig = rig_with_profile(backend, log, Some(profile));
    play(&rig, vec![fixture("wav_16_44_a.wav")], 0);
    let events = wait_finished(&rig);
    assert!(
        matches!(events.last(), Some(Event::QueueFinished)),
        "{events:#?}"
    );
    let store = ProfileStore::load(&rig.profile_path).unwrap();
    let profile = store.get(&capture_id()).expect("profile saved");
    assert_eq!(profile.probed, profile_for(&caps, true).probed);
    assert!(profile.dop_enabled);
    assert_eq!(profile.max_rate, Some(96_000));
}

#[test]
fn empty_probe_is_an_error_and_not_saved() {
    let (backend, log) = CaptureBackend::new(Capabilities::default());
    let rig = rig_with_profile(backend, log, None);
    play(&rig, vec![fixture("wav_16_44_a.wav")], 0);
    let events = wait_finished(&rig);
    assert!(
        matches!(
            events.last(),
            Some(Event::Error(EngineError::Output(OutputError::NoFormats)))
        ),
        "{events:#?}"
    );
    assert!(rig.log.streams().is_empty());
    assert!(!rig.profile_path.exists());
}

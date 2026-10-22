use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bp_core::{Codec, DecodeError, SourceKind};
use bp_decode::{Block, Source, open};
use md5::{Digest, Md5};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn bits_of(src: &dyn Source) -> u8 {
    match src.format().kind {
        SourceKind::Pcm { bits, .. } => bits,
        k => panic!("expected PCM, got {k:?}"),
    }
}

fn drain_pcm(src: &mut dyn Source) -> Vec<i32> {
    let mut out = Vec::new();
    while let Some(b) = src.next_block().unwrap() {
        let Block::Pcm(s) = b else {
            panic!("expected PCM block")
        };
        out.extend_from_slice(s);
    }
    out
}

/// MD5 per the FLAC spec: interleaved samples, each little-endian in ceil(bits/8) bytes.
fn md5_of_decoded(name: &str) -> [u8; 16] {
    let mut src = open(&fixture(name)).unwrap();
    let bytes_per = bits_of(src.as_ref()).div_ceil(8) as usize;
    let mut md5 = Md5::new();
    for s in drain_pcm(src.as_mut()) {
        md5.update(&s.to_le_bytes()[..bytes_per]);
    }
    md5.finalize().into()
}

fn streaminfo_md5(name: &str) -> [u8; 16] {
    let bytes = fs::read(fixture(name)).unwrap();
    assert_eq!(&bytes[..4], b"fLaC");
    assert_eq!(bytes[4] & 0x7f, 0, "first block must be STREAMINFO");
    bytes[26..42].try_into().unwrap()
}

fn wav_data_chunk(name: &str) -> Vec<u8> {
    let bytes = fs::read(fixture(name)).unwrap();
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        if &bytes[at..at + 4] == b"data" {
            return bytes[at + 8..at + 8 + size].to_vec();
        }
        at += 8 + size + (size & 1);
    }
    panic!("no data chunk");
}

#[test]
fn flac_decodes_bit_exact_per_streaminfo_md5() {
    for f in ["flac_16_44.flac", "flac_24_96.flac", "flac_24_192.flac"] {
        assert_eq!(md5_of_decoded(f), streaminfo_md5(f), "{f}");
    }
}

#[test]
fn wav16_samples_equal_data_chunk() {
    let mut src = open(&fixture("wav_16_44_a.wav")).unwrap();
    assert_eq!(src.format().codec, Codec::Wav);
    assert_eq!(bits_of(src.as_ref()), 16);
    let got: Vec<u8> = drain_pcm(src.as_mut())
        .iter()
        .flat_map(|s| (*s as i16).to_le_bytes())
        .collect();
    assert_eq!(got, wav_data_chunk("wav_16_44_a.wav"));
}

#[test]
fn wav24_samples_equal_data_chunk() {
    let mut src = open(&fixture("wav_24_96.wav")).unwrap();
    assert_eq!(bits_of(src.as_ref()), 24);
    assert_eq!(src.format().sample_rate, 96_000);
    let got: Vec<u8> = drain_pcm(src.as_mut())
        .iter()
        .flat_map(|s| s.to_le_bytes()[..3].to_vec())
        .collect();
    assert_eq!(got, wav_data_chunk("wav_24_96.wav"));
}

#[test]
fn wav_float_yields_float_blocks() {
    let mut src = open(&fixture("wav_f32_44.wav")).unwrap();
    assert_eq!(
        src.format().kind,
        SourceKind::Pcm {
            bits: 32,
            float: true
        }
    );
    let mut got = Vec::new();
    while let Some(b) = src.next_block().unwrap() {
        let Block::Float(f) = b else {
            panic!("expected float block")
        };
        got.extend_from_slice(f);
    }
    let expected: Vec<f32> = wav_data_chunk("wav_f32_44.wav")
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn aiff_and_alac_open_with_right_format() {
    let src = open(&fixture("aiff_16_44.aiff")).unwrap();
    let f = src.format();
    assert_eq!(f.codec, Codec::Aiff);
    assert_eq!(f.sample_rate, 44_100);
    assert_eq!(f.channels, 2);
    assert_eq!(
        f.kind,
        SourceKind::Pcm {
            bits: 16,
            float: false
        }
    );

    let mut src = open(&fixture("alac_24_48.m4a")).unwrap();
    let f = src.format();
    assert_eq!(f.codec, Codec::Alac);
    assert_eq!(f.sample_rate, 48_000);
    assert_eq!(f.channels, 2);
    assert_eq!(
        f.kind,
        SourceKind::Pcm {
            bits: 24,
            float: false
        }
    );
    let samples = drain_pcm(src.as_mut());
    assert_eq!(samples.len(), 24_000 * 2);
    assert!(samples.iter().all(|s| (-(1 << 23)..(1 << 23)).contains(s)));
}

#[test]
fn open_sniffs_content_not_extension() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("x.wav");
    fs::copy(fixture("flac_16_44.flac"), &wav).unwrap();
    assert_eq!(open(&wav).unwrap().format().codec, Codec::Flac);

    let jpg = dir.path().join("x.jpg");
    fs::write(&jpg, b"\xff\xd8\xff\xe0 not really audio at all").unwrap();
    assert!(matches!(open(&jpg), Err(DecodeError::UnknownFormat)));

    let short = dir.path().join("short.flac");
    fs::write(&short, b"fLa").unwrap();
    assert!(matches!(open(&short), Err(DecodeError::UnknownFormat)));
}

#[test]
fn pcm_seek_lands_near_target() {
    let mut src = open(&fixture("flac_16_44.flac")).unwrap();
    let dur = src.duration().unwrap();
    assert!((dur.as_secs_f64() - 0.5).abs() < 0.01, "{dur:?}");
    let target = Duration::from_millis(250);
    let pos = src.seek(target).unwrap();
    let Some(Block::Pcm(first)) = src.next_block().unwrap() else {
        panic!("no block after seek")
    };
    let packet = Duration::from_secs_f64(first.len() as f64 / 2.0 / 44_100.0);
    assert!(pos <= target, "accurate seek must not overshoot: {pos:?}");
    assert!(target - pos <= packet, "{pos:?} too far from {target:?}");
    // Decoding continues to the end of the track.
    let rest = drain_pcm(src.as_mut()).len() / 2;
    let remaining = Duration::from_secs_f64(rest as f64 / 44_100.0) + packet;
    assert!(remaining.as_secs_f64() > 0.2, "{remaining:?}");
}
#[test]
fn open_skips_leading_id3v2() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tagged.flac");
    let mut bytes = b"ID3\x03\x00\x00".to_vec();
    bytes.extend_from_slice(&[0, 0, 0, 37]); // syncsafe size: 37 bytes of padding
    bytes.extend_from_slice(&[0u8; 37]);
    bytes.extend_from_slice(&fs::read(fixture("flac_16_44.flac")).unwrap());
    fs::write(&path, &bytes).unwrap();

    let mut src = open(&path).unwrap();
    assert_eq!(src.format().codec, Codec::Flac);
    let mut md5 = Md5::new();
    for s in drain_pcm(src.as_mut()) {
        md5.update(&s.to_le_bytes()[..2]);
    }
    let got: [u8; 16] = md5.finalize().into();
    assert_eq!(got, streaminfo_md5("flac_16_44.flac"));
}

/// MD5 of the fixture's 24-bit samples as decoded by ffmpeg (`-f s24le`).
#[test]
fn alac_decodes_bit_exact_against_ffmpeg() {
    let hex: String = md5_of_decoded("alac_24_48.m4a")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(hex, "2527a30d2a7c0ee724c51c566d0c74cd");
}

/// Companded (μ-law) PCM would be expanded, not passed through: not playable bit-perfect.
#[test]
fn mulaw_wav_is_unknown_format() {
    assert!(matches!(
        open(&fixture("wav_mulaw_8.wav")),
        Err(DecodeError::UnknownFormat)
    ));
}

#[test]
fn aac_opens_as_24_bit_pcm() {
    let mut src = open(&fixture("aac_44.m4a")).unwrap();
    let f = src.format();
    assert_eq!(f.codec, Codec::Aac);
    assert_eq!(f.sample_rate, 44_100);
    assert_eq!(f.channels, 2);
    assert_eq!(
        f.kind,
        SourceKind::Pcm {
            bits: 24,
            float: false
        }
    );
    let samples = drain_pcm(src.as_mut());
    assert!(samples.len() >= 22_050 * 2, "{} samples", samples.len());
    assert!(samples.iter().all(|s| (-(1 << 23)..(1 << 23)).contains(s)));
    assert!(samples.iter().any(|&s| s != 0));
}

/// `aac_44.m4a` (LC, 44.1 kHz, stereo) with its AudioSpecificConfig replaced by `asc`;
/// the container's sample entry still says 44.1 kHz stereo.
fn aac_with_asc(dir: &Path, asc: [u8; 2]) -> PathBuf {
    let mut bytes = fs::read(fixture("aac_44.m4a")).unwrap();
    let at = bytes.windows(2).position(|w| w == [0x12, 0x10]).unwrap();
    bytes[at..at + 2].copy_from_slice(&asc);
    let path = dir.join("patched.m4a");
    fs::write(&path, &bytes).unwrap();
    path
}

/// Implicit HE-AAC: the ASC holds the 22.05 kHz core rate, the container the 44.1 kHz output
/// rate. Played as LC it would run at twice the speed.
#[test]
fn aac_with_core_rate_below_container_rate_is_unknown_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = aac_with_asc(dir.path(), [0x13, 0x90]);
    assert!(matches!(open(&path), Err(DecodeError::UnknownFormat)));
}

/// The channel count is the ASC's, as the decoder's blocks are: they never outgrow the
/// window sliced from them.
#[test]
fn aac_channels_come_from_the_asc() {
    let dir = tempfile::tempdir().unwrap();
    let path = aac_with_asc(dir.path(), [0x12, 0x08]);
    let mut src = open(&path).unwrap();
    assert_eq!(src.format().channels, 1);
    while let Ok(Some(_)) = src.next_block() {}
}

#[test]
fn aac_trims_to_exact_length() {
    let mut src = open(&fixture("aac_44.m4a")).unwrap();
    let d = src.duration().unwrap();
    assert!(
        d.abs_diff(Duration::from_secs_f64(0.5)) < Duration::from_micros(1),
        "{d:?}"
    );
    assert_eq!(drain_pcm(src.as_mut()).len(), 22_050 * 2);
}

/// Left channel of a decoded file.
fn left(name: &str) -> Vec<f64> {
    let mut src = open(&fixture(name)).unwrap();
    drain_pcm(src.as_mut())
        .chunks(2)
        .map(|f| f64::from(f[0]))
        .collect()
}

/// The lag in `-64..=64` at which `x` best matches a 1 kHz sine over 4 096 frames from `start`.
fn sine_lag(x: &[f64], start: usize) -> i64 {
    let ideal = |n: usize| (2.0 * std::f64::consts::PI * 1000.0 * n as f64 / 44_100.0).sin();
    (-64i64..=64)
        .max_by(|&a, &b| {
            let corr = |lag: i64| -> f64 {
                (start + 64..start + 4096 - 64)
                    .map(|n| x[(n as i64 + lag) as usize] * ideal(n))
                    .sum()
            };
            corr(a).total_cmp(&corr(b))
        })
        .unwrap()
}

#[test]
fn aac_sine_joins_without_gap() {
    let mut x = left("aac_sine_a.m4a");
    x.extend(left("aac_sine_b.m4a"));
    assert_eq!(x.len(), 44_100);
    assert_eq!(sine_lag(&x, 0), 0, "start");
    assert_eq!(sine_lag(&x, 22_050 - 2048), 0, "join");
}

#[test]
fn aac_seek_respects_trim() {
    let mut src = open(&fixture("aac_44.m4a")).unwrap();
    let pos = src.seek(Duration::from_secs_f64(0.25)).unwrap();
    assert!(
        pos.abs_diff(Duration::from_secs_f64(0.25)) <= Duration::from_secs_f64(1024.0 / 44_100.0),
        "{pos:?}"
    );
    let rest = drain_pcm(src.as_mut()).len() / 2;
    assert!(rest <= 22_050, "{rest} frames after seek");

    let mut src = open(&fixture("aac_44.m4a")).unwrap();
    // Past the window: lands at its end, with nothing left to play.
    let pos = src.seek(Duration::from_secs_f64(0.6)).unwrap();
    assert_eq!(pos, Duration::from_secs_f64(0.5));
    assert!(src.next_block().unwrap().is_none());
}

#[test]
fn aac_seek_decodes_same_samples_as_linear() {
    let linear = drain_pcm(open(&fixture("aac_44.m4a")).unwrap().as_mut());
    for target in [0.0001, 0.25, 0.4999] {
        let mut src = open(&fixture("aac_44.m4a")).unwrap();
        let pos = src.seek(Duration::from_secs_f64(target)).unwrap();
        let at = (pos.as_secs_f64() * 44_100.0).round() as usize * 2;
        let rest = drain_pcm(src.as_mut());
        assert_eq!(
            rest.len(),
            linear.len() - at,
            "length after seek to {target}"
        );
        assert!(
            rest == linear[at..],
            "samples after seek to {target} differ"
        );
    }
}

use std::io::{Seek, Write};
use std::time::Duration;

use bp_core::{Codec, DecodeError, SourceFormat, SourceKind};
use bp_decode::testgen::dff_bytes;
use bp_decode::{Block, DffSource, Source};

fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut s = seed;
    (0..len)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 24) as u8
        })
        .collect()
}

fn open_bytes(bytes: &[u8]) -> Result<DffSource, DecodeError> {
    let mut tmp = tempfile::tempfile().unwrap();
    tmp.write_all(bytes).unwrap();
    tmp.rewind().unwrap();
    DffSource::open(tmp)
}

fn drain(src: &mut DffSource) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(b) = src.next_block().unwrap() {
        let Block::Dsd(d) = b else {
            panic!("expected DSD block")
        };
        out.extend_from_slice(d);
    }
    out
}

fn interleave(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b).flat_map(|(x, y)| [*x, *y]).collect()
}

#[test]
fn dff_roundtrip() {
    // Larger than one 64 KiB read piece so the piecewise path is covered.
    let c0 = pattern(100_001, 1);
    let c1 = pattern(100_001, 2);
    let f = dff_bytes(2, 64, &[c0.clone(), c1.clone()], false);
    let mut src = open_bytes(&f).unwrap();
    assert_eq!(drain(&mut src), interleave(&c0, &c1));
    assert!(src.next_block().unwrap().is_none());
}

#[test]
fn dff_mono_roundtrip() {
    let c0 = pattern(70_000, 3);
    let f = dff_bytes(1, 64, std::slice::from_ref(&c0), false);
    let mut src = open_bytes(&f).unwrap();
    assert_eq!(src.format().channels, 1);
    assert_eq!(drain(&mut src), c0);
}

#[test]
fn dff_skips_unknown_odd_chunk() {
    // The builder puts an odd-length COMT chunk between PROP and the DSD data.
    let c0 = pattern(1_000, 4);
    let c1 = pattern(1_000, 5);
    let f = dff_bytes(2, 64, &[c0.clone(), c1.clone()], false);
    assert!(f.windows(4).any(|w| w == b"COMT"));
    let mut src = open_bytes(&f).unwrap();
    assert_eq!(drain(&mut src), interleave(&c0, &c1));
}

#[test]
fn dff_dst_is_unsupported() {
    let c = vec![pattern(1_000, 6), pattern(1_000, 7)];
    let f = dff_bytes(2, 64, &c, true);
    assert!(matches!(open_bytes(&f), Err(DecodeError::DstUnsupported)));
}

#[test]
fn dff_format() {
    let c = vec![vec![0x55u8; 705_600]; 2];
    let f = dff_bytes(2, 128, &c, false);
    let src = open_bytes(&f).unwrap();
    assert_eq!(
        src.format(),
        SourceFormat {
            codec: Codec::Dff,
            sample_rate: 5_644_800,
            channels: 2,
            kind: SourceKind::Dsd { multiple: 128 },
        }
    );
    // 705_600 bytes * 8 / 5_644_800 Hz = 1 s
    assert_eq!(src.duration(), Some(Duration::from_secs(1)));
}

#[test]
fn dff_seek_rounds_to_dop_frame() {
    let c0 = pattern(400_000, 8);
    let c1 = pattern(400_000, 9);
    let f = dff_bytes(2, 64, &[c0.clone(), c1.clone()], false);
    let mut src = open_bytes(&f).unwrap();
    let reached = src.seek(Duration::from_secs(1)).unwrap();
    assert_eq!(reached, Duration::from_secs(1));
    assert_eq!(drain(&mut src), interleave(&c0[352_800..], &c1[352_800..]));

    // 3 bytes' worth of time rounds down to byte 2
    let reached = src.seek(Duration::from_nanos(8_700)).unwrap();
    assert_eq!(
        reached,
        Duration::from_nanos(2 * 8 * 1_000_000_000 / 2_822_400)
    );
    let first = match src.next_block().unwrap().unwrap() {
        Block::Dsd(d) => d.to_vec(),
        _ => panic!(),
    };
    assert_eq!(&first[..4], &interleave(&c0[2..4], &c1[2..4])[..]);

    // past the end clamps
    let reached = src.seek(Duration::from_secs(60)).unwrap();
    assert_eq!(Some(reached), src.duration());
    assert!(src.next_block().unwrap().is_none());
}

#[test]
fn dff_truncated_data_stops_without_error() {
    let c = vec![pattern(1_000, 10), pattern(1_000, 11)];
    let f = dff_bytes(2, 64, &c, false);
    let mut src = open_bytes(&f[..f.len() - 501]).unwrap();
    // 2000 - 501 = 1499 bytes present -> 749 whole frames per channel
    assert_eq!(drain(&mut src).len(), 749 * 2);
}

#[test]
fn dff_rejects_bad_files() {
    let c = vec![pattern(1_000, 12), pattern(1_000, 13)];
    let good = dff_bytes(2, 64, &c, false);
    let corrupt = |bytes: &[u8]| matches!(open_bytes(bytes), Err(DecodeError::Corrupt(_)));

    assert!(open_bytes(&good).is_ok());
    assert!(corrupt(&good[..8])); // truncated header
    assert!(corrupt(&good[..40])); // chunk runs past the end of the file
    let mut b = good.clone();
    b[0] = b'X';
    assert!(corrupt(&b));
    // FS = 44_101: find the FS chunk and patch its value
    let mut b = good.clone();
    let at = b.windows(4).position(|w| w == b"FS  ").unwrap() + 12;
    b[at..at + 4].copy_from_slice(&44_101u32.to_be_bytes());
    assert!(corrupt(&b));
    // channel count 0
    let mut b = good.clone();
    let at = b.windows(4).position(|w| w == b"CHNL").unwrap() + 12;
    b[at..at + 2].copy_from_slice(&0u16.to_be_bytes());
    assert!(corrupt(&b));
}

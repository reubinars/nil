use std::io::{Seek, Write};
use std::time::Duration;

use bp_core::{
    Codec, DecodeError, DeviceFormat, PathKind, PlaybackPath, SampleEncoding, SourceFormat,
    SourceKind,
};
use bp_decode::testgen::dsf_bytes;
use bp_decode::{Block, DOP_MARKERS, DSD_IDLE, DsfSource, Packer, Source};

fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut s = seed;
    (0..len)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 24) as u8
        })
        .collect()
}

fn open_bytes(bytes: &[u8]) -> Result<DsfSource, DecodeError> {
    let mut tmp = tempfile::tempfile().unwrap();
    tmp.write_all(bytes).unwrap();
    tmp.rewind().unwrap();
    DsfSource::open(tmp)
}

fn drain(src: &mut DsfSource) -> Vec<u8> {
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
fn dsf_roundtrip_interleaves_and_reverses() {
    let c0 = pattern(10_000, 1);
    let c1 = pattern(10_000, 2);
    let f = dsf_bytes(2, 64, &[c0.clone(), c1.clone()], None);
    let mut src = open_bytes(&f).unwrap();
    assert_eq!(drain(&mut src), interleave(&c0, &c1));
    assert!(src.next_block().unwrap().is_none());
}

#[test]
fn dsf_mono_roundtrip() {
    let c0 = pattern(5_000, 3);
    let f = dsf_bytes(1, 64, std::slice::from_ref(&c0), None);
    let mut src = open_bytes(&f).unwrap();
    assert_eq!(src.format().channels, 1);
    assert_eq!(drain(&mut src), c0);
}

#[test]
fn dsf_trailing_id3_not_played_and_tail_padded() {
    let c0 = pattern(4097, 4);
    let c1 = pattern(4097, 5);
    let id3 = b"ID3\x03\x00\x00\x00\x00\x00\x0aTITLEXXXXX";
    let f = dsf_bytes(2, 64, &[c0.clone(), c1.clone()], Some(id3));
    let mut src = open_bytes(&f).unwrap();
    let bytes = drain(&mut src);
    assert_eq!(bytes.len(), 4097 * 2);
    assert_eq!(bytes, interleave(&c0, &c1));

    let path = PlaybackPath {
        source: src.format(),
        device: DeviceFormat {
            sample_rate: 176_400,
            channels: 2,
            encoding: SampleEncoding::Int {
                container: 32,
                valid: 24,
            },
        },
        kind: PathKind::Dop,
    };
    let mut packer = Packer::new(&path);
    let mut out = Vec::new();
    packer.pack(Block::Dsd(&bytes), &mut out);
    assert_eq!(out.len(), 2048 * 8);
    packer.finish(&mut out);
    assert_eq!(out.len(), 2049 * 8);
    // 2048 frames sent, so the tail frame uses marker index 0.
    let m = DOP_MARKERS[0];
    assert_eq!(
        &out[2048 * 8..],
        &[0, DSD_IDLE, c0[4096], m, 0, DSD_IDLE, c1[4096], m]
    );
}

#[test]
fn dsf_format_and_duration() {
    let c = vec![vec![0x55u8; 8_820]; 2];
    let f = dsf_bytes(2, 128, &c, None);
    let src = open_bytes(&f).unwrap();
    assert_eq!(
        src.format(),
        SourceFormat {
            codec: Codec::Dsf,
            sample_rate: 5_644_800,
            channels: 2,
            kind: SourceKind::Dsd { multiple: 128 },
        }
    );
    // 8820 bytes * 8 bits / 5_644_800 Hz = 12.5 ms
    assert_eq!(src.duration(), Some(Duration::from_micros(12_500)));
}

#[test]
fn dsf_seek_rounds_to_dop_frame() {
    let c0 = pattern(400_000, 6);
    let c1 = pattern(400_000, 7);
    let f = dsf_bytes(2, 64, &[c0.clone(), c1.clone()], None);
    let mut src = open_bytes(&f).unwrap();
    let reached = src.seek(Duration::from_secs(1)).unwrap();
    assert_eq!(reached, Duration::from_secs(1));
    assert_eq!(drain(&mut src), interleave(&c0[352_800..], &c1[352_800..]));

    // 1 s + 3 us = 352_800.375 bytes -> floor 352_800
    let reached = src.seek(Duration::from_nanos(1_000_003_000)).unwrap();
    assert_eq!(reached, Duration::from_secs(1));
    // 3 bytes' worth of time (8.5 us) -> byte 2 (odd 3 rounded down to even)
    let reached = src.seek(Duration::from_nanos(8_700)).unwrap();
    assert_eq!(
        reached,
        Duration::from_nanos(2 * 8 * 1_000_000_000 / 2_822_400)
    );
    // seek into the middle of a block: the remainder of that block comes first
    src.seek(Duration::from_micros(1_000)).unwrap(); // 352 bytes
    let first = match src.next_block().unwrap().unwrap() {
        Block::Dsd(d) => d.to_vec(),
        _ => panic!(),
    };
    assert_eq!(first, interleave(&c0[352..4096], &c1[352..4096]));
}

#[test]
fn dsf_seek_past_end_clamps() {
    let c = vec![pattern(5_000, 8), pattern(5_000, 9)];
    let f = dsf_bytes(2, 64, &c, None);
    let mut src = open_bytes(&f).unwrap();
    let reached = src.seek(Duration::from_secs(60)).unwrap();
    assert_eq!(Some(reached), src.duration());
    assert!(src.next_block().unwrap().is_none());
}

#[test]
fn dsf_truncated_data_stops_without_error() {
    let c0 = pattern(10_000, 10);
    let c1 = pattern(10_000, 11);
    let f = dsf_bytes(2, 64, &[c0.clone(), c1.clone()], None);
    // Cut inside block group 1: ch0's block is complete, ch1's is only partly there.
    let cut = 92 + 8192 + 4096 + 100;
    let mut src = open_bytes(&f[..cut]).unwrap();
    let got = drain(&mut src);
    assert_eq!(got, interleave(&c0[..4096 + 100], &c1[..4096 + 100]));
}

#[test]
fn dsf_rejects_bad_header() {
    let c = vec![vec![0u8; 4096]; 2];
    let good = dsf_bytes(2, 64, &c, None);
    let corrupt = |bytes: &[u8]| matches!(open_bytes(bytes), Err(DecodeError::Corrupt(_)));

    assert!(open_bytes(&good).is_ok());
    assert!(corrupt(&good[..60])); // truncated fmt chunk
    assert!(corrupt(&good[..10])); // truncated DSD chunk
    assert!(corrupt(&good[..85])); // truncated data header
    let mut b = good.clone();
    b[44] = 1; // format id
    assert!(corrupt(&b));
    let mut b = good.clone();
    b[60] = 8; // bits per sample
    assert!(corrupt(&b));
    let mut b = good.clone();
    b[72..76].copy_from_slice(&2048u32.to_le_bytes()); // block size
    assert!(corrupt(&b));
    let mut b = good.clone();
    b[52] = 0; // channel count
    assert!(corrupt(&b));
    let mut b = good.clone();
    b[0] = b'X'; // magic
    assert!(corrupt(&b));
}

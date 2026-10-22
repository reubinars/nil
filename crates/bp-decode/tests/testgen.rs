use bp_decode::testgen::{dff_bytes, dsf_bytes};

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn u64be(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

#[test]
fn testgen_dsf_structure() {
    let mut c0 = vec![0u8; 5000];
    c0[0] = 0x80;
    let c1 = vec![0xAA; 5000];
    let f = dsf_bytes(2, 1, &[c0, c1], None);
    assert_eq!(&f[0..4], b"DSD ");
    assert_eq!(u64le(&f, 4), 28);
    assert_eq!(u64le(&f, 12), f.len() as u64);
    assert_eq!(u64le(&f, 20), 0);
    assert_eq!(&f[28..32], b"fmt ");
    assert_eq!(u64le(&f, 32), 52);
    assert_eq!(u32le(&f, 40), 1); // version
    assert_eq!(u32le(&f, 44), 0); // format id
    assert_eq!(u32le(&f, 48), 2); // channel type
    assert_eq!(u32le(&f, 52), 2); // channels
    assert_eq!(u32le(&f, 56), 44_100);
    assert_eq!(u32le(&f, 60), 1); // bits
    assert_eq!(u64le(&f, 64), 40_000); // samples per channel
    assert_eq!(u32le(&f, 72), 4096);
    assert_eq!(&f[80..84], b"data");
    assert_eq!(u64le(&f, 84), 12 + 2 * 2 * 4096);
    assert_eq!(f[92], 0x01);
    assert_eq!(f.len(), 92 + 2 * 2 * 4096);
    // channel 1 block 0 follows channel 0 block 0; 0xAA reversed = 0x55
    assert_eq!(f[92 + 4096], 0x55);
    // zero padding in the last block of channel 0 (904 bytes used of 4096)
    assert_eq!(f[92 + 2 * 4096 + 903], 0);
    assert_eq!(f[92 + 2 * 4096 + 905], 0);
}

#[test]
fn testgen_dsf_id3_pointer_and_multiple() {
    let id3 = b"ID3\x04\x00\x00\x00\x00\x00\x00";
    let f = dsf_bytes(1, 2, &[vec![0xFF; 4096]], Some(id3));
    assert_eq!(u32le(&f, 48), 1);
    assert_eq!(u32le(&f, 56), 88_200);
    let ptr = u64le(&f, 20) as usize;
    assert_eq!(&f[ptr..], id3);
    assert_eq!(u64le(&f, 12), f.len() as u64);
}

#[test]
fn testgen_dff_structure() {
    let f = dff_bytes(2, 1, &[vec![1, 2, 3], vec![4, 5, 6]], false);
    assert_eq!(&f[0..4], b"FRM8");
    assert_eq!(u64be(&f, 4), f.len() as u64 - 12);
    assert_eq!(&f[12..16], b"DSD ");
    assert_eq!(&f[16..20], b"FVER");
    // data is byte-interleaved at the end of the file
    assert_eq!(&f[f.len() - 6..], &[1, 4, 2, 5, 3, 6]);
    assert!(f.windows(4).any(|w| w == b"COMT"));
    assert!(f.windows(4).any(|w| w == b"SLFT"));
}

#[test]
fn testgen_dff_dst_flag() {
    let f = dff_bytes(1, 1, &[vec![1, 2]], true);
    assert!(f.windows(4).any(|w| w == b"CMPR"));
    assert!(f.windows(4).any(|w| w == b"DST "));
}

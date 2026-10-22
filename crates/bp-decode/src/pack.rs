pub use bp_core::{DOP_MARKERS, DSD_IDLE};
use bp_core::{PathKind, PlaybackPath, SampleEncoding, SourceKind};

use crate::Block;

enum Mode {
    /// Integer container: `src_bits` source bits left-justified into `container` bits.
    Int {
        container: u8,
        src_bits: u8,
    },
    Float,
    Dop {
        container: u8,
        channels: usize,
        /// Index into `DOP_MARKERS` for the next frame.
        marker: usize,
        /// Input bytes not yet forming a full DoP frame.
        leftover: Vec<u8>,
    },
}

/// Turns decoded blocks into the exact bytes sent to the device.
pub struct Packer {
    mode: Mode,
}

impl Packer {
    pub fn new(path: &PlaybackPath) -> Packer {
        let mode = match (path.kind, path.device.encoding) {
            (PathKind::Dop, SampleEncoding::Int { container, .. }) => Mode::Dop {
                container,
                channels: usize::from(path.device.channels),
                marker: 0,
                leftover: Vec::new(),
            },
            (PathKind::Dop, SampleEncoding::Float32) => {
                panic!("DoP cannot be packed into a float device encoding")
            }
            (_, SampleEncoding::Float32) => Mode::Float,
            (_, SampleEncoding::Int { container, .. }) => {
                let SourceKind::Pcm { bits, .. } = path.source.kind else {
                    panic!("DSD source on a non-DoP path");
                };
                assert!(bits <= container, "source bits exceed device container");
                Mode::Int {
                    container,
                    src_bits: bits,
                }
            }
        };
        Packer { mode }
    }

    /// Appends whole device frames to `out`.
    ///
    /// Precondition: for PCM and float blocks the slice length is a multiple of the channel
    /// count. DSD blocks may be any length; a partial DoP frame is kept for the next call.
    /// A block variant that does not match the path is a programming error and panics.
    pub fn pack(&mut self, block: Block<'_>, out: &mut Vec<u8>) {
        match (&mut self.mode, block) {
            (
                Mode::Int {
                    container,
                    src_bits,
                },
                Block::Pcm(samples),
            ) => {
                let shift = u32::from(*container - *src_bits);
                let n = usize::from(*container) / 8;
                out.reserve(samples.len() * n);
                for &s in samples {
                    let word = (s as u32) << shift;
                    out.extend_from_slice(&word.to_le_bytes()[..n]);
                }
            }
            (Mode::Float, Block::Float(samples)) => {
                out.reserve(samples.len() * 4);
                for &s in samples {
                    out.extend_from_slice(&s.to_le_bytes());
                }
            }
            (Mode::Dop { leftover, .. }, Block::Dsd(bytes)) => {
                leftover.extend_from_slice(bytes);
                self.drain_dop(out);
            }
            _ => panic!("block type does not match the playback path"),
        }
    }

    /// Flushes leftover DSD bytes, padding the final DoP frame with [`DSD_IDLE`].
    /// Emits nothing when there is no leftover.
    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if let Mode::Dop {
            channels, leftover, ..
        } = &mut self.mode
        {
            if leftover.is_empty() {
                return;
            }
            // Leftover is always < one frame, so this pads to exactly one frame.
            leftover.resize(2 * *channels, DSD_IDLE);
            self.drain_dop(out);
        }
    }

    fn drain_dop(&mut self, out: &mut Vec<u8>) {
        let Mode::Dop {
            container,
            channels,
            marker,
            leftover,
        } = &mut self.mode
        else {
            return;
        };
        let ch_count = *channels;
        let frame_in = 2 * ch_count;
        let n = usize::from(*container) / 8;
        let frames = leftover.len() / frame_in;
        out.reserve(frames * ch_count * n);
        for frame in leftover[..frames * frame_in].chunks_exact(frame_in) {
            let m = u32::from(DOP_MARKERS[*marker]);
            *marker ^= 1;
            for ch in 0..ch_count {
                // Input is byte-interleaved: the earlier byte of a channel comes first.
                let b0 = u32::from(frame[ch]);
                let b1 = u32::from(frame[ch_count + ch]);
                let word24 = m << 16 | b0 << 8 | b1;
                if n == 4 {
                    out.extend_from_slice(&(word24 << 8).to_le_bytes());
                } else {
                    out.extend_from_slice(&word24.to_le_bytes()[..n]);
                }
            }
        }
        leftover.drain(..frames * frame_in);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bp_core::{Codec, DeviceFormat, SourceFormat};

    fn i16_16() -> SampleEncoding {
        SampleEncoding::Int {
            container: 16,
            valid: 16,
        }
    }
    fn i24_24() -> SampleEncoding {
        SampleEncoding::Int {
            container: 24,
            valid: 24,
        }
    }
    fn i32_24() -> SampleEncoding {
        SampleEncoding::Int {
            container: 32,
            valid: 24,
        }
    }
    fn i32_32() -> SampleEncoding {
        SampleEncoding::Int {
            container: 32,
            valid: 32,
        }
    }

    fn path(
        kind: SourceKind,
        channels: u16,
        encoding: SampleEncoding,
        pk: PathKind,
    ) -> PlaybackPath {
        PlaybackPath {
            source: SourceFormat {
                codec: Codec::Flac,
                sample_rate: 44_100,
                channels,
                kind,
            },
            device: DeviceFormat {
                sample_rate: 44_100,
                channels,
                encoding,
            },
            kind: pk,
        }
    }

    /// Packs interleaved PCM as stereo (an odd sample count is packed as mono).
    fn pack_pcm(bits: u8, encoding: SampleEncoding, samples: &[i32]) -> Vec<u8> {
        let channels = if samples.len().is_multiple_of(2) {
            2
        } else {
            1
        };
        let p = path(
            SourceKind::Pcm { bits, float: false },
            channels,
            encoding,
            PathKind::Padded { pad_bits: 0 },
        );
        let mut out = Vec::new();
        Packer::new(&p).pack(Block::Pcm(samples), &mut out);
        out
    }

    fn dop_path(encoding: SampleEncoding) -> PlaybackPath {
        path(SourceKind::Dsd { multiple: 64 }, 2, encoding, PathKind::Dop)
    }

    #[test]
    fn pcm16_native() {
        assert_eq!(
            pack_pcm(16, i16_16(), &[1, -2]),
            vec![0x01, 0x00, 0xFE, 0xFF]
        );
    }
    #[test]
    fn pcm16_into_32_32() {
        assert_eq!(pack_pcm(16, i32_32(), &[0x1234]), vec![0, 0, 0x34, 0x12]);
    }
    #[test]
    fn pcm16_into_24_24() {
        assert_eq!(pack_pcm(16, i24_24(), &[-1]), vec![0x00, 0xFF, 0xFF]);
    }
    #[test]
    fn pcm24_into_32_24() {
        assert_eq!(
            pack_pcm(24, i32_24(), &[0x123456]),
            vec![0, 0x56, 0x34, 0x12]
        );
    }
    #[test]
    fn padding_is_reversible() {
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        let samples: Vec<i32> = (0..1000)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                i32::from((state >> 48) as u16 as i16)
            })
            .collect();
        let bytes = pack_pcm(16, i32_32(), &samples);
        let back: Vec<i32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes(*c) >> 16)
            .collect();
        assert_eq!(back, samples);
    }
    #[test]
    fn float_is_le_bytes() {
        let p = path(
            SourceKind::Pcm {
                bits: 32,
                float: true,
            },
            1,
            SampleEncoding::Float32,
            PathKind::Native,
        );
        let mut out = Vec::new();
        Packer::new(&p).pack(Block::Float(&[1.5, -0.25]), &mut out);
        let mut want = 1.5f32.to_le_bytes().to_vec();
        want.extend_from_slice(&(-0.25f32).to_le_bytes());
        assert_eq!(out, want);
    }
    #[test]
    fn dop_markers_alternate_and_shared_across_channels() {
        let mut out = Vec::new();
        let input = [0xA1, 0xB1, 0xA2, 0xB2, 0xA3, 0xB3, 0xA4, 0xB4];
        Packer::new(&dop_path(i32_24())).pack(Block::Dsd(&input), &mut out);
        assert_eq!(
            out,
            vec![
                0x00, 0xA2, 0xA1, 0x05, 0x00, 0xB2, 0xB1, 0x05, //
                0x00, 0xA4, 0xA3, 0xFA, 0x00, 0xB4, 0xB3, 0xFA
            ]
        );
    }
    #[test]
    fn dop_24_container() {
        let mut out = Vec::new();
        Packer::new(&dop_path(i24_24())).pack(Block::Dsd(&[0xA1, 0xB1, 0xA2, 0xB2]), &mut out);
        assert_eq!(out, vec![0xA2, 0xA1, 0x05, 0xB2, 0xB1, 0x05]);
    }
    #[test]
    fn dop_continues_across_pack_calls() {
        let input = [0xA1, 0xB1, 0xA2, 0xB2, 0xA3, 0xB3, 0xA4, 0xB4];
        let mut whole = Vec::new();
        Packer::new(&dop_path(i32_24())).pack(Block::Dsd(&input), &mut whole);
        let mut split = Vec::new();
        let mut p = Packer::new(&dop_path(i32_24()));
        p.pack(Block::Dsd(&input[..6]), &mut split);
        p.pack(Block::Dsd(&input[6..]), &mut split);
        assert_eq!(split, whole);
        assert_eq!(split[3], 0x05);
        assert_eq!(split[11], 0xFA);
    }
    #[test]
    fn dop_finish_pads_with_idle() {
        let mut p = Packer::new(&dop_path(i32_24()));
        let mut out = Vec::new();
        p.pack(Block::Dsd(&[0xAA, 0xBB]), &mut out);
        assert!(out.is_empty());
        p.finish(&mut out);
        assert_eq!(out, vec![0x00, 0x69, 0xAA, 0x05, 0x00, 0x69, 0xBB, 0x05]);
        p.finish(&mut out);
        assert_eq!(out.len(), 8);
    }
}

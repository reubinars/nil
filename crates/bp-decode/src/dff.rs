use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::time::Duration;

use bp_core::{Codec, DecodeError, SourceFormat, SourceKind};

use crate::dsd::{bytes_to_duration, seek_target};
use crate::{Block, Source};

/// Upper bound of bytes read per `next_block` call; rounded down to whole frames.
const PIECE: usize = 64 * 1024;
const CHUNK_HEADER: u64 = 12;

fn corrupt(msg: &str) -> DecodeError {
    DecodeError::Corrupt(format!("DFF: {msg}"))
}

/// Reads exactly `buf.len()` bytes; a short file is a corrupt header.
fn read_exact_at(file: &mut File, at: u64, buf: &mut [u8]) -> Result<(), DecodeError> {
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(buf).map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => corrupt("truncated header"),
        _ => DecodeError::Io(e),
    })
}

/// DFF (DSDIFF) source for uncompressed DSD: byte-interleaved, MSB-first.
pub struct DffSource {
    file: File,
    fs: u32,
    channels: usize,
    /// File offset of the first data byte.
    data_start: u64,
    /// Playable bytes per channel.
    total: u64,
    /// Next byte (per channel) to deliver.
    pos: u64,
    buf: Vec<u8>,
}

impl DffSource {
    pub fn open(mut file: File) -> Result<Self, DecodeError> {
        let file_len = file.metadata()?.len();

        let mut head = [0u8; 16];
        read_exact_at(&mut file, 0, &mut head)?;
        if &head[0..4] != b"FRM8" || &head[12..16] != b"DSD " {
            return Err(corrupt("not a DSDIFF file"));
        }

        let mut fs: Option<u32> = None;
        let mut channels: Option<u16> = None;
        let mut pos = 16u64;
        let (data_start, data_size) = loop {
            if pos + CHUNK_HEADER > file_len {
                return Err(corrupt("no DSD data chunk"));
            }
            let mut h = [0u8; CHUNK_HEADER as usize];
            read_exact_at(&mut file, pos, &mut h)?;
            let id: [u8; 4] = h[0..4].try_into().unwrap();
            let size = u64::from_be_bytes(h[4..12].try_into().unwrap());
            let body = pos + CHUNK_HEADER;
            let end = body
                .checked_add(size)
                .ok_or_else(|| corrupt("chunk size overflows"))?;

            match &id {
                b"DSD " => break (body, size),
                b"DST " => return Err(DecodeError::DstUnsupported),
                _ if end > file_len => return Err(corrupt("chunk runs past end of file")),
                // Descend into the property chunk: its children follow its "SND " tag.
                b"PROP" => {
                    pos = body + 4;
                    continue;
                }
                b"FS  " => {
                    if size < 4 {
                        return Err(corrupt("FS chunk too short"));
                    }
                    let mut v = [0u8; 4];
                    read_exact_at(&mut file, body, &mut v)?;
                    fs = Some(u32::from_be_bytes(v));
                }
                b"CHNL" => {
                    if size < 2 {
                        return Err(corrupt("CHNL chunk too short"));
                    }
                    let mut v = [0u8; 2];
                    read_exact_at(&mut file, body, &mut v)?;
                    channels = Some(u16::from_be_bytes(v));
                }
                b"CMPR" => {
                    if size < 4 {
                        return Err(corrupt("CMPR chunk too short"));
                    }
                    let mut v = [0u8; 4];
                    read_exact_at(&mut file, body, &mut v)?;
                    match &v {
                        b"DSD " => {}
                        b"DST " => return Err(DecodeError::DstUnsupported),
                        _ => return Err(corrupt("unknown compression type")),
                    }
                }
                _ => {}
            }
            pos = end + (size & 1);
        };

        let fs = fs.ok_or_else(|| corrupt("missing FS chunk"))?;
        let channels = channels.ok_or_else(|| corrupt("missing CHNL chunk"))?;
        if channels == 0 {
            return Err(corrupt("channel count is zero"));
        }
        if fs == 0 || !fs.is_multiple_of(44_100) || u16::try_from(fs / 44_100).is_err() {
            return Err(corrupt("sampling frequency is not a multiple of 44100"));
        }

        // Truncated data is not an error: play what is actually there.
        let avail = data_size.min(file_len.saturating_sub(data_start));
        let total = avail / u64::from(channels);

        let frames_per_piece = PIECE / usize::from(channels);
        Ok(DffSource {
            file,
            fs,
            channels: usize::from(channels),
            data_start,
            total,
            pos: 0,
            buf: Vec::with_capacity(frames_per_piece * usize::from(channels)),
        })
    }
}

impl Source for DffSource {
    fn format(&self) -> SourceFormat {
        SourceFormat {
            codec: Codec::Dff,
            sample_rate: self.fs,
            channels: self.channels as u16,
            kind: SourceKind::Dsd {
                multiple: (self.fs / 44_100) as u16,
            },
        }
    }

    fn duration(&self) -> Option<Duration> {
        Some(bytes_to_duration(self.total, self.fs))
    }

    fn next_block(&mut self) -> Result<Option<Block<'_>>, DecodeError> {
        if self.pos >= self.total {
            return Ok(None);
        }
        let frames = ((PIECE / self.channels) as u64).min(self.total - self.pos) as usize;
        let len = frames * self.channels;
        self.file.seek(SeekFrom::Start(
            self.data_start + self.pos * self.channels as u64,
        ))?;
        self.buf.resize(len, 0);
        self.file.read_exact(&mut self.buf)?;
        self.pos += frames as u64;
        Ok(Some(Block::Dsd(&self.buf)))
    }

    fn seek(&mut self, to: Duration) -> Result<Duration, DecodeError> {
        self.pos = seek_target(to, self.fs, self.total);
        Ok(bytes_to_duration(self.pos, self.fs))
    }
}

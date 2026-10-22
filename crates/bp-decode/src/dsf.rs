use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::time::Duration;

use bp_core::{Codec, DecodeError, SourceFormat, SourceKind};

use crate::dsd::{bytes_to_duration, seek_target};
use crate::{Block, Source};

const BLOCK: usize = 4096;
/// Sizes of the "DSD " chunk, the "fmt " chunk and the "data" chunk header (all with headers).
const DSD_CHUNK: usize = 28;
const FMT_CHUNK: usize = 52;
const DATA_HEADER: usize = 12;

fn corrupt(msg: &str) -> DecodeError {
    DecodeError::Corrupt(format!("DSF: {msg}"))
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Reads exactly `buf.len()` bytes; a short file is a corrupt header.
fn read_header(file: &mut File, buf: &mut [u8], what: &str) -> Result<(), DecodeError> {
    file.read_exact(buf).map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => corrupt(&format!("truncated {what}")),
        _ => DecodeError::Io(e),
    })
}

/// Reads until `buf` is full or the file ends; returns the bytes read.
fn read_up_to(file: &mut File, buf: &mut [u8]) -> Result<usize, DecodeError> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(n)
}

/// DSF (Sony DSD stream file) source: yields MSB-first, byte-interleaved DSD.
pub struct DsfSource {
    file: File,
    fs: u32,
    channels: usize,
    /// File offset of the first data byte.
    data_start: u64,
    /// Playable bytes per channel.
    total: u64,
    /// Next byte (per channel) to deliver.
    pos: u64,
    /// One block group (BLOCK bytes per channel) as stored in the file.
    raw: Vec<u8>,
    out: Vec<u8>,
}

impl DsfSource {
    pub fn open(mut file: File) -> Result<Self, DecodeError> {
        let file_len = file.metadata()?.len();
        file.seek(SeekFrom::Start(0))?;

        let mut dsd = [0u8; DSD_CHUNK];
        read_header(&mut file, &mut dsd, "DSD chunk")?;
        if &dsd[0..4] != b"DSD " || u64le(&dsd, 4) != DSD_CHUNK as u64 {
            return Err(corrupt("missing DSD chunk"));
        }

        let mut fmt = [0u8; FMT_CHUNK];
        read_header(&mut file, &mut fmt, "fmt chunk")?;
        if &fmt[0..4] != b"fmt " || u64le(&fmt, 4) != FMT_CHUNK as u64 {
            return Err(corrupt("missing or malformed fmt chunk"));
        }
        if u32le(&fmt, 16) != 0 {
            return Err(corrupt("unsupported format id (only raw DSD)"));
        }
        let channel_num = u32le(&fmt, 24);
        if channel_num == 0 {
            return Err(corrupt("channel count is zero"));
        }
        let fs = u32le(&fmt, 28);
        if fs == 0 || !fs.is_multiple_of(44_100) {
            return Err(corrupt("sampling frequency is not a multiple of 44100"));
        }
        if u32le(&fmt, 32) != 1 {
            return Err(corrupt("only 1 bit per sample is supported"));
        }
        let sample_count = u64le(&fmt, 36);
        if u32le(&fmt, 44) != BLOCK as u32 {
            return Err(corrupt("block size per channel must be 4096"));
        }
        if u16::try_from(channel_num).is_err() || u16::try_from(fs / 44_100).is_err() {
            return Err(corrupt("channel count or rate out of range"));
        }
        let channels = channel_num as usize;

        let mut data = [0u8; DATA_HEADER];
        read_header(&mut file, &mut data, "data chunk header")?;
        if &data[0..4] != b"data" {
            return Err(corrupt("missing data chunk"));
        }
        let data_len = u64le(&data, 4)
            .checked_sub(DATA_HEADER as u64)
            .ok_or_else(|| corrupt("data chunk size too small"))?;
        let data_start = (DSD_CHUNK + FMT_CHUNK + DATA_HEADER) as u64;

        // Truncated data is not an error: play what is actually there. In the last,
        // partly present block group only the bytes present for every channel count.
        let avail = data_len.min(file_len.saturating_sub(data_start));
        let group = (BLOCK * channels) as u64;
        let partial = (avail % group).saturating_sub((BLOCK * (channels - 1)) as u64);
        let avail_per_channel = avail / group * BLOCK as u64 + partial.min(BLOCK as u64);
        let total = (sample_count / 8).min(avail_per_channel);

        Ok(DsfSource {
            file,
            fs,
            channels,
            data_start,
            total,
            pos: 0,
            raw: vec![0; BLOCK * channels],
            out: Vec::with_capacity(BLOCK * channels),
        })
    }
}

impl Source for DsfSource {
    fn format(&self) -> SourceFormat {
        SourceFormat {
            codec: Codec::Dsf,
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
        let block_index = self.pos / BLOCK as u64;
        let offset = (self.pos % BLOCK as u64) as usize;
        let n = (BLOCK - offset).min((self.total - self.pos) as usize);

        let group = (BLOCK * self.channels) as u64;
        self.file
            .seek(SeekFrom::Start(self.data_start + block_index * group))?;
        // The last group may be short; `total` already bounds `n` to bytes that exist.
        self.raw.fill(0);
        read_up_to(&mut self.file, &mut self.raw)?;

        self.out.clear();
        for i in 0..n {
            for ch in 0..self.channels {
                self.out
                    .push(self.raw[ch * BLOCK + offset + i].reverse_bits());
            }
        }
        self.pos += n as u64;
        Ok(Some(Block::Dsd(&self.out)))
    }

    fn seek(&mut self, to: Duration) -> Result<Duration, DecodeError> {
        self.pos = seek_target(to, self.fs, self.total);
        Ok(bytes_to_duration(self.pos, self.fs))
    }
}

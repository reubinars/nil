//! Decoding sources and packing their samples into device bytes.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use bp_core::{Codec, DecodeError, SourceFormat};

mod dff;
mod dsd;
mod dsf;
mod mp4_trim;
mod pack;
mod pcm;
#[cfg(any(test, feature = "testgen"))]
pub mod testgen;

pub use dff::DffSource;
pub use dsf::DsfSource;
pub use pack::{DOP_MARKERS, DSD_IDLE, Packer};
pub use pcm::PcmSource;

/// A block of decoded audio, borrowed from the source.
pub enum Block<'a> {
    /// Interleaved, original sample values (right-justified, sign-extended).
    Pcm(&'a [i32]),
    /// Interleaved floats.
    Float(&'a [f32]),
    /// Byte-interleaved per channel, MSB-first (oldest bit = bit 7).
    Dsd(&'a [u8]),
}

pub trait Source: Send {
    fn format(&self) -> SourceFormat;
    fn duration(&self) -> Option<Duration>;
    /// `None` = end of track.
    fn next_block(&mut self) -> Result<Option<Block<'_>>, DecodeError>;
    /// Returns the position reached.
    fn seek(&mut self, to: Duration) -> Result<Duration, DecodeError>;
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

/// Opens a file, dispatching by content, not extension. Leading ID3v2 tags are skipped.
pub fn open(path: &Path) -> Result<Box<dyn Source>, DecodeError> {
    let mut file = File::open(path)?;
    let mut offset = 0u64;
    let mut head = [0u8; 12];
    let n = loop {
        file.seek(SeekFrom::Start(offset))?;
        let n = read_up_to(&mut file, &mut head)?;
        if n >= 10 && &head[..3] == b"ID3" {
            let size = head[6..10]
                .iter()
                .fold(0u64, |acc, &b| (acc << 7) | u64::from(b & 0x7f));
            let footer = if head[5] & 0x10 != 0 { 10 } else { 0 };
            offset += 10 + size + footer;
        } else {
            break n;
        }
    };
    file.seek(SeekFrom::Start(0))?;
    let head = &head[..n];
    let starts = |at: usize, tag: &[u8]| head.get(at..at + tag.len()) == Some(tag);

    Ok(if starts(0, b"fLaC") {
        Box::new(PcmSource::open_at(file, offset, Some(Codec::Flac))?)
    } else if starts(0, b"RIFF") && starts(8, b"WAVE") {
        Box::new(PcmSource::open_at(file, offset, Some(Codec::Wav))?)
    } else if starts(0, b"FORM") && (starts(8, b"AIFF") || starts(8, b"AIFC")) {
        Box::new(PcmSource::open_at(file, offset, Some(Codec::Aiff))?)
    } else if starts(4, b"ftyp") {
        Box::new(PcmSource::open_at(file, offset, None)?)
    } else if offset == 0 && starts(0, b"DSD ") {
        Box::new(DsfSource::open(file)?)
    } else if offset == 0 && starts(0, b"FRM8") {
        Box::new(DffSource::open(file)?)
    } else {
        return Err(DecodeError::UnknownFormat);
    })
}

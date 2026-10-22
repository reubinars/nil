use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::time::Duration;

use bp_core::{Codec, DecodeError, SourceFormat, SourceKind};

use crate::mp4_trim::{self, Trim};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::*;
use symphonia::core::codecs::audio::{AudioCodecId, AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

/// Integer PCM in any width, sign and byte order, plus the lossless integer codecs: decoded
/// samples equal the stored ones.
fn is_lossless_int(id: AudioCodecId) -> bool {
    [
        CODEC_ID_PCM_S32LE,
        CODEC_ID_PCM_S32LE_PLANAR,
        CODEC_ID_PCM_S32BE,
        CODEC_ID_PCM_S32BE_PLANAR,
        CODEC_ID_PCM_S24LE,
        CODEC_ID_PCM_S24LE_PLANAR,
        CODEC_ID_PCM_S24BE,
        CODEC_ID_PCM_S24BE_PLANAR,
        CODEC_ID_PCM_S16LE,
        CODEC_ID_PCM_S16LE_PLANAR,
        CODEC_ID_PCM_S16BE,
        CODEC_ID_PCM_S16BE_PLANAR,
        CODEC_ID_PCM_S8,
        CODEC_ID_PCM_S8_PLANAR,
        CODEC_ID_PCM_U32LE,
        CODEC_ID_PCM_U32LE_PLANAR,
        CODEC_ID_PCM_U32BE,
        CODEC_ID_PCM_U32BE_PLANAR,
        CODEC_ID_PCM_U24LE,
        CODEC_ID_PCM_U24LE_PLANAR,
        CODEC_ID_PCM_U24BE,
        CODEC_ID_PCM_U24BE_PLANAR,
        CODEC_ID_PCM_U16LE,
        CODEC_ID_PCM_U16LE_PLANAR,
        CODEC_ID_PCM_U16BE,
        CODEC_ID_PCM_U16BE_PLANAR,
        CODEC_ID_PCM_U8,
        CODEC_ID_PCM_U8_PLANAR,
        CODEC_ID_FLAC,
        CODEC_ID_ALAC,
    ]
    .contains(&id)
}

/// 32-bit float PCM, passed through as stored.
fn is_float32(id: AudioCodecId) -> bool {
    [
        CODEC_ID_PCM_F32LE,
        CODEC_ID_PCM_F32LE_PLANAR,
        CODEC_ID_PCM_F32BE,
        CODEC_ID_PCM_F32BE_PLANAR,
    ]
    .contains(&id)
}

fn map_err(e: SymError) -> DecodeError {
    match e {
        SymError::IoError(e) => DecodeError::Io(e),
        SymError::Unsupported(_) => DecodeError::UnknownFormat,
        e => DecodeError::Corrupt(e.to_string()),
    }
}

/// A file viewed from `start` onwards, so the container sees its own offset 0.
struct Offset {
    file: File,
    start: u64,
}

impl Offset {
    fn new(mut file: File, start: u64) -> std::io::Result<Self> {
        file.seek(SeekFrom::Start(start))?;
        Ok(Self { file, start })
    }
}

impl Read for Offset {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl Seek for Offset {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let pos = match pos {
            SeekFrom::Start(n) => SeekFrom::Start(n + self.start),
            other => other,
        };
        Ok(self.file.seek(pos)?.saturating_sub(self.start))
    }
}

impl MediaSource for Offset {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.file
            .metadata()
            .ok()
            .map(|m| m.len().saturating_sub(self.start))
    }
}

/// Bit depth from an ALAC magic cookie (optionally wrapped in `frma` and `alac` atoms).
fn alac_bit_depth(mut cookie: &[u8]) -> Option<u32> {
    for tag in [b"frma", b"alac"] {
        if cookie.get(4..8) == Some(tag) {
            cookie = cookie.get(12..)?;
        }
    }
    cookie.get(5).map(|&b| u32::from(b))
}

/// Audio object type from an AAC AudioSpecificConfig: 5 bits, or `32 + 6 bits` after an escape of 31.
fn aac_object_type(asc: &[u8]) -> Option<u8> {
    let first = *asc.first()? >> 3;
    if first != 31 {
        return Some(first);
    }
    let bits = u16::from_be_bytes([asc[0], *asc.get(1)?]);
    Some(32 + ((bits >> 5) & 0x3f) as u8)
}

/// Frames per AAC-LC packet.
const AAC_FRAME: u64 = 1024;

/// Decoded samples, kept in a reusable buffer.
enum Out {
    Int {
        shift: u32,
        buf: Vec<i32>,
    },
    Float(Vec<f32>),
    /// Lossy float output rounded to 24-bit integers.
    Lossy {
        decoded: Vec<f32>,
        buf: Vec<i32>,
    },
}

/// PCM source (FLAC, WAV, AIFF, ALAC, AAC) decoded with symphonia.
pub struct PcmSource {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    format: SourceFormat,
    total_frames: Option<u64>,
    out: Out,
    /// Gapless window (AAC only).
    trim: Option<Trim>,
    /// Absolute decoded frame index of the next frame out of the decoder.
    pos: u64,
    /// Frames below this are decoded only to rebuild the decoder's state after a seek.
    floor: u64,
}

impl PcmSource {
    pub fn open(file: File, codec: Codec) -> Result<Self, DecodeError> {
        Self::open_at(file, 0, Some(codec))
    }

    /// Opens a stream whose audio container starts `offset` bytes into the file
    /// (after leading ID3v2 tags). `None` means an MP4 holding ALAC or AAC.
    pub(crate) fn open_at(
        file: File,
        offset: u64,
        codec: Option<Codec>,
    ) -> Result<Self, DecodeError> {
        let mut file = file;
        // MP4 only: the edit list is read before symphonia takes the file.
        let moov = if codec.is_none() {
            mp4_trim::read_moov(&mut file, offset)
        } else {
            None
        };
        let source = Offset::new(file, offset)?;
        let mss = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
        let reader = symphonia::default::get_probe()
            .probe(
                &Hint::new(),
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(map_err)?;

        let track = reader
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::UnknownFormat)?;
        let Some(CodecParameters::Audio(params)) = &track.codec_params else {
            return Err(DecodeError::UnknownFormat);
        };
        let codec = match codec {
            Some(Codec::Alac) | None if params.codec == CODEC_ID_ALAC => Codec::Alac,
            None if params.codec == CODEC_ID_AAC => Codec::Aac,
            Some(Codec::Alac) | None => return Err(DecodeError::UnknownFormat),
            Some(codec) => codec,
        };
        // Only the LC profile; HE-AAC (SBR, PS) would play as its LC core at half the rate.
        if codec == Codec::Aac
            && params
                .extra_data
                .as_deref()
                .is_some_and(|asc| aac_object_type(asc) != Some(2))
        {
            return Err(DecodeError::UnknownFormat);
        }

        let sample_rate = params
            .sample_rate
            .ok_or_else(|| DecodeError::Corrupt("no sample rate".into()))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .filter(|&c| c > 0)
            .ok_or_else(|| DecodeError::Corrupt("no channel layout".into()))?;
        let channels = u16::try_from(channels)
            .map_err(|_| DecodeError::Corrupt("too many channels".into()))?;

        let (bits, float) = if codec == Codec::Aac {
            (24, false)
        } else if is_float32(params.codec) {
            (32, true)
        } else if !is_lossless_int(params.codec) {
            // Companded (A-law/μ-law), 64-bit float and lossy codecs would be converted.
            return Err(DecodeError::UnknownFormat);
        } else {
            let bits = params
                .bits_per_sample
                .or_else(|| {
                    // MP4 does not carry the depth for ALAC; it lives in the magic cookie.
                    (params.codec == CODEC_ID_ALAC)
                        .then(|| params.extra_data.as_deref().and_then(alac_bit_depth))
                        .flatten()
                })
                .ok_or_else(|| DecodeError::Corrupt("no bit depth".into()))?;
            if !(8..=32).contains(&bits) {
                return Err(DecodeError::Corrupt(format!(
                    "unsupported bit depth {bits}"
                )));
            }
            (bits as u8, false)
        };

        let total_frames = track.num_frames;
        let trim = if codec == Codec::Aac {
            moov.and_then(|m| mp4_trim::trim_from(&m, sample_rate))
        } else {
            None
        };
        let track_id = track.id;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(map_err)?;
        // The decoder must run at the container's rate. Implicit HE-AAC does not: the AAC
        // decoder runs at the ASC's core rate, half the container's.
        if decoder.codec_params().sample_rate != Some(sample_rate) {
            return Err(DecodeError::UnknownFormat);
        }

        let out = if codec.is_lossy() {
            Out::Lossy {
                decoded: Vec::new(),
                buf: Vec::new(),
            }
        } else if float {
            Out::Float(Vec::new())
        } else {
            Out::Int {
                shift: 32 - u32::from(bits),
                buf: Vec::new(),
            }
        };
        Ok(Self {
            reader,
            decoder,
            track_id,
            format: SourceFormat {
                codec,
                sample_rate,
                channels,
                kind: SourceKind::Pcm { bits, float },
            },
            total_frames,
            out,
            trim,
            pos: 0,
            floor: 0,
        })
    }
}

impl crate::Source for PcmSource {
    fn format(&self) -> SourceFormat {
        self.format
    }

    fn duration(&self) -> Option<Duration> {
        let frames = match self.trim {
            Some(t) => t.frames,
            None => self.total_frames?,
        };
        Some(Duration::from_secs_f64(
            frames as f64 / f64::from(self.format.sample_rate),
        ))
    }

    fn next_block(&mut self) -> Result<Option<crate::Block<'_>>, DecodeError> {
        let end = self.trim.map(|t| t.skip + t.frames);
        loop {
            if end.is_some_and(|end| self.pos >= end) {
                return Ok(None);
            }
            let Some(packet) = self.reader.next_packet().map_err(map_err)? else {
                return Ok(None);
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let decoded = self.decoder.decode(&packet).map_err(map_err)?;
            let frames = decoded.frames() as u64;
            if frames == 0 {
                continue;
            }
            let start = self.pos;
            self.pos += frames;
            // The frames of this block inside the gapless window.
            let first = self.floor.max(self.trim.map_or(0, |t| t.skip));
            let end = self.trim.map_or(u64::MAX, |t| t.skip + t.frames);
            let (lo, hi) = (
                start.max(first).min(self.pos) - start,
                self.pos.min(end).max(start) - start,
            );
            if lo >= hi {
                continue;
            }
            let ch = usize::from(self.format.channels);
            let window = lo as usize * ch..hi as usize * ch;
            return Ok(Some(match &mut self.out {
                Out::Int { shift, buf } => {
                    decoded.copy_to_vec_interleaved::<i32>(buf);
                    // The decoders left-justify; an arithmetic shift restores the originals.
                    for s in buf.iter_mut() {
                        *s >>= *shift;
                    }
                    crate::Block::Pcm(&buf[window])
                }
                Out::Float(buf) => {
                    decoded.copy_to_vec_interleaved::<f32>(buf);
                    crate::Block::Float(&buf[window])
                }
                Out::Lossy { decoded: f, buf } => {
                    decoded.copy_to_vec_interleaved::<f32>(f);
                    buf.clear();
                    buf.extend(f.iter().map(|&x| {
                        (x * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32
                    }));
                    crate::Block::Pcm(&buf[window])
                }
            }));
        }
    }

    fn seek(&mut self, to: Duration) -> Result<Duration, DecodeError> {
        let rate = f64::from(self.format.sample_rate);
        let skip = self.trim.map_or(0, |t| t.skip);
        if let Some(t) = self.trim
            && to.as_secs_f64() * rate >= t.frames as f64
        {
            // Past the window: end the track rather than play padding.
            self.pos = t.skip + t.frames;
            return Ok(Duration::from_secs_f64(t.frames as f64 / rate));
        }
        // AAC overlaps each frame with the one before: decode one packet ahead of the target
        // and drop it, or the first packet after a reset comes out wrong.
        let preroll = if self.format.codec.is_lossy() {
            AAC_FRAME
        } else {
            0
        };
        let time = Time::try_from_secs_f64(
            (to.as_secs_f64() + (skip as f64 - preroll as f64) / rate).max(0.0),
        )
        .ok_or_else(|| DecodeError::Corrupt("seek target out of range".into()))?;
        let seeked = self
            .reader
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(map_err)?;
        self.decoder.reset();
        let pos = self
            .reader
            .tracks()
            .iter()
            .find(|t| t.id == self.track_id)
            .and_then(|t| t.time_base)
            .and_then(|tb| tb.calc_time(seeked.actual_ts))
            .map_or(0.0, |t| t.as_secs_f64())
            .max(0.0);
        self.pos = (pos * rate).round() as u64;
        self.floor = if self.pos > 0 { self.pos + preroll } else { 0 };
        Ok(Duration::from_secs_f64(
            self.pos.max(self.floor).saturating_sub(skip) as f64 / rate,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asc_object_types() {
        assert_eq!(aac_object_type(&[0x12, 0x10]), Some(2));
        assert_eq!(aac_object_type(&[0x2B, 0x92, 0x08, 0x00]), Some(5));
        assert_eq!(aac_object_type(&[0xEB, 0x09]), Some(29));
        assert_eq!(aac_object_type(&[0xF8, 0x00]), Some(32));
        assert_eq!(aac_object_type(&[]), None);
    }
}

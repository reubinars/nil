use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Container/codec of a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Codec {
    Flac,
    Wav,
    Alac,
    Aiff,
    Dsf,
    Dff,
    Aac,
}

impl Codec {
    /// True for codecs whose decoded samples are not the original ones.
    pub fn is_lossy(self) -> bool {
        self == Codec::Aac
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Codec::Flac => "FLAC",
            Codec::Wav => "WAV",
            Codec::Alac => "ALAC",
            Codec::Aiff => "AIFF",
            Codec::Dsf => "DSF",
            Codec::Dff => "DFF",
            Codec::Aac => "AAC",
        })
    }
}

/// Kind of sample data in a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceKind {
    Pcm { bits: u8, float: bool },
    Dsd { multiple: u16 },
}

/// Format of a source file. For DSD, `sample_rate` is the DSD bit rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceFormat {
    pub codec: Codec,
    pub sample_rate: u32,
    pub channels: u16,
    pub kind: SourceKind,
}

/// Sample encoding of a device stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum SampleEncoding {
    Int { container: u8, valid: u8 },
    Float32,
}

impl SampleEncoding {
    /// Bytes occupied by one sample in the container.
    pub fn container_bytes(&self) -> usize {
        match self {
            SampleEncoding::Int { container, .. } => usize::from(*container) / 8,
            SampleEncoding::Float32 => 4,
        }
    }
}

impl fmt::Display for SampleEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SampleEncoding::Int { container, valid } => write!(f, "int{container}/{valid}"),
            SampleEncoding::Float32 => f.write_str("float32"),
        }
    }
}

/// Error parsing a [`SampleEncoding`] from text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown sample encoding {0:?}")]
pub struct ParseEncodingError(pub String);

impl FromStr for SampleEncoding {
    type Err = ParseEncodingError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PROBE_ENCODINGS
            .into_iter()
            .find(|e| e.to_string() == s)
            .ok_or_else(|| ParseEncodingError(s.to_owned()))
    }
}

impl From<SampleEncoding> for String {
    fn from(e: SampleEncoding) -> String {
        e.to_string()
    }
}

impl TryFrom<String> for SampleEncoding {
    type Error = ParseEncodingError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// Format of a device stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub encoding: SampleEncoding,
}

impl DeviceFormat {
    pub fn bytes_per_frame(&self) -> usize {
        self.encoding.container_bytes() * usize::from(self.channels)
    }
}

/// How source samples reach the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathKind {
    Native,
    Padded { pad_bits: u8 },
    Dop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlaybackPath {
    pub source: SourceFormat,
    pub device: DeviceFormat,
    pub kind: PathKind,
}

/// DoP marker bytes, alternating per frame starting with the first.
pub const DOP_MARKERS: [u8; 2] = [0x05, 0xFA];
/// DSD idle pattern used to pad a final partial DoP frame and for DoP silence.
pub const DSD_IDLE: u8 = 0x69;

/// Sample rates probed on a device.
pub const PROBE_RATES: [u32; 10] = [
    44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000, 705_600, 768_000,
];

/// Sample encodings probed on a device.
pub const PROBE_ENCODINGS: [SampleEncoding; 5] = [
    SampleEncoding::Int {
        container: 16,
        valid: 16,
    },
    SampleEncoding::Int {
        container: 24,
        valid: 24,
    },
    SampleEncoding::Int {
        container: 32,
        valid: 24,
    },
    SampleEncoding::Int {
        container: 32,
        valid: 32,
    },
    SampleEncoding::Float32,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_aac_is_lossy() {
        assert_eq!(Codec::Aac.to_string(), "AAC");
        assert!(Codec::Aac.is_lossy());
        assert!(!Codec::Flac.is_lossy() && !Codec::Alac.is_lossy());
    }

    #[test]
    fn bytes_per_frame() {
        let f = DeviceFormat {
            sample_rate: 96_000,
            channels: 2,
            encoding: SampleEncoding::Int {
                container: 24,
                valid: 24,
            },
        };
        assert_eq!(f.bytes_per_frame(), 6);
        let f = DeviceFormat {
            encoding: SampleEncoding::Int {
                container: 32,
                valid: 24,
            },
            ..f
        };
        assert_eq!(f.bytes_per_frame(), 8);
    }

    #[test]
    fn encoding_text_roundtrip() {
        for e in PROBE_ENCODINGS {
            assert_eq!(e.to_string().parse::<SampleEncoding>().unwrap(), e);
        }
        assert_eq!(
            SampleEncoding::Int {
                container: 32,
                valid: 24
            }
            .to_string(),
            "int32/24"
        );
        assert!("int12/9".parse::<SampleEncoding>().is_err());
        assert!("int16/24".parse::<SampleEncoding>().is_err());
    }
}

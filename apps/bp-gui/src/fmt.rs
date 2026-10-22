//! Display labels for formats, devices and durations.

use std::time::Duration;

use bp_core::{PathKind, PlaybackPath, SampleEncoding, SourceFormat, SourceKind};

/// kHz with a trailing `.0` dropped: `44.1`, `192`, `352.8`.
pub fn khz_short(rate: u32) -> String {
    let s = format!("{:.1}", f64::from(rate) / 1000.0);
    s.strip_suffix(".0").map(str::to_owned).unwrap_or(s)
}

/// Compact label: `24/192`, `32f/48`, `DSD64`, `AAC/44.1`.
pub fn short_format(f: &SourceFormat) -> String {
    match f.kind {
        _ if f.codec.is_lossy() => format!("{}/{}", f.codec, khz_short(f.sample_rate)),
        SourceKind::Pcm { bits, float } => {
            let f_mark = if float { "f" } else { "" };
            format!("{bits}{f_mark}/{}", khz_short(f.sample_rate))
        }
        SourceKind::Dsd { multiple } => format!("DSD{multiple}"),
    }
}

/// Detailed label: `FLAC 24bit 192.0kHz`, `DSF DSD128 5.6MHz`, `AAC 44.1kHz lossy`.
pub fn long_source(f: &SourceFormat) -> String {
    match f.kind {
        _ if f.codec.is_lossy() => format!(
            "{} {:.1}kHz lossy",
            f.codec,
            f64::from(f.sample_rate) / 1000.0
        ),
        SourceKind::Pcm { bits, .. } => format!(
            "{} {bits}bit {:.1}kHz",
            f.codec,
            f64::from(f.sample_rate) / 1000.0
        ),
        SourceKind::Dsd { multiple } => format!(
            "{} DSD{multiple} {:.1}MHz",
            f.codec,
            f64::from(f.sample_rate) / 1e6
        ),
    }
}

/// Device name plus the stream the device receives: `DAC A 32bit`, `DAC A DoP`.
pub fn device_label(name: &str, path: &PlaybackPath) -> String {
    let stream = match (path.kind, path.device.encoding) {
        (PathKind::Dop, _) => "DoP".to_owned(),
        (_, SampleEncoding::Int { container, .. }) => format!("{container}bit"),
        (_, SampleEncoding::Float32) => "32f".to_owned(),
    };
    format!("{name} {stream}")
}

/// `m:ss`, or `h:mm:ss` from one hour up.
pub fn duration(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Placeholder-art initials: first letters of the first two words starting
/// with an uppercase letter; failing two of those, of the first two words
/// starting with any letter; else the first two characters; `?` if empty.
/// Uppercased.
pub fn initials(title: &str) -> String {
    let firsts: Vec<char> = title
        .split_whitespace()
        .filter_map(|w| w.chars().next().filter(|c| c.is_alphabetic()))
        .collect();
    let upper: Vec<char> = firsts
        .iter()
        .copied()
        .filter(|c| c.is_uppercase())
        .collect();
    let pick = if upper.len() >= 2 { upper } else { firsts };
    let letters: String = pick
        .into_iter()
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if !letters.is_empty() {
        return letters;
    }
    let first_two: String = title
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(2)
        .collect();
    if first_two.is_empty() {
        "?".to_owned()
    } else {
        first_two
    }
}

/// The compact tag shown on a tile, from an album's format summary: `FLAC 24/192` →
/// `24/192`, `DSF DSD64` → `DSD64`, `AAC AAC/44.1` → `AAC`, `Mixed` stays.
pub fn format_tag(summary: &str) -> String {
    let Some((codec, rest)) = summary.split_once(' ') else {
        return summary.to_owned();
    };
    if rest.starts_with(&format!("{codec}/")) {
        codec.to_owned()
    } else {
        rest.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bp_core::{Codec, DeviceFormat};

    fn pcm(codec: Codec, rate: u32, bits: u8) -> SourceFormat {
        SourceFormat {
            codec,
            sample_rate: rate,
            channels: 2,
            kind: SourceKind::Pcm { bits, float: false },
        }
    }
    fn float(codec: Codec, rate: u32) -> SourceFormat {
        SourceFormat {
            kind: SourceKind::Pcm {
                bits: 32,
                float: true,
            },
            ..pcm(codec, rate, 32)
        }
    }
    fn dsd(codec: Codec, multiple: u16) -> SourceFormat {
        SourceFormat {
            codec,
            sample_rate: 2_822_400 * u32::from(multiple) / 64,
            channels: 2,
            kind: SourceKind::Dsd { multiple },
        }
    }
    const INT32_24: SampleEncoding = SampleEncoding::Int {
        container: 32,
        valid: 24,
    };
    fn path(encoding: SampleEncoding, kind: PathKind) -> PlaybackPath {
        PlaybackPath {
            source: pcm(Codec::Flac, 96_000, 24),
            device: DeviceFormat {
                sample_rate: 96_000,
                channels: 2,
                encoding,
            },
            kind,
        }
    }

    #[test]
    fn short_format_labels() {
        assert_eq!(short_format(&pcm(Codec::Flac, 192_000, 24)), "24/192");
        assert_eq!(short_format(&pcm(Codec::Flac, 44_100, 16)), "16/44.1");
        assert_eq!(short_format(&pcm(Codec::Wav, 352_800, 24)), "24/352.8");
        assert_eq!(short_format(&float(Codec::Wav, 48_000)), "32f/48");
        assert_eq!(short_format(&dsd(Codec::Dsf, 64)), "DSD64");
    }

    #[test]
    fn aac_labels() {
        let aac = pcm(Codec::Aac, 44_100, 24);
        assert_eq!(short_format(&aac), "AAC/44.1");
        assert_eq!(long_source(&aac), "AAC 44.1kHz lossy");
    }

    #[test]
    fn long_source_labels() {
        assert_eq!(
            long_source(&pcm(Codec::Flac, 192_000, 24)),
            "FLAC 24bit 192.0kHz"
        );
        assert_eq!(
            long_source(&pcm(Codec::Alac, 44_100, 16)),
            "ALAC 16bit 44.1kHz"
        );
        assert_eq!(long_source(&dsd(Codec::Dsf, 128)), "DSF DSD128 5.6MHz");
    }

    #[test]
    fn device_labels() {
        assert_eq!(
            device_label("DAC A", &path(INT32_24, PathKind::Padded { pad_bits: 8 })),
            "DAC A 32bit"
        );
        assert_eq!(
            device_label("DAC A", &path(INT32_24, PathKind::Dop)),
            "DAC A DoP"
        );
        assert_eq!(
            device_label("DAC B", &path(SampleEncoding::Float32, PathKind::Native)),
            "DAC B 32f"
        );
    }

    #[test]
    fn durations_and_initials() {
        assert_eq!(duration(Duration::from_secs(562)), "9:22");
        assert_eq!(duration(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(initials("Kind of Blue"), "KB");
        assert_eq!(initials("in a silent way"), "IA");
        assert_eq!(initials("The Dark Side of the Moon"), "TD");
        assert_eq!(initials("Kid A"), "KA");
        assert_eq!(initials("1999"), "19");
        assert_eq!(initials(""), "?");
    }

    #[test]
    fn format_tag_shortens_summaries() {
        assert_eq!(format_tag("FLAC 24/192"), "24/192");
        assert_eq!(format_tag("DSF DSD64"), "DSD64");
        assert_eq!(format_tag("AAC AAC/44.1"), "AAC");
        assert_eq!(format_tag("Mixed"), "Mixed");
    }
}

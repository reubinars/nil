//! Human-readable descriptions of playback paths and skip reasons.

use std::fmt::Write;

use bp_core::{
    PROBE_ENCODINGS, PROBE_RATES, PathKind, PlaybackPath, SourceKind, UnsupportedReason,
};
use bp_engine::negotiate::max_dop_multiple;
use bp_engine::profile::DeviceProfile;

/// Formats a rate in kHz without trailing zeros: 44100 -> "44.1", 96000 -> "96".
fn khz(rate: u32) -> String {
    let mut s = format!("{:.3}", f64::from(rate) / 1000.0);
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

/// e.g. `FLAC 24/96 → int32/24  BIT-PERFECT`.
pub fn describe_path(p: &PlaybackPath) -> String {
    let src = &p.source;
    let source = match src.kind {
        SourceKind::Pcm { bits, float } => format!(
            "{} {}{}/{}",
            src.codec,
            bits,
            if float { "f" } else { "" },
            khz(src.sample_rate)
        ),
        SourceKind::Dsd { multiple } => format!("{} DSD{multiple}", src.codec),
    };
    let enc = p.device.encoding;
    let dest = match p.kind {
        PathKind::Native => enc.to_string(),
        PathKind::Padded { pad_bits } => format!("{enc} (pad {pad_bits})"),
        PathKind::Dop => format!("DoP {}k {enc}", khz(p.device.sample_rate)),
    };
    format!("{source} → {dest}  BIT-PERFECT")
}

/// e.g. `skipped: DSD256 exceeds <device> (max DSD128)`.
pub fn describe_reason(r: &UnsupportedReason, device_name: &str) -> String {
    match r {
        UnsupportedReason::DsdRateExceedsDevice {
            multiple,
            max_multiple,
        } => format!("skipped: DSD{multiple} exceeds {device_name} (max DSD{max_multiple})"),
        UnsupportedReason::RateExceedsDevice { rate, max } => format!(
            "skipped: {} kHz exceeds {device_name} (max {} kHz)",
            khz(*rate),
            khz(*max)
        ),
        UnsupportedReason::DopNotSupported => {
            format!("skipped: {device_name} does not support DSD")
        }
        other => format!("skipped: {other}"),
    }
}

/// e.g. `DoP: on (up to DSD128)`, or `DoP: not supported (no DSD)` when the device
/// accepts no DoP rate.
pub fn dop_line(profile: &DeviceProfile) -> String {
    match max_dop_multiple(&profile.capabilities(), 2) {
        Some(max) => format!(
            "DoP: {} (up to DSD{max})",
            if profile.dop_enabled { "on" } else { "off" }
        ),
        None => "DoP: not supported (no DSD)".to_string(),
    }
}

/// The `bp check` verdict for a file: its best path, or why it cannot play.
pub fn describe_check(
    res: &Result<Vec<PlaybackPath>, UnsupportedReason>,
    device_name: &str,
) -> String {
    match res {
        Ok(paths) => match paths.first() {
            Some(p) => describe_path(p),
            None => describe_reason(&UnsupportedReason::FormatNotAccepted, device_name),
        },
        Err(r) => describe_reason(r, device_name),
    }
}

/// Probe results as a table: rows are rates, columns are encodings (2 channels).
pub fn probe_matrix(profile: &DeviceProfile) -> String {
    let mut out = String::new();
    let _ = write!(out, "{:>9}", "rate");
    for e in PROBE_ENCODINGS {
        let _ = write!(out, " {:>9}", e.to_string());
    }
    out.push('\n');
    for rate in PROBE_RATES {
        let _ = write!(out, "{:>9}", format!("{} kHz", khz(rate)));
        for e in PROBE_ENCODINGS {
            let ok = profile
                .probed
                .iter()
                .any(|p| p.rate == rate && p.channels == 2 && p.encoding == e);
            let _ = write!(out, " {:>9}", if ok { "✓" } else { "·" });
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bp_core::*;
    use bp_engine::profile::ProbedFormat;

    #[test]
    fn probe_matrix_marks_supported_cells() {
        let profile = DeviceProfile {
            name: "X".into(),
            probed: vec![ProbedFormat {
                rate: 96_000,
                channels: 2,
                encoding: PROBE_ENCODINGS[2],
            }],
            ..DeviceProfile::default()
        };
        let m = probe_matrix(&profile);
        let lines: Vec<_> = m.lines().collect();
        assert_eq!(lines.len(), 1 + PROBE_RATES.len());
        assert!(lines[0].contains("int32/24") && lines[0].contains("float32"));
        assert!(lines[4].trim_start().starts_with("96 kHz"));
        assert_eq!(lines[4].matches('✓').count(), 1);
        assert_eq!(
            lines.iter().map(|l| l.matches('✓').count()).sum::<usize>(),
            1
        );
    }

    const I32_24: SampleEncoding = SampleEncoding::Int {
        container: 32,
        valid: 24,
    };

    fn path(source: SourceFormat, rate: u32, kind: PathKind) -> PlaybackPath {
        PlaybackPath {
            source,
            device: DeviceFormat {
                sample_rate: rate,
                channels: 2,
                encoding: I32_24,
            },
            kind,
        }
    }

    #[test]
    fn describe_padded_and_dop() {
        let flac = SourceFormat {
            codec: Codec::Flac,
            sample_rate: 96_000,
            channels: 2,
            kind: SourceKind::Pcm {
                bits: 24,
                float: false,
            },
        };
        assert_eq!(
            describe_path(&path(flac, 96_000, PathKind::Native)),
            "FLAC 24/96 → int32/24  BIT-PERFECT"
        );
        let wav = SourceFormat {
            codec: Codec::Wav,
            sample_rate: 44_100,
            kind: SourceKind::Pcm {
                bits: 16,
                float: false,
            },
            ..flac
        };
        assert_eq!(
            describe_path(&path(wav, 44_100, PathKind::Padded { pad_bits: 8 })),
            "WAV 16/44.1 → int32/24 (pad 8)  BIT-PERFECT"
        );
        let dsf = SourceFormat {
            codec: Codec::Dsf,
            sample_rate: 2_822_400,
            channels: 2,
            kind: SourceKind::Dsd { multiple: 64 },
        };
        assert_eq!(
            describe_path(&path(dsf, 176_400, PathKind::Dop)),
            "DSF DSD64 → DoP 176.4k int32/24  BIT-PERFECT"
        );
    }

    #[test]
    fn describe_check_without_candidates_is_not_accepted() {
        assert_eq!(
            describe_check(&Ok(vec![]), "DAC"),
            "skipped: the device does not accept this sample format"
        );
        assert_eq!(
            describe_check(&Err(UnsupportedReason::DopDisabled), "DAC"),
            describe_reason(&UnsupportedReason::DopDisabled, "DAC")
        );
    }

    #[test]
    fn describe_reason_dac_without_dsd() {
        assert_eq!(
            describe_reason(&UnsupportedReason::DopNotSupported, "Speakers (ECHO-A)"),
            "skipped: Speakers (ECHO-A) does not support DSD"
        );
    }

    fn profile_with(formats: &[(u32, SampleEncoding)], dop_enabled: bool) -> DeviceProfile {
        DeviceProfile {
            name: "X".into(),
            probed: formats
                .iter()
                .map(|(rate, encoding)| ProbedFormat {
                    rate: *rate,
                    channels: 2,
                    encoding: *encoding,
                })
                .collect(),
            dop_enabled,
            ..DeviceProfile::default()
        }
    }

    #[test]
    fn dop_line_states_dsd_limit() {
        let dsd128 = profile_with(&[(176_400, I32_24), (352_800, I32_24)], true);
        assert_eq!(dop_line(&dsd128), "DoP: on (up to DSD128)");
        let dsd64 = profile_with(&[(176_400, I32_24)], false);
        assert_eq!(dop_line(&dsd64), "DoP: off (up to DSD64)");
        let none = profile_with(&[(44_100, I32_24), (384_000, I32_24)], false);
        assert_eq!(dop_line(&none), "DoP: not supported (no DSD)");
    }

    #[test]
    fn describe_reason_dsd256() {
        let r = UnsupportedReason::DsdRateExceedsDevice {
            multiple: 256,
            max_multiple: 128,
        };
        assert_eq!(
            describe_reason(&r, "USB DAC"),
            "skipped: DSD256 exceeds USB DAC (max DSD128)"
        );
        let r = UnsupportedReason::DsdRateExceedsDevice {
            multiple: 128,
            max_multiple: 64,
        };
        assert_eq!(
            describe_reason(&r, "X"),
            "skipped: DSD128 exceeds X (max DSD64)"
        );
    }
}

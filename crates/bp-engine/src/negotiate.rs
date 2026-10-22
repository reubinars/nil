//! Strict bit-perfect format negotiation: which device formats a source may use.

use bp_core::{
    Capabilities, DeviceFormat, PathKind, PlaybackPath, SampleEncoding, SourceFormat, SourceKind,
    UnsupportedReason,
};

const INT_ENCODINGS: [SampleEncoding; 4] = [
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
];

/// DoP needs 24 valid bits; in a 32/32 container the low byte is zero padding.
const DOP_ENCODINGS: [SampleEncoding; 3] = [
    SampleEncoding::Int {
        container: 32,
        valid: 24,
    },
    SampleEncoding::Int {
        container: 24,
        valid: 24,
    },
    SampleEncoding::Int {
        container: 32,
        valid: 32,
    },
];

/// DSD multiples DoP can carry within the probed rates (176.4, 352.8, 705.6 kHz).
const DOP_MULTIPLES: [u16; 3] = [64, 128, 256];

/// The device rate that carries DSD`multiple` over DoP.
fn dop_rate(multiple: u16) -> u32 {
    44_100 * u32::from(multiple) / 16
}

/// The highest DSD multiple the device takes over DoP with `channels`, or `None` when it
/// accepts no DoP rate in a DoP-capable encoding (a DAC without DSD support).
pub fn max_dop_multiple(caps: &Capabilities, channels: u16) -> Option<u16> {
    DOP_MULTIPLES
        .into_iter()
        .filter(|m| {
            DOP_ENCODINGS.into_iter().any(|encoding| {
                caps.supports(&DeviceFormat {
                    sample_rate: dop_rate(*m),
                    channels,
                    encoding,
                })
            })
        })
        .max()
}

/// Lists the device formats `src` may play in, best first, or why it cannot play.
///
/// The device rate always equals the source rate (DoP: `44_100 * multiple / 16`) and the
/// device channels always equal the source channels. If the device has no probe data for
/// the channel count, the candidates are returned unfiltered.
pub fn candidates(
    src: &SourceFormat,
    caps: &Capabilities,
    dop_enabled: bool,
    prefer: Option<&DeviceFormat>,
) -> Result<Vec<PlaybackPath>, UnsupportedReason> {
    let (rate, mut paths): (u32, Vec<(SampleEncoding, PathKind)>) = match src.kind {
        SourceKind::Pcm { bits, float: false } => {
            let paths = INT_ENCODINGS
                .into_iter()
                .filter_map(|enc| {
                    let SampleEncoding::Int { valid, .. } = enc else {
                        return None;
                    };
                    if valid < bits {
                        return None;
                    }
                    let kind = if valid == bits {
                        PathKind::Native
                    } else {
                        PathKind::Padded {
                            pad_bits: valid - bits,
                        }
                    };
                    Some((enc, kind))
                })
                .collect();
            (src.sample_rate, paths)
        }
        SourceKind::Pcm { float: true, .. } => (
            src.sample_rate,
            vec![(SampleEncoding::Float32, PathKind::Native)],
        ),
        SourceKind::Dsd { multiple } => {
            if caps.has_channels(src.channels) && max_dop_multiple(caps, src.channels).is_none() {
                return Err(UnsupportedReason::DopNotSupported);
            }
            if !dop_enabled {
                return Err(UnsupportedReason::DopDisabled);
            }
            let rate = dop_rate(multiple);
            (
                rate,
                DOP_ENCODINGS
                    .into_iter()
                    .map(|e| (e, PathKind::Dop))
                    .collect(),
            )
        }
    };

    if caps.has_channels(src.channels) {
        paths.retain(|(encoding, _)| {
            caps.supports(&DeviceFormat {
                sample_rate: rate,
                channels: src.channels,
                encoding: *encoding,
            })
        });
        if paths.is_empty() {
            let max = caps.max_rate(src.channels).unwrap_or(0);
            return Err(match src.kind {
                SourceKind::Dsd { multiple } => match max_dop_multiple(caps, src.channels) {
                    Some(max_multiple) if multiple > max_multiple => {
                        UnsupportedReason::DsdRateExceedsDevice {
                            multiple,
                            max_multiple,
                        }
                    }
                    _ => UnsupportedReason::FormatNotAccepted,
                },
                SourceKind::Pcm { .. } if rate > max => {
                    UnsupportedReason::RateExceedsDevice { rate, max }
                }
                _ => UnsupportedReason::FormatNotAccepted,
            });
        }
    }

    let mut out: Vec<PlaybackPath> = paths
        .into_iter()
        .map(|(encoding, kind)| PlaybackPath {
            source: *src,
            device: DeviceFormat {
                sample_rate: rate,
                channels: src.channels,
                encoding,
            },
            kind,
        })
        .collect();
    if let Some(pos) = prefer.and_then(|p| out.iter().position(|c| c.device == *p)) {
        out[..=pos].rotate_right(1);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bp_core::{Codec, PROBE_ENCODINGS, PROBE_RATES};

    fn pcm(codec: Codec, rate: u32, bits: u8) -> SourceFormat {
        SourceFormat {
            codec,
            sample_rate: rate,
            channels: 2,
            kind: SourceKind::Pcm { bits, float: false },
        }
    }

    fn float(rate: u32) -> SourceFormat {
        SourceFormat {
            codec: Codec::Wav,
            sample_rate: rate,
            channels: 2,
            kind: SourceKind::Pcm {
                bits: 32,
                float: true,
            },
        }
    }

    fn dsd(multiple: u16) -> SourceFormat {
        SourceFormat {
            codec: Codec::Dsf,
            sample_rate: 2_822_400 * u32::from(multiple) / 64,
            channels: 2,
            kind: SourceKind::Dsd { multiple },
        }
    }

    fn only(entries: &[(u32, &str)]) -> Capabilities {
        let mut caps = Capabilities::default();
        for (rate, enc) in entries {
            caps.formats.insert((*rate, 2, enc.parse().unwrap()));
        }
        caps
    }

    fn dac_up_to(max: u32) -> Capabilities {
        let mut caps = Capabilities::default();
        for rate in PROBE_RATES.into_iter().filter(|r| *r <= max) {
            for enc in PROBE_ENCODINGS
                .into_iter()
                .filter(|e| *e != SampleEncoding::Float32)
            {
                caps.formats.insert((rate, 2, enc));
            }
        }
        caps
    }

    fn dac_a() -> Capabilities {
        dac_up_to(384_000)
    }

    fn dac_b() -> Capabilities {
        dac_up_to(192_000)
    }

    /// Like the ECHO-A: no 44.1k-family rate above 44.1 kHz, so no DoP at all.
    fn no_dop_dac() -> Capabilities {
        let mut caps = dac_a();
        caps.formats
            .retain(|(rate, _, _)| ![88_200, 176_400, 352_800].contains(rate));
        caps
    }

    fn enc(s: &str) -> SampleEncoding {
        s.parse().unwrap()
    }

    #[test]
    fn pcm16_on_32bit_only_device_pads_16() {
        let caps = only(&[(44_100, "int32/32")]);
        let p = candidates(&pcm(Codec::Flac, 44_100, 16), &caps, false, None).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, PathKind::Padded { pad_bits: 16 });
    }

    #[test]
    fn pcm24_prefers_packed_then_32_24() {
        let p = candidates(&pcm(Codec::Flac, 96_000, 24), &dac_a(), false, None).unwrap();
        assert_eq!(p[0].device.encoding, enc("int24/24"));
        assert_eq!(p[0].kind, PathKind::Native);
        assert_eq!(p[1].device.encoding, enc("int32/24"));
        assert_eq!(p[1].kind, PathKind::Native);
        assert_eq!(p[2].device.encoding, enc("int32/32"));
        assert_eq!(p[2].kind, PathKind::Padded { pad_bits: 8 });
    }

    #[test]
    fn pcm20_gets_pad_4() {
        let caps = only(&[(48_000, "int24/24")]);
        let p = candidates(&pcm(Codec::Flac, 48_000, 20), &caps, false, None).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, PathKind::Padded { pad_bits: 4 });
    }

    #[test]
    fn pcm384_on_dac_b_exceeds() {
        assert_eq!(
            candidates(&pcm(Codec::Flac, 384_000, 24), &dac_b(), false, None).unwrap_err(),
            UnsupportedReason::RateExceedsDevice {
                rate: 384_000,
                max: 192_000
            }
        );
    }

    #[test]
    fn odd_rate_not_accepted() {
        assert_eq!(
            candidates(&pcm(Codec::Flac, 64_000, 24), &dac_a(), false, None).unwrap_err(),
            UnsupportedReason::FormatNotAccepted
        );
    }

    #[test]
    fn dsd_needs_dop() {
        assert_eq!(
            candidates(&dsd(64), &dac_a(), false, None).unwrap_err(),
            UnsupportedReason::DopDisabled
        );
    }

    #[test]
    fn dsd64_both_dacs_dsd128_only_a() {
        let p = candidates(&dsd(64), &dac_b(), true, None).unwrap();
        assert_eq!(p[0].device.sample_rate, 176_400);
        assert_eq!(p[0].device.encoding, enc("int32/24"));
        assert_eq!(p[0].kind, PathKind::Dop);
        assert_eq!(p[1].device.encoding, enc("int24/24"));

        let p = candidates(&dsd(128), &dac_a(), true, None).unwrap();
        assert_eq!(p[0].device.sample_rate, 352_800);
        assert_eq!(p[0].kind, PathKind::Dop);

        assert_eq!(
            candidates(&dsd(128), &dac_b(), true, None).unwrap_err(),
            UnsupportedReason::DsdRateExceedsDevice {
                multiple: 128,
                max_multiple: 64
            }
        );
        assert_eq!(
            candidates(&dsd(256), &dac_a(), true, None).unwrap_err(),
            UnsupportedReason::DsdRateExceedsDevice {
                multiple: 256,
                max_multiple: 128
            }
        );
    }

    #[test]
    fn max_dop_multiple_counts_only_dop_rates() {
        assert_eq!(max_dop_multiple(&dac_a(), 2), Some(128));
        assert_eq!(max_dop_multiple(&dac_b(), 2), Some(64));
        assert_eq!(max_dop_multiple(&no_dop_dac(), 2), None);
        assert_eq!(max_dop_multiple(&dac_a(), 6), None);
        // 176.4 kHz only as 16-bit cannot carry DoP.
        assert_eq!(max_dop_multiple(&only(&[(176_400, "int16/16")]), 2), None);
    }

    #[test]
    fn dsd_on_device_without_dop_rates_is_not_supported() {
        for multiple in [64, 128, 256] {
            for dop_enabled in [true, false] {
                assert_eq!(
                    candidates(&dsd(multiple), &no_dop_dac(), dop_enabled, None).unwrap_err(),
                    UnsupportedReason::DopNotSupported,
                    "DSD{multiple}, dop_enabled {dop_enabled}"
                );
            }
        }
    }

    #[test]
    fn dsd64_dop_in_32_32_only_device() {
        let caps = only(&[(176_400, "int32/32")]);
        let p = candidates(&dsd(64), &caps, true, None).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].device.sample_rate, 176_400);
        assert_eq!(p[0].device.encoding, enc("int32/32"));
        assert_eq!(p[0].kind, PathKind::Dop);
        // Last resort after the 24-bit-valid containers.
        let p = candidates(&dsd(64), &dac_b(), true, None).unwrap();
        let encs: Vec<_> = p.iter().map(|c| c.device.encoding).collect();
        assert_eq!(encs, [enc("int32/24"), enc("int24/24"), enc("int32/32")]);
    }

    #[test]
    fn float_needs_float_device() {
        assert_eq!(
            candidates(&float(48_000), &dac_a(), false, None).unwrap_err(),
            UnsupportedReason::FormatNotAccepted
        );
    }

    #[test]
    fn unprobed_channels_unfiltered() {
        let mut src = pcm(Codec::Flac, 48_000, 24);
        src.channels = 6;
        let p = candidates(&src, &dac_a(), false, None).unwrap();
        assert_eq!(p.len(), 3);
        assert!(
            p.iter()
                .all(|c| c.device.channels == 6 && c.device.sample_rate == 48_000)
        );
    }

    #[test]
    fn prefer_current_stream_format() {
        let prefer = DeviceFormat {
            sample_rate: 44_100,
            channels: 2,
            encoding: enc("int32/24"),
        };
        let p = candidates(
            &pcm(Codec::Flac, 44_100, 16),
            &dac_a(),
            false,
            Some(&prefer),
        )
        .unwrap();
        assert_eq!(p[0].device.encoding, enc("int32/24"));
        assert_eq!(p[0].kind, PathKind::Padded { pad_bits: 8 });
        assert_eq!(p.len(), 4);
        assert_eq!(p[1].device.encoding, enc("int16/16"));
    }

    #[test]
    fn prefer_not_in_candidates_is_ignored() {
        let prefer = DeviceFormat {
            sample_rate: 48_000,
            channels: 2,
            encoding: enc("int32/24"),
        };
        let without = candidates(&pcm(Codec::Flac, 44_100, 16), &dac_a(), false, None).unwrap();
        let with = candidates(
            &pcm(Codec::Flac, 44_100, 16),
            &dac_a(),
            false,
            Some(&prefer),
        )
        .unwrap();
        assert_eq!(with, without);
    }
}

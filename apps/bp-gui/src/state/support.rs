//! Whether a device can play a track bit-perfectly.

use bp_core::{PlaybackPath, SourceFormat, UnsupportedReason};

use crate::backend::DeviceCaps;

/// The best playback path for `src` on `dev`, or why there is none.
pub fn track_support(
    src: &SourceFormat,
    dev: &DeviceCaps,
) -> Result<PlaybackPath, UnsupportedReason> {
    bp_engine::negotiate::candidates(src, &dev.caps, dev.dop_enabled, None)?
        .into_iter()
        .next()
        .ok_or(UnsupportedReason::FormatNotAccepted)
}

#[cfg(test)]
pub(crate) mod testutil {
    use bp_core::{Capabilities, Codec, PROBE_ENCODINGS, PROBE_RATES, SourceFormat, SourceKind};

    use crate::backend::DeviceCaps;

    /// Stereo device accepting every probed encoding at every probed rate up to `max_rate`.
    pub fn caps_up_to(max_rate: u32, dop_enabled: bool) -> DeviceCaps {
        let mut caps = Capabilities::default();
        for rate in PROBE_RATES.into_iter().filter(|r| *r <= max_rate) {
            for encoding in PROBE_ENCODINGS {
                caps.formats.insert((rate, 2, encoding));
            }
        }
        DeviceCaps { caps, dop_enabled }
    }

    pub fn dsd(multiple: u16) -> SourceFormat {
        SourceFormat {
            codec: Codec::Dsf,
            sample_rate: 2_822_400 * u32::from(multiple) / 64,
            channels: 2,
            kind: SourceKind::Dsd { multiple },
        }
    }

    pub fn pcm(rate: u32, bits: u8) -> SourceFormat {
        SourceFormat {
            codec: Codec::Flac,
            sample_rate: rate,
            channels: 2,
            kind: SourceKind::Pcm { bits, float: false },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn dsd256_unsupported_when_dop_tops_out_at_dsd128() {
        let dev = caps_up_to(384_000, true);
        assert!(track_support(&dsd(256), &dev).is_err());
        assert!(track_support(&dsd(128), &dev).is_ok());
    }

    #[test]
    fn empty_candidates_is_unsupported_not_panic() {
        let dev = DeviceCaps {
            caps: bp_core::Capabilities::default(),
            dop_enabled: false,
        };
        assert_eq!(
            track_support(&pcm(44_100, 40), &dev).unwrap_err(),
            UnsupportedReason::FormatNotAccepted
        );
    }

    #[test]
    fn dop_disabled_reason() {
        assert_eq!(
            track_support(&dsd(64), &caps_up_to(384_000, false)).unwrap_err(),
            UnsupportedReason::DopDisabled
        );
    }

    #[test]
    fn pcm_rate_above_max_reason() {
        let e = track_support(&pcm(384_000, 24), &caps_up_to(192_000, true)).unwrap_err();
        assert_eq!(
            e,
            UnsupportedReason::RateExceedsDevice {
                rate: 384_000,
                max: 192_000
            }
        );
    }
}

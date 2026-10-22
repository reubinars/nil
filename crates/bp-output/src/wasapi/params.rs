//! COM-free pieces of the WASAPI backend: format parameters, the probe matrix, and period math.

use bp_core::{
    Capabilities, DeviceFormat, OutputError, PROBE_ENCODINGS, PROBE_RATES, SampleEncoding,
};

/// Sample type of a `WAVEFORMATEXTENSIBLE` (maps 1:1 to `wasapi::SampleType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SampleKind {
    Int,
    Float,
}

/// `(storebits, validbits, sample type)` of an encoding, as `WaveFormat::new` takes them.
pub(crate) fn wave_params(enc: SampleEncoding) -> (usize, usize, SampleKind) {
    match enc {
        SampleEncoding::Int { container, valid } => {
            (usize::from(container), usize::from(valid), SampleKind::Int)
        }
        SampleEncoding::Float32 => (32, 32, SampleKind::Float),
    }
}

/// Every format the probe asks about for `channels`: each probe rate × each probe encoding,
/// rates ascending.
pub(crate) fn probe_formats(channels: u16) -> impl Iterator<Item = DeviceFormat> {
    PROBE_RATES.into_iter().flat_map(move |sample_rate| {
        PROBE_ENCODINGS
            .into_iter()
            .map(move |encoding| DeviceFormat {
                sample_rate,
                channels,
                encoding,
            })
    })
}

/// Runs `check` (an exclusive-mode `IsFormatSupported`) on every probe format.
///
/// `Ok` records the format. `UnsupportedFormat` and other plain OS errors (some drivers answer
/// odd formats with `E_INVALIDARG`) mean "not supported". Errors about the device itself
/// (not found, in use, exclusive mode disallowed, invalidated) abort the probe, so a busy or
/// unplugged device never yields an empty matrix that would be saved as its profile.
pub(crate) fn probe_with(
    channels: u16,
    mut check: impl FnMut(&DeviceFormat) -> Result<(), OutputError>,
) -> Result<Capabilities, OutputError> {
    let mut caps = Capabilities::default();
    for f in probe_formats(channels) {
        match check(&f) {
            Ok(()) => {
                caps.formats.insert((f.sample_rate, f.channels, f.encoding));
            }
            Err(OutputError::UnsupportedFormat | OutputError::Os(_)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(caps)
}

/// Period (100 ns units) whose buffer holds exactly `frames` at `rate`:
/// `10_000_000 × frames / rate`, rounded. Same value as `wasapi::calculate_period_100ns`.
pub(crate) fn aligned_period_hns(frames: u32, rate: u32) -> i64 {
    let rate = u64::from(rate);
    ((10_000_000 * u64::from(frames) + rate / 2) / rate) as i64
}

/// Shortest event wait before a device counts as stalled. The first event after
/// `Start` (open, resume, seek) can take tens of ms on USB devices, and a busy system can
/// delay any event by more than a period; a late event is not a dead device. Same value as
/// Microsoft's exclusive-mode render sample.
const MIN_EVENT_TIMEOUT_MS: u32 = 2_000;

/// Event wait timeout: 2 × the period, in whole milliseconds rounded up, but at least
/// [`MIN_EVENT_TIMEOUT_MS`].
pub(crate) fn event_timeout_ms(period_hns: i64) -> u32 {
    let ms = (2 * period_hns.max(0) + 9_999) / 10_000;
    u32::try_from(ms)
        .unwrap_or(u32::MAX)
        .max(MIN_EVENT_TIMEOUT_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_period_rounds_to_nearest() {
        assert_eq!(aligned_period_hns(441, 44_100), 100_000);
        assert_eq!(aligned_period_hns(480, 48_000), 100_000);
        // 10_000_000 × 448 / 44_100 = 101_587.30…
        assert_eq!(aligned_period_hns(448, 44_100), 101_587);
        // 10_000_000 × 2 / 3 = 6_666_666.67 → up; × 1 / 3 = 3_333_333.33 → down.
        assert_eq!(aligned_period_hns(2, 3), 6_666_667);
        assert_eq!(aligned_period_hns(1, 3), 3_333_333);
        // Large buffer at a high rate does not overflow.
        assert_eq!(aligned_period_hns(7_680, 768_000), 100_000);
    }

    #[test]
    fn event_timeout_is_two_periods_rounded_up_with_a_floor() {
        // Typical periods: a late event must not count as a stall.
        assert_eq!(event_timeout_ms(100_000), 2_000);
        assert_eq!(event_timeout_ms(30_000), 2_000);
        assert_eq!(event_timeout_ms(0), 2_000);
        // Above the floor: 2 × 1_000.0001 ms = 2_000.0002 ms → 2_001.
        assert_eq!(event_timeout_ms(10_000_001), 2_001);
        assert_eq!(event_timeout_ms(20_000_000), 4_000);
    }

    #[cfg(windows)]
    #[test]
    fn aligned_period_matches_wasapi() {
        for (frames, rate) in [(441, 44_100), (448, 44_100), (1_000, 192_000), (2, 3)] {
            assert_eq!(
                aligned_period_hns(frames, rate),
                ::wasapi::calculate_period_100ns(i64::from(frames), i64::from(rate))
            );
        }
    }

    #[test]
    fn wave_params_per_encoding() {
        let expected = [
            (16, 16, SampleKind::Int),
            (24, 24, SampleKind::Int),
            (32, 24, SampleKind::Int),
            (32, 32, SampleKind::Int),
            (32, 32, SampleKind::Float),
        ];
        for (enc, want) in PROBE_ENCODINGS.into_iter().zip(expected) {
            assert_eq!(wave_params(enc), want, "{enc}");
        }
    }

    #[test]
    fn probe_formats_is_rates_times_encodings() {
        let all: Vec<_> = probe_formats(2).collect();
        assert_eq!(all.len(), 50);
        assert!(all.iter().all(|f| f.channels == 2));
        assert_eq!(
            all[0],
            DeviceFormat {
                sample_rate: 44_100,
                channels: 2,
                encoding: PROBE_ENCODINGS[0]
            }
        );
        assert_eq!(all[4].encoding, SampleEncoding::Float32);
        assert_eq!(all[5].sample_rate, 48_000);
        assert_eq!(all[49].sample_rate, 768_000);
        for rate in PROBE_RATES {
            for enc in PROBE_ENCODINGS {
                assert!(
                    all.iter()
                        .any(|f| f.sample_rate == rate && f.encoding == enc)
                );
            }
        }
        assert!(probe_formats(6).all(|f| f.channels == 6));
    }

    #[test]
    fn probe_records_accepted_formats_for_the_channel_count() {
        let i32_24 = SampleEncoding::Int {
            container: 32,
            valid: 24,
        };
        let caps = probe_with(2, |f| {
            if f.encoding == i32_24 && f.sample_rate <= 192_000 {
                Ok(())
            } else if f.sample_rate == 768_000 {
                Err(OutputError::Os(0x8007_0057_u32 as i32)) // E_INVALIDARG
            } else {
                Err(OutputError::UnsupportedFormat)
            }
        })
        .unwrap();
        assert_eq!(caps.formats.len(), 6);
        assert_eq!(caps.max_rate(2), Some(192_000));
        assert!(caps.formats.iter().all(|&(_, c, e)| c == 2 && e == i32_24));

        let caps = probe_with(6, |_| Ok(())).unwrap();
        assert_eq!(caps.formats.len(), 50);
        assert!(caps.has_channels(6) && !caps.has_channels(2));
    }

    #[test]
    fn probe_aborts_on_device_errors() {
        for err in [
            OutputError::DeviceNotFound,
            OutputError::DeviceInUse,
            OutputError::ExclusiveModeNotAllowed,
            OutputError::Invalidated,
        ] {
            let mut calls = 0;
            let want = std::mem::discriminant(&err);
            let mut err = Some(err);
            let got = probe_with(2, |_| {
                calls += 1;
                if calls == 3 {
                    Err(err.take().unwrap())
                } else {
                    Ok(())
                }
            });
            assert_eq!(std::mem::discriminant(&got.unwrap_err()), want);
            assert_eq!(calls, 3, "stops at the first device error");
        }
    }
}

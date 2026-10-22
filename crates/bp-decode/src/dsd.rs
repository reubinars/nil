//! Helpers shared by the DSD file sources.

use std::time::Duration;

/// Time covered by `bytes` DSD bytes of one channel at bit rate `fs`.
pub(crate) fn bytes_to_duration(bytes: u64, fs: u32) -> Duration {
    let nanos = u128::from(bytes) * 8 * 1_000_000_000 / u128::from(fs);
    Duration::from_nanos(nanos as u64)
}

/// Per-channel byte position for a seek target: floor(to * fs / 8), rounded down to even
/// (a whole DoP frame), clamped to `total`.
pub(crate) fn seek_target(to: Duration, fs: u32, total: u64) -> u64 {
    let bytes = to.as_nanos() * u128::from(fs) / 8 / 1_000_000_000;
    let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
    (bytes & !1).min(total)
}

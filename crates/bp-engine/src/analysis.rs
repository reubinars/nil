//! A display-only copy of the audio being played, for the GUI's spectrum. The decoder
//! writes a mono downmix of every block; the control thread stores where playback is. The
//! bytes sent to the device never pass through here.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bp_core::{SourceFormat, SourceKind};
use bp_decode::Block;

/// How much analysis audio is kept.
pub const HISTORY: Duration = Duration::from_secs(4);

/// DSD bits averaged into one analysis sample.
const DSD_BOXCAR_BITS: u32 = 64;

/// A window of mono analysis audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub rate: u32,
    pub samples: Vec<f32>,
}

/// Shared between the decoder (writes samples), the control thread (stores the played
/// frame) and the GUI (reads windows). Cheap to clone.
#[derive(Clone, Default)]
pub struct AnalysisTap {
    inner: Arc<Mutex<Inner>>,
    /// Samples a contended `write` could not store; written as zeros next time so the ring
    /// stays aligned with the stream.
    skipped: Arc<AtomicU64>,
}

#[derive(Default)]
struct Inner {
    active: bool,
    /// Device frames per second of the stream.
    device_rate: u32,
    /// Analysis samples per second.
    rate: u32,
    ring: Vec<f32>,
    /// Analysis samples written since `begin`.
    written: u64,
    /// `(frames played, when, paused)`.
    played: Option<(u64, Instant, bool)>,
}

impl AnalysisTap {
    /// A new stream: forgets everything and takes the stream's device frame rate and the
    /// analysis sample rate.
    pub fn begin(&self, device_rate: u32, analysis_rate: u32) {
        let mut g = self.lock();
        let cap = (HISTORY.as_secs_f64() * f64::from(analysis_rate)).ceil() as usize;
        *g = Inner {
            active: true,
            device_rate: device_rate.max(1),
            rate: analysis_rate.max(1),
            ring: vec![0.0; cap.max(1)],
            written: 0,
            played: None,
        };
        self.skipped.store(0, Ordering::Relaxed);
    }

    /// Appends samples. Never blocks: when the lock is held, the samples are counted and
    /// become zeros on the next write.
    pub fn write(&self, samples: &[f32]) {
        let Ok(mut g) = self.inner.try_lock() else {
            self.skipped
                .fetch_add(samples.len() as u64, Ordering::Relaxed);
            return;
        };
        if !g.active {
            return;
        }
        let zeros = self.skipped.swap(0, Ordering::Relaxed);
        for _ in 0..zeros {
            g.push(0.0);
        }
        for &s in samples {
            g.push(s);
        }
    }

    /// Where playback is: `frames` device frames played as of `at`.
    pub fn played(&self, frames: u64, at: Instant, paused: bool) {
        self.lock().played = Some((frames, at, paused));
    }

    /// No stream: windows are `None` until the next `begin`.
    pub fn clear(&self) {
        let mut g = self.lock();
        g.active = false;
        g.written = 0;
        g.played = None;
    }

    /// The `n` analysis samples just before the frame estimated to be playing at `now`;
    /// `None` without a stream, while paused, or when those samples are not held.
    pub fn window(&self, n: usize, now: Instant) -> Option<Window> {
        let g = self.lock();
        if !g.active {
            return None;
        }
        let (frames, at, paused) = g.played?;
        if paused {
            return None;
        }
        let elapsed = now.saturating_duration_since(at).as_secs_f64();
        let frame = frames + (elapsed * f64::from(g.device_rate)) as u64;
        let end = (u128::from(frame) * u128::from(g.rate) / u128::from(g.device_rate)) as u64;
        let end = end.min(g.written);
        let start = end.checked_sub(n as u64)?;
        let cap = g.ring.len() as u64;
        if g.written - start > cap {
            return None;
        }
        let samples = (start..end).map(|i| g.ring[(i % cap) as usize]).collect();
        Some(Window {
            rate: g.rate,
            samples,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    fn lock_for_test(&self) -> MutexGuard<'_, Inner> {
        self.lock()
    }
}

impl Inner {
    fn push(&mut self, s: f32) {
        let cap = self.ring.len() as u64;
        self.ring[(self.written % cap) as usize] = s;
        self.written += 1;
    }
}

/// Analysis samples per second for a source: its sample rate for PCM; for DSD the bit
/// rate over the boxcar length.
pub fn analysis_rate(src: &SourceFormat) -> u32 {
    match src.kind {
        SourceKind::Pcm { .. } => src.sample_rate,
        SourceKind::Dsd { .. } => src.sample_rate / DSD_BOXCAR_BITS,
    }
}

/// Appends the mono downmix of `block` (the channels' average) to `out`. PCM is scaled by
/// `bits` to −1…1; DSD becomes a boxcar average over 64 bits per channel, for display
/// only.
pub fn downmix(block: &Block, channels: usize, bits: u32, out: &mut Vec<f32>) {
    let channels = channels.max(1);
    match block {
        Block::Pcm(samples) => {
            let scale = 1.0 / (1u64 << bits.clamp(1, 32).saturating_sub(1)) as f32;
            out.extend(samples.chunks_exact(channels).map(|frame| {
                frame.iter().map(|&s| s as f32 * scale).sum::<f32>() / channels as f32
            }));
        }
        Block::Float(samples) => {
            out.extend(
                samples
                    .chunks_exact(channels)
                    .map(|frame| frame.iter().sum::<f32>() / channels as f32),
            );
        }
        Block::Dsd(bytes) => {
            // Bytes interleave by channel: one byte of each channel per byte-frame.
            let per_group = (DSD_BOXCAR_BITS / 8) as usize * channels;
            out.extend(bytes.chunks(per_group).map(|group| {
                let ones: u32 = group.iter().map(|b| b.count_ones()).sum();
                let bits = group.len() as f32 * 8.0;
                ones as f32 * 2.0 / bits - 1.0
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use bp_core::{Codec, SourceFormat, SourceKind};
    use bp_decode::Block;

    use super::*;

    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32).collect()
    }

    #[test]
    fn window_ends_at_the_played_frame() {
        let tap = AnalysisTap::default();
        tap.begin(48_000, 48_000);
        tap.write(&ramp(48_000));
        let t0 = Instant::now();
        tap.played(24_000, t0, false);
        let w = tap.window(4, t0).unwrap();
        assert_eq!(w.rate, 48_000);
        assert_eq!(w.samples, vec![23_996.0, 23_997.0, 23_998.0, 23_999.0]);
        let later = tap.window(4, t0 + Duration::from_millis(10)).unwrap();
        assert_eq!(later.samples[3], 24_479.0);
    }

    #[test]
    fn rates_map_between_device_and_analysis() {
        // DoP DSD64: 176.4 kHz device frames, 44.1 kHz analysis samples.
        let tap = AnalysisTap::default();
        tap.begin(176_400, 44_100);
        tap.write(&ramp(44_100));
        let t0 = Instant::now();
        tap.played(88_200, t0, false);
        let w = tap.window(2, t0).unwrap();
        assert_eq!(w.rate, 44_100);
        assert_eq!(w.samples, vec![22_048.0, 22_049.0]);
    }

    #[test]
    fn paused_cleared_or_short_gives_none() {
        let tap = AnalysisTap::default();
        let t0 = Instant::now();
        assert_eq!(tap.window(4, t0), None, "never begun");
        tap.begin(1_000, 1_000);
        tap.write(&ramp(1_000));
        tap.played(500, t0, true);
        assert_eq!(tap.window(4, t0), None, "paused");
        tap.played(2, t0, false);
        assert_eq!(
            tap.window(4, t0),
            None,
            "not enough before the played frame"
        );
        tap.played(500, t0, false);
        assert!(tap.window(4, t0).is_some());
        tap.clear();
        assert_eq!(tap.window(4, t0), None, "cleared");
    }

    #[test]
    fn history_keeps_the_last_four_seconds() {
        let tap = AnalysisTap::default();
        tap.begin(1_000, 1_000);
        tap.write(&ramp(5_000));
        let t0 = Instant::now();
        tap.played(500, t0, false);
        assert_eq!(tap.window(100, t0), None, "overwritten");
        tap.played(4_900, t0, false);
        assert_eq!(tap.window(100, t0).unwrap().samples[99], 4_899.0);
    }

    #[test]
    fn contended_write_becomes_zeros() {
        let tap = AnalysisTap::default();
        tap.begin(1_000, 1_000);
        tap.write(&[5.0; 20]);
        {
            let _held = tap.lock_for_test();
            tap.write(&[7.0; 10]);
        }
        tap.write(&[1.0]);
        let t0 = Instant::now();
        tap.played(31, t0, false);
        let w = tap.window(12, t0).unwrap();
        let mut expected = vec![5.0];
        expected.extend([0.0; 10]);
        expected.push(1.0);
        assert_eq!(w.samples, expected);
    }

    #[test]
    fn downmix_averages_channels() {
        let mut out = Vec::new();
        downmix(
            &Block::Pcm(&[16_384, -16_384, 32_767, 32_767]),
            2,
            16,
            &mut out,
        );
        assert_eq!(out[0], 0.0);
        assert!((out[1] - 1.0).abs() < 1e-3, "{}", out[1]);
        out.clear();
        downmix(&Block::Pcm(&[-8_388_608]), 1, 24, &mut out);
        assert_eq!(out, vec![-1.0]);
        out.clear();
        downmix(&Block::Pcm(&[600, 0, 0, 0, 0, 0]), 6, 16, &mut out);
        assert!((out[0] - 100.0 / 32_768.0).abs() < 1e-6);
        out.clear();
        downmix(&Block::Float(&[0.5, -0.25]), 2, 32, &mut out);
        assert_eq!(out, vec![0.125]);
    }

    #[test]
    fn dsd_boxcar() {
        let interleave = |l: u8, r: u8| -> Vec<u8> { (0..8).flat_map(|_| [l, r]).collect() };
        let mut out = Vec::new();
        downmix(&Block::Dsd(&interleave(0xFF, 0x00)), 2, 1, &mut out);
        assert_eq!(out, vec![0.0]);
        out.clear();
        downmix(&Block::Dsd(&interleave(0xFF, 0xFF)), 2, 1, &mut out);
        assert_eq!(out, vec![1.0]);
        out.clear();
        downmix(&Block::Dsd(&interleave(0xAA, 0xAA)), 2, 1, &mut out);
        assert_eq!(out, vec![0.0]);
    }

    #[test]
    fn analysis_rate_of_dsd64_is_44100() {
        let dsd64 = SourceFormat {
            codec: Codec::Dsf,
            sample_rate: 2_822_400,
            channels: 2,
            kind: SourceKind::Dsd { multiple: 64 },
        };
        assert_eq!(analysis_rate(&dsd64), 44_100);
        let pcm = SourceFormat {
            codec: Codec::Flac,
            sample_rate: 96_000,
            channels: 2,
            kind: SourceKind::Pcm {
                bits: 24,
                float: false,
            },
        };
        assert_eq!(analysis_rate(&pcm), 96_000);
    }
}

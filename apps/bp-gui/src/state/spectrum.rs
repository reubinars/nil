//! The spectrum analyser: log-spaced band levels from windows of the player's audio.

use std::sync::Arc;
use std::time::Instant;

pub use bp_engine::Window;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Samples per analysis.
pub const FFT_SIZE: usize = 4096;
/// Bars of the large spectrum; the mini one shows them in pairs.
pub const BANDS: usize = 48;
/// How fast a bar falls, in full heights per second.
pub const DECAY_PER_SEC: f32 = 1.6;
const LOW_HZ: f32 = 30.0;
const HIGH_HZ: f32 = 20_000.0;
/// Levels span this many dB below full scale.
const FLOOR_DB: f32 = 70.0;

/// `BANDS + 1` log-spaced edges from 30 Hz to 20 kHz, or to half the rate if lower.
pub fn band_edges(rate: u32) -> [f32; BANDS + 1] {
    let high = HIGH_HZ.min(rate as f32 / 2.0);
    let ratio = high / LOW_HZ;
    std::array::from_fn(|i| {
        if i == BANDS {
            high
        } else {
            LOW_HZ * ratio.powf(i as f32 / BANDS as f32)
        }
    })
}

pub struct Spectrum {
    levels: [f32; BANDS],
    last: Option<Instant>,
    fft: Arc<dyn RealToComplex<f32>>,
    hann: Vec<f32>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
}

impl Default for Spectrum {
    fn default() -> Self {
        Self::new()
    }
}

impl Spectrum {
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let hann = (0..FFT_SIZE)
            .map(|i| {
                let x = std::f32::consts::TAU * i as f32 / FFT_SIZE as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        Spectrum {
            levels: [0.0; BANDS],
            last: None,
            input: fft.make_input_vec(),
            output: fft.make_output_vec(),
            fft,
            hann,
        }
    }

    /// Bars rise at once to the newest window's levels and fall at `DECAY_PER_SEC`
    /// otherwise. Returns whether any bar is above zero (keep repainting).
    pub fn update(&mut self, window: Option<&Window>, now: Instant) -> bool {
        let dt = self
            .last
            .map_or(0.0, |l| now.saturating_duration_since(l).as_secs_f32());
        self.last = Some(now);
        let fresh = window
            .filter(|w| w.samples.len() >= FFT_SIZE)
            .map(|w| self.analyse(w));
        for (i, level) in self.levels.iter_mut().enumerate() {
            let decayed = (*level - DECAY_PER_SEC * dt).max(0.0);
            *level = fresh.map_or(decayed, |f| f[i].max(decayed));
        }
        self.levels.iter().any(|&l| l > 0.0)
    }

    pub fn levels(&self) -> &[f32; BANDS] {
        &self.levels
    }

    /// Each pair of bands as one bar.
    pub fn mini(&self) -> [f32; BANDS / 2] {
        std::array::from_fn(|i| self.levels[2 * i].max(self.levels[2 * i + 1]))
    }

    /// Band levels of the last `FFT_SIZE` samples of `w`.
    fn analyse(&mut self, w: &Window) -> [f32; BANDS] {
        let tail = &w.samples[w.samples.len() - FFT_SIZE..];
        for ((x, s), h) in self.input.iter_mut().zip(tail).zip(&self.hann) {
            *x = s * h;
        }
        if self.fft.process(&mut self.input, &mut self.output).is_err() {
            return [0.0; BANDS];
        }
        // A full-scale sine peaks at N/4 under a Hann window.
        let norm = 4.0 / FFT_SIZE as f32;
        let bin_hz = w.rate as f32 / FFT_SIZE as f32;
        let edges = band_edges(w.rate);
        let last = self.output.len() - 1;
        std::array::from_fn(|b| {
            let lo = (edges[b] / bin_hz).ceil() as usize;
            let hi = ((edges[b + 1] / bin_hz).floor() as usize).min(last);
            // A band narrower than a bin takes the bin nearest its centre.
            let bins = if lo <= hi {
                lo..=hi
            } else {
                let c = ((edges[b] * edges[b + 1]).sqrt() / bin_hz).round() as usize;
                c.min(last)..=c.min(last)
            };
            let mag = bins.map(|k| self.output[k].norm()).fold(0.0f32, f32::max) * norm;
            let db = 20.0 * mag.max(1e-9).log10();
            ((db + FLOOR_DB) / FLOOR_DB).clamp(0.0, 1.0)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn sine(rate: u32, freq: f32, amp: f32, n: usize) -> Window {
        Window {
            rate,
            samples: (0..n)
                .map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / rate as f32).sin())
                .collect(),
        }
    }

    #[test]
    fn band_edges_are_log_spaced_and_capped() {
        let e = band_edges(48_000);
        assert_eq!(e[0], 30.0);
        assert!((e[BANDS] - 20_000.0).abs() < 1e-2);
        let ratio = e[1] / e[0];
        for w in e.windows(2) {
            assert!((w[1] / w[0] - ratio).abs() < 1e-3);
        }
        assert!((band_edges(22_050)[BANDS] - 11_025.0).abs() < 1e-2);
    }

    #[test]
    fn sine_peaks_in_its_band() {
        let mut s = Spectrum::new();
        s.update(Some(&sine(48_000, 1_000.0, 0.5, FFT_SIZE)), Instant::now());
        let e = band_edges(48_000);
        let band = (0..BANDS)
            .find(|&i| e[i] <= 1_000.0 && 1_000.0 < e[i + 1])
            .unwrap();
        let levels = s.levels();
        let max = (0..BANDS)
            .max_by(|&a, &b| levels[a].total_cmp(&levels[b]))
            .unwrap();
        assert_eq!(max, band);
        assert!(levels[band] > 0.8, "{}", levels[band]);
    }

    #[test]
    fn silence_and_none_decay_to_zero() {
        let mut s = Spectrum::new();
        let t = Instant::now();
        s.levels = [1.0; BANDS];
        s.last = Some(t);
        assert!(s.update(None, t + Duration::from_millis(300)));
        assert!(s.levels().iter().all(|&l| l <= 1.0 - 0.48 + 1e-4));
        let silence = Window {
            rate: 48_000,
            samples: vec![0.0; FFT_SIZE],
        };
        assert!(!s.update(Some(&silence), t + Duration::from_secs(1)));
        assert!(s.levels().iter().all(|&l| l == 0.0));
    }

    #[test]
    fn attack_is_instant() {
        let mut s = Spectrum::new();
        let t = Instant::now();
        s.update(None, t);
        assert!(s.update(Some(&sine(48_000, 1_000.0, 0.5, FFT_SIZE)), t));
        assert!(s.levels().iter().any(|&l| l > 0.8));
    }

    #[test]
    fn short_window_counts_as_none() {
        let mut s = Spectrum::new();
        s.update(Some(&sine(48_000, 1_000.0, 0.5, 100)), Instant::now());
        assert!(s.levels().iter().all(|&l| l == 0.0));
    }

    #[test]
    fn mini_takes_pair_maxima() {
        let mut s = Spectrum::new();
        for (i, l) in s.levels.iter_mut().enumerate() {
            *l = i as f32 / 100.0;
        }
        let m = s.mini();
        assert_eq!(m[0], 0.01);
        assert_eq!(m[23], 0.47);
    }
}

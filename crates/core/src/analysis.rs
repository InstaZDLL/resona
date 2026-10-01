//! Bit-transparency check of the captured stream.
//!
//! WASAPI hands us `f32` samples. When nothing between Spotify's decoder
//! and our capture touched the signal (volume 100 %, normalization off, no
//! resampling, no enhancements), every sample is an integer PCM value
//! divided by a power of two, so it maps back exactly to 16 or 24 bits.
//! Any gain or resampling stage breaks that, and this module notices.
//! That is the only way to tell a real lossless rip from a lossy one
//! stored in a FLAC container.

const SCALE_16: f32 = 32_768.0; // 2^15
const SCALE_24: f32 = 8_388_608.0; // 2^23

#[derive(Debug, Clone, Default)]
pub struct BitAnalysis {
    samples: u64,
    not_16_bit: u64,
    not_24_bit: u64,
    clipped: u64,
    peak: f32,
}

impl BitAnalysis {
    pub fn feed(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.samples += 1;
            let magnitude = sample.abs();
            self.peak = self.peak.max(magnitude);
            if magnitude > 1.0 {
                self.clipped += 1;
            }
            // Multiplying by a power of two is exact in f32, so these
            // comparisons have no rounding slack to account for.
            let scaled_24 = sample * SCALE_24;
            if scaled_24 != scaled_24.round() {
                self.not_24_bit += 1;
                self.not_16_bit += 1;
                continue;
            }
            let scaled_16 = sample * SCALE_16;
            if scaled_16 != scaled_16.round() {
                self.not_16_bit += 1;
            }
        }
    }

    pub const fn samples(&self) -> u64 {
        self.samples
    }

    pub const fn peak(&self) -> f32 {
        self.peak
    }

    pub fn peak_dbfs(&self) -> f32 {
        20.0 * self.peak.log10()
    }

    pub const fn clipped(&self) -> u64 {
        self.clipped
    }

    /// Share of samples off the 24-bit grid: near 0 for a clean capture,
    /// near 1 once any gain or resampling stage touched the signal.
    pub fn off_grid_ratio(&self) -> f64 {
        if self.samples == 0 {
            return 0.0;
        }
        self.not_24_bit as f64 / self.samples as f64
    }

    /// Digital silence (or nothing captured). Silence sits on every grid,
    /// so it proves nothing about transparency.
    pub fn is_silent(&self) -> bool {
        self.peak == 0.0
    }

    /// The capture is an exact image of an integer PCM stream.
    pub fn is_bit_transparent(&self) -> bool {
        !self.is_silent() && self.not_24_bit == 0 && self.clipped == 0
    }

    /// Smallest integer depth that holds the capture without loss, or
    /// `None` when the signal was processed on the way.
    pub fn effective_depth(&self) -> Option<u8> {
        if !self.is_bit_transparent() {
            None
        } else if self.not_16_bit == 0 {
            Some(16)
        } else {
            Some(24)
        }
    }
}

/// How far a block of samples sits from the 16- and 24-bit grids. Tells
/// processing apart: an equalizer or resampler spreads the residual evenly
/// up to half a step (mean ≈ 0.25 step) on both grids; a clean 16-bit
/// source leaves none on either; a clean 24-bit source leaves ≈ 0.25 on
/// the 16-bit grid but none on the 24-bit one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridResidual {
    /// Share of non-zero samples off the 24-bit grid.
    pub off_grid_24: f64,
    /// Mean distance to the nearest 16-bit value, in 16-bit steps (0 to 0.5).
    pub mean_lsb_16: f64,
    /// Largest such distance.
    pub max_lsb_16: f64,
    /// Mean distance to the nearest 24-bit value, in 24-bit steps.
    pub mean_lsb_24: f64,
}

pub fn grid_residual(samples: &[f32]) -> GridResidual {
    let distance = |sample: f32, scale: f32| {
        let scaled = f64::from(sample) * f64::from(scale);
        (scaled - scaled.round()).abs()
    };
    let (mut non_zero, mut off_24) = (0_u64, 0_u64);
    let (mut sum_16, mut max_16, mut sum_24) = (0.0_f64, 0.0_f64, 0.0_f64);
    for &sample in samples.iter().filter(|s| **s != 0.0) {
        non_zero += 1;
        let to_24 = distance(sample, SCALE_24);
        if to_24 != 0.0 {
            off_24 += 1;
        }
        sum_24 += to_24;
        let to_16 = distance(sample, SCALE_16);
        sum_16 += to_16;
        max_16 = max_16.max(to_16);
    }
    let count = non_zero.max(1) as f64;
    GridResidual {
        off_grid_24: off_24 as f64 / count,
        mean_lsb_16: sum_16 / count,
        max_lsb_16: max_16,
        mean_lsb_24: sum_24 / count,
    }
}

/// A capture that is integer PCM times a constant: the signature of a
/// volume stage, as opposed to resampling or effects, which leave no
/// regular step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainEstimate {
    /// The step between consecutive values in the capture.
    pub quantum: f32,
    /// Share of the tested samples that are a whole number of steps.
    pub score: f64,
}

impl GainEstimate {
    /// The gain, assuming a 16-bit source. Above 0 dB means the source has
    /// a finer step than 16 bits: see [`Self::gain_db_24`].
    pub fn gain_db_16(&self) -> f32 {
        20.0 * (self.quantum * SCALE_16).log10()
    }

    pub fn gain_db_24(&self) -> f32 {
        20.0 * (self.quantum * SCALE_24).log10()
    }
}

/// Looks for a constant step `q` such that every sample is a whole multiple
/// of it. The smallest non-zero magnitude is some multiple `k · q`; small
/// `k` are tried in turn.
///
/// A step so fine that full scale spans more than 2^24 of them is rejected:
/// no integer source has that, and an f32 sample that far out has no
/// fractional precision left, so noise would pass for whole steps (seen as
/// a bogus "−115 dB" on processed audio).
pub fn estimate_gain(samples: &[f32]) -> Option<GainEstimate> {
    const MAX_MULTIPLE: u16 = 16;
    const TESTED: usize = 200_000;
    const TOLERANCE: f64 = 0.01;
    const MIN_SCORE: f64 = 0.99;
    const MAX_STEPS: f64 = 16_777_216.0; // 2^24

    let smallest = samples
        .iter()
        .map(|s| s.abs())
        .filter(|&s| s > 0.0)
        .min_by(f32::total_cmp)?;
    let stride = (samples.len() / TESTED).max(1);
    let tested: Vec<f32> = samples
        .iter()
        .step_by(stride)
        .copied()
        .filter(|&s| s != 0.0)
        .collect();

    let peak = tested
        .iter()
        .fold(0.0_f64, |p, &s| p.max(f64::from(s).abs()));

    (1..=MAX_MULTIPLE).find_map(|k| {
        let quantum = smallest / f32::from(k);
        let step = f64::from(quantum);
        if peak / step > MAX_STEPS {
            return None;
        }
        let whole = tested
            .iter()
            .filter(|&&s| {
                let steps = f64::from(s) / step;
                (steps - steps.round()).abs() < TOLERANCE
            })
            .count();
        let score = whole as f64 / tested.len() as f64;
        (score >= MIN_SCORE).then_some(GainEstimate { quantum, score })
    })
}

/// Converts a float sample to signed integer PCM of `bits` bits, rounding
/// to nearest and saturating at full scale.
pub fn quantize(sample: f32, bits: u8) -> i32 {
    let scale = (1_i64 << (bits - 1)) as f32;
    (sample * scale).round().clamp(-scale, scale - 1.0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyse(samples: &[f32]) -> BitAnalysis {
        let mut analysis = BitAnalysis::default();
        analysis.feed(samples);
        analysis
    }

    #[test]
    fn sixteen_bit_source_is_detected_as_16() {
        let samples: Vec<f32> = [-32_768, -1, 0, 1, 12_345, 32_767]
            .iter()
            .map(|&v| v as f32 / SCALE_16)
            .collect();
        assert_eq!(analyse(&samples).effective_depth(), Some(16));
    }

    #[test]
    fn twenty_four_bit_source_is_detected_as_24() {
        let samples: Vec<f32> = [-8_388_608, 1, 4_660_801, 8_388_607]
            .iter()
            .map(|&v| v as f32 / SCALE_24)
            .collect();
        assert_eq!(analyse(&samples).effective_depth(), Some(24));
    }

    #[test]
    fn processed_signal_is_not_transparent() {
        // Gain or resampling yields values off the 24-bit grid. (A gain
        // like 0.9 on a 16-bit sample can land back on it by accident:
        // the extra 8 bits absorb the fraction.)
        let analysis = analyse(&[0.3]);
        assert!(!analysis.is_bit_transparent());
        assert_eq!(analysis.effective_depth(), None);
    }

    #[test]
    fn empty_or_silent_capture_has_no_depth() {
        assert_eq!(analyse(&[]).effective_depth(), None);
        // Observed: paused Spotify (Store build) still sends silent packets.
        let silence = analyse(&[0.0; 1024]);
        assert!(silence.is_silent());
        assert_eq!(silence.effective_depth(), None);
    }

    /// A deterministic 16-bit "music" signal with a quiet tail.
    fn sixteen_bit_signal() -> Vec<i32> {
        (0..100_000)
            .map(|i: i32| {
                let loud = ((i.wrapping_mul(7_919)) % 60_000) - 30_000;
                if i % 1000 == 0 {
                    (i / 1000) % 5 - 2
                } else {
                    loud
                }
            })
            .collect()
    }

    #[test]
    fn constant_gain_on_16_bit_source_is_estimated() {
        let gain = 0.153_f32; // about −16.3 dB, as captured on 2026-10-01
        let samples: Vec<f32> = sixteen_bit_signal()
            .iter()
            .map(|&n| n as f32 / SCALE_16 * gain)
            .collect();
        assert!(!analyse(&samples).is_bit_transparent());
        let estimate = estimate_gain(&samples).expect("a constant gain");
        assert!((estimate.gain_db_16() - 20.0 * gain.log10()).abs() < 0.01);
    }

    #[test]
    fn transparent_16_bit_source_has_unity_gain() {
        let samples: Vec<f32> = sixteen_bit_signal()
            .iter()
            .map(|&n| n as f32 / SCALE_16)
            .collect();
        let estimate = estimate_gain(&samples).unwrap();
        assert!(estimate.gain_db_16().abs() < 0.01);
    }

    #[test]
    fn tiny_outlier_does_not_fake_a_gain() {
        // Processed audio whose smallest value is minute: dividing by it
        // used to leave no fractional precision and pass as whole steps.
        let mut samples: Vec<f32> = (1..100_000)
            .map(|i| ((i as f32) * 0.618_034).sin() * 0.9)
            .collect();
        samples.push(1.0e-9);
        assert_eq!(estimate_gain(&samples), None);
    }

    #[test]
    fn irregular_signal_has_no_gain_estimate() {
        // Like a resampler's output: no common step.
        let samples: Vec<f32> = (1..100_000)
            .map(|i| ((i as f32) * 0.618_034).sin() * 0.5)
            .collect();
        assert_eq!(estimate_gain(&samples), None);
    }

    #[test]
    fn grid_residual_tells_clean_from_processed() {
        let clean: Vec<f32> = sixteen_bit_signal()
            .iter()
            .map(|&n| n as f32 / SCALE_16)
            .collect();
        let r = grid_residual(&clean);
        assert_eq!((r.off_grid_24, r.max_lsb_16), (0.0, 0.0));

        let processed: Vec<f32> = clean.iter().map(|s| s * 0.9).collect();
        let r = grid_residual(&processed);
        assert!(r.off_grid_24 > 0.5);
        assert!(r.mean_lsb_16 > 0.1, "{r:?}");
    }

    #[test]
    fn quantize_round_trips_and_saturates() {
        assert_eq!(quantize(4_660_801.0 / SCALE_24, 24), 4_660_801);
        assert_eq!(quantize(1.0, 24), 8_388_607);
        assert_eq!(quantize(-1.5, 16), -32_768);
    }
}

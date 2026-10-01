//! A track is captured into an intermediate float WAV ([`wav::CaptureWav`]),
//! then encoded to the user's format once the track ends. Encoding after
//! the fact keeps the capture path cheap and lets the encoder use the
//! whole-track [`crate::analysis::BitAnalysis`] (FLAC bit depth).

pub mod flac;
pub mod mp3;
pub mod wav;

use crate::analysis::quantize;

/// Float to integer PCM for the lossless outputs.
///
/// At the capture's own depth, plain rounding is exact. Going below it
/// (a 24-bit source written at 16 bits) adds TPDF dither: rounding alone
/// would turn the lost bits into distortion correlated with the music.
pub struct Quantizer {
    bits: u8,
    dither: Option<XorShift>,
}

impl Quantizer {
    /// `source_bits` is the depth the capture holds (from the analysis).
    pub fn new(bits: u8, source_bits: u8) -> Self {
        Self {
            bits,
            dither: (bits < source_bits).then_some(XorShift(0x9E37_79B9_7F4A_7C15)),
        }
    }

    pub fn bits(&self) -> u8 {
        self.bits
    }

    pub fn quantize(&mut self, sample: f32) -> i32 {
        let Some(rng) = self.dither.as_mut() else {
            return quantize(sample, self.bits);
        };
        // Triangular noise of ±1 LSB: the difference of two uniform draws.
        let lsb = 1.0 / (1_i64 << (self.bits - 1)) as f32;
        let noise = (rng.unit() - rng.unit()) * lsb;
        quantize(sample + noise, self.bits)
    }
}

/// Tiny deterministic generator: dither needs decorrelation, not
/// cryptographic quality, and a fixed seed keeps encodes reproducible.
struct XorShift(u64);

impl XorShift {
    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1_u64 << 24) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_depth_is_plain_rounding() {
        let mut q = Quantizer::new(16, 16);
        assert_eq!(q.quantize(1234.0 / 32_768.0), 1234);
    }

    #[test]
    fn lower_depth_is_dithered_but_centred() {
        // A 24-bit value half-way between two 16-bit steps: dither must
        // spread it over both, averaging to the true value.
        let sample = 1234.5 / 32_768.0;
        let mut q = Quantizer::new(16, 24);
        let values: Vec<i32> = (0..10_000).map(|_| q.quantize(sample)).collect();
        assert!(values.iter().all(|v| (1233..=1236).contains(v)));
        let mean = values.iter().map(|&v| f64::from(v)).sum::<f64>() / values.len() as f64;
        assert!((mean - 1234.5).abs() < 0.05, "{mean}");
    }
}

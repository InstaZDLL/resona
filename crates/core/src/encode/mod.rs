//! A track is captured into an intermediate float WAV ([`wav::CaptureWav`]),
//! then encoded to the user's format once the track ends. Encoding after
//! the fact keeps the capture path cheap and lets the encoder use the
//! whole-track [`crate::analysis::BitAnalysis`] (FLAC bit depth).

pub mod flac;
pub mod mp3;
pub mod wav;

use crate::analysis::quantize;
use crate::format::{ChannelLayout, downmix_stereo};
use crate::{Error, Result};

/// Which channels of the intermediate WAV go into the output file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channels {
    /// Front left / right: a stereo track, whatever the capture's layout
    /// (on a surround cable, a stereo track leaves the others silent).
    FrontPair,
    /// Every channel, in the capture's layout: a surround track
    /// ([`crate::analysis::Fidelity::Surround`]) in FLAC or WAV.
    All(ChannelLayout),
    /// A stereo downmix of every channel: a surround track in MP3.
    Downmix(ChannelLayout),
}

impl Channels {
    /// Channels per frame of the output.
    pub fn count(self) -> u16 {
        match self {
            Self::All(layout) => layout.channels,
            Self::FrontPair | Self::Downmix(_) => ChannelLayout::STEREO.channels,
        }
    }
}

/// The samples of an intermediate WAV with `source` channels per frame,
/// turned into [`Channels`]' output, frame by frame.
pub(crate) struct Remix<I> {
    samples: I,
    source: usize,
    channels: Channels,
    frame: Vec<f32>,
    out: Vec<f32>,
    next: usize,
}

impl<I: Iterator<Item = hound::Result<f32>>> Remix<I> {
    pub(crate) fn new(samples: I, source: u16, channels: Channels) -> Result<Self> {
        let source = usize::from(source);
        let fits = match channels {
            Channels::FrontPair => source >= 2,
            Channels::All(layout) | Channels::Downmix(layout) => {
                usize::from(layout.channels) == source
            }
        };
        if !fits {
            return Err(Error::UnsupportedWav(
                "the capture's channels do not match the output",
            ));
        }
        Ok(Self {
            samples,
            source,
            channels,
            frame: Vec::with_capacity(source),
            out: Vec::with_capacity(source),
            next: 0,
        })
    }

    /// Reads the next source frame into `out`; `false` at the end.
    fn refill(&mut self) -> Result<bool> {
        self.frame.clear();
        for sample in self.samples.by_ref().take(self.source) {
            self.frame.push(sample?);
        }
        if self.frame.is_empty() {
            return Ok(false);
        }
        if self.frame.len() != self.source {
            return Err(Error::UnsupportedWav("incomplete audio frame"));
        }
        self.out.clear();
        match self.channels {
            Channels::FrontPair => self.out.extend_from_slice(&self.frame[..2]),
            Channels::All(_) => self.out.extend_from_slice(&self.frame),
            Channels::Downmix(layout) => self.out.extend(downmix_stereo(&self.frame, layout)),
        }
        self.next = 0;
        Ok(true)
    }
}

impl<I: Iterator<Item = hound::Result<f32>>> Iterator for Remix<I> {
    type Item = Result<f32>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.out.len() {
            match self.refill() {
                Ok(true) => {}
                Ok(false) => return None,
                Err(e) => return Some(Err(e)),
            }
        }
        self.next += 1;
        Some(Ok(self.out[self.next - 1]))
    }
}

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

    fn remix(samples: &[f32], source: u16, channels: Channels) -> Result<Vec<f32>> {
        Remix::new(samples.iter().map(|&s| Ok(s)), source, channels)?.collect()
    }

    #[test]
    fn remix_keeps_or_reduces_the_channels() {
        let layout = ChannelLayout::SURROUND_7_1;
        let frames = [
            0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, //
            -0.1, -0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ];
        assert_eq!(
            remix(&frames, 8, Channels::FrontPair).unwrap(),
            [0.1, 0.2, -0.1, -0.2]
        );
        assert_eq!(remix(&frames, 8, Channels::All(layout)).unwrap(), frames);
        assert_eq!(
            remix(&frames, 8, Channels::Downmix(layout)).unwrap().len(),
            4
        );
        assert_eq!(
            remix(&[0.5, -0.5], 2, Channels::FrontPair).unwrap(),
            [0.5, -0.5]
        );
        assert!(remix(&frames[..7], 8, Channels::All(layout)).is_err());
        assert!(remix(&frames, 2, Channels::All(layout)).is_err());
    }

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

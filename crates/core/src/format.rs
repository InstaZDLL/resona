//! Capture format and output formats.

/// Rate the per-process loopback stream is requested at.
///
/// Spotify streams at 44.1 kHz in every quality tier, Lossless included
/// (up to 24-bit / 44.1 kHz FLAC). Asking WASAPI for the same rate means
/// no resampling on our side; whether Windows resampled *before* us is
/// what [`crate::analysis::BitAnalysis`] detects.
pub const CAPTURE_SAMPLE_RATE: u32 = 44_100;
pub const CAPTURE_CHANNELS: u16 = 2;

/// The quality the user selected in Spotify's own settings. Spytify cannot
/// read it from the client, so it is a user setting that drives the default
/// output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotifyQuality {
    /// Free tier, 160 kbps Ogg Vorbis.
    Free,
    /// Premium "Very high", 320 kbps.
    Premium,
    /// Premium "Lossless", FLAC up to 24-bit / 44.1 kHz.
    Lossless,
}

impl SpotifyQuality {
    pub const fn default_output(self) -> OutputFormat {
        match self {
            Self::Free => OutputFormat::Mp3 { kbps: 160 },
            Self::Premium => OutputFormat::Mp3 { kbps: 320 },
            Self::Lossless => OutputFormat::Flac {
                depth: BitDepth::Auto,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    /// 16 when every captured sample is an exact 16-bit value, 24 otherwise.
    /// Spotify Lossless serves 16-bit masters as 16-bit, so this avoids
    /// padding them to 24 for nothing.
    Auto,
    Bits16,
    Bits24,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Wav,
    Mp3 { kbps: u32 },
    Flac { depth: BitDepth },
}

/// Keeps the front-left / front-right pair of an interleaved stream of
/// `channels` channels (WAVE channel order puts them first), and returns
/// the peak found in all other channels.
///
/// Capturing in the device's own channel count and dropping the rest
/// avoids Windows' downmix to stereo, which mixes the other channels in
/// with fractional coefficients and ruins bit-transparency.
pub fn front_stereo(interleaved: &[f32], channels: usize) -> (Vec<f32>, f32) {
    debug_assert!(channels >= 2);
    let mut stereo = Vec::with_capacity(interleaved.len() / channels * 2);
    let mut other_peak = 0.0_f32;
    for frame in interleaved.chunks_exact(channels) {
        stereo.extend_from_slice(&frame[..2]);
        for sample in &frame[2..] {
            other_peak = other_peak.max(sample.abs());
        }
    }
    (stereo, other_peak)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_stereo_keeps_the_front_pair() {
        // Two 7.1 frames: FL FR FC LFE BL BR SL SR.
        let frames = [
            0.1, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, //
            0.3, 0.4, 0.5, 0.0, 0.0, -0.6, 0.0, 0.0,
        ];
        let (stereo, other_peak) = front_stereo(&frames, 8);
        assert_eq!(stereo, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(other_peak, 0.6);
    }

    #[test]
    fn stereo_passes_through() {
        let (stereo, other_peak) = front_stereo(&[0.1, -0.2], 2);
        assert_eq!(stereo, [0.1, -0.2]);
        assert_eq!(other_peak, 0.0);
    }
}

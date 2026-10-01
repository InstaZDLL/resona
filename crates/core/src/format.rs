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

impl BitDepth {
    /// Bits to write for a capture that holds `source_bits` (see
    /// [`crate::analysis::Fidelity::lossless_depth`]).
    pub fn resolve(self, source_bits: u8) -> u8 {
        match self {
            Self::Auto => source_bits,
            Self::Bits16 => 16,
            Self::Bits24 => 24,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Wav { depth: BitDepth },
    Mp3 { kbps: u32 },
    Flac { depth: BitDepth },
}

impl OutputFormat {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Wav { .. } => "wav",
            Self::Mp3 { .. } => "mp3",
            Self::Flac { .. } => "flac",
        }
    }
}

/// The form [`OutputFormat::from_str`](std::str::FromStr) reads back.
impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let depth = |depth: BitDepth| match depth {
            BitDepth::Auto => "",
            BitDepth::Bits16 => "16",
            BitDepth::Bits24 => "24",
        };
        match *self {
            Self::Flac { depth: d } => write!(f, "flac{}", depth(d)),
            Self::Wav { depth: d } => write!(f, "wav{}", depth(d)),
            Self::Mp3 { kbps } => write!(f, "mp3:{kbps}"),
        }
    }
}

impl std::str::FromStr for OutputFormat {
    type Err = String;

    /// `flac`, `flac16`, `flac24`, `wav`, `wav16`, `wav24`, `mp3` (320 kbps)
    /// or `mp3:<kbps>`.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let depth = |suffix: &str| match suffix {
            "" => Ok(BitDepth::Auto),
            "16" => Ok(BitDepth::Bits16),
            "24" => Ok(BitDepth::Bits24),
            other => Err(format!("unknown bit depth {other:?}")),
        };
        let value = value.to_ascii_lowercase();
        if let Some(rest) = value.strip_prefix("flac") {
            Ok(Self::Flac {
                depth: depth(rest)?,
            })
        } else if let Some(rest) = value.strip_prefix("wav") {
            Ok(Self::Wav {
                depth: depth(rest)?,
            })
        } else if let Some(rest) = value.strip_prefix("mp3") {
            let kbps = match rest.strip_prefix(':') {
                Some(kbps) => kbps.parse().map_err(|_| format!("bad bitrate {kbps:?}"))?,
                None if rest.is_empty() => 320,
                None => return Err(format!("unknown format {value:?}")),
            };
            if !matches!(kbps, 128 | 160 | 192 | 256 | 320) {
                return Err(format!(
                    "unsupported MP3 bitrate {kbps} (128, 160, 192, 256, 320)"
                ));
            }
            Ok(Self::Mp3 { kbps })
        } else {
            Err(format!("unknown format {value:?} (flac, wav, mp3)"))
        }
    }
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
    fn parses_output_formats() {
        let parse = |s: &str| s.parse::<OutputFormat>();
        assert_eq!(
            parse("FLAC"),
            Ok(OutputFormat::Flac {
                depth: BitDepth::Auto
            })
        );
        assert_eq!(
            parse("flac16"),
            Ok(OutputFormat::Flac {
                depth: BitDepth::Bits16
            })
        );
        assert_eq!(
            parse("wav24"),
            Ok(OutputFormat::Wav {
                depth: BitDepth::Bits24
            })
        );
        assert_eq!(parse("mp3"), Ok(OutputFormat::Mp3 { kbps: 320 }));
        assert_eq!(parse("mp3:160"), Ok(OutputFormat::Mp3 { kbps: 160 }));
        assert!(parse("mp3:300").is_err());
        assert!(parse("ogg").is_err());
        for text in ["flac", "flac16", "wav24", "mp3:192"] {
            assert_eq!(parse(text).unwrap().to_string(), text);
        }
        assert_eq!(BitDepth::Auto.resolve(24), 24);
        assert_eq!(BitDepth::Bits16.resolve(24), 16);
    }

    #[test]
    fn stereo_passes_through() {
        let (stereo, other_peak) = front_stereo(&[0.1, -0.2], 2);
        assert_eq!(stereo, [0.1, -0.2]);
        assert_eq!(other_peak, 0.0);
    }
}

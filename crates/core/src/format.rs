//! Capture format and output formats.

/// Rate the per-process loopback stream is requested at.
///
/// Spotify streams at 44.1 kHz in every quality tier, Lossless included
/// (up to 24-bit / 44.1 kHz FLAC). Asking WASAPI for the same rate means
/// no resampling on our side; whether Windows resampled *before* us is
/// what [`crate::analysis::BitAnalysis`] detects.
pub const CAPTURE_SAMPLE_RATE: u32 = 44_100;
pub const CAPTURE_CHANNELS: u16 = 2;

/// The quality the user selected in Spotify's own settings. Resona cannot
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

/// Speaker positions of a `WAVEFORMATEXTENSIBLE` channel mask, in the
/// order their channels come in (lowest bit first).
mod speaker {
    pub const FRONT_LEFT: u32 = 0x1;
    pub const FRONT_RIGHT: u32 = 0x2;
    pub const FRONT_CENTER: u32 = 0x4;
    pub const LOW_FREQUENCY: u32 = 0x8;
    pub const BACK_LEFT: u32 = 0x10;
    pub const BACK_RIGHT: u32 = 0x20;
    pub const FRONT_LEFT_OF_CENTER: u32 = 0x40;
    pub const FRONT_RIGHT_OF_CENTER: u32 = 0x80;
    pub const BACK_CENTER: u32 = 0x100;
    pub const SIDE_LEFT: u32 = 0x200;
    pub const SIDE_RIGHT: u32 = 0x400;
}

/// The channels of a capture: how many, and on which speakers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelLayout {
    pub channels: u16,
    /// `WAVEFORMATEXTENSIBLE` speaker mask; channels come in the order of
    /// its bits.
    pub mask: u32,
}

impl ChannelLayout {
    pub const STEREO: Self = Self {
        channels: CAPTURE_CHANNELS,
        mask: speaker::FRONT_LEFT | speaker::FRONT_RIGHT,
    };
    /// 7.1 as Windows sets a device up ("7.1 Surround"): FL FR FC LFE BL BR
    /// SL SR.
    pub const SURROUND_7_1: Self = Self {
        channels: 8,
        mask: 0x63f,
    };

    pub fn is_stereo(self) -> bool {
        self.channels == CAPTURE_CHANNELS
    }

    /// More than the front pair, in an order FLAC knows without a channel
    /// mask tag: FLAC's default order for the count is WAVE's for these
    /// masks (5.1 counts with its back or side pair). Every layout Windows
    /// offers in a device's speaker setup is one.
    pub fn is_standard_surround(self) -> bool {
        matches!(
            (self.channels, self.mask),
            (4, 0x33) | (5, 0x37) | (6, 0x3f | 0x60f) | (7, 0x70f) | (8, 0x63f)
        )
    }

    /// Left and right gains of each channel for a stereo downmix: the front
    /// pair as is, centre and surrounds at -3 dB (ITU-R BS.775), the LFE
    /// left out. Scaled so that no output can exceed full scale (as
    /// FFmpeg's default rematrixing does): quieter than the surround mix,
    /// never clipped.
    fn downmix_gains(self) -> Vec<(f32, f32)> {
        use speaker::*;
        const HALF_POWER: f32 = std::f32::consts::FRAC_1_SQRT_2;
        let mut gains: Vec<(f32, f32)> = (0..32)
            .map(|bit| 1_u32 << bit)
            .filter(|bit| self.mask & bit != 0)
            .map(|bit| match bit {
                FRONT_LEFT | FRONT_LEFT_OF_CENTER => (1.0, 0.0),
                FRONT_RIGHT | FRONT_RIGHT_OF_CENTER => (0.0, 1.0),
                FRONT_CENTER => (HALF_POWER, HALF_POWER),
                BACK_LEFT | SIDE_LEFT => (HALF_POWER, 0.0),
                BACK_RIGHT | SIDE_RIGHT => (0.0, HALF_POWER),
                BACK_CENTER => (0.5, 0.5),
                // Bass the speakers of a stereo pair do not reproduce.
                LOW_FREQUENCY => (0.0, 0.0),
                // The height speakers no device layout has.
                _ => (0.0, 0.0),
            })
            .collect();
        // A mask that names fewer speakers than there are channels: the
        // rest are not placed, and stay out.
        gains.resize(usize::from(self.channels), (0.0, 0.0));
        gains.truncate(usize::from(self.channels));
        let left: f32 = gains.iter().map(|g| g.0).sum();
        let right: f32 = gains.iter().map(|g| g.1).sum();
        let scale = left.max(right).max(1.0);
        gains
            .into_iter()
            .map(|(l, r)| (l / scale, r / scale))
            .collect()
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
    for frame in interleaved.chunks_exact(channels) {
        stereo.extend_from_slice(&frame[..2]);
    }
    (stereo, other_channels_peak(interleaved, channels))
}

/// The peak of the channels beyond the front pair of an interleaved
/// stream of `channels` channels; 0 for stereo.
pub fn other_channels_peak(interleaved: &[f32], channels: usize) -> f32 {
    interleaved
        .chunks_exact(channels)
        .flat_map(|frame| &frame[2..])
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

/// A stereo downmix of an interleaved stream in `layout` (see
/// [`ChannelLayout::downmix_gains`]), for outputs that only hold stereo
/// (MP3).
pub fn downmix_stereo(interleaved: &[f32], layout: ChannelLayout) -> Vec<f32> {
    let gains = layout.downmix_gains();
    let mut stereo = Vec::with_capacity(interleaved.len() / gains.len() * 2);
    for frame in interleaved.chunks_exact(gains.len()) {
        let (left, right) = frame
            .iter()
            .zip(&gains)
            .fold((0.0, 0.0), |(l, r), (s, g)| (l + s * g.0, r + s * g.1));
        stereo.extend_from_slice(&[left, right]);
    }
    stereo
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
    fn surround_layouts_flac_can_name() {
        assert!(ChannelLayout::SURROUND_7_1.is_standard_surround());
        assert!(!ChannelLayout::STEREO.is_standard_surround());
        // 7.1 with the front-of-centre pair (Windows' old "7.1 wide").
        let wide = ChannelLayout {
            channels: 8,
            mask: 0xff,
        };
        assert!(!wide.is_standard_surround());
        let side = ChannelLayout {
            channels: 6,
            mask: 0x60f,
        };
        assert!(side.is_standard_surround());
    }

    #[test]
    fn downmix_never_clips_and_drops_the_lfe() {
        // FL FR FC LFE BL BR SL SR at full scale: the loudest a side gets.
        let full = [1.0; 8];
        let stereo = downmix_stereo(&full, ChannelLayout::SURROUND_7_1);
        assert_eq!(stereo.len(), 2);
        assert!((stereo[0] - 1.0).abs() < 1e-6 && (stereo[1] - 1.0).abs() < 1e-6);
        // The LFE alone is silent; the centre goes to both sides equally;
        // a side speaker only to its own side.
        let lfe = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        assert_eq!(
            downmix_stereo(&lfe, ChannelLayout::SURROUND_7_1),
            [0.0, 0.0]
        );
        let centre = [0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mixed = downmix_stereo(&centre, ChannelLayout::SURROUND_7_1);
        assert!(mixed[0] > 0.0 && mixed[0] == mixed[1]);
        let side_left = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0];
        let mixed = downmix_stereo(&side_left, ChannelLayout::SURROUND_7_1);
        assert!(mixed[0] > 0.0 && mixed[1] == 0.0);
        // Stereo is left as it is.
        assert_eq!(
            downmix_stereo(&[0.25, -0.5], ChannelLayout::STEREO),
            [0.25, -0.5]
        );
    }

    #[test]
    fn stereo_passes_through() {
        let (stereo, other_peak) = front_stereo(&[0.1, -0.2], 2);
        assert_eq!(stereo, [0.1, -0.2]);
        assert_eq!(other_peak, 0.0);
    }
}

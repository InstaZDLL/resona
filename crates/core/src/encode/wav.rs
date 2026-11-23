use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use hound::{SampleFormat, WavSpec, WavWriter};

use super::{Channels, Remix};
use crate::format::ChannelLayout;
use crate::{Error, Result};

/// Spec of the intermediate file: the capture format, untouched.
pub fn capture_spec(sample_rate: u32, channels: u16) -> WavSpec {
    WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    }
}

/// Intermediate 32-bit float WAV a track is captured into.
pub struct CaptureWav {
    writer: WavWriter<BufWriter<File>>,
    path: PathBuf,
    layout: ChannelLayout,
}

impl CaptureWav {
    /// Stereo, at the rate the audio was captured at, never resampled.
    pub fn create(path: &Path, sample_rate: u32) -> Result<Self> {
        Self::with_layout(path, sample_rate, ChannelLayout::STEREO)
    }

    /// In `layout`, written as `WAVE_FORMAT_EXTENSIBLE` with its speaker
    /// mask beyond stereo.
    pub fn with_layout(path: &Path, sample_rate: u32, layout: ChannelLayout) -> Result<Self> {
        Ok(Self {
            writer: WavWriter::create(path, capture_spec(sample_rate, layout.channels))?,
            path: path.to_path_buf(),
            layout,
        })
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        for &sample in samples {
            self.writer.write_sample(sample)?;
        }
        Ok(())
    }

    pub fn finalize(self) -> Result<()> {
        self.writer.finalize()?;
        set_channel_mask(&self.path, self.layout)
    }
}

/// Writes an intermediate float WAV as integer PCM WAV at the quantizer's
/// depth: the lossless output for players without FLAC.
pub fn export_wav(
    src: &Path,
    dst: &Path,
    mut quantizer: super::Quantizer,
    channels: Channels,
) -> Result<()> {
    let mut reader = hound::WavReader::open(src)?;
    let source = reader.spec();
    let spec = WavSpec {
        channels: channels.count(),
        bits_per_sample: u16::from(quantizer.bits()),
        sample_format: SampleFormat::Int,
        ..source
    };
    let mut writer = WavWriter::create(dst, spec)?;
    for sample in Remix::new(reader.samples::<f32>(), source.channels, channels)? {
        writer.write_sample(quantizer.quantize(sample?))?;
    }
    writer.finalize()?;
    match channels {
        Channels::All(layout) => set_channel_mask(dst, layout),
        Channels::FrontPair | Channels::Downmix(_) => Ok(()),
    }
}

/// Offset of `dwChannelMask` in a WAV whose `fmt ` chunk comes first, as
/// hound writes it: RIFF header (12), chunk header (8), then 20 bytes of
/// `WAVEFORMATEXTENSIBLE` before the mask.
const CHANNEL_MASK_OFFSET: u64 = 40;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xfffe;

/// Writes `layout`'s speaker mask into the WAV at `path`. hound only
/// writes "the first n speakers" (`0xff` for 8 channels: front-of-centre
/// instead of side speakers for 7.1). Stereo needs nothing.
fn set_channel_mask(path: &Path, layout: ChannelLayout) -> Result<()> {
    if layout.is_stereo() {
        return Ok(());
    }
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut header = [0_u8; 44];
    file.read_exact(&mut header)?;
    let extensible = &header[..4] == b"RIFF"
        && &header[8..16] == b"WAVEfmt "
        && u16::from_le_bytes([header[20], header[21]]) == WAVE_FORMAT_EXTENSIBLE
        && u16::from_le_bytes([header[22], header[23]]) == layout.channels;
    if !extensible {
        return Err(Error::UnsupportedWav("no WAVE_FORMAT_EXTENSIBLE header"));
    }
    file.seek(SeekFrom::Start(CHANNEL_MASK_OFFSET))?;
    file.write_all(&layout.mask.to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::Quantizer;

    #[test]
    fn exports_integer_pcm() {
        let dir = std::env::temp_dir().join(format!("resona-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (capture, out) = (dir.join("track.wav"), dir.join("track.out.wav"));
        let values = [1234, -32_768, 32_767, 0];
        let samples: Vec<f32> = values.iter().map(|&v| v as f32 / 32_768.0).collect();
        let mut writer = CaptureWav::create(&capture, crate::format::CAPTURE_SAMPLE_RATE).unwrap();
        writer.write(&samples).unwrap();
        writer.finalize().unwrap();

        export_wav(&capture, &out, Quantizer::new(16, 16), Channels::FrontPair).unwrap();
        let mut reader = hound::WavReader::open(&out).unwrap();
        assert_eq!(reader.spec().bits_per_sample, 16);
        let read: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read, values.map(|v| v as i16));
    }

    /// `dwChannelMask` of a WAV file hound wrote.
    fn channel_mask(path: &Path) -> u32 {
        let bytes = std::fs::read(path).unwrap();
        u32::from_le_bytes(bytes[40..44].try_into().unwrap())
    }

    #[test]
    fn keeps_a_7_1_capture_with_its_speakers() {
        let dir = std::env::temp_dir().join(format!("resona-wav71-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (capture, out) = (dir.join("track.wav"), dir.join("track.out.wav"));
        let layout = ChannelLayout::SURROUND_7_1;
        // Two frames, every channel its own 24-bit value.
        let samples: Vec<f32> = (1..=16).map(|v| (v * 1000) as f32 / 8_388_608.0).collect();
        let mut writer = CaptureWav::with_layout(&capture, 48_000, layout).unwrap();
        writer.write(&samples).unwrap();
        writer.finalize().unwrap();
        assert_eq!(channel_mask(&capture), 0x63f);
        let reader = hound::WavReader::open(&capture).unwrap();
        assert_eq!(reader.spec().channels, 8);
        assert_eq!(reader.duration(), 2);

        export_wav(
            &capture,
            &out,
            Quantizer::new(24, 24),
            Channels::All(layout),
        )
        .unwrap();
        assert_eq!(channel_mask(&out), 0x63f);
        let mut reader = hound::WavReader::open(&out).unwrap();
        assert_eq!(reader.spec().channels, 8);
        let read: Vec<i32> = reader.samples::<i32>().map(Result::unwrap).collect();
        assert_eq!(read, (1..=16).map(|v| v * 1000).collect::<Vec<_>>());

        // The front pair alone, for a stereo track on a 7.1 cable.
        export_wav(&capture, &out, Quantizer::new(24, 24), Channels::FrontPair).unwrap();
        let mut reader = hound::WavReader::open(&out).unwrap();
        assert_eq!(reader.spec().channels, 2);
        let read: Vec<i32> = reader.samples::<i32>().map(Result::unwrap).collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read, [1000, 2000, 9000, 10_000]);
    }
}

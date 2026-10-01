use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};

use crate::Result;
use crate::format::{CAPTURE_CHANNELS, CAPTURE_SAMPLE_RATE};

/// Spec of the intermediate file: the capture format, untouched.
pub const CAPTURE_SPEC: WavSpec = WavSpec {
    channels: CAPTURE_CHANNELS,
    sample_rate: CAPTURE_SAMPLE_RATE,
    bits_per_sample: 32,
    sample_format: SampleFormat::Float,
};

/// Intermediate 32-bit float WAV a track is captured into.
pub struct CaptureWav {
    writer: WavWriter<BufWriter<File>>,
}

impl CaptureWav {
    pub fn create(path: &Path) -> Result<Self> {
        Ok(Self {
            writer: WavWriter::create(path, CAPTURE_SPEC)?,
        })
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        for &sample in samples {
            self.writer.write_sample(sample)?;
        }
        Ok(())
    }

    pub fn finalize(self) -> Result<()> {
        Ok(self.writer.finalize()?)
    }
}

/// Writes an intermediate float WAV as integer PCM WAV at the quantizer's
/// depth: the lossless output for players without FLAC.
pub fn export_wav(src: &Path, dst: &Path, mut quantizer: super::Quantizer) -> Result<()> {
    let mut reader = hound::WavReader::open(src)?;
    let spec = WavSpec {
        bits_per_sample: u16::from(quantizer.bits()),
        sample_format: SampleFormat::Int,
        ..reader.spec()
    };
    let mut writer = WavWriter::create(dst, spec)?;
    for sample in reader.samples::<f32>() {
        writer.write_sample(quantizer.quantize(sample?))?;
    }
    Ok(writer.finalize()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::Quantizer;

    #[test]
    fn exports_integer_pcm() {
        let dir = std::env::temp_dir().join(format!("spytify-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (capture, out) = (dir.join("track.wav"), dir.join("track.out.wav"));
        let values = [1234, -32_768, 32_767, 0];
        let samples: Vec<f32> = values.iter().map(|&v| v as f32 / 32_768.0).collect();
        let mut writer = CaptureWav::create(&capture).unwrap();
        writer.write(&samples).unwrap();
        writer.finalize().unwrap();

        export_wav(&capture, &out, Quantizer::new(16, 16)).unwrap();
        let mut reader = hound::WavReader::open(&out).unwrap();
        assert_eq!(reader.spec().bits_per_sample, 16);
        let read: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read, values.map(|v| v as i16));
    }
}

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

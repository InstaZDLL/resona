//! FLAC output, the format of Spotify Lossless rips.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use flacenc::component::BitRepr;
use flacenc::error::{SourceError, Verify};
use flacenc::source::{Fill, Source};
use hound::{SampleFormat, WavReader};

use super::Quantizer;
use crate::{Error, Result};

/// Encodes an intermediate float WAV (see [`super::wav::CaptureWav`]) to
/// FLAC at the quantizer's depth (16 or 24 bits).
pub fn encode_wav_to_flac(src: &Path, dst: &Path, quantizer: Quantizer) -> Result<()> {
    debug_assert!(quantizer.bits() == 16 || quantizer.bits() == 24);
    let reader = WavReader::open(src)?;
    let spec = reader.spec();
    if spec.sample_format != SampleFormat::Float || spec.bits_per_sample != 32 {
        return Err(Error::UnsupportedWav("expected 32-bit float samples"));
    }

    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| Error::Flac(e.to_string()))?;
    let source = WavSource {
        reader,
        quantizer,
        buffer: Vec::new(),
    };
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| Error::Flac(e.to_string()))?;

    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| Error::Flac(e.to_string()))?;
    std::fs::write(dst, sink.as_slice())?;
    Ok(())
}

/// Streams the WAV block by block, so a long track is never held in
/// memory as raw PCM.
struct WavSource {
    reader: WavReader<BufReader<File>>,
    quantizer: Quantizer,
    buffer: Vec<i32>,
}

impl Source for WavSource {
    fn channels(&self) -> usize {
        usize::from(self.reader.spec().channels)
    }

    fn bits_per_sample(&self) -> usize {
        usize::from(self.quantizer.bits())
    }

    fn sample_rate(&self) -> usize {
        self.reader.spec().sample_rate as usize
    }

    fn read_samples<F: Fill>(
        &mut self,
        block_size: usize,
        dest: &mut F,
    ) -> Result<usize, SourceError> {
        let channels = self.channels();
        self.buffer.clear();
        for sample in self.reader.samples::<f32>().take(block_size * channels) {
            let sample = sample.map_err(SourceError::from_io_error)?;
            self.buffer.push(self.quantizer.quantize(sample));
        }
        dest.fill_interleaved(&self.buffer)?;
        Ok(self.buffer.len() / channels)
    }

    fn len_hint(&self) -> Option<usize> {
        Some(self.reader.duration() as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::wav::CaptureWav;

    #[test]
    fn encodes_a_24_bit_capture() {
        let dir = std::env::temp_dir().join(format!("spytify-flac-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wav_path, flac_path) = (dir.join("in.wav"), dir.join("out.flac"));

        // One second of a 24-bit-exact ramp, stereo.
        let samples: Vec<f32> = (0..44_100 * 2)
            .map(|i| ((i * 97) % 8_388_608 - 4_194_304) as f32 / 8_388_608.0)
            .collect();
        let mut wav = CaptureWav::create(&wav_path).unwrap();
        wav.write(&samples).unwrap();
        wav.finalize().unwrap();

        encode_wav_to_flac(&wav_path, &flac_path, Quantizer::new(24, 24)).unwrap();
        let bytes = std::fs::read(&flac_path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(&bytes[..4], b"fLaC");
        // STREAMINFO: bits-per-sample minus one sits across bytes 20-21.
        let bits = (((bytes[20] & 0x01) << 4) | (bytes[21] >> 4)) + 1;
        assert_eq!(bits, 24);
    }
}

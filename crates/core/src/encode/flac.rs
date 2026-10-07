//! FLAC output, the format of Spotify Lossless rips.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use flac_bound::{FlacEncoder, WriteWrapper};
use hound::{SampleFormat, WavReader};
use md5::{Digest, Md5};

use super::Quantizer;
use crate::{Error, Result};

/// Encodes an intermediate float WAV (see [`super::wav::CaptureWav`]) to
/// FLAC at the quantizer's depth (16 or 24 bits).
pub fn encode_wav_to_flac(src: &Path, dst: &Path, quantizer: Quantizer) -> Result<()> {
    debug_assert!(quantizer.bits() == 16 || quantizer.bits() == 24);
    let mut reader = WavReader::open(src)?;
    let spec = reader.spec();
    if spec.sample_format != SampleFormat::Float || spec.bits_per_sample != 32 {
        return Err(Error::UnsupportedWav("expected 32-bit float samples"));
    }
    let sample_frames = reader.duration();
    let bits = quantizer.bits();

    // libFLAC writes standard predictive frames. The previous pure-Rust
    // encoder produced files that decoded sequentially but failed seeking in
    // PotPlayer, even after their metadata was stripped and remuxed.
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)?;
    let mut pcm_md5 = Md5::new();
    {
        let mut writer = WriteWrapper(&mut output);
        let config = FlacEncoder::new()
            .ok_or_else(|| Error::Flac("could not create FLAC encoder".into()))?
            .channels(u32::from(spec.channels))
            .bits_per_sample(u32::from(bits))
            .sample_rate(spec.sample_rate)
            .total_samples_estimate(u64::from(sample_frames))
            .compression_level(5)
            .verify(true);
        let mut encoder = config
            .init_write(&mut writer)
            .map_err(|e| Error::Flac(format!("could not initialize FLAC encoder: {e:?}")))?;

        let channels = usize::from(spec.channels);
        let mut quantizer = quantizer;
        let mut buffer = Vec::with_capacity(4096 * channels);
        let mut pcm_bytes = Vec::with_capacity(4096 * channels * usize::from(bits / 8));
        for sample in reader.samples::<f32>() {
            let quantized = quantizer.quantize(sample?);
            buffer.push(quantized);
            pcm_bytes.extend_from_slice(&quantized.to_le_bytes()[..usize::from(bits / 8)]);
            if buffer.len() == 4096 * channels {
                pcm_md5.update(&pcm_bytes);
                pcm_bytes.clear();
                encoder
                    .process_interleaved(&buffer, 4096)
                    .map_err(|()| Error::Flac(format!("FLAC encoder: {:?}", encoder.state())))?;
                buffer.clear();
            }
        }
        if !buffer.is_empty() {
            if buffer.len() % channels != 0 {
                return Err(Error::UnsupportedWav("incomplete audio frame"));
            }
            let frames = (buffer.len() / channels) as u32;
            pcm_md5.update(&pcm_bytes);
            encoder
                .process_interleaved(&buffer, frames)
                .map_err(|()| Error::Flac(format!("FLAC encoder: {:?}", encoder.state())))?;
        }
        encoder
            .finish()
            .map_err(|encoder| Error::Flac(format!("FLAC encoder: {:?}", encoder.state())))?;
    }
    // flac-bound supplies no seek callback, so libFLAC cannot backpatch the
    // STREAMINFO checksum itself. The MD5 covers signed, interleaved PCM with
    // each sample stored little-endian in exactly 16 or 24 bits.
    output.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; 8];
    output.read_exact(&mut header)?;
    if &header[..4] != b"fLaC" || header[4] & 0x7f != 0 || header[5..8] != [0, 0, 34] {
        return Err(Error::Flac("missing FLAC STREAMINFO header".into()));
    }
    output.seek(SeekFrom::Start(26))?;
    output.write_all(&pcm_md5.finalize())?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::wav::CaptureWav;

    #[test]
    fn encodes_a_24_bit_capture() {
        let dir = std::env::temp_dir().join(format!("resona-flac-{}", std::process::id()));
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
        assert_eq!(&bytes[..4], b"fLaC");
        // STREAMINFO: bits-per-sample minus one sits across bytes 20-21.
        let bits = (((bytes[20] & 0x01) << 4) | (bytes[21] >> 4)) + 1;
        assert_eq!(bits, 24);
        // Lossless FLAC must remain smaller than raw PCM for this predictable ramp.
        assert!(bytes.len() < samples.len() * 3 + 4096);
        // STREAMINFO must give players an exact duration for random access.
        let total_samples = u64::from_be_bytes(bytes[18..26].try_into().unwrap()) & ((1 << 36) - 1);
        assert_eq!(total_samples, 44_100);

        let decoded: Vec<i32> = claxon::FlacReader::open(&flac_path)
            .unwrap()
            .samples()
            .map(Result::unwrap)
            .collect();
        let expected: Vec<i32> = samples
            .iter()
            .map(|&sample| Quantizer::new(24, 24).quantize(sample))
            .collect();
        assert_eq!(decoded, expected);
        let mut pcm_md5 = Md5::new();
        for sample in expected {
            pcm_md5.update(&sample.to_le_bytes()[..3]);
        }
        assert_eq!(&bytes[26..42], pcm_md5.finalize().as_slice());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

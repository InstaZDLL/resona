//! FLAC output, the format of Spotify Lossless rips.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use flac_bound::{FlacEncoder, WriteWrapper};
use hound::{SampleFormat, WavReader};
use md5::{Digest, Md5};

use super::{Channels, Quantizer, Remix};
use crate::{Error, Result};

/// Encodes an intermediate float WAV (see [`super::wav::CaptureWav`]) to
/// FLAC at the quantizer's depth (16 or 24 bits).
///
/// FLAC has no channel mask of its own: from 3 to 8 channels it names
/// them in WAVE's order for the usual layouts (8: FL FR FC LFE BL BR SL
/// SR), which [`crate::format::ChannelLayout::is_standard_surround`] keeps
/// to.
pub fn encode_wav_to_flac(
    src: &Path,
    dst: &Path,
    quantizer: Quantizer,
    channels: Channels,
) -> Result<()> {
    debug_assert!(quantizer.bits() == 16 || quantizer.bits() == 24);
    if let Channels::All(layout) = channels {
        debug_assert!(layout.is_stereo() || layout.is_standard_surround());
    }
    let mut reader = WavReader::open(src)?;
    let source = reader.spec();
    if source.sample_format != SampleFormat::Float || source.bits_per_sample != 32 {
        return Err(Error::UnsupportedWav("expected 32-bit float samples"));
    }
    let output_channels = channels.count();
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
            .channels(u32::from(output_channels))
            .bits_per_sample(u32::from(bits))
            .sample_rate(source.sample_rate)
            .total_samples_estimate(u64::from(sample_frames))
            .compression_level(5)
            .verify(true);
        let mut encoder = config
            .init_write(&mut writer)
            .map_err(|e| Error::Flac(format!("could not initialize FLAC encoder: {e:?}")))?;

        let samples = Remix::new(reader.samples::<f32>(), source.channels, channels)?;
        let channels = usize::from(output_channels);
        let mut quantizer = quantizer;
        let mut buffer = Vec::with_capacity(4096 * channels);
        let mut pcm_bytes = Vec::with_capacity(4096 * channels * usize::from(bits / 8));
        for sample in samples {
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
    fn encodes_a_24_bit_capture_at_its_own_rate() {
        // 48 kHz: Apple Music's rate for many tracks, kept as is.
        const RATE: u32 = 48_000;
        let dir = std::env::temp_dir().join(format!("resona-flac-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wav_path, flac_path) = (dir.join("in.wav"), dir.join("out.flac"));

        // One second of a 24-bit-exact ramp, stereo.
        let samples: Vec<f32> = (0..RATE as i32 * 2)
            .map(|i| ((i * 97) % 8_388_608 - 4_194_304) as f32 / 8_388_608.0)
            .collect();
        let mut wav = CaptureWav::create(&wav_path, RATE).unwrap();
        wav.write(&samples).unwrap();
        wav.finalize().unwrap();

        encode_wav_to_flac(
            &wav_path,
            &flac_path,
            Quantizer::new(24, 24),
            Channels::FrontPair,
        )
        .unwrap();
        let bytes = std::fs::read(&flac_path).unwrap();
        assert_eq!(&bytes[..4], b"fLaC");
        // STREAMINFO: bits-per-sample minus one sits across bytes 20-21.
        let bits = (((bytes[20] & 0x01) << 4) | (bytes[21] >> 4)) + 1;
        assert_eq!(bits, 24);
        // Lossless FLAC must remain smaller than raw PCM for this predictable ramp.
        assert!(bytes.len() < samples.len() * 3 + 4096);
        // STREAMINFO must give players an exact duration for random access.
        let total_samples = u64::from_be_bytes(bytes[18..26].try_into().unwrap()) & ((1 << 36) - 1);
        assert_eq!(total_samples, u64::from(RATE));
        let reader = claxon::FlacReader::open(&flac_path).unwrap();
        assert_eq!(reader.streaminfo().sample_rate, RATE);

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

    #[test]
    fn encodes_every_channel_of_a_7_1_track() {
        use crate::format::ChannelLayout;
        const RATE: u32 = 48_000;
        let dir = std::env::temp_dir().join(format!("resona-flac71-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wav_path, flac_path) = (dir.join("in.wav"), dir.join("out.flac"));
        let layout = ChannelLayout::SURROUND_7_1;
        // A tenth of a second, each channel its own ramp.
        let samples: Vec<f32> = (0..RATE as i32 / 10)
            .flat_map(|frame| {
                (0..8).map(move |channel| ((frame * (channel + 1) * 37) % 65_536 - 32_768) as f32)
            })
            .map(|v| v / 8_388_608.0)
            .collect();
        let mut wav = CaptureWav::with_layout(&wav_path, RATE, layout).unwrap();
        wav.write(&samples).unwrap();
        wav.finalize().unwrap();

        encode_wav_to_flac(
            &wav_path,
            &flac_path,
            Quantizer::new(24, 24),
            Channels::All(layout),
        )
        .unwrap();
        let mut reader = claxon::FlacReader::open(&flac_path).unwrap();
        assert_eq!(reader.streaminfo().channels, 8);
        assert_eq!(reader.streaminfo().bits_per_sample, 24);
        let decoded: Vec<i32> = reader.samples().map(Result::unwrap).collect();
        let expected: Vec<i32> = samples
            .iter()
            .map(|&s| Quantizer::new(24, 24).quantize(s))
            .collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(decoded, expected);
    }
}

//! FLAC output, the format of Spotify Lossless rips.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use flacenc::component::{BitRepr, MetadataBlockData, Stream};
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
    let sample_frames = reader.duration();
    let bits = quantizer.bits();

    // flacenc 0.5.1's predictive modes have produced pathological 24-bit
    // frames on real captures (up to 302 MB for 4096 samples). Such files
    // report impossible STREAMINFO frame sizes and cannot be sought reliably.
    // Verbatim/constant subframes remain bit-exact and have a hard size bound.
    let mut config = flacenc::config::Encoder::default();
    config.multithread = false;
    config.subframe_coding.use_fixed = false;
    config.subframe_coding.use_lpc = false;
    let config = config
        .into_verified()
        .map_err(|(_, e)| Error::Flac(e.to_string()))?;
    let source = WavSource {
        reader,
        quantizer,
        buffer: Vec::new(),
    };
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| Error::Flac(e.to_string()))?;
    add_seek_table(&mut stream)?;

    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| Error::Flac(e.to_string()))?;
    let pcm_bytes = u64::from(sample_frames) * u64::from(spec.channels) * u64::from(bits) / 8;
    let maximum = pcm_bytes + stream.frame_count() as u64 * 64 + 1_048_576;
    if sink.as_slice().len() as u64 > maximum {
        return Err(Error::Flac(
            "FLAC frame size exceeds uncompressed audio".into(),
        ));
    }
    std::fs::write(dst, sink.as_slice())?;
    Ok(())
}

/// FLAC seek points store sample numbers and byte offsets relative to the
/// first audio frame. They make seeking faster; valid FLAC does not require one.
fn add_seek_table(stream: &mut Stream) -> Result<()> {
    let interval = stream.stream_info().sample_rate() as u64 * 10;
    let frames = (0..stream.frame_count()).map(|i| {
        let frame = stream.frame(i).expect("frame index is in range");
        debug_assert_eq!(frame.count_bits() % 8, 0);
        (
            frame.header().block_size() as u64,
            frame.count_bits() as u64 / 8,
        )
    });
    let points = seek_points(frames, interval);
    if !points.is_empty() {
        let block =
            MetadataBlockData::new_unknown(3, &points).map_err(|e| Error::Flac(e.to_string()))?;
        stream.add_metadata_block(block);
    }
    Ok(())
}

fn seek_points(frames: impl IntoIterator<Item = (u64, u64)>, interval: u64) -> Vec<u8> {
    let mut points = Vec::new();
    let (mut sample, mut offset, mut next) = (0u64, 0u64, 0u64);
    for (block_samples, frame_bytes) in frames {
        if sample >= next {
            points.extend_from_slice(&sample.to_be_bytes());
            points.extend_from_slice(&offset.to_be_bytes());
            points.extend_from_slice(&(block_samples as u16).to_be_bytes());
            next = sample.saturating_add(interval);
        }
        sample += block_samples;
        offset += frame_bytes;
    }
    points
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
        // Verbatim coding is bounded by the source PCM size plus frame headers.
        assert!(bytes.len() < samples.len() * 3 + 4096);
        // The encoder must emit a SEEKTABLE immediately after STREAMINFO.
        assert_eq!(bytes[42] & 0x7f, 3);
        assert_eq!(u32::from_be_bytes([0, bytes[43], bytes[44], bytes[45]]), 18);
        assert_eq!(&bytes[46..54], &[0; 8]);
        assert_eq!(&bytes[54..62], &[0; 8]);

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
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn seek_points_reference_audio_frame_starts() {
        let points = seek_points([(4, 10), (4, 12), (4, 14), (4, 16)], 8);
        assert_eq!(points.len(), 36);
        assert_eq!(&points[0..16], &[0; 16]);
        assert_eq!(&points[16..18], &[0, 4]);
        assert_eq!(u64::from_be_bytes(points[18..26].try_into().unwrap()), 8);
        assert_eq!(u64::from_be_bytes(points[26..34].try_into().unwrap()), 22);
        assert_eq!(u16::from_be_bytes(points[34..36].try_into().unwrap()), 4);
    }
}

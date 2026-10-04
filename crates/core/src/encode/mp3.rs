//! MP3 output (LAME, constant bitrate), for Spotify Free and Premium
//! "Very high" recordings.

use std::path::Path;

use hound::{SampleFormat, WavReader};
use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, InterleavedPcm, Quality};

use crate::{Error, Result};

/// Frames per `encode` call; any multiple of an MP3 frame (1152) works.
const CHUNK_FRAMES: usize = 1152 * 16;

pub fn bitrate(kbps: u32) -> Option<Bitrate> {
    Some(match kbps {
        128 => Bitrate::Kbps128,
        160 => Bitrate::Kbps160,
        192 => Bitrate::Kbps192,
        256 => Bitrate::Kbps256,
        320 => Bitrate::Kbps320,
        _ => return None,
    })
}

fn lame(what: &'static str) -> impl Fn(mp3lame_encoder::BuildError) -> Error {
    move |e| Error::Mp3(format!("{what}: {e}"))
}

/// Encodes an intermediate float WAV to a CBR MP3 at `kbps`.
///
/// LAME's Info header is kept and written over the placeholder frame it
/// reserves at the start: it records the encoder delay and padding, which
/// players need for gapless playback across the split tracks.
pub fn encode_wav_to_mp3(src: &Path, dst: &Path, kbps: u32) -> Result<()> {
    let mut reader = WavReader::open(src)?;
    let spec = reader.spec();
    if spec.sample_format != SampleFormat::Float || spec.channels != 2 {
        return Err(Error::UnsupportedWav(
            "expected stereo 32-bit float samples",
        ));
    }
    let mut builder = Builder::new().ok_or_else(|| Error::Mp3("LAME unavailable".into()))?;
    builder.set_num_channels(2).map_err(lame("channels"))?;
    builder
        .set_sample_rate(spec.sample_rate)
        .map_err(lame("sample rate"))?;
    builder
        .set_brate(
            bitrate(kbps).ok_or_else(|| Error::Mp3(format!("unsupported bitrate {kbps} kbps")))?,
        )
        .map_err(lame("bitrate"))?;
    builder
        .set_quality(Quality::Best)
        .map_err(lame("quality"))?;
    let mut encoder = builder.build().map_err(lame("init"))?;

    let mut mp3 = Vec::new();
    let mut chunk = Vec::with_capacity(CHUNK_FRAMES * 2);
    let mut samples = reader.samples::<f32>();
    loop {
        chunk.clear();
        for sample in samples.by_ref().take(CHUNK_FRAMES * 2) {
            chunk.push(sample?);
        }
        if chunk.is_empty() {
            break;
        }
        mp3.reserve(mp3lame_encoder::max_required_buffer_size(chunk.len() / 2));
        encoder
            .encode_to_vec(InterleavedPcm(&chunk), &mut mp3)
            .map_err(|e| Error::Mp3(e.to_string()))?;
    }
    mp3.reserve(mp3lame_encoder::max_required_buffer_size(0).max(7200));
    encoder
        .flush_to_vec::<FlushNoGap>(&mut mp3)
        .map_err(|e| Error::Mp3(e.to_string()))?;

    let mut info = Vec::with_capacity(encoder.lame_tag_size());
    if encoder.lame_tag_size() > 0 && encoder.lame_tag_encode_to_vec(&mut info).is_some() {
        // No ID3 tag was given to LAME, so the placeholder starts at 0.
        let start = encoder.id3v2_tag_size();
        if let Some(slot) = mp3.get_mut(start..start + info.len()) {
            slot.copy_from_slice(&info);
        }
    }
    std::fs::write(dst, mp3)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::wav::CaptureWav;

    #[test]
    fn encodes_a_playable_cbr_mp3() {
        let dir = std::env::temp_dir().join(format!("resona-mp3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wav, mp3) = (dir.join("in.wav"), dir.join("out.mp3"));
        let samples: Vec<f32> = (0..44_100 * 2 * 3)
            .map(|i| ((i / 2) as f32 * 440.0 * std::f32::consts::TAU / 44_100.0).sin() * 0.5)
            .collect();
        let mut writer = CaptureWav::create(&wav).unwrap();
        writer.write(&samples).unwrap();
        writer.finalize().unwrap();

        encode_wav_to_mp3(&wav, &mp3, 320).unwrap();
        let bytes = std::fs::read(&mp3).unwrap();
        let file = lofty::read_from_path(&mp3).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        // Frame sync, and an Info header in the first frame.
        assert_eq!(bytes[0], 0xFF);
        assert!(
            bytes[..1000]
                .windows(4)
                .any(|w| w == b"Info" || w == b"Xing")
        );
        use lofty::file::AudioFile;
        let props = file.properties();
        // Averaged over the file, headers included.
        assert!(
            props
                .audio_bitrate()
                .is_some_and(|b| (315..=325).contains(&b))
        );
        let seconds = props.duration().as_secs_f32();
        assert!((2.9..3.1).contains(&seconds), "{seconds}");
    }

    #[test]
    fn only_lame_cbr_bitrates() {
        assert!(bitrate(320).is_some());
        assert!(bitrate(300).is_none());
    }
}

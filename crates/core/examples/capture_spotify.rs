//! Phase 0 spike: capture Spotify's own audio and write it as FLAC, then
//! report whether the capture was bit-transparent (a real lossless rip),
//! along with the Windows settings that decide it.
//!
//! ```text
//! cargo run -p resona-core --example capture_spotify -- [seconds] [out.flac]
//! ```

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use crossbeam_channel::RecvTimeoutError;
    use resona_core::Error;
    use resona_core::analysis::{self, BitAnalysis};
    use resona_core::audio_setup;
    use resona_core::capture::{CaptureConfig, ProcessCapture};
    use resona_core::encode::{Quantizer, flac, wav::CaptureWav};
    use resona_core::format::{CAPTURE_CHANNELS, CAPTURE_SAMPLE_RATE};
    use resona_core::spotify::process::SpotifyProcesses;

    // flacenc logs its worker statistics at INFO.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .init();
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(30);
    let output = PathBuf::from(args.next().unwrap_or_else(|| "capture.flac".into()));
    let intermediate = output.with_extension("capture.wav");

    let _ = wasapi::initialize_mta();
    let processes = SpotifyProcesses::find().ok_or(Error::SpotifyNotRunning)?;
    let device = audio_setup::spotify_output_device(processes.root)?;
    println!(
        "output device: {} — {} Hz, {} bits, {} channels (mask {:#x}){}, volume {:.0} % ({:.1} dB){}{}{}",
        device.name,
        device.sample_rate,
        device.bits_per_sample,
        device.channels,
        device.channel_mask,
        if device.downmixes() {
            " — front pair kept"
        } else {
            ""
        },
        device.volume_scalar * 100.0,
        device.volume_db,
        if device.muted { ", MUTED" } else { "" },
        if device.enhancements == Some(true) {
            "  ⚠ audio enhancements ON"
        } else {
            ""
        },
        if device.resamples() {
            "  ⚠ not 44100 Hz"
        } else {
            ""
        },
    );
    for session in audio_setup::session_volumes(&processes.all)? {
        println!(
            "Spotify session (PID {}): volume {:.0} %{}",
            session.pid,
            session.volume * 100.0,
            if session.muted { ", MUTED" } else { "" },
        );
    }
    println!(
        "Spotify root PID {}, capturing {seconds} s…",
        processes.root
    );

    // RESONA_FORCE_STEREO=1 lets Windows downmix to stereo, for comparison.
    let (channels, mask) = if std::env::var_os("RESONA_FORCE_STEREO").is_some() {
        println!("forcing a stereo capture (Windows downmix)");
        (2, 0x3)
    } else {
        (device.channels, device.channel_mask)
    };
    let (capture, packets) = ProcessCapture::start(CaptureConfig {
        pid: processes.root,
        source_channels: channels,
        channel_mask: mask,
    })?;
    let mut wav = CaptureWav::create(&intermediate)?;
    let mut analysis = BitAnalysis::default();
    let mut discontinuities = 0;
    let mut other_channels_peak = 0.0_f32;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match packets.recv_timeout(left) {
            Ok(packet) => {
                discontinuities += u32::from(packet.discontinuity);
                other_channels_peak = other_channels_peak.max(packet.other_channels_peak);
                analysis.feed(&packet.samples);
                wav.write(&packet.samples)?;
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        }
    }
    capture.stop()?;
    wav.finalize()?;

    let frames = analysis.samples() / u64::from(CAPTURE_CHANNELS);
    println!(
        "captured {frames} frames ({:.1} s), peak {:.4} ({:.1} dBFS), clipped {}, discontinuities {discontinuities}",
        frames as f64 / f64::from(CAPTURE_SAMPLE_RATE),
        analysis.peak(),
        analysis.peak_dbfs(),
        analysis.clipped(),
    );
    if channels > 2 {
        println!(
            "peak of the {} other channels: {:.4}{}",
            channels - 2,
            other_channels_peak,
            if other_channels_peak > 0.0 {
                " ⚠ Spotify uses them: front pair is not the whole mix"
            } else {
                " (empty, as expected)"
            },
        );
    }
    println!(
        "samples off the 24-bit grid: {:.2} %",
        analysis.off_grid_ratio() * 100.0
    );
    let bits = match analysis.effective_depth() {
        Some(bits) => {
            println!("bit-transparent: yes, source is {bits}-bit");
            bits
        }
        None if analysis.is_silent() => {
            println!("bit-transparent: unknown — only silence captured (is Spotify playing?)");
            16
        }
        None => {
            println!("bit-transparent: NO");
            let samples: Vec<f32> = hound::WavReader::open(&intermediate)?
                .samples::<f32>()
                .collect::<Result<_, _>>()?;
            match analysis::estimate_gain(&samples) {
                Some(gain) => println!(
                    "constant gain detected ({:.1} % of samples on a regular step): \
                     {:.2} dB if the source is 16-bit, {:.2} dB if 24-bit \
                     — a volume is applied before the capture",
                    gain.score * 100.0,
                    gain.gain_db_16(),
                    gain.gain_db_24(),
                ),
                None => println!(
                    "no constant gain: the signal was resampled or processed \
                     (normalization, equalizer, audio enhancements, spatial sound)"
                ),
            }
            // Whether the processing is constant or limited to some moments
            // (a fade, an Automix transition), and how far off it is.
            // A processed signal sits about 0.25 step from both grids; a
            // clean 24-bit one only from the 16-bit grid.
            println!("mean distance to the grids, per second (in steps of each grid):");
            let second = CAPTURE_SAMPLE_RATE as usize * usize::from(CAPTURE_CHANNELS);
            for (index, block) in samples.chunks(second).enumerate() {
                let r = analysis::grid_residual(block);
                println!(
                    "  {index:>3} s  off 24-bit grid {:>5.1} %   16-bit {:.4}   24-bit {:.4}",
                    r.off_grid_24 * 100.0,
                    r.mean_lsb_16,
                    r.mean_lsb_24,
                );
            }
            let overall = analysis::grid_residual(&samples);
            if overall.mean_lsb_24 > 0.1 {
                println!(
                    "verdict: processed (resampler or effect){}",
                    if device.resamples() {
                        " — the device is not at 44100 Hz, try that first"
                    } else {
                        ""
                    }
                );
            } else {
                println!("verdict: close to the 24-bit grid — a light processing stage");
            }
            24
        }
    };

    flac::encode_wav_to_flac(&intermediate, &output, Quantizer::new(bits, bits))?;
    if std::env::var_os("RESONA_KEEP_WAV").is_some() {
        println!("kept the raw float capture: {}", intermediate.display());
    } else {
        std::fs::remove_file(&intermediate)?;
    }
    println!("wrote {}", output.display());
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("capture_spotify needs Windows (WASAPI process loopback).");
}

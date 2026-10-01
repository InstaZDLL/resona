//! Phase 2 harness: a real recording session. Play music in Spotify and
//! watch tracks being saved as FLAC.
//!
//! ```text
//! cargo run -p spytify-core --example record -- <output dir> [minutes] [--keep-partial]
//! ```

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use spytify_core::recorder::engine::{Recorder, RecorderConfig};

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let keep_partial = args.iter().any(|a| a == "--keep-partial");
    let mut positional = args.iter().filter(|a| !a.starts_with("--"));
    let output_dir = PathBuf::from(positional.next().map_or("recordings", String::as_str));
    let minutes: u64 = positional
        .next()
        .map(|m| m.parse())
        .transpose()?
        .unwrap_or(10);

    let (recorder, events) = Recorder::start(RecorderConfig {
        output_dir: output_dir.clone(),
        keep_partial,
        min_duration: Duration::from_secs(10),
    })?;
    println!(
        "recording into {} for {minutes} min{}",
        output_dir.display(),
        if keep_partial {
            " (keeping partial tracks)"
        } else {
            ""
        }
    );

    let deadline = Instant::now() + Duration::from_secs(minutes * 60);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        print_event(event);
    }
    println!("stopping, finishing the current track…");
    recorder.stop();
    while let Ok(event) = events.try_recv() {
        print_event(event);
    }
    Ok(())
}

#[cfg(windows)]
fn print_event(event: spytify_core::recorder::engine::RecorderEvent) {
    use std::time::Duration;

    use spytify_core::analysis::Fidelity;
    use spytify_core::audio_setup::SetupIssue;
    use spytify_core::metadata::TagsOutcome;
    use spytify_core::recorder::engine::RecorderEvent;
    use spytify_core::spotify::state::Event;

    let quality = |fidelity: Fidelity| match fidelity {
        Fidelity::BitPerfect { depth } => format!("bit-perfect {depth}-bit"),
        Fidelity::NearTransparent => "near-transparent 24-bit".into(),
        Fidelity::PeakLimited => "peaks limited by Spotify, 24-bit".into(),
        Fidelity::Processed => "PROCESSED (check audio settings)".into(),
        Fidelity::Silent => "silent".into(),
    };
    let length = |d: Duration| format!("{}:{:02}", d.as_secs() / 60, d.as_secs() % 60);

    match event {
        RecorderEvent::CaptureStarted { pid, device } => {
            println!(
                "capture started: Spotify PID {pid}, {} ({} Hz, {} channels)",
                device.name, device.sample_rate, device.channels
            );
            for issue in device.lossless_issues() {
                match issue {
                    SetupIssue::Enhancements => println!(
                        "  ⚠ Windows audio enhancements are on: recordings will not be lossless \
                         (Settings → Sound → device → Audio enhancements → Off)"
                    ),
                    SetupIssue::SampleRate(rate) => println!(
                        "  ⚠ the device runs at {rate} Hz: set it to 44100 Hz for lossless recordings"
                    ),
                }
            }
        }
        RecorderEvent::CaptureLost => println!("capture lost, retrying…"),
        RecorderEvent::Recording(title) => println!("● recording  {title}"),
        RecorderEvent::Saved {
            title,
            path,
            fidelity,
            duration,
            tags,
        } => println!(
            "✔ saved      {title}  [{}, {}, tags: {}]  → {}",
            length(duration),
            quality(fidelity),
            match tags {
                TagsOutcome::Deezer => "Deezer".to_string(),
                TagsOutcome::NoMatch => "Spotify only (no Deezer match)".to_string(),
                TagsOutcome::Unavailable(why) => format!("Spotify only ({why})"),
            },
            path.display()
        ),
        RecorderEvent::Discarded {
            title,
            reason,
            fidelity,
            duration,
        } => println!(
            "✘ discarded  {title} ({reason:?})  [{}, {}]",
            length(duration),
            quality(fidelity)
        ),
        RecorderEvent::Spotify(Event::PlayStateChanged { playing }) => {
            println!("  {}", if playing { "▶ playing" } else { "⏸ paused" });
        }
        RecorderEvent::Spotify(_) => {}
        RecorderEvent::Error(e) => println!("error: {e}"),
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("record needs Windows.");
}

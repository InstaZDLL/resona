//! A real recording session from the command line, with the app's saved
//! settings (`%APPDATA%\Resona\settings.toml`), overridden by the flags.
//!
//! ```text
//! cargo run -p resona-core --example record -- [output dir] [minutes] [--keep-partial] [--cable|--no-cable]
//!     [--format=flac|flac16|flac24|wav|wav16|wav24|mp3|mp3:<kbps>]
//! ```

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use resona_core::recorder::engine::Recorder;
    use resona_core::settings::Settings;

    // lofty warns on every FLAC it tags that it adds a padding block: our
    // encoder writes none, which is fine.
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::filter::Targets::new()
                .with_default(tracing::Level::WARN)
                .with_target("lofty", tracing::Level::ERROR),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut settings = Settings::load();
    settings.min_duration_secs = 10;
    if args.iter().any(|a| a == "--no-cable") {
        settings.virtual_cable = false;
    }
    if args.iter().any(|a| a == "--cable") {
        settings.virtual_cable = true;
    }
    if args.iter().any(|a| a == "--keep-partial") {
        settings.keep_partial = true;
    }
    if let Some(format) = args.iter().find_map(|a| a.strip_prefix("--format=")) {
        settings.format = format.parse().map_err(anyhow::Error::msg)?;
    }
    let mut positional = args.iter().filter(|a| !a.starts_with("--"));
    if let Some(dir) = positional.next() {
        settings.output_dir = PathBuf::from(dir);
    }
    let minutes: u64 = positional
        .next()
        .map(|m| m.parse())
        .transpose()?
        .unwrap_or(10);

    let (recorder, events) = Recorder::start(settings.recorder_config())?;
    println!(
        "recording into {} as {} for {minutes} min — {:?}, existing tracks: {:?}{}",
        settings.output_dir.display(),
        settings.format,
        settings.layout,
        settings.existing,
        if settings.keep_partial {
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
fn print_event(event: resona_core::recorder::engine::RecorderEvent) {
    use std::time::Duration;

    use resona_core::analysis::Fidelity;
    use resona_core::audio_setup::SetupIssue;
    use resona_core::recorder::engine::RecorderEvent;
    use resona_core::spotify::state::Event;

    let quality = |fidelity: Fidelity| match fidelity {
        Fidelity::BitPerfect { depth, touched: 0 } => format!("bit-perfect {depth}-bit"),
        Fidelity::BitPerfect { depth, touched } => {
            format!("bit-perfect {depth}-bit, {touched} samples touched by Spotify")
        }
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
                        "  ⚠ Windows audio enhancements may be on (an effect is installed and not \
                         disabled in the registry). Check Settings → Sound → device → Audio \
                         enhancements → Off; each saved track says whether it was altered."
                    ),
                    SetupIssue::SampleRate(rate) => println!(
                        "  ⚠ the device runs at {rate} Hz: set it to 44100 Hz for lossless recordings"
                    ),
                }
            }
        }
        RecorderEvent::CaptureLost => println!("capture lost, retrying…"),
        RecorderEvent::CableIsDefault(_) => {
            println!("  ⚠ the virtual cable is the Windows default device: nothing is heard")
        }
        RecorderEvent::SpotifyFades(title) => {
            println!("  ⚠ {title} starts with a fade Spotify added: turn crossfade and Automix off")
        }
        RecorderEvent::CableMissing => {
            println!("  ⚠ no virtual cable installed: Spotify stays on its device")
        }
        RecorderEvent::Recording(title) => println!("● recording  {title}"),
        RecorderEvent::AlreadyRecorded {
            title,
            existing,
            skipped_in_spotify,
        } => println!(
            "↷ already    {title} ({}){}",
            existing.display(),
            if skipped_in_spotify {
                ", skipped in Spotify"
            } else {
                ""
            }
        ),
        RecorderEvent::AdMuted(muted) => println!(
            "  {}",
            if muted {
                "ad: Spotify muted"
            } else {
                "Spotify unmuted"
            }
        ),
        RecorderEvent::Saved {
            title,
            path,
            fidelity,
            duration,
            tags,
            lyrics,
        } => println!(
            "✔ saved      {title}  [{}, {}, tags: {}, lyrics: {lyrics:?}]  → {}",
            length(duration),
            quality(fidelity),
            describe_tags(&tags),
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

#[cfg(windows)]
fn describe_tags(outcome: &resona_core::metadata::TagsOutcome) -> String {
    use resona_core::metadata::DeezerOutcome;
    let deezer = match &outcome.deezer {
        DeezerOutcome::Matched => "Deezer".to_owned(),
        DeezerOutcome::NoMatch => "no Deezer match".to_owned(),
        DeezerOutcome::Unavailable(why) => format!("Deezer unavailable ({why})"),
    };
    let found = if outcome.spotify {
        format!("Spotify catalogue + {deezer}")
    } else {
        deezer
    };
    match &outcome.write_error {
        Some(e) => format!("{found}, NOT WRITTEN ({e})"),
        None => found,
    }
}

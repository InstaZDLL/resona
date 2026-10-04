//! Records a playlist, album or track from its link: Spotify is driven
//! through its own `spotify_cli`, the rest of the settings come from the
//! settings file.
//!
//! ```text
//! cargo run -p spytify-core --example record_playlist -- <link> [output dir] [--cable|--no-cable]
//! ```

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;

    use spytify_core::recorder::engine::RecorderEvent;
    use spytify_core::recorder::playlist_session::{PlaylistEvent, PlaylistSession, Source};
    use spytify_core::settings::Settings;
    use spytify_core::spotify::link::SpotifyLink;

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
    let mut positional = args.iter().filter(|a| !a.starts_with("--"));
    let link = SpotifyLink::parse(positional.next().map_or("", String::as_str))?;
    let mut settings = Settings::load();
    if let Some(dir) = positional.next() {
        settings.output_dir = PathBuf::from(dir);
    }
    if args.iter().any(|a| a == "--cable") {
        settings.virtual_cable = true;
    }
    if args.iter().any(|a| a == "--no-cable") {
        settings.virtual_cable = false;
    }

    let (session, events) = PlaylistSession::start(settings.recorder_config(), Source::Link(link))?;
    let mut titles = Vec::new();
    for event in events {
        match event {
            PlaylistEvent::Loaded { name, entries } => {
                println!("{name}: {} tracks", entries.len());
                for (i, entry) in entries.iter().enumerate() {
                    let length = entry
                        .duration
                        .map(|d| format!("{}:{:02}", d.as_secs() / 60, d.as_secs() % 60))
                        .unwrap_or_default();
                    println!(
                        "  {:>2}. {} — {}  {length}",
                        i + 1,
                        entry.title,
                        entry.artists.join(", ")
                    );
                }
                titles = entries.into_iter().map(|e| e.title).collect();
            }
            PlaylistEvent::Entry { index, state } => {
                println!(
                    "  [{}/{}] {:?}  {}",
                    index + 1,
                    titles.len(),
                    state,
                    titles[index]
                );
            }
            PlaylistEvent::Recorder(event) => match *event {
                RecorderEvent::CaptureStarted { device, .. } => {
                    println!("capture started on {}", device.name);
                }
                RecorderEvent::Saved {
                    title,
                    fidelity,
                    duration,
                    ..
                } => println!(
                    "✔ saved {title}  [{}:{:02}, {fidelity:?}]",
                    duration.as_secs() / 60,
                    duration.as_secs() % 60
                ),
                RecorderEvent::Discarded { title, reason, .. } => {
                    println!("✘ discarded {title}  ({reason:?})");
                }
                RecorderEvent::SpotifyFades(title) => println!("⚠ {title} starts with a fade"),
                RecorderEvent::Error(e) => println!("error: {e}"),
                _ => {}
            },
            PlaylistEvent::Error(e) => println!("error: {e}"),
            PlaylistEvent::Finished { name, missed } => {
                println!("{name} done, {} missed", missed.len());
                for entry in missed {
                    println!("  missed: {} — {}", entry.title, entry.artists.join(", "));
                }
            }
        }
    }
    session.stop();
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("record_playlist needs Windows.");
}

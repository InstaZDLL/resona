//! Phase 1 diagnostic: prints what the monitor sees (window title, SMTC
//! session) and the status it derives, every time something changes.
//! Use it to check how Spotify behaves on ads, pauses and track changes.
//!
//! ```text
//! cargo run -p resona-core --example spotify_probe -- [seconds]
//! ```

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    use resona_core::spotify::monitor::{POLL_INTERVAL, Poller, status_of};
    use resona_core::spotify::state::Content;

    tracing_subscriber::fmt::init();
    let seconds: u64 = std::env::args()
        .nth(1)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(120);
    let _ = wasapi::initialize_mta();

    let mut poller = Poller::new();
    let mut last = None;
    let mut content = Content::Nothing;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        let observation = poller.observe();
        // Position moves on every poll; leave it out of change detection.
        let mut comparable = observation.clone();
        if let Some(smtc) = &mut comparable.smtc {
            smtc.position = None;
        }
        if last.as_ref() != Some(&comparable) {
            println!("\n{observation:#?}");
            let status = status_of(&observation, false, &content);
            println!("=> {status:#?}");
            content = status.content;
            last = Some(comparable);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("spotify_probe needs Windows.");
}

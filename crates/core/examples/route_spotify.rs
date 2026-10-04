//! Shows which output device Spotify plays on, and moves it to the
//! virtual cable or back to the default device.
//!
//! ```text
//! cargo run -p resona-core --example route_spotify -- [cable|default|cable-rate]
//! ```

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use resona_core::spotify::process::SpotifyProcesses;
    use resona_core::{Error, audio_setup, routing};

    let _ = wasapi::initialize_mta();
    let processes = SpotifyProcesses::find().ok_or(Error::SpotifyNotRunning)?;
    let cable = audio_setup::virtual_cable()?;

    match std::env::args().nth(1).as_deref() {
        Some("cable") => {
            let (id, name) = cable
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no virtual cable installed"))?;
            routing::route_spotify(processes.root, Some(id))?;
            println!("Spotify sent to {name}");
        }
        Some("default") => {
            routing::route_spotify(processes.root, None)?;
            println!("Spotify back on the default device");
        }
        Some("cable-rate") => {
            let (id, name) = cable
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no virtual cable installed"))?;
            resona_core::device_config::set_sample_rate(id, 44100)?;
            println!("{name} set to 44100 Hz");
        }
        Some(other) => anyhow::bail!("unknown action {other:?}: cable, default or cable-rate"),
        None => {}
    }

    let default = audio_setup::default_output_device()?;
    println!(
        "Windows default device: {} — {} Hz",
        default.name, default.sample_rate
    );
    println!("render devices:");
    for (id, name) in audio_setup::render_devices()? {
        println!("  {name}  {id}");
    }
    match &cable {
        Some((id, name)) => println!(
            "virtual cable: {name} — {} Hz",
            audio_setup::output_device(id)?.sample_rate
        ),
        None => println!("virtual cable: none installed"),
    }
    let routed = routing::spotify_endpoint(processes.root)?;
    let device = audio_setup::spotify_output_device(processes.root)?;
    println!(
        "Spotify (PID {}) plays on: {}{} — {} Hz, {} channels, enhancements {}",
        processes.root,
        device.name,
        if routed.is_some() {
            " (chosen for Spotify)"
        } else {
            " (default device)"
        },
        device.sample_rate,
        device.channels,
        match device.enhancements {
            Some(true) => "may be on",
            Some(false) => "off",
            None => "unknown",
        },
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("route_spotify needs Windows.");
}

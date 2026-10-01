#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let window = AppWindow::new()?;
    window.set_spotify_status(spotify_status().into());
    let weak = window.as_weak();
    window.on_refresh(move || {
        if let Some(window) = weak.upgrade() {
            window.set_spotify_status(spotify_status().into());
        }
    });
    window.run()?;
    Ok(())
}

#[cfg(windows)]
fn spotify_status() -> String {
    match spytify_core::spotify::find_root_pid() {
        Some(pid) => format!("Spotify détecté (PID {pid})"),
        None => "Spotify introuvable".into(),
    }
}

#[cfg(not(windows))]
fn spotify_status() -> String {
    "Spytify ne fonctionne que sous Windows".into()
}

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

#[cfg(windows)]
mod app;
mod mapping;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    app::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Spytify records the Spotify desktop client and needs Windows.");
}

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod cover;
mod mapping;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    app::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Resona records the Spotify desktop client and needs Windows.");
}

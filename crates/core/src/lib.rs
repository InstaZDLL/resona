//! Resona's engine: find Spotify, capture only its audio, split it into
//! tracks, encode and tag them. No UI code lives here.
//!
//! The roadmap and the reasoning behind each module are in `docs/PLAN.md`.

pub mod analysis;
pub mod encode;
pub mod error;
pub mod format;
pub mod metadata;
pub mod recorder;
pub mod settings;
pub mod spotify;
pub mod update;

#[cfg(windows)]
pub mod audio_setup;
#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod device_config;
#[cfg(windows)]
pub mod playback;
#[cfg(windows)]
pub mod routing;

pub use error::{Error, Result};

//! Spytify's engine: find Spotify, capture only its audio, split it into
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

#[cfg(windows)]
pub mod audio_setup;
#[cfg(windows)]
pub mod capture;

pub use error::{Error, Result};

//! A track is captured into an intermediate float WAV ([`wav::CaptureWav`]),
//! then encoded to the user's format once the track ends. Encoding after
//! the fact keeps the capture path cheap and lets the encoder use the
//! whole-track [`crate::analysis::BitAnalysis`] (FLAC bit depth).

pub mod flac;
pub mod wav;

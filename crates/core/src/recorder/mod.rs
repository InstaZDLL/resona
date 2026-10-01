//! Turning the capture into one file per track: [`splitter`] decides where
//! each track starts and ends, the engine writes and encodes.

pub mod boundary;
pub mod clock;
pub mod naming;
pub mod splitter;

#[cfg(windows)]
pub mod engine;

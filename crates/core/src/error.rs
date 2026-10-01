pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Spotify is not running")]
    SpotifyNotRunning,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("WAV error: {0}")]
    Wav(#[from] hound::Error),

    #[error("FLAC encoder error: {0}")]
    Flac(String),

    #[error("unsupported intermediate WAV: {0}")]
    UnsupportedWav(&'static str),

    #[cfg(windows)]
    #[error("Windows API error: {0}")]
    Windows(#[from] windows::core::Error),

    #[cfg(windows)]
    #[error("WASAPI error: {0}")]
    Wasapi(#[from] wasapi::WasapiError),

    #[error("capture thread stopped unexpectedly")]
    CaptureThread,
}

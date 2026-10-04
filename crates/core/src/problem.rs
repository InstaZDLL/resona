//! What went wrong, for the interface to word in its own language. The
//! [`Error`] messages are English and stay for logs and examples; a
//! [`Problem`] says which situation it is, plus the technical cause as is
//! (from Windows, a library or Spotify's command-line tool), shown for
//! diagnosis.

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemKind {
    SpotifyNotRunning,
    /// `spotify_cli.exe` is not installed.
    SpotifyCliMissing,
    /// Spotify's command-line tool refused or failed.
    SpotifyCommand,
    NotALink,
    /// Spotify did not start the playlist it was asked to play.
    ListNotStarted,
    /// Spotify stopped answering during a playlist.
    LostSpotify,
    CaptureNotStarted,
    /// A track link Spotify does not know.
    UnknownTrack,
    /// Spotify could not be sent to the virtual cable.
    CableRouting,
    /// The capture could not be played back on the headset.
    HeadsetPlayback,
    /// A track could not be encoded or filed; `subject` is its title.
    Encoding,
    /// The Windows default output device could not be changed.
    DefaultDevice,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub kind: ProblemKind,
    /// What it is about (a track title), when anything.
    pub subject: String,
    /// The technical cause, untranslated.
    pub detail: String,
}

impl Problem {
    pub fn new(kind: ProblemKind, detail: impl ToString) -> Self {
        Self {
            kind,
            subject: String::new(),
            detail: detail.to_string(),
        }
    }

    pub fn about(kind: ProblemKind, subject: impl ToString, detail: impl ToString) -> Self {
        Self {
            kind,
            subject: subject.to_string(),
            detail: detail.to_string(),
        }
    }
}

/// For logs and the command-line examples: the kind, then the rest.
impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.kind)?;
        if !self.subject.is_empty() {
            write!(f, " ({})", self.subject)?;
        }
        if !self.detail.is_empty() {
            write!(f, ": {}", self.detail)?;
        }
        Ok(())
    }
}

impl From<&Error> for Problem {
    fn from(error: &Error) -> Self {
        let kind = match error {
            Error::SpotifyNotRunning => ProblemKind::SpotifyNotRunning,
            Error::SpotifyCliMissing => ProblemKind::SpotifyCliMissing,
            Error::SpotifyCli(_) => ProblemKind::SpotifyCommand,
            Error::NotASpotifyLink(_) => ProblemKind::NotALink,
            Error::ListNotStarted => ProblemKind::ListNotStarted,
            Error::LostSpotify => ProblemKind::LostSpotify,
            Error::CaptureNotStarted => ProblemKind::CaptureNotStarted,
            Error::UnknownTrack(_) => ProblemKind::UnknownTrack,
            _ => ProblemKind::Other,
        };
        // The kind says it all for these; the others keep their cause.
        let detail = match error {
            Error::SpotifyCli(cause)
            | Error::NotASpotifyLink(cause)
            | Error::UnknownTrack(cause) => cause.clone(),
            Error::SpotifyNotRunning
            | Error::SpotifyCliMissing
            | Error::ListNotStarted
            | Error::LostSpotify
            | Error::CaptureNotStarted => String::new(),
            other => other.to_string(),
        };
        Self::new(kind, detail)
    }
}

impl From<Error> for Problem {
    fn from(error: Error) -> Self {
        Self::from(&error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_become_problems_with_their_cause() {
        let link = Problem::from(Error::NotASpotifyLink("hello".into()));
        assert_eq!(link.kind, ProblemKind::NotALink);
        assert_eq!(link.detail, "hello");
        let missing = Problem::from(Error::SpotifyCliMissing);
        assert_eq!(missing.kind, ProblemKind::SpotifyCliMissing);
        assert_eq!(missing.detail, "");
        let io = Problem::from(Error::Io(std::io::Error::other("disk full")));
        assert_eq!(io.kind, ProblemKind::Other);
        assert!(io.detail.contains("disk full"));
    }
}

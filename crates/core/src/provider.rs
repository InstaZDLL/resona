//! The music app being recorded. Spotify was the only one; Apple Music for
//! Windows came second (spike of 2026-11-23, see `docs/PLAN.md`). Pure: what
//! differs between the two, for the Windows modules to act on.

use serde::{Deserialize, Serialize};

use crate::format::CAPTURE_SAMPLE_RATE;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    #[default]
    Spotify,
    AppleMusic,
}

/// Where the current track's identity comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleMode {
    /// Spotify: the window title (`Artist - Title`), SMTC adding the album.
    /// An unknown title over sound is an ad.
    WindowTitle,
    /// Apple Music: its window is always titled `Apple Music`; SMTC alone
    /// tells the track. There are no ads.
    Smtc,
}

impl Provider {
    pub const ALL: [Self; 2] = [Self::Spotify, Self::AppleMusic];

    pub fn name(self) -> &'static str {
        match self {
            Self::Spotify => "Spotify",
            Self::AppleMusic => "Apple Music",
        }
    }

    /// The app's own executable: running means this process exists.
    pub fn process_name(self) -> &'static str {
        match self {
            Self::Spotify => "Spotify.exe",
            Self::AppleMusic => "AppleMusic.exe",
        }
    }

    /// The process that renders the sound, captured with its tree.
    /// Spotify plays from a child of its root process; Apple Music from its
    /// library agent, an out-of-process COM server started by `svchost`,
    /// outside `AppleMusic.exe`'s tree (measured 2026-11-23).
    pub fn audio_process_name(self) -> &'static str {
        match self {
            Self::Spotify => "Spotify.exe",
            Self::AppleMusic => "AMPLibraryAgent.exe",
        }
    }

    /// Class of the app's top-level window, as a prefix.
    pub fn window_class_prefix(self) -> &'static str {
        match self {
            // Chromium. Spotify also owns IME and message-only windows with
            // titles of their own.
            Self::Spotify => "Chrome_WidgetWin",
            Self::AppleMusic => "WinUIDesktopWin32WindowClass",
        }
    }

    /// Whether a SMTC session's `SourceAppUserModelId` is this app's:
    /// `Spotify.exe` or a Store package name for Spotify,
    /// `AppleInc.AppleMusicWin_nzyj5cx40ttqa!App` for Apple Music.
    pub fn matches_app_id(self, app_id: &str) -> bool {
        let app_id = app_id.to_ascii_lowercase();
        match self {
            Self::Spotify => app_id.contains("spotify"),
            Self::AppleMusic => app_id.contains("applemusic"),
        }
    }

    pub fn title_mode(self) -> TitleMode {
        match self {
            Self::Spotify => TitleMode::WindowTitle,
            Self::AppleMusic => TitleMode::Smtc,
        }
    }

    /// The rate the app always streams at, when it has one: the capture
    /// runs at it, and a device at another rate is a resampler before the
    /// capture. Apple Music's tracks are at 44.1 or 48 kHz (up to 192 in
    /// Hi-Res): it is captured at the device's rate, and a track at another
    /// rate shows as not bit-transparent.
    pub fn fixed_rate(self) -> Option<u32> {
        match self {
            Self::Spotify => Some(CAPTURE_SAMPLE_RATE),
            Self::AppleMusic => None,
        }
    }

    /// SMTC's timeline dates a track's start to within milliseconds.
    /// Apple Music rounds position and duration to the second: its starts
    /// are left to the title change and the silence between tracks.
    pub fn dates_track_start(self) -> bool {
        match self {
            Self::Spotify => true,
            Self::AppleMusic => false,
        }
    }

    /// Spotify's command-line tool and preferences file: exact catalogue
    /// entries, playlist mode, the settings check.
    pub fn has_spotify_tools(self) -> bool {
        self == Self::Spotify
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_its_own_smtc_sessions_only() {
        let apple = "AppleInc.AppleMusicWin_nzyj5cx40ttqa!App";
        assert!(Provider::AppleMusic.matches_app_id(apple));
        assert!(!Provider::Spotify.matches_app_id(apple));
        for spotify in [
            "Spotify.exe",
            "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify",
        ] {
            assert!(Provider::Spotify.matches_app_id(spotify));
            assert!(!Provider::AppleMusic.matches_app_id(spotify));
        }
        assert!(!Provider::Spotify.matches_app_id("app.waveflow"));
    }

    #[test]
    fn settings_names() {
        #[derive(Serialize, Deserialize)]
        struct Wrapper {
            provider: Provider,
        }
        let text = toml::to_string(&Wrapper {
            provider: Provider::AppleMusic,
        })
        .unwrap();
        assert_eq!(text.trim(), "provider = \"apple-music\"");
    }
}

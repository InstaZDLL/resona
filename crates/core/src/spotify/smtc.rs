//! Spotify's System Media Transport Controls session: what Windows shows in
//! its media overlay. Gives album, album artist, track number and duration,
//! which the window title does not.

use std::time::Duration;

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtcSnapshot {
    /// `Spotify.exe` for the desktop installer, a package family name for
    /// the Microsoft Store build.
    pub app_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub track_number: Option<u32>,
    pub playing: bool,
    /// As of the session's last timeline update, not "now".
    pub position: Option<Duration>,
    pub duration: Option<Duration>,
}

pub struct Smtc {
    manager: SessionManager,
}

impl Smtc {
    /// Needs COM initialised on the calling thread (MTA).
    pub fn new() -> Result<Self> {
        Ok(Self {
            manager: SessionManager::RequestAsync()?.join()?,
        })
    }

    /// Spotify's session, if Spotify registered one.
    pub fn spotify(&self) -> Result<Option<SmtcSnapshot>> {
        for session in self.manager.GetSessions()? {
            let app_id = session.SourceAppUserModelId()?.to_string();
            if !app_id.to_ascii_lowercase().contains("spotify") {
                continue;
            }
            let properties = session.TryGetMediaPropertiesAsync()?.join()?;
            let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == PlaybackStatus::Playing;
            let timeline = session.GetTimelineProperties()?;
            return Ok(Some(SmtcSnapshot {
                app_id,
                title: properties.Title()?.to_string(),
                artist: properties.Artist()?.to_string(),
                album: properties.AlbumTitle()?.to_string(),
                album_artist: properties.AlbumArtist()?.to_string(),
                track_number: u32::try_from(properties.TrackNumber()?)
                    .ok()
                    .filter(|&n| n > 0),
                playing,
                position: from_timespan(timeline.Position()?.Duration),
                duration: from_timespan(timeline.EndTime()?.Duration),
            }));
        }
        Ok(None)
    }
}

/// `TimeSpan` counts 100 ns ticks; zero means "not provided".
fn from_timespan(ticks: i64) -> Option<Duration> {
    let ticks = u64::try_from(ticks).ok().filter(|&t| t > 0)?;
    Some(Duration::from_nanos(ticks * 100))
}

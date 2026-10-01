//! Combining the window title, SMTC and audio activity into one status,
//! and diffing statuses into events. Pure logic, testable anywhere.

use std::time::Duration;

use super::title::{self, TitleState, TrackTitle};

/// Album details SMTC adds to what the window title says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackDetails {
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<u32>,
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub title: TrackTitle,
    pub details: TrackDetails,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Nothing,
    Ad,
    Track(Track),
}

impl Content {
    /// Same thing playing. Ignores [`TrackDetails`], which SMTC may fill in
    /// a poll or two after the title changed.
    pub fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Track(a), Self::Track(b)) => a.title == b.title,
            (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub running: bool,
    pub playing: bool,
    pub content: Content,
}

impl Status {
    pub const NOT_RUNNING: Self = Self {
        running: false,
        playing: false,
        content: Content::Nothing,
    };
}

/// SMTC fields used here, decoupled from the Windows type for tests.
#[derive(Debug, Clone, Default)]
pub struct SmtcView<'a> {
    pub title: &'a str,
    pub album: &'a str,
    pub album_artist: &'a str,
    pub track_number: Option<u32>,
    pub duration: Option<Duration>,
    pub playing: bool,
}

/// Mirrors the C# rules: a track title means playing; an idle or unknown
/// title while audio plays is an ad.
///
/// One addition: a paused track keeps its identity. Spotify resets the
/// window title to `Spotify Premium` on pause, but SMTC keeps the track,
/// so pausing `previous` reports it as not playing instead of a change of
/// track (which would split the recording in two). This wins over
/// `audio_active`: Spotify fades out over the first poll of a pause, and
/// that tail of sound once passed for an ad (measured 2026-10-01).
pub fn classify(
    window_title: Option<&str>,
    smtc: Option<&SmtcView>,
    audio_active: bool,
    previous: &Content,
) -> Status {
    let sound = audio_active || smtc.is_some_and(|s| s.playing);
    let state = window_title.map_or(TitleState::Idle, title::parse);
    let (playing, content) = match state {
        TitleState::Track(title) => {
            let details = smtc.map(|s| details_for(&title, s)).unwrap_or_default();
            (true, Content::Track(Track { title, details }))
        }
        TitleState::Other(other) if title::is_ad_title(&other) || sound => (true, Content::Ad),
        TitleState::Idle => match (previous, smtc) {
            (Content::Track(track), Some(smtc))
                if !smtc.playing && smtc_describes(smtc, &track.title) =>
            {
                (false, previous.clone())
            }
            _ if sound => (true, Content::Ad),
            _ => (false, Content::Nothing),
        },
        TitleState::Other(_) => (false, Content::Nothing),
    };
    Status {
        running: true,
        playing,
        content,
    }
}

/// The SMTC title is the full one (`Song (Live)`), the parsed title only
/// its first part.
fn smtc_describes(smtc: &SmtcView, title: &TrackTitle) -> bool {
    smtc_title_matches(smtc.title, title)
}

pub fn smtc_title_matches(smtc_title: &str, title: &TrackTitle) -> bool {
    smtc_title
        .to_lowercase()
        .starts_with(&title.title.to_lowercase())
}

/// SMTC details, only when SMTC describes the same track: the two sources
/// do not update at the same instant.
///
/// Even then a field can be stale: SMTC updates its fields one by one
/// (the next track's duration was seen next to the previous title). The
/// consumer should trust details that stayed stable, not the last ones
/// received before a track change.
fn details_for(title: &TrackTitle, smtc: &SmtcView) -> TrackDetails {
    if !smtc_describes(smtc, title) {
        return TrackDetails::default();
    }
    let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    TrackDetails {
        album: non_empty(smtc.album),
        album_artist: non_empty(smtc.album_artist),
        track_number: smtc.track_number,
        duration: smtc.duration,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Also sent for the first observation, and when Spotify starts or quits.
    ContentChanged {
        previous: Content,
        current: Content,
    },
    PlayStateChanged {
        playing: bool,
    },
    /// Same track, SMTC details arrived or changed.
    DetailsUpdated(Track),
}

pub fn diff(previous: &Status, current: &Status) -> Vec<Event> {
    let mut events = Vec::new();
    if previous.playing != current.playing {
        events.push(Event::PlayStateChanged {
            playing: current.playing,
        });
    }
    if !previous.content.same_as(&current.content) {
        events.push(Event::ContentChanged {
            previous: previous.content.clone(),
            current: current.content.clone(),
        });
    } else if let (Content::Track(before), Content::Track(after)) =
        (&previous.content, &current.content)
        && before.details != after.details
    {
        events.push(Event::DetailsUpdated(after.clone()));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTHING: Content = Content::Nothing;

    fn smtc(title: &'static str, playing: bool) -> SmtcView<'static> {
        SmtcView {
            title,
            album: "Album",
            album_artist: "Album Artist",
            track_number: Some(3),
            duration: Some(Duration::from_secs(200)),
            playing,
        }
    }

    fn track_of(status: &Status) -> &Track {
        match &status.content {
            Content::Track(track) => track,
            other => panic!("expected a track, got {other:?}"),
        }
    }

    #[test]
    fn track_title_is_playing_with_smtc_details() {
        let status = classify(
            Some("Artist - Song (Live)"),
            Some(&smtc("Song (Live)", true)),
            false,
            &NOTHING,
        );
        assert!(status.playing);
        let track = track_of(&status);
        assert_eq!(track.title.title, "Song");
        assert_eq!(track.details.album.as_deref(), Some("Album"));
        assert_eq!(track.details.track_number, Some(3));
    }

    #[test]
    fn smtc_of_another_track_is_ignored() {
        // Observed: the window title changes one poll before SMTC.
        let status = classify(
            Some("Artist - Song"),
            Some(&smtc("Previous Song", true)),
            false,
            &NOTHING,
        );
        assert_eq!(track_of(&status).details, TrackDetails::default());
    }

    #[test]
    fn idle_title_with_sound_is_an_ad() {
        let status = classify(Some("Spotify Free"), None, true, &NOTHING);
        assert_eq!((status.playing, status.content), (true, Content::Ad));
        let status = classify(Some("Spotify Free"), Some(&smtc("", true)), false, &NOTHING);
        assert_eq!(status.content, Content::Ad);
    }

    #[test]
    fn idle_title_without_sound_or_known_track_is_nothing() {
        let status = classify(
            Some("Spotify Premium"),
            Some(&smtc("Song", false)),
            false,
            &NOTHING,
        );
        assert_eq!((status.playing, status.content), (false, Content::Nothing));
    }

    #[test]
    fn advertisement_title_is_an_ad_even_silent() {
        let ad = |title, sound| classify(Some(title), None, sound, &NOTHING).content;
        assert_eq!(ad("Advertisement", false), Content::Ad);
        assert_eq!(ad("Spotify Sponsor", true), Content::Ad);
        assert_eq!(ad("Spotify Sponsor", false), Content::Nothing);
    }

    #[test]
    fn pause_keeps_the_track_and_resume_is_not_a_change() {
        // Observed sequence: "Beach Riot - Tell Me I'm Wrong" playing, pause
        // (title "Spotify Premium", SMTC still on the track), resume.
        let smtc_playing = smtc("Tell Me I'm Wrong", true);
        let smtc_paused = smtc("Tell Me I'm Wrong", false);
        let title = "Beach Riot - Tell Me I'm Wrong";

        let playing = classify(Some(title), Some(&smtc_playing), false, &NOTHING);
        let paused = classify(
            Some("Spotify Premium"),
            Some(&smtc_paused),
            false,
            &playing.content,
        );
        assert!(!paused.playing);
        assert_eq!(paused.content, playing.content);
        assert_eq!(
            diff(&playing, &paused),
            [Event::PlayStateChanged { playing: false }]
        );

        let resumed = classify(Some(title), Some(&smtc_playing), false, &paused.content);
        assert_eq!(
            diff(&paused, &resumed),
            [Event::PlayStateChanged { playing: true }]
        );
    }

    #[test]
    fn fade_out_at_pause_is_not_an_ad() {
        // Observed: the poll right after pausing still hears the fade-out.
        let playing = classify(Some("A - One"), Some(&smtc("One", true)), false, &NOTHING);
        let pausing = classify(
            Some("Spotify Premium"),
            Some(&smtc("One", false)),
            true,
            &playing.content,
        );
        assert!(!pausing.playing);
        assert_eq!(pausing.content, playing.content);
    }

    #[test]
    fn idle_title_while_smtc_still_plays_the_track_is_an_ad() {
        // An ad on the Free tier, SMTC not updated yet: still an ad.
        let playing = classify(Some("A - One"), Some(&smtc("One", true)), false, &NOTHING);
        let ad = classify(
            Some("Spotify Free"),
            Some(&smtc("One", true)),
            true,
            &playing.content,
        );
        assert_eq!(ad.content, Content::Ad);
    }

    #[test]
    fn pause_then_skip_is_a_change() {
        let playing = classify(Some("A - One"), Some(&smtc("One", true)), false, &NOTHING);
        let skipped_while_paused = classify(
            Some("Spotify Premium"),
            Some(&smtc("Two", false)),
            false,
            &playing.content,
        );
        assert_eq!(skipped_while_paused.content, Content::Nothing);
    }

    #[test]
    fn diff_reports_track_change_then_details() {
        let first = classify(Some("A - One"), None, false, &NOTHING);
        let events = diff(&Status::NOT_RUNNING, &first);
        assert!(matches!(
            events[..],
            [
                Event::PlayStateChanged { playing: true },
                Event::ContentChanged { .. }
            ]
        ));

        let enriched = classify(
            Some("A - One"),
            Some(&smtc("One", true)),
            false,
            &first.content,
        );
        assert!(matches!(
            diff(&first, &enriched)[..],
            [Event::DetailsUpdated(_)]
        ));

        let second = classify(Some("A - Two"), None, false, &enriched.content);
        assert!(matches!(
            diff(&enriched, &second)[..],
            [Event::ContentChanged { .. }]
        ));
        assert!(diff(&second, &second).is_empty());
    }
}

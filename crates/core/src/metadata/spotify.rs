//! Tags from Spotify's own catalogue, through `spotify_cli`: the release
//! actually played (album, its artists, date, copyright, cover, position
//! on the album), where Deezer may pick another release of the same song.
//!
//! The track is found by `search`, then told apart from its other releases
//! (single, album, compilation) by SMTC's album name and its duration.

use std::collections::HashMap;
use std::time::Duration;

use super::name_match::{normalize_name, title_similarity};
use crate::Result;
use crate::spotify::cli::{SpotifyCli, TrackInfo};
use crate::spotify::state::Track;

/// Search results looked at.
const SEARCH_LIMIT: usize = 10;
/// Same title, as for Deezer.
const SAME_TITLE: f64 = 0.85;
/// A release whose length differs more is another recording.
const DURATION_TOLERANCE: Duration = Duration::from_secs(3);
/// Spotify's cover URLs name their size: 64 px in `lookup`, 640 px here.
const COVER_64: &str = "ab67616d00004851";
const COVER_640: &str = "ab67616d0000b273";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyDetails {
    pub uri: String,
    pub album: String,
    pub album_artists: Vec<String>,
    /// `YYYY-MM-DD`.
    pub date: Option<String>,
    pub copyright: Option<String>,
    pub track_number: Option<u32>,
    pub track_total: Option<u32>,
    /// 640 px.
    pub cover_url: Option<String>,
}

/// Looks `track` up in Spotify's catalogue, by the entry `played` when
/// known, else by search. `None` when no release agrees with what was
/// recorded.
pub fn find(
    cli: &SpotifyCli,
    track: &Track,
    played: Option<&str>,
) -> Result<Option<SpotifyDetails>> {
    // The entry played, when the recorder saw it: exact, no search.
    let hits: Vec<String> = match played {
        Some(uri) => vec![uri.to_owned()],
        None => {
            let title = track.title.full_title();
            let query = format!("{} {}", track.title.artist, title);
            cli.search_tracks(&query, SEARCH_LIMIT)?
                .into_iter()
                .filter(|hit| title_similarity(&hit.name, &title) >= SAME_TITLE)
                .map(|hit| hit.uri)
                .collect()
        }
    };
    if hits.is_empty() {
        return Ok(None);
    }
    let infos = cli.lookup(&hits)?;
    let candidates: Vec<(&String, &TrackInfo)> = hits
        .iter()
        .filter_map(|uri| Some((uri, infos.get(uri)?)))
        .collect();
    let Some((uri, info)) = pick(
        &candidates,
        track.details.album.as_deref(),
        track.details.duration,
    ) else {
        return Ok(None);
    };
    let Some((album_name, album_uri)) = info.album.clone() else {
        return Ok(None);
    };

    let album = cli
        .lookup(std::slice::from_ref(&album_uri))?
        .remove(&album_uri)
        .unwrap_or_default();
    // The album's track list gives the position; a failure only loses it.
    let listing = cli
        .collection(&album_uri)
        .inspect_err(|e| tracing::warn!("album {album_uri}: {e}"))
        .ok();
    // By entry, else by title: the entry played can be a relinked copy
    // that the album lists under another id (2026-10-04).
    let title = track.title.full_title();
    let position = listing.as_ref().and_then(|l| {
        l.tracks
            .iter()
            .position(|t| t.uri == *uri)
            .or_else(|| {
                l.tracks
                    .iter()
                    .position(|t| title_similarity(&t.name, &title) >= SAME_TITLE)
            })
            // The album lists romanized titles (Ashura-chan for
            // 阿修羅ちゃん): the one track of the same length.
            .or_else(|| {
                let uris: Vec<String> = l.tracks.iter().map(|t| t.uri.clone()).collect();
                let lengths = cli.lookup(&uris).ok()?;
                same_length(&uris, &lengths, info.duration?)
            })
    });
    Ok(Some(SpotifyDetails {
        uri: uri.clone(),
        // SMTC names the album played in the script Spotify shows (狂言);
        // the command-line tool may romanize it (Kyougen).
        album: track.details.album.clone().unwrap_or(album_name),
        album_artists: album.artists,
        date: album
            .release_date
            .as_deref()
            .or(info.release_date.as_deref())
            .and_then(normalize_date),
        copyright: album.copyright.filter(|c| !c.is_empty()),
        track_number: position.and_then(|p| u32::try_from(p + 1).ok()),
        track_total: listing.and_then(|l| u32::try_from(l.tracks.len()).ok()),
        cover_url: album
            .image_url
            .or(info.image_url.clone())
            .map(|url| large_cover(&url)),
    }))
}

/// The release that was recorded: the one on SMTC's album, else the first
/// of the right length. A release of another length is never taken.
fn pick<'a>(
    candidates: &[(&'a String, &'a TrackInfo)],
    album: Option<&str>,
    duration: Option<Duration>,
) -> Option<(&'a String, &'a TrackInfo)> {
    let fits = |info: &TrackInfo| match (duration, info.duration) {
        (Some(expected), Some(actual)) => expected.abs_diff(actual) <= DURATION_TOLERANCE,
        _ => true,
    };
    let on_album = |info: &TrackInfo| {
        album.is_some_and(|wanted| {
            info.album
                .as_ref()
                .is_some_and(|(name, _)| normalize_name(name) == normalize_name(wanted))
        })
    };
    candidates
        .iter()
        .find(|(_, info)| fits(info) && on_album(info))
        .or_else(|| candidates.iter().find(|(_, info)| fits(info)))
        .copied()
}

/// The position of the only entry lasting `length` (within a second), or
/// `None` when none or several do.
fn same_length(
    uris: &[String],
    lengths: &HashMap<String, TrackInfo>,
    length: Duration,
) -> Option<usize> {
    let mut matching = uris.iter().enumerate().filter(|(_, uri)| {
        lengths
            .get(*uri)
            .and_then(|i| i.duration)
            .is_some_and(|d| d.abs_diff(length) <= Duration::from_secs(1))
    });
    let (position, _) = matching.next()?;
    matching.next().is_none().then_some(position)
}

/// `2022-7-27` → `2022-07-27`; a year alone stays as is.
fn normalize_date(date: &str) -> Option<String> {
    let parts: Vec<&str> = date.split('-').collect();
    let number = |s: &str| s.parse::<u32>().ok();
    match parts[..] {
        [year] => number(year).map(|y| format!("{y:04}")),
        [year, month] => Some(format!("{:04}-{:02}", number(year)?, number(month)?)),
        [year, month, day] => Some(format!(
            "{:04}-{:02}-{:02}",
            number(year)?,
            number(month)?,
            number(day)?
        )),
        _ => None,
    }
}

fn large_cover(url: &str) -> String {
    url.replace(COVER_64, COVER_640)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(album: &str, seconds: u64) -> TrackInfo {
        TrackInfo {
            name: "Flow".into(),
            album: Some((album.into(), format!("spotify:album:{album}"))),
            duration: Some(Duration::from_secs(seconds)),
            ..TrackInfo::default()
        }
    }

    #[test]
    fn the_release_on_smtcs_album_wins() {
        let (single, album) = (
            "spotify:track:single".to_owned(),
            "spotify:track:album".to_owned(),
        );
        let (a, b) = (info("Flow", 184), info("PLASMA", 184));
        let candidates = [(&single, &a), (&album, &b)];
        let (uri, _) = pick(&candidates, Some("PLASMA"), Some(Duration::from_secs(185))).unwrap();
        assert_eq!(uri, &album);
        // Without SMTC's album, the first of the right length.
        let (uri, _) = pick(&candidates, None, Some(Duration::from_secs(185))).unwrap();
        assert_eq!(uri, &single);
    }

    #[test]
    fn another_length_is_another_recording() {
        let uri = "spotify:track:live".to_owned();
        let live = info("Live", 240);
        assert!(
            pick(
                &[(&uri, &live)],
                Some("Live"),
                Some(Duration::from_secs(184))
            )
            .is_none()
        );
    }

    #[test]
    fn position_by_length_needs_a_single_match() {
        let uris: Vec<String> = (0..3).map(|i| format!("spotify:track:{i}")).collect();
        let lengths: HashMap<String, TrackInfo> = [200, 195, 230]
            .iter()
            .zip(&uris)
            .map(|(&s, uri)| (uri.clone(), info("Kyougen", s)))
            .collect();
        assert_eq!(
            same_length(&uris, &lengths, Duration::from_millis(195_400)),
            Some(1)
        );
        assert_eq!(same_length(&uris, &lengths, Duration::from_secs(300)), None);
        let mut twins = lengths.clone();
        twins.insert(uris[2].clone(), info("Kyougen", 195));
        assert_eq!(same_length(&uris, &twins, Duration::from_secs(195)), None);
    }

    #[test]
    fn dates_and_covers() {
        assert_eq!(normalize_date("2022-7-27").as_deref(), Some("2022-07-27"));
        assert_eq!(normalize_date("1999").as_deref(), Some("1999"));
        assert_eq!(normalize_date("2001-3").as_deref(), Some("2001-03"));
        assert_eq!(normalize_date("soon"), None);
        assert_eq!(
            large_cover("https://i.scdn.co/image/ab67616d000048518f08f8275990d14cec9894b2"),
            "https://i.scdn.co/image/ab67616d0000b2738f08f8275990d14cec9894b2"
        );
    }
}

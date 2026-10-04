//! Tags for recorded tracks: Spotify's own details (window title, SMTC),
//! made exact from Spotify's catalogue ([`spotify`], the release actually
//! played) and completed from Deezer's public catalogue when a hit agrees
//! on every signal ([`lookup`]).

pub mod deezer;
pub mod lookup;
pub mod lyrics;
pub mod name_match;
pub mod spotify;
pub mod tags;

use deezer::DeezerClient;
use lookup::Query;
use tags::{Cover, TrackTags};

use crate::spotify::cli::SpotifyCli;
use crate::spotify::state::Track;

/// How the tags of a track were obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagsOutcome {
    /// Spotify's catalogue knew the release played.
    pub spotify: bool,
    pub deezer: DeezerOutcome,
    /// `None` once written into the file; the error otherwise (the audio
    /// is kept either way).
    pub write_error: Option<String>,
}

impl TagsOutcome {
    /// More than the window title and SMTC went into the file's tags.
    pub fn is_complete(&self) -> bool {
        self.write_error.is_none() && (self.spotify || self.deezer == DeezerOutcome::Matched)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeezerOutcome {
    /// Completed from Deezer.
    Matched,
    /// Deezer has no hit that agrees.
    NoMatch,
    /// Deezer could not be reached or refused.
    Unavailable(String),
}

/// Spotify's details, completed from Deezer, then made exact from
/// Spotify's catalogue (`uri`: the entry played, when known). Never fails: a lookup problem leaves the tags it
/// could not fill and says why.
pub fn tags_for(
    deezer: Option<&DeezerClient>,
    spotify: Option<&SpotifyCli>,
    track: &Track,
    uri: Option<&str>,
) -> (TrackTags, TagsOutcome) {
    let mut tags = TrackTags::from_spotify(track);
    let deezer_outcome = match deezer {
        Some(client) => from_deezer(client, track, &mut tags),
        None => DeezerOutcome::Unavailable("no HTTP client".into()),
    };
    let details = spotify.and_then(|cli| {
        spotify::find(cli, track, uri)
            .inspect_err(|e| tracing::warn!("spotify catalogue: {e}"))
            .ok()
            .flatten()
    });
    if let Some(details) = &details {
        tags.merge_spotify(details);
        // Spotify's cover is the one of the release played.
        if let (Some(url), Some(client)) = (&details.cover_url, deezer)
            && let Some(cover) = client
                .image(url)
                .inspect_err(|e| tracing::warn!("cover {url}: {e}"))
                .ok()
                .and_then(Cover::from_bytes)
        {
            tags.cover = Some(cover);
        }
    }
    (
        tags,
        TagsOutcome {
            spotify: details.is_some(),
            deezer: deezer_outcome,
            write_error: None,
        },
    )
}

fn from_deezer(client: &DeezerClient, track: &Track, tags: &mut TrackTags) -> DeezerOutcome {
    let title = track.title.full_title();
    let query = Query {
        artist: &track.title.artist,
        title: &title,
        album: track.details.album.as_deref(),
        duration: track.details.duration,
    };
    (|| -> deezer::DeezerResult<DeezerOutcome> {
        // The full title first; Deezer sometimes lists the version only
        // in the album, so the bare title is tried next.
        let mut hits = client.search_track(query.artist, query.title)?;
        if lookup::best_hit(&query, &hits).is_none() && title != track.title.title {
            hits = client.search_track(query.artist, &track.title.title)?;
        }
        let Some(hit) = lookup::best_hit(&query, &hits) else {
            return Ok(DeezerOutcome::NoMatch);
        };
        let deezer_track = client.track(hit.id)?;
        let album = match deezer_track.album.as_ref() {
            Some(album) => client
                .album(album.id)
                .inspect_err(|e| tracing::warn!("deezer album {}: {e}", album.id))
                .ok(),
            None => None,
        };
        tags.merge_deezer(&deezer_track, album.as_ref());
        if let Some(url) = TrackTags::cover_url(&deezer_track, album.as_ref()) {
            tags.cover = client
                .image(&url)
                .inspect_err(|e| tracing::warn!("cover {url}: {e}"))
                .ok()
                .and_then(Cover::from_bytes);
        }
        Ok(DeezerOutcome::Matched)
    })()
    .unwrap_or_else(|e| DeezerOutcome::Unavailable(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_not_written_are_not_complete() {
        let mut outcome = TagsOutcome {
            spotify: true,
            deezer: DeezerOutcome::NoMatch,
            write_error: None,
        };
        assert!(outcome.is_complete());
        outcome.write_error = Some("file locked".into());
        assert!(!outcome.is_complete());
        outcome = TagsOutcome {
            spotify: false,
            deezer: DeezerOutcome::NoMatch,
            write_error: None,
        };
        assert!(!outcome.is_complete());
    }
}

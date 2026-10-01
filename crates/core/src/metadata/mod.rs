//! Tags for recorded tracks: Spotify's own details (window title, SMTC),
//! completed from Deezer's public catalogue when a hit agrees on every
//! signal ([`lookup`]).

pub mod deezer;
pub mod lookup;
pub mod name_match;
pub mod tags;

use deezer::DeezerClient;
use lookup::Query;
use tags::{Cover, TrackTags};

use crate::spotify::state::Track;

/// How the tags of a track were obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagsOutcome {
    /// Completed from Deezer, cover included when it had one.
    Deezer,
    /// Deezer has no hit that agrees: Spotify's details only.
    NoMatch,
    /// Deezer could not be reached or refused: Spotify's details only.
    Unavailable(String),
}

/// Spotify's details, completed from Deezer when possible. Never fails:
/// a lookup problem leaves the Spotify tags and says why.
pub fn tags_for(client: &DeezerClient, track: &Track) -> (TrackTags, TagsOutcome) {
    let mut tags = TrackTags::from_spotify(track);
    let title = track.title.full_title();
    let query = Query {
        artist: &track.title.artist,
        title: &title,
        album: track.details.album.as_deref(),
        duration: track.details.duration,
    };
    let outcome = (|| -> deezer::DeezerResult<TagsOutcome> {
        // The full title first; Deezer sometimes lists the version only
        // in the album, so the bare title is tried next.
        let mut hits = client.search_track(query.artist, query.title)?;
        if lookup::best_hit(&query, &hits).is_none() && title != track.title.title {
            hits = client.search_track(query.artist, &track.title.title)?;
        }
        let Some(hit) = lookup::best_hit(&query, &hits) else {
            return Ok(TagsOutcome::NoMatch);
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
        Ok(TagsOutcome::Deezer)
    })()
    .unwrap_or_else(|e| TagsOutcome::Unavailable(e.to_string()));
    (tags, outcome)
}

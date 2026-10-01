//! Picking the Deezer track that is the one Spotify played. Pure logic.
//!
//! A wrong match writes another song's album, date and cover into the
//! file, which is worse than no match: every signal must agree, and when
//! none of the hits passes, the file keeps the tags Spotify gave.

use std::time::Duration;

use super::deezer::TrackHit;
use super::name_match::{normalize_name, select_by_name, title_similarity};

/// Below this, two titles are different songs ("Song" vs "Song 2" scores
/// about 0.67, "Song (Live)" vs "Song - Live" scores 1).
const MIN_TITLE_SIMILARITY: f64 = 0.85;
/// Spotify and Deezer round durations differently, and may carry
/// slightly different masters of the same release.
const DURATION_TOLERANCE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy)]
pub struct Query<'a> {
    /// As Spotify shows it, possibly several artists (`A, B`).
    pub artist: &'a str,
    /// Full title, extension included.
    pub title: &'a str,
    /// From SMTC, when known.
    pub album: Option<&'a str>,
    /// From SMTC, when known.
    pub duration: Option<Duration>,
}

/// The best hit that agrees on title, artist and (when both sides have
/// it) duration; ties broken by album, then by Deezer's own ranking.
pub fn best_hit<'a>(query: &Query, hits: &'a [TrackHit]) -> Option<&'a TrackHit> {
    let mut best: Option<(f64, &TrackHit)> = None;
    for hit in hits {
        let Some(score) = score(query, hit) else {
            continue;
        };
        if best.is_none_or(|(top, _)| score > top) {
            best = Some((score, hit));
        }
    }
    best.map(|(_, hit)| hit)
}

fn score(query: &Query, hit: &TrackHit) -> Option<f64> {
    let title = title_similarity(
        &without_featuring(query.title),
        &without_featuring(&hit.title),
    );
    if title < MIN_TITLE_SIMILARITY {
        return None;
    }
    if !artist_matches(query.artist, hit.artist.as_ref()?.name.as_str()) {
        return None;
    }
    if let (Some(ours), Some(theirs)) = (query.duration, hit.duration)
        && ours.abs_diff(Duration::from_secs(u64::from(theirs))) > DURATION_TOLERANCE
    {
        return None;
    }
    let album = match (query.album, hit.album.as_ref()) {
        (Some(ours), Some(theirs)) => title_similarity(ours, &theirs.title),
        _ => 0.0,
    };
    Some(title * 2.0 + album)
}

/// Drops a featuring credit: Spotify writes `Waterfalls (feat. X & Y)`
/// where Deezer has `Waterfalls` and the guests as contributors. Unlike
/// `(Remix)` or `(Live)`, a feature does not make another version.
fn without_featuring(title: &str) -> String {
    const MARKERS: [&str; 4] = ["feat.", "ft.", "featuring ", "with "];
    let lower = title.to_lowercase();
    for (open, close) in [("(", ')'), ("[", ']')] {
        for marker in MARKERS {
            let needle = format!("{open}{marker}");
            if let Some(start) = lower.find(&needle) {
                let end = lower[start..]
                    .find(close)
                    .map_or(title.len(), |i| start + i + 1);
                let mut stripped = title[..start].to_owned();
                stripped.push_str(&title[end..]);
                return stripped.trim().to_owned();
            }
        }
    }
    // Dash form: `Song - feat. X`.
    for marker in MARKERS {
        if let Some(start) = lower.find(&format!(" - {marker}")) {
            return title[..start].trim().to_owned();
        }
    }
    title.to_owned()
}

/// Spotify lists every artist (`A, B`), Deezer the main one: the hit must
/// be one of ours, give or take a backing-band suffix.
fn artist_matches(ours: &str, theirs: &str) -> bool {
    let theirs = normalize_name(theirs);
    std::iter::once(ours)
        .chain(ours.split(", "))
        .any(|candidate| select_by_name([candidate], &theirs, |c| Some(*c)).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::deezer::{AlbumRef, Named};

    fn hit(id: i64, artist: &str, title: &str, album: &str, seconds: u32) -> TrackHit {
        TrackHit {
            id,
            title: title.into(),
            duration: Some(seconds),
            artist: Some(Named {
                name: artist.into(),
            }),
            album: Some(AlbumRef {
                id: id * 10,
                title: album.into(),
                cover_xl: None,
                cover_big: None,
            }),
        }
    }

    fn query<'a>(
        artist: &'a str,
        title: &'a str,
        album: Option<&'a str>,
        seconds: u64,
    ) -> Query<'a> {
        Query {
            artist,
            title,
            album,
            duration: Some(Duration::from_secs(seconds)),
        }
    }

    #[test]
    fn picks_the_matching_album_among_versions() {
        let hits = [
            hit(
                1,
                "Beach Riot",
                "Tell Me I'm Wrong",
                "Some Compilation",
                202,
            ),
            hit(
                2,
                "Beach Riot",
                "Tell Me I'm Wrong",
                "Tell Me I'm Wrong",
                202,
            ),
        ];
        let q = query(
            "Beach Riot",
            "Tell Me I'm Wrong",
            Some("Tell Me I'm Wrong"),
            202,
        );
        assert_eq!(best_hit(&q, &hits).unwrap().id, 2);
    }

    #[test]
    fn rejects_another_song_with_a_close_title() {
        let hits = [hit(1, "Blur", "Song 2", "Blur", 122)];
        assert!(best_hit(&query("Blur", "Song", None, 122), &hits).is_none());
    }

    #[test]
    fn rejects_a_live_version_by_duration() {
        let hits = [hit(1, "Artist", "Song", "Live at X", 300)];
        assert!(best_hit(&query("Artist", "Song", None, 200), &hits).is_none());
    }

    #[test]
    fn spotify_extension_matches_deezer_parentheses() {
        let hits = [hit(1, "Artist", "Song (Remastered 2011)", "Album", 200)];
        let q = query("Artist", "Song - Remastered 2011", None, 201);
        assert!(best_hit(&q, &hits).is_some());
    }

    #[test]
    fn one_of_several_spotify_artists_is_enough() {
        let hits = [hit(
            1,
            "James Hype",
            "Waterfalls (feat. Sam Harper & Bobby Harvey)",
            "Waterfalls",
            180,
        )];
        let q = query(
            "James Hype, Sam Harper, Bobby Harvey",
            "Waterfalls (feat. Sam Harper & Bobby Harvey)",
            None,
            180,
        );
        assert!(best_hit(&q, &hits).is_some());
    }

    #[test]
    fn a_featuring_credit_is_not_another_version() {
        // Observed on Deezer, 2026-10-01: the guests are only contributors.
        let hits = [
            hit(
                1,
                "James Hype",
                "Waterfalls (Ely Oaks Remix)",
                "Waterfalls",
                111,
            ),
            hit(2, "James Hype", "Waterfalls", "Waterfalls", 121),
        ];
        let q = query(
            "James Hype",
            "Waterfalls (feat. Sam Harper & Bobby Harvey)",
            None,
            121,
        );
        assert_eq!(best_hit(&q, &hits).unwrap().id, 2);
        assert_eq!(without_featuring("Song [ft. X] (Live)"), "Song  (Live)");
        assert_eq!(without_featuring("Song - feat. X"), "Song");
        assert_eq!(without_featuring("Without You"), "Without You");
    }

    #[test]
    fn rejects_another_artist() {
        let hits = [hit(1, "Someone Else", "Heaven", "Heaven", 167)];
        assert!(best_hit(&query("swim school", "Heaven", None, 167), &hits).is_none());
    }
}

//! Lyrics from LRCLIB (lrclib.net), an open database of synced lyrics
//! (LRC): free, no key. Written as a `.lrc` file next to each track, which
//! players such as foobar2000, MusicBee, Poweramp or Plexamp pick up.
//!
//! Spotify's own lyrics come from Musixmatch under licence and are only
//! reachable through its private API: not used.
//!
//! Measured on 2026-10-05: the exact lookup gave synced lyrics for "yes,
//! and?", plain ones for Perfume's "Flow" and nothing for Ado's
//! "阿修羅ちゃん"; the search after it found synced lyrics for all three.
//! The server sometimes answers 503 (busy): one retry, then the track
//! simply has no lyrics.

use std::time::Duration;

use serde::Deserialize;

use super::name_match::title_similarity;

const API: &str = "https://lrclib.net/api";
const USER_AGENT: &str = concat!(
    "Resona/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/InstaZDLL/resona)"
);
const TIMEOUT: Duration = Duration::from_secs(15);
const BUSY_RETRY: Duration = Duration::from_secs(2);
/// LRCLIB matches on duration; a search hit further off is another version.
const DURATION_TOLERANCE: f64 = 3.0;
const SAME_TITLE: f64 = 0.85;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lyrics {
    /// LRC, with timestamps.
    Synced(String),
    /// Text only.
    Plain(String),
}

impl Lyrics {
    /// The content of the `.lrc` file.
    pub fn text(&self) -> &str {
        match self {
            Self::Synced(text) | Self::Plain(text) => text,
        }
    }
}

/// The track to find, as recorded.
pub struct LyricsQuery<'a> {
    /// The main artist (LRCLIB lists one).
    pub artist: &'a str,
    pub title: &'a str,
    pub album: Option<&'a str>,
    pub duration: Duration,
}

#[derive(Debug, Clone, Deserialize)]
struct Record {
    #[serde(default, rename = "trackName")]
    track_name: String,
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    instrumental: bool,
    #[serde(default, rename = "syncedLyrics")]
    synced: Option<String>,
    #[serde(default, rename = "plainLyrics")]
    plain: Option<String>,
}

impl Record {
    fn lyrics(self) -> Option<Lyrics> {
        let present = |text: Option<String>| text.filter(|t| !t.trim().is_empty());
        if self.instrumental {
            return None;
        }
        present(self.synced)
            .map(Lyrics::Synced)
            .or_else(|| present(self.plain).map(Lyrics::Plain))
    }
}

pub struct LyricsClient {
    http: reqwest::blocking::Client,
}

impl LyricsClient {
    pub fn new() -> reqwest::Result<Self> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(TIMEOUT)
                .build()?,
        })
    }

    /// Synced lyrics when LRCLIB has them, else plain ones. `Ok(None)`: not
    /// found, or an instrumental.
    pub fn find(&self, query: &LyricsQuery<'_>) -> Result<Option<Lyrics>, String> {
        // The exact lookup first (artist, title, album, duration), then a
        // search when the album or the title's spelling differ.
        let seconds = query.duration.as_secs_f64().round().to_string();
        let mut params = vec![
            ("artist_name", query.artist),
            ("track_name", query.title),
            ("duration", seconds.as_str()),
        ];
        if let Some(album) = query.album {
            params.push(("album_name", album));
        }
        // Only synced lyrics end the search here: a search may find a synced
        // copy where the exact lookup only had plain ones.
        let exact = self.get::<Option<Record>>("get", &params)?.flatten();
        if let Some(Lyrics::Synced(text)) = exact.clone().and_then(Record::lyrics) {
            return Ok(Some(Lyrics::Synced(text)));
        }
        let hits: Vec<Record> = self
            .get(
                "search",
                &[("artist_name", query.artist), ("track_name", query.title)],
            )?
            .unwrap_or_default();
        Ok(best(hits, query)
            .and_then(Record::lyrics)
            .or_else(|| exact.and_then(Record::lyrics)))
    }

    /// `Ok(None)` on 404. One retry when LRCLIB is busy.
    fn get<T: for<'de> Deserialize<'de>>(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
    ) -> Result<Option<T>, String> {
        let url = format!("{API}/{endpoint}");
        for attempt in 0..2 {
            let response = self
                .http
                .get(&url)
                .query(params)
                .send()
                .map_err(|e| e.to_string())?;
            let status = response.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if status == reqwest::StatusCode::SERVICE_UNAVAILABLE && attempt == 0 {
                std::thread::sleep(BUSY_RETRY);
                continue;
            }
            let response = response.error_for_status().map_err(|e| e.to_string())?;
            return response.json().map(Some).map_err(|e| e.to_string());
        }
        Err("LRCLIB is busy".into())
    }
}

/// The search hit for the recorded track: same title, same length within
/// a few seconds, synced lyrics preferred.
fn best(hits: Vec<Record>, query: &LyricsQuery<'_>) -> Option<Record> {
    let length = query.duration.as_secs_f64();
    let mut fitting: Vec<Record> = hits
        .into_iter()
        .filter(|r| title_similarity(&r.track_name, query.title) >= SAME_TITLE)
        .filter(|r| (r.duration - length).abs() <= DURATION_TOLERANCE)
        .collect();
    // Synced first (stable: LRCLIB's own order otherwise).
    fitting.sort_by_key(|r| r.synced.as_deref().is_none_or(|s| s.trim().is_empty()));
    fitting.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, duration: f64, synced: Option<&str>, plain: Option<&str>) -> Record {
        Record {
            track_name: name.into(),
            duration,
            instrumental: false,
            synced: synced.map(Into::into),
            plain: plain.map(Into::into),
        }
    }

    fn query(title: &str, seconds: u64) -> LyricsQuery<'_> {
        LyricsQuery {
            artist: "Artist",
            title,
            album: None,
            duration: Duration::from_secs(seconds),
        }
    }

    #[test]
    fn synced_lyrics_win_over_plain_ones() {
        let lyrics = record("Song", 200.0, Some("[00:01.00] la"), Some("la")).lyrics();
        assert_eq!(lyrics, Some(Lyrics::Synced("[00:01.00] la".into())));
        let plain = record("Song", 200.0, Some("  "), Some("la")).lyrics();
        assert_eq!(plain, Some(Lyrics::Plain("la".into())));
        let mut instrumental = record("Song", 200.0, Some("[00:01.00] la"), None);
        instrumental.instrumental = true;
        assert_eq!(instrumental.lyrics(), None);
    }

    #[test]
    fn the_search_hit_must_fit_the_recording() {
        let hits = vec![
            record("Song (Live)", 260.0, Some("[00:01.00] live"), None),
            record("Song", 201.0, None, Some("plain")),
            record("Song", 199.0, Some("[00:01.00] synced"), None),
            record("Other", 200.0, Some("[00:01.00] other"), None),
        ];
        let hit = best(hits, &query("Song", 200)).unwrap();
        assert_eq!(hit.synced.as_deref(), Some("[00:01.00] synced"));
        let none = best(
            vec![record("Song", 260.0, Some("x"), None)],
            &query("Song", 200),
        );
        assert!(none.is_none());
    }

    #[test]
    fn reads_lrclib_records() {
        // Shape of https://lrclib.net/api/get, 2026-10-05.
        let json = r#"{"id":2948339,"name":"Flow","trackName":"Flow","artistName":"Perfume","albumName":"Plasma","duration":185.0,"instrumental":false,"hasWordSync":false,"plainLyrics":"僕はただ信じていたくて","syncedLyrics":null}"#;
        let record: Record = serde_json::from_str(json).unwrap();
        assert_eq!(
            record.lyrics(),
            Some(Lyrics::Plain("僕はただ信じていたくて".into()))
        );
    }
}

//! Deezer public API client: no key, no account. Derived from WaveFlow's
//! (`waveflow-core/src/metadata/deezer.rs`), made blocking (Spytify runs on
//! plain threads) and reduced to what tagging a track needs.
//!
//! Rate limit: about 50 requests per 5 seconds per IP. Spytify makes three
//! or four per recorded track, minutes apart.

use std::time::Duration;

use serde::Deserialize;

const BASE_URL: &str = "https://api.deezer.com";
const USER_AGENT: &str = concat!("Spytify/", env!("CARGO_PKG_VERSION"));
const TIMEOUT: Duration = Duration::from_secs(8);

/// Deezer's in-band error object: refusals arrive with **HTTP 200** and
/// `{"error":{"type":"Exception","message":"Quota limit exceeded","code":4}}`
/// instead of the payload.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DeezerApiError {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub message: Option<String>,
    pub code: Option<i64>,
}

impl std::fmt::Display for DeezerApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.message, self.code) {
            (Some(msg), Some(code)) => write!(f, "{msg} (code {code})"),
            (Some(msg), None) => write!(f, "{msg}"),
            (None, Some(code)) => write!(f, "code {code}"),
            (None, None) => f.write_str("no reason given"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DeezerError {
    #[error("deezer request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("deezer refused the request: {0}")]
    Api(DeezerApiError),
    /// Throttling from the edge, as a status code with no JSON body.
    #[error("deezer rate-limited the request (HTTP 429)")]
    RateLimited,
}

impl DeezerError {
    pub fn is_quota_exceeded(&self) -> bool {
        match self {
            Self::Api(err) => {
                err.code == Some(4)
                    || err
                        .message
                        .as_deref()
                        .is_some_and(|m| m.to_ascii_lowercase().contains("quota"))
            }
            Self::RateLimited => true,
            Self::Http(_) => false,
        }
    }
}

pub type DeezerResult<T> = Result<T, DeezerError>;

/// Either the payload or the in-band error. `Error` first, so a body
/// carrying both reads as the refusal it is.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DeezerReply<T> {
    Error { error: DeezerApiError },
    Payload(T),
}

impl<T> DeezerReply<T> {
    fn into_result(self) -> DeezerResult<T> {
        match self {
            Self::Payload(value) => Ok(value),
            Self::Error { error } => Err(DeezerError::Api(error)),
        }
    }
}

#[derive(Debug, Deserialize)]
struct List<T> {
    data: Vec<T>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Named {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AlbumRef {
    pub id: i64,
    pub title: String,
    pub cover_xl: Option<String>,
    pub cover_big: Option<String>,
}

/// A `/search/track` hit.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TrackHit {
    pub id: i64,
    pub title: String,
    /// Seconds.
    pub duration: Option<u32>,
    pub artist: Option<Named>,
    pub album: Option<AlbumRef>,
}

/// `/track/{id}`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Track {
    pub id: i64,
    pub title: String,
    pub isrc: Option<String>,
    /// Position on its disc.
    pub track_position: Option<u32>,
    pub disk_number: Option<u32>,
    /// `YYYY-MM-DD`.
    pub release_date: Option<String>,
    /// Seconds.
    pub duration: Option<u32>,
    pub artist: Option<Named>,
    #[serde(default)]
    pub contributors: Vec<Named>,
    pub album: Option<AlbumRef>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Genres {
    pub data: Vec<Named>,
}

/// `/album/{id}`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Album {
    pub id: i64,
    pub title: String,
    pub label: Option<String>,
    pub release_date: Option<String>,
    pub nb_tracks: Option<u32>,
    pub genres: Option<Genres>,
    pub artist: Option<Named>,
    pub cover_xl: Option<String>,
    pub cover_big: Option<String>,
}

#[derive(Clone)]
pub struct DeezerClient {
    http: reqwest::blocking::Client,
}

impl DeezerClient {
    pub fn new() -> reqwest::Result<Self> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(TIMEOUT)
                .build()?,
        })
    }

    fn fetch<T: serde::de::DeserializeOwned>(
        request: reqwest::blocking::RequestBuilder,
    ) -> DeezerResult<T> {
        let response = request.send()?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(DeezerError::RateLimited);
        }
        response
            .error_for_status()?
            .json::<DeezerReply<T>>()?
            .into_result()
    }

    /// Free-text track search on `artist title`. Deezer's field syntax
    /// (`artist:"…" track:"…"`) returns nothing at all any more, even for
    /// exact names (checked 2026-10-01); [`super::lookup`] does the
    /// strict matching instead.
    pub fn search_track(&self, artist: &str, title: &str) -> DeezerResult<Vec<TrackHit>> {
        let query = format!("{} {}", quoted(artist), quoted(title));
        let list: List<TrackHit> = Self::fetch(
            self.http
                .get(format!("{BASE_URL}/search/track"))
                .query(&[("q", query.as_str())]),
        )?;
        Ok(list.data)
    }

    pub fn track(&self, id: i64) -> DeezerResult<Track> {
        Self::fetch(self.http.get(format!("{BASE_URL}/track/{id}")))
    }

    pub fn album(&self, id: i64) -> DeezerResult<Album> {
        Self::fetch(self.http.get(format!("{BASE_URL}/album/{id}")))
    }

    /// Raw bytes of a cover image.
    pub fn image(&self, url: &str) -> DeezerResult<Vec<u8>> {
        Ok(self
            .http
            .get(url)
            .send()?
            .error_for_status()?
            .bytes()?
            .to_vec())
    }
}

/// Double quotes would start a phrase search in a Deezer query.
fn quoted(value: &str) -> String {
    value.replace('"', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode<T: serde::de::DeserializeOwned>(body: &str) -> DeezerResult<T> {
        serde_json::from_str::<DeezerReply<T>>(body)
            .expect("body should parse as one arm or the other")
            .into_result()
    }

    #[test]
    fn an_in_band_error_is_an_error_and_not_an_empty_result() {
        let err = decode::<List<TrackHit>>(
            r#"{"error":{"type":"Exception","message":"Quota limit exceeded","code":4}}"#,
        )
        .expect_err("an error body must not read as a result");
        assert!(err.is_quota_exceeded());
        assert_eq!(
            err.to_string(),
            "deezer refused the request: Quota limit exceeded (code 4)"
        );
    }

    #[test]
    fn a_genuinely_empty_result_stays_a_result() {
        assert!(
            decode::<List<TrackHit>>(r#"{"data":[]}"#)
                .unwrap()
                .data
                .is_empty()
        );
    }

    #[test]
    fn decodes_a_track() {
        let track: Track = decode(
            r#"{"id":3135556,"title":"Harder, Better, Faster, Stronger","isrc":"GBDUW0000059",
                "track_position":4,"disk_number":1,"release_date":"2001-03-07","duration":224,
                "artist":{"name":"Daft Punk"},"contributors":[{"name":"Daft Punk"}],
                "album":{"id":302127,"title":"Discovery","cover_xl":"https://x/1000x1000.jpg"}}"#,
        )
        .unwrap();
        assert_eq!(track.isrc.as_deref(), Some("GBDUW0000059"));
        assert_eq!(
            (track.track_position, track.disk_number),
            (Some(4), Some(1))
        );
        assert_eq!(track.album.unwrap().title, "Discovery");
    }

    #[test]
    fn decodes_an_album_with_genres() {
        let album: Album = decode(
            r#"{"id":302127,"title":"Discovery","label":"Parlophone (France)",
                "release_date":"2001-03-07","nb_tracks":14,
                "genres":{"data":[{"name":"Electro"}]},"artist":{"name":"Daft Punk"}}"#,
        )
        .unwrap();
        assert_eq!(album.genres.unwrap().data[0].name, "Electro");
        assert_eq!(album.nb_tracks, Some(14));
    }

    #[test]
    fn quotes_cannot_break_the_query() {
        assert_eq!(quoted(r#"The "Real" One"#), "The  Real  One");
    }
}

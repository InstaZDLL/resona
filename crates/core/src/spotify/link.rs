//! Spotify links as users paste them, turned into the `spotify:` URIs the
//! command-line tool takes.

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Playlist,
    Album,
    Track,
}

impl LinkKind {
    fn from_segment(segment: &str) -> Option<Self> {
        match segment {
            "playlist" => Some(Self::Playlist),
            "album" => Some(Self::Album),
            "track" => Some(Self::Track),
            _ => None,
        }
    }

    const fn segment(self) -> &'static str {
        match self {
            Self::Playlist => "playlist",
            Self::Album => "album",
            Self::Track => "track",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyLink {
    pub kind: LinkKind,
    /// Base62 id.
    pub id: String,
}

impl SpotifyLink {
    /// Accepts `https://open.spotify.com/playlist/<id>?si=…` (with or
    /// without a locale segment such as `intl-fr`), `spotify:playlist:<id>`,
    /// and the same for albums and tracks.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let invalid = || Error::NotASpotifyLink(text.to_owned());
        let segments: Vec<&str> = if let Some(uri) = text.strip_prefix("spotify:") {
            uri.split(':').collect()
        } else {
            let rest = text
                .strip_prefix("https://")
                .or_else(|| text.strip_prefix("http://"))
                .unwrap_or(text);
            let rest = rest.strip_prefix("open.spotify.com/").ok_or_else(invalid)?;
            let path = rest.split(['?', '#']).next().unwrap_or_default();
            path.split('/')
                .filter(|s| !s.is_empty() && !s.starts_with("intl-"))
                .collect()
        };
        let [kind, id] = segments[..] else {
            return Err(invalid());
        };
        let kind = LinkKind::from_segment(kind).ok_or_else(invalid)?;
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(invalid());
        }
        Ok(Self {
            kind,
            id: id.to_owned(),
        })
    }

    pub fn uri(&self) -> String {
        format!("spotify:{}:{}", self.kind.segment(), self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_links() {
        let link = SpotifyLink::parse(
            "https://open.spotify.com/playlist/2nJLB0SB27TZfxfgvJDgXt?si=d15965c808c84997&pt=93e3",
        )
        .unwrap();
        assert_eq!(link.kind, LinkKind::Playlist);
        assert_eq!(link.uri(), "spotify:playlist:2nJLB0SB27TZfxfgvJDgXt");
        let album =
            SpotifyLink::parse("open.spotify.com/intl-fr/album/4gqRmcXiuzlxB9nEnFiK4y").unwrap();
        assert_eq!(album.uri(), "spotify:album:4gqRmcXiuzlxB9nEnFiK4y");
    }

    #[test]
    fn uris() {
        let track = SpotifyLink::parse(" spotify:track:6Si3ppdntTNAEaBUokB4Yv ").unwrap();
        assert_eq!(track.kind, LinkKind::Track);
        assert_eq!(track.id, "6Si3ppdntTNAEaBUokB4Yv");
    }

    #[test]
    fn other_text_is_refused() {
        for text in [
            "",
            "hello",
            "https://open.spotify.com/artist/2XMxWKPKCxoLkSdpCViCnr",
            "https://open.spotify.com/playlist/",
            "https://example.com/playlist/abc",
            "spotify:playlist:abc:extra",
            "spotify:playlist:a-b",
        ] {
            assert!(SpotifyLink::parse(text).is_err(), "{text}");
        }
    }
}

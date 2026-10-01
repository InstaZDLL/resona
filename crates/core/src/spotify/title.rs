//! Reading Spotify's state from its window title. Pure logic, ported from
//! the C# `SpotifyStatus` (and its tests).
//!
//! The title is `Artist - Title` while a track plays, `Spotify`,
//! `Spotify Free` or `Spotify Premium` when paused or idle, and
//! `Advertisement` (or an arbitrary one-part title) during ads.

const IDLE_TITLES: [&str; 3] = ["Spotify", "Spotify Free", "Spotify Premium"];
const AD_TITLE: &str = "Advertisement";

/// How the part after the title was attached, kept so a file name can be
/// rebuilt the way Spotify displays it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSeparator {
    None,
    /// `Song - Live`
    Dash,
    /// `Song (Live)`
    Parenthesis,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackTitle {
    pub artist: String,
    pub title: String,
    /// `Live`, `Remastered 2011`… split off the title.
    pub title_extended: Option<String>,
    pub separator: TitleSeparator,
}

impl std::fmt::Display for TrackTitle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} - {}", self.artist, self.title)?;
        match (&self.title_extended, self.separator) {
            (Some(extended), TitleSeparator::Dash) => write!(f, " - {extended}"),
            (Some(extended), TitleSeparator::Parenthesis) => write!(f, " ({extended})"),
            _ => Ok(()),
        }
    }
}

/// What the window title says Spotify is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleState {
    /// No window, or an idle title (`Spotify Premium`…): paused or stopped.
    /// While audio plays this is an ad.
    Idle,
    /// `Advertisement`, or a title that is not `Artist - Title`.
    Other(String),
    Track(TrackTitle),
}

pub fn is_idle_title(title: &str) -> bool {
    let title = title.trim();
    title.is_empty()
        || IDLE_TITLES
            .iter()
            .any(|idle| idle.eq_ignore_ascii_case(title))
}

pub fn is_ad_title(title: &str) -> bool {
    title.trim().eq_ignore_ascii_case(AD_TITLE)
}

pub fn parse(window_title: &str) -> TitleState {
    if is_idle_title(window_title) {
        return TitleState::Idle;
    }
    let Some((artist, rest)) = split_once_non_empty(window_title, " - ") else {
        return TitleState::Other(window_title.to_owned());
    };
    let (title, title_extended, separator) = split_title(rest);
    TitleState::Track(TrackTitle {
        artist: artist.to_owned(),
        title,
        title_extended,
        separator,
    })
}

fn split_title(title: &str) -> (String, Option<String>, TitleSeparator) {
    if let Some((title, extended)) = split_once_non_empty(title, " - ") {
        return (
            title.to_owned(),
            Some(extended.to_owned()),
            TitleSeparator::Dash,
        );
    }
    if let Some((title, extended)) = split_once_non_empty(title, " (") {
        let extended = extended.replace(')', "");
        return (
            title.to_owned(),
            Some(extended),
            TitleSeparator::Parenthesis,
        );
    }
    (title.to_owned(), None, TitleSeparator::None)
}

/// Like C#'s `Split(sep, 2, RemoveEmptyEntries)` reporting two parts.
fn split_once_non_empty<'a>(value: &'a str, separator: &str) -> Option<(&'a str, &'a str)> {
    let (left, right) = value.split_once(separator)?;
    (!left.is_empty() && !right.is_empty()).then_some((left, right))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(window_title: &str) -> TrackTitle {
        match parse(window_title) {
            TitleState::Track(track) => track,
            other => panic!("{window_title:?} parsed as {other:?}"),
        }
    }

    #[test]
    fn idle_titles_are_case_insensitive() {
        for title in [
            "",
            "  ",
            "Spotify",
            "Spotify Free",
            "Spotify Premium",
            "SPOTIFY",
            "spotify",
        ] {
            assert!(is_idle_title(title), "{title:?}");
            assert_eq!(parse(title), TitleState::Idle);
        }
        assert!(!is_idle_title("Artist Name - Song Title"));
        assert!(!is_idle_title("Advertisement"));
    }

    #[test]
    fn simple_track() {
        let t = track("Artist Name - Song Title");
        assert_eq!(
            (t.artist.as_str(), t.title.as_str()),
            ("Artist Name", "Song Title")
        );
        assert_eq!(t.title_extended, None);
        assert_eq!(t.separator, TitleSeparator::None);
        assert_eq!(t.to_string(), "Artist Name - Song Title");
    }

    #[test]
    fn dash_extended_title() {
        let t = track("Artist Name - Song Title - Live");
        assert_eq!(t.title, "Song Title");
        assert_eq!(t.title_extended.as_deref(), Some("Live"));
        assert_eq!(t.separator, TitleSeparator::Dash);
        assert_eq!(t.to_string(), "Artist Name - Song Title - Live");
    }

    #[test]
    fn parenthesis_extended_title() {
        let t = track("Artist Name - Song Title (Remastered 2011)");
        assert_eq!(t.title, "Song Title");
        assert_eq!(t.title_extended.as_deref(), Some("Remastered 2011"));
        assert_eq!(t.separator, TitleSeparator::Parenthesis);
        assert_eq!(t.to_string(), "Artist Name - Song Title (Remastered 2011)");
    }

    #[test]
    fn ads_and_unknown_titles() {
        for title in ["Advertisement", "Spotify Sponsor", "#1337: DAILY NEWS"] {
            assert_eq!(
                parse(title),
                TitleState::Other(title.to_owned()),
                "{title:?}"
            );
        }
        assert!(is_ad_title("advertisement"));
    }
}

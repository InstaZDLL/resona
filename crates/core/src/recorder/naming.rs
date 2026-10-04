//! Output file names.

use std::path::{Path, PathBuf};

use crate::spotify::title::TrackTitle;

/// Names Windows refuses for a file, whatever the extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// `Artist - Title.ext`, written the way Spotify displays the title
/// (`Song - Live`, `Song (Remastered)`), made safe for Windows.
pub fn file_name(title: &TrackTitle, extension: &str) -> String {
    format!("{}.{extension}", sanitize(&title.to_string()))
}

/// First free path for `name` in `dir`: `name`, then `name (2)`, …
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).{extension}")))
        .find(|path| !path.exists())
        .expect("an unbounded range always yields a free name")
}

/// Sub-folders a track is filed under, inside the output folder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Folders {
    #[default]
    None,
    /// `Album Artist/`
    Artist,
    /// `Album Artist/Album/`
    ArtistAlbum,
}

/// What goes in front of `Artist - Title`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Prefix {
    #[default]
    None,
    /// `04 `, the track's number on its album, when known.
    TrackNumber,
    /// `001 `, the order the tracks were recorded in this session.
    OrderNumber,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Layout {
    pub folders: Folders,
    pub prefix: Prefix,
}

/// What a track's place in the output folder depends on.
#[derive(Debug, Clone, Copy)]
pub struct Placement<'a> {
    pub title: &'a TrackTitle,
    pub album_artist: Option<&'a str>,
    pub album: Option<&'a str>,
    pub track_number: Option<u32>,
    /// 1 for the first track saved in the session.
    pub order: u32,
}

/// Path of the track relative to the output folder.
pub fn relative_path(layout: Layout, track: &Placement, extension: &str) -> PathBuf {
    let mut path = PathBuf::new();
    // The album artist groups an album's tracks together even when some
    // feature guests; Spotify's first listed artist is the fallback.
    let artist = track
        .album_artist
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| track.title.artist.split(", ").next().unwrap_or_default());
    let album = track
        .album
        .filter(|a| !a.trim().is_empty())
        .unwrap_or("Unknown album");
    match layout.folders {
        Folders::None => {}
        Folders::Artist => path.push(sanitize(artist)),
        Folders::ArtistAlbum => {
            path.push(sanitize(artist));
            path.push(sanitize(album));
        }
    }
    let prefix = match (layout.prefix, track.track_number) {
        (Prefix::TrackNumber, Some(number)) => format!("{number:02} "),
        (Prefix::OrderNumber, _) => format!("{:03} ", track.order),
        _ => String::new(),
    };
    path.push(format!("{prefix}{}", file_name(track.title, extension)));
    path
}

/// `Artist - Title` of a file name, without the numeric prefix
/// [`relative_path`] may have put in front, the ` (2)` of a duplicate, or
/// the extension: the key under which a track counts as already recorded.
pub fn track_key(file_name: &str) -> String {
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem);
    let stem = match stem.split_once(' ') {
        Some((number, rest))
            if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) =>
        {
            rest
        }
        _ => stem,
    };
    let stem = match stem.rsplit_once(" (") {
        Some((base, n))
            if n.strip_suffix(')')
                .is_some_and(|n| n.parse::<u32>().is_ok()) =>
        {
            base
        }
        _ => stem,
    };
    stem.to_lowercase()
}

/// The key of a track about to be recorded, comparable with [`track_key`].
pub fn title_key(title: &TrackTitle) -> String {
    track_key(&file_name(title, "x"))
}

fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // Windows drops trailing dots and spaces, which would make two names
    // collide or a name end up empty.
    let trimmed = cleaned.trim().trim_end_matches('.').trim_end();
    let name = if trimmed.is_empty() {
        "Untitled"
    } else {
        trimmed
    };
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name)) {
        format!("_{name}")
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::title::TitleSeparator;

    fn title(artist: &str, title: &str) -> TrackTitle {
        TrackTitle {
            artist: artist.into(),
            title: title.into(),
            title_extended: None,
            separator: TitleSeparator::None,
        }
    }

    #[test]
    fn keeps_the_spotify_display() {
        let mut t = title("Beach Riot", "Tell Me I'm Wrong");
        assert_eq!(file_name(&t, "flac"), "Beach Riot - Tell Me I'm Wrong.flac");
        t.title_extended = Some("Remastered 2011".into());
        t.separator = TitleSeparator::Parenthesis;
        assert_eq!(
            file_name(&t, "flac"),
            "Beach Riot - Tell Me I'm Wrong (Remastered 2011).flac"
        );
    }

    #[test]
    fn replaces_characters_windows_forbids() {
        let t = title("AC/DC", "What? Who: Me* <live> \"x\" | y.");
        assert_eq!(
            file_name(&t, "mp3"),
            "AC_DC - What_ Who_ Me_ _live_ _x_ _ y.mp3"
        );
    }

    #[test]
    fn avoids_reserved_names() {
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize(" ..."), "Untitled");
    }

    #[test]
    fn unique_path_numbers_duplicates() {
        let dir = std::env::temp_dir().join(format!("resona-naming-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("A - B.flac"), b"").unwrap();
        std::fs::write(dir.join("A - B (2).flac"), b"").unwrap();
        let path = unique_path(&dir, "A - B.flac");
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "A - B (3).flac");
    }

    fn placement<'a>(
        t: &'a TrackTitle,
        album: Option<&'a str>,
        number: Option<u32>,
    ) -> Placement<'a> {
        Placement {
            title: t,
            album_artist: None,
            album,
            track_number: number,
            order: 7,
        }
    }

    #[test]
    fn layouts() {
        let t = title("Beach Riot, Guest", "Modern Dinosaur");
        let p = placement(&t, Some("Sub Atomic Party Cool"), Some(4));
        let path = |folders, prefix| relative_path(Layout { folders, prefix }, &p, "flac");
        assert_eq!(
            path(Folders::None, Prefix::None),
            PathBuf::from("Beach Riot, Guest - Modern Dinosaur.flac")
        );
        assert_eq!(
            path(Folders::ArtistAlbum, Prefix::TrackNumber),
            PathBuf::from(
                "Beach Riot/Sub Atomic Party Cool/04 Beach Riot, Guest - Modern Dinosaur.flac"
            )
        );
        assert_eq!(
            path(Folders::Artist, Prefix::OrderNumber),
            PathBuf::from("Beach Riot/007 Beach Riot, Guest - Modern Dinosaur.flac")
        );
        let unknown = placement(&t, None, None);
        assert_eq!(
            relative_path(
                Layout {
                    folders: Folders::ArtistAlbum,
                    prefix: Prefix::TrackNumber
                },
                &unknown,
                "mp3"
            ),
            PathBuf::from("Beach Riot/Unknown album/Beach Riot, Guest - Modern Dinosaur.mp3")
        );
    }

    #[test]
    fn already_recorded_keys_ignore_prefix_duplicate_and_extension() {
        let t = title("Beach Riot", "Tell Me I'm Wrong");
        let key = title_key(&t);
        for name in [
            "Beach Riot - Tell Me I'm Wrong.flac",
            "04 Beach Riot - Tell Me I'm Wrong.mp3",
            "012 Beach Riot - Tell Me I'm Wrong (2).wav",
            "beach riot - tell me i'm wrong.FLAC",
        ] {
            assert_eq!(track_key(name), key, "{name}");
        }
        assert_ne!(track_key("Beach Riot - Modern Dinosaur.flac"), key);
        // A leading number in the artist goes through the same path.
        assert_eq!(
            track_key("10 Years - Wasteland.flac"),
            title_key(&title("10 Years", "Wasteland"))
        );
    }
}

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
        let dir = std::env::temp_dir().join(format!("spytify-naming-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("A - B.flac"), b"").unwrap();
        std::fs::write(dir.join("A - B (2).flac"), b"").unwrap();
        let path = unique_path(&dir, "A - B.flac");
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "A - B (3).flac");
    }
}

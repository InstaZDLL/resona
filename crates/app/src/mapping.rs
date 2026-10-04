//! Between the settings and the indexes of the UI's lists. The order of
//! each array is the order of the matching `ComboBox` model in
//! `ui/app-window.slint`.

use std::time::Duration;

use resona_core::format::{BitDepth, OutputFormat};
use resona_core::recorder::library::ExistingTracks;
use resona_core::recorder::naming::{Folders, Prefix};
use resona_core::settings::{Language, Quality};

pub const FORMATS: [OutputFormat; 11] = [
    OutputFormat::Flac {
        depth: BitDepth::Auto,
    },
    OutputFormat::Flac {
        depth: BitDepth::Bits16,
    },
    OutputFormat::Flac {
        depth: BitDepth::Bits24,
    },
    OutputFormat::Wav {
        depth: BitDepth::Auto,
    },
    OutputFormat::Wav {
        depth: BitDepth::Bits16,
    },
    OutputFormat::Wav {
        depth: BitDepth::Bits24,
    },
    OutputFormat::Mp3 { kbps: 320 },
    OutputFormat::Mp3 { kbps: 256 },
    OutputFormat::Mp3 { kbps: 192 },
    OutputFormat::Mp3 { kbps: 160 },
    OutputFormat::Mp3 { kbps: 128 },
];
pub const QUALITIES: [Quality; 3] = [Quality::Free, Quality::Premium, Quality::Lossless];
pub const FOLDERS: [Folders; 3] = [Folders::None, Folders::Artist, Folders::ArtistAlbum];
pub const PREFIXES: [Prefix; 3] = [Prefix::None, Prefix::TrackNumber, Prefix::OrderNumber];
pub const EXISTING: [ExistingTracks; 3] = [
    ExistingTracks::Skip,
    ExistingTracks::Replace,
    ExistingTracks::KeepBoth,
];
pub const LANGUAGES: [Language; 2] = [Language::Fr, Language::En];

/// Index of `value` in `list`, 0 if absent (the list's first entry is
/// always a sensible default).
pub fn index_of<T: PartialEq>(list: &[T], value: &T) -> i32 {
    list.iter().position(|v| v == value).unwrap_or(0) as i32
}

/// `list[index]`, the first entry for an out-of-range index.
pub fn at<T: Copy>(list: &[T], index: i32) -> T {
    usize::try_from(index)
        .ok()
        .and_then(|i| list.get(i))
        .copied()
        .unwrap_or(list[0])
}

pub fn language_code(language: Language) -> &'static str {
    match language {
        Language::Fr => "fr",
        Language::En => "en",
    }
}

/// `3:21`, or `1:02:03` past an hour.
pub fn length(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// `01:02:03` for the session timer.
pub fn clock(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_round_trips() {
        for (i, format) in FORMATS.iter().enumerate() {
            assert_eq!(index_of(&FORMATS, format), i as i32);
            assert_eq!(at(&FORMATS, i as i32), *format);
        }
    }

    #[test]
    fn out_of_range_falls_back_to_the_first_entry() {
        assert_eq!(at(&QUALITIES, -1), Quality::Free);
        assert_eq!(at(&QUALITIES, 99), Quality::Free);
        assert_eq!(index_of(&FORMATS, &OutputFormat::Mp3 { kbps: 96 }), 0);
    }

    #[test]
    fn lengths() {
        assert_eq!(length(Duration::from_secs(201)), "3:21");
        assert_eq!(length(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(clock(Duration::from_secs(3723)), "01:02:03");
    }
}

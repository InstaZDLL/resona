//! The tracks already in the output folder, to avoid recording one twice.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::naming::{title_key, track_key};
use crate::spotify::title::TrackTitle;

const AUDIO_EXTENSIONS: [&str; 3] = ["flac", "wav", "mp3"];
const TEMP_DIR: &str = ".resona-tmp";
/// The same, from before the app was renamed (Spytify).
const OLD_TEMP_DIR: &str = ".spytify-tmp";

/// What to do with a track that is already in the output folder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExistingTracks {
    /// Not recorded again.
    #[default]
    Skip,
    /// Recorded again, the old file removed.
    Replace,
    /// Recorded again next to the old one (`Artist - Title (2)`).
    KeepBoth,
}

/// Recorded tracks by [`track_key`], whatever their folder, prefix or
/// format.
#[derive(Debug, Default)]
pub struct Library {
    tracks: HashMap<String, PathBuf>,
}

impl Library {
    /// Every audio file under `dir`, recursively. Unreadable folders are
    /// skipped: a missing entry only means a track may be recorded twice.
    pub fn scan(dir: &Path) -> Self {
        let mut library = Self::default();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(folder) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&folder) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path
                        .file_name()
                        .is_some_and(|n| n != TEMP_DIR && n != OLD_TEMP_DIR)
                    {
                        pending.push(path);
                    }
                } else {
                    library.add(path);
                }
            }
        }
        library
    }

    pub fn add(&mut self, path: PathBuf) {
        let is_audio = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| AUDIO_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(e)));
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && is_audio
        {
            self.tracks.insert(track_key(name), path);
        }
    }

    pub fn remove(&mut self, path: &Path) {
        self.tracks.retain(|_, p| p != path);
    }

    pub fn find(&self, title: &TrackTitle) -> Option<&Path> {
        self.tracks.get(&title_key(title)).map(PathBuf::as_path)
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::title::TitleSeparator;

    #[test]
    fn finds_tracks_in_sub_folders_whatever_the_format() {
        let dir = std::env::temp_dir().join(format!("resona-library-{}", std::process::id()));
        let album = dir.join("Beach Riot").join("Sub Atomic Party Cool");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::create_dir_all(dir.join(TEMP_DIR)).unwrap();
        std::fs::write(album.join("04 Beach Riot - Modern Dinosaur.mp3"), b"").unwrap();
        std::fs::write(dir.join("cover.jpg"), b"").unwrap();
        std::fs::write(dir.join(TEMP_DIR).join("Beach Riot - Heaven.flac"), b"").unwrap();

        let library = Library::scan(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        let title = |t: &str| TrackTitle {
            artist: "Beach Riot".into(),
            title: t.into(),
            title_extended: None,
            separator: TitleSeparator::None,
        };
        assert_eq!(library.len(), 1);
        assert!(library.find(&title("Modern Dinosaur")).is_some());
        assert!(library.find(&title("Heaven")).is_none());
    }
}

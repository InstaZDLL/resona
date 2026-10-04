//! User settings, kept in `%APPDATA%\Resona\settings.toml`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::format::{OutputFormat, SpotifyQuality};
use crate::recorder::library::ExistingTracks;
use crate::recorder::naming::Layout;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub output_dir: PathBuf,
    /// The quality chosen in Spotify's own settings: Resona cannot read
    /// it, and it decides the natural output format.
    pub spotify_quality: Quality,
    /// `flac`, `flac16`, `flac24`, `wav`, `wav16`, `wav24`, `mp3:<kbps>`.
    #[serde(with = "as_string")]
    pub format: OutputFormat,
    pub min_duration_secs: u64,
    pub keep_partial: bool,
    pub layout: Layout,
    pub existing: ExistingTracks,
    pub skip_existing_in_spotify: bool,
    pub mute_ads: bool,
    /// Send Spotify to a virtual audio cable while recording (see
    /// `RecorderConfig::virtual_cable`).
    pub virtual_cable: bool,
    /// Hear Spotify while it plays into the cable.
    pub listen: bool,
    /// Ask GitHub for a newer release at launch.
    pub check_updates: bool,
    pub language: Language,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Quality {
    Free,
    Premium,
    #[default]
    Lossless,
}

impl From<Quality> for SpotifyQuality {
    fn from(quality: Quality) -> Self {
        match quality {
            Quality::Free => Self::Free,
            Quality::Premium => Self::Premium,
            Quality::Lossless => Self::Lossless,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    Fr,
    /// The default: the interface is written in English, French is a
    /// translation.
    #[default]
    En,
}

impl Default for Settings {
    fn default() -> Self {
        let music = std::env::var_os("USERPROFILE")
            .map(|home| PathBuf::from(home).join("Music"))
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            output_dir: music.join("Resona"),
            spotify_quality: Quality::Lossless,
            format: SpotifyQuality::Lossless.default_output(),
            min_duration_secs: 30,
            keep_partial: false,
            layout: Layout::default(),
            existing: ExistingTracks::Skip,
            skip_existing_in_spotify: false,
            mute_ads: true,
            virtual_cable: false,
            listen: false,
            check_updates: true,
            language: Language::En,
        }
    }
}

impl Settings {
    /// `%APPDATA%\Resona\settings.toml`.
    pub fn path() -> Option<PathBuf> {
        std::env::var_os("APPDATA")
            .map(|dir| PathBuf::from(dir).join("Resona").join("settings.toml"))
    }

    /// Where the settings were kept before the app was renamed from
    /// Spytify (2026-10-05): read once, then saved under the new name.
    fn old_path() -> Option<PathBuf> {
        std::env::var_os("APPDATA")
            .map(|dir| PathBuf::from(dir).join("Spytify").join("settings.toml"))
    }

    /// The saved settings, or the defaults when there are none. A damaged
    /// file is set aside (`settings.toml.bad`) rather than lost or fatal.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        if !path.exists()
            && let Some(old) = Self::old_path().filter(|p| p.exists())
        {
            return Self::load_from(&old);
        }
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("{}: {e}; using defaults", path.display());
            let _ = std::fs::rename(path, path.with_extension("toml.bad"));
            Self::default()
        })
    }

    pub fn save(&self) -> crate::Result<()> {
        let path = Self::path().ok_or(crate::Error::Settings("APPDATA is not set".into()))?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> crate::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text =
            toml::to_string_pretty(self).map_err(|e| crate::Error::Settings(e.to_string()))?;
        // Written aside then moved over, so a crash never leaves half a file.
        let staged = path.with_extension("toml.tmp");
        std::fs::write(&staged, text)?;
        std::fs::rename(&staged, path)?;
        Ok(())
    }

    #[cfg(windows)]
    pub fn recorder_config(&self) -> crate::recorder::engine::RecorderConfig {
        crate::recorder::engine::RecorderConfig {
            output_dir: self.output_dir.clone(),
            keep_partial: self.keep_partial,
            min_duration: Duration::from_secs(self.min_duration_secs),
            format: self.format,
            layout: self.layout,
            existing: self.existing,
            skip_existing_in_spotify: self.skip_existing_in_spotify,
            mute_ads: self.mute_ads,
            virtual_cable: self.virtual_cable,
            listen: self.listen,
        }
    }

    pub fn min_duration(&self) -> Duration {
        Duration::from_secs(self.min_duration_secs)
    }
}

mod as_string {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::format::OutputFormat;

    pub fn serialize<S: Serializer>(
        format: &OutputFormat,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<OutputFormat, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::naming::{Folders, Prefix};

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("resona-settings-{}-{name}", std::process::id()))
    }

    #[test]
    fn round_trips_through_toml() {
        let path = temp("round-trip.toml");
        let settings = Settings {
            format: OutputFormat::Mp3 { kbps: 192 },
            layout: Layout {
                folders: Folders::ArtistAlbum,
                prefix: Prefix::TrackNumber,
            },
            existing: ExistingTracks::KeepBoth,
            ..Settings::default()
        };
        settings.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = Settings::load_from(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(loaded, settings);
        assert!(text.contains("format = \"mp3:192\""), "{text}");
        assert!(text.contains("folders = \"artist-album\""), "{text}");
    }

    #[test]
    fn missing_fields_take_their_default() {
        let path = temp("partial.toml");
        std::fs::write(&path, "mute_ads = false\n").unwrap();
        let loaded = Settings::load_from(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(!loaded.mute_ads);
        assert_eq!(loaded.format, Settings::default().format);
    }

    #[test]
    fn a_damaged_file_is_set_aside() {
        let path = temp("bad.toml");
        std::fs::write(&path, "format = 42\n").unwrap();
        let loaded = Settings::load_from(&path);
        let aside = path.with_extension("toml.bad");
        assert!(aside.exists());
        std::fs::remove_file(&aside).unwrap();
        assert_eq!(loaded, Settings::default());
    }
}

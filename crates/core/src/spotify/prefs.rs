//! Spotify's own settings that alter what it plays, read from the client's
//! `prefs` file before recording: a Spotify update has switched them back
//! on silently (2026-10-03), and catching it afterwards costs tracks.
//!
//! One file per account, `Users\<name>-user\prefs` under the client's data
//! folder: `%LOCALAPPDATA%\Packages\SpotifyAB.SpotifyMusic_…\LocalState\
//! Spotify` for the Microsoft Store build, `%APPDATA%\Spotify` for the
//! classic installer. Lines are `key=value`. A key Spotify never changed
//! is absent and means its default. Crossfade moved to Spotify's servers
//! (`audio.crossfade_migrated_to_ps`): the fade check after each track
//! (`BitAnalysis::fades_in`) covers it.

use std::path::{Path, PathBuf};

/// `audio.play_bitrate_enumeration` for Lossless (measured on an account
/// set to Lossless).
const LOSSLESS_BITRATE: u8 = 5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpotifyPrefs {
    /// "Normalize volume", on by default: gain and a limiter.
    pub normalize: Option<bool>,
    /// "Automix", on by default: mixes playlist transitions.
    pub automix: Option<bool>,
    /// Streaming quality (`5` for Lossless).
    pub bitrate: Option<u8>,
}

/// A Spotify setting that keeps recordings from being lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefsIssue {
    Normalize,
    Automix,
    /// Lossless expected, Spotify streams another quality.
    NotLossless,
}

impl SpotifyPrefs {
    pub fn parse(text: &str) -> Self {
        let mut prefs = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let flag = || match value.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
            match key.trim() {
                "audio.normalize_v2" => prefs.normalize = flag(),
                "audio.automix" => prefs.automix = flag(),
                "audio.play_bitrate_enumeration" => prefs.bitrate = value.trim().parse().ok(),
                _ => {}
            }
        }
        prefs
    }

    /// What to switch off in Spotify. `lossless`: the user said Spotify
    /// is set to Lossless.
    pub fn issues(&self, lossless: bool) -> Vec<PrefsIssue> {
        let mut issues = Vec::new();
        // Absent keys are Spotify's defaults: both on.
        if self.normalize.unwrap_or(true) {
            issues.push(PrefsIssue::Normalize);
        }
        if self.automix.unwrap_or(true) {
            issues.push(PrefsIssue::Automix);
        }
        if lossless && self.bitrate.is_some_and(|b| b != LOSSLESS_BITRATE) {
            issues.push(PrefsIssue::NotLossless);
        }
        issues
    }

    /// The settings of the account used last, if a Spotify client is
    /// installed.
    pub fn load() -> Option<Self> {
        let text = std::fs::read_to_string(latest_prefs()?).ok()?;
        Some(Self::parse(&text))
    }
}

/// The most recently written account `prefs` among both client builds.
fn latest_prefs() -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let packages = Path::new(&local).join("Packages");
        if let Ok(entries) = std::fs::read_dir(&packages) {
            roots.extend(
                entries
                    .flatten()
                    .filter(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .starts_with("SpotifyAB.SpotifyMusic_")
                    })
                    .map(|e| e.path().join(r"LocalState\Spotify\Users")),
            );
        }
    }
    if let Some(roaming) = std::env::var_os("APPDATA") {
        roots.push(Path::new(&roaming).join(r"Spotify\Users"));
    }
    roots
        .iter()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flat_map(|entries| entries.flatten())
        .map(|account| account.path().join("prefs"))
        .filter_map(|prefs| Some((prefs.metadata().ok()?.modified().ok()?, prefs)))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, prefs)| prefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From the account recorded on 2026-10-04 (identifiers left out).
    const SET_UP: &str = "audio.sync_bitrate_enumeration=5\n\
        audio.play_bitrate_non_metered_migrated=true\n\
        ui.hide_hpto=true\n\
        audio.normalize_v2=false\n\
        audio.automix=false\n\
        audio.allow_downgrade=false\n\
        audio.play_bitrate_enumeration=5\n\
        audio.crossfade_migrated_to_ps=true\n";

    #[test]
    fn a_set_up_account_has_no_issue() {
        let prefs = SpotifyPrefs::parse(SET_UP);
        assert_eq!(prefs.normalize, Some(false));
        assert_eq!(prefs.automix, Some(false));
        assert_eq!(prefs.bitrate, Some(5));
        assert!(prefs.issues(true).is_empty());
    }

    #[test]
    fn missing_keys_are_spotify_defaults() {
        let prefs = SpotifyPrefs::parse("ui.hide_hpto=true\n");
        assert_eq!(
            prefs.issues(true),
            [PrefsIssue::Normalize, PrefsIssue::Automix]
        );
    }

    #[test]
    fn another_quality_matters_only_for_lossless() {
        let prefs = SpotifyPrefs::parse(&SET_UP.replace(
            "audio.play_bitrate_enumeration=5",
            "audio.play_bitrate_enumeration=4",
        ));
        assert_eq!(prefs.issues(true), [PrefsIssue::NotLossless]);
        assert!(prefs.issues(false).is_empty());
    }
}

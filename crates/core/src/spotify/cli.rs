//! Spotify's own command-line tool, `spotify_cli.exe`, shipped with the
//! desktop client (Microsoft Store build 1.3.1, found 2026-10-04). It talks
//! to the running, logged-in client: plays a URI, sets shuffle, repeat and
//! volume, lists any playlist the user can open (private ones included),
//! and answers in JSON. About 0.3 s per call.
//!
//! An official tool: nothing is injected into Spotify, and the
//! undocumented internals other recorders hook into stay untouched.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::{Error, Result};

/// A call that takes longer has hung (Spotify closed mid-call, a prompt;
/// a `now-playing` hung 20 s once, 2026-10-04). Usually 0.3 s.
const CALL_TIMEOUT: Duration = Duration::from_secs(8);
/// URIs per `lookup` call.
const LOOKUP_BATCH: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Status {
    pub running: bool,
    pub logged_in: bool,
}

/// A playlist or an album, with its tracks in order.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Collection {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub track_count: usize,
    #[serde(default)]
    pub tracks: Vec<CollectionTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CollectionTrack {
    /// `spotify:track:…`; local files and episodes have other kinds.
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub artists: Vec<String>,
    #[serde(default)]
    pub album: String,
}

impl CollectionTrack {
    pub fn is_track(&self) -> bool {
        self.uri.starts_with("spotify:track:")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NowPlaying {
    pub uri: String,
    /// `Title — Artist, Artist`.
    #[serde(default)]
    pub description: String,
    /// The playlist or album name, `Liked Songs`, …
    #[serde(default)]
    pub context_description: String,
    #[serde(default)]
    pub is_playing: bool,
}

/// What `lookup` tells about a track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    pub name: String,
    pub artists: Vec<String>,
    pub duration: Option<Duration>,
}

pub struct SpotifyCli {
    exe: PathBuf,
}

impl SpotifyCli {
    /// The tool from the Microsoft Store build's app alias, or next to the
    /// classic installer's `Spotify.exe`.
    pub fn find() -> Result<Self> {
        let candidates = [
            std::env::var_os("LOCALAPPDATA")
                .map(|d| PathBuf::from(d).join(r"Microsoft\WindowsApps\spotify_cli.exe")),
            std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join(r"Spotify\spotify_cli.exe")),
        ];
        candidates
            .into_iter()
            .flatten()
            // An app alias is a reparse point that `exists` cannot follow.
            .find(|path| path.symlink_metadata().is_ok())
            .map(|exe| Self { exe })
            .ok_or(Error::SpotifyCliMissing)
    }

    pub fn status(&self) -> Result<Status> {
        parse(&self.run(&["status"])?)
    }

    /// A playlist or album (`spotify:playlist:…`, `spotify:album:…`) with
    /// all its tracks.
    pub fn collection(&self, uri: &str) -> Result<Collection> {
        parse(&self.run(&["playlist", "get", uri])?)
    }

    /// `None` when Spotify has nothing loaded.
    pub fn now_playing(&self) -> Result<Option<NowPlaying>> {
        parse_now_playing(&self.run(&["now-playing"])?)
    }

    /// Plays a track, playlist or album from its start.
    pub fn play(&self, uri: &str) -> Result<()> {
        self.run(&["play", uri]).map(drop)
    }

    pub fn pause(&self) -> Result<()> {
        self.run(&["pause"]).map(drop)
    }

    pub fn shuffle(&self, on: bool) -> Result<()> {
        self.run(&["shuffle", if on { "on" } else { "off" }])
            .map(drop)
    }

    pub fn repeat_off(&self) -> Result<()> {
        self.run(&["repeat", "off"]).map(drop)
    }

    /// Spotify's own volume, 0.0 to 1.0. Below 1.0 the capture is scaled
    /// and no longer bit-perfect.
    pub fn volume(&self, level: f32) -> Result<()> {
        self.run(&["volume", &format!("{level:.2}")]).map(drop)
    }

    /// Name, artists and duration of tracks, for the URIs Spotify knows.
    pub fn lookup(&self, uris: &[String]) -> Result<HashMap<String, TrackInfo>> {
        let mut found = HashMap::new();
        for batch in uris.chunks(LOOKUP_BATCH) {
            let mut args = vec!["lookup"];
            args.extend(batch.iter().map(String::as_str));
            args.extend(["--fields", "duration"]);
            found.extend(parse_lookup(&self.run(&args)?)?);
        }
        Ok(found)
    }

    /// Runs one command, without a console window, and returns its JSON.
    fn run(&self, args: &[&str]) -> Result<String> {
        let mut command = Command::new(&self.exe);
        command
            .args(args)
            .args(["--format", "json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn()?;
        let started = Instant::now();
        while child.try_wait()?.is_none() {
            if started.elapsed() > CALL_TIMEOUT {
                let _ = child.kill();
                return Err(Error::SpotifyCli(format!("{} timed out", args.join(" "))));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = if stderr.trim().is_empty() {
                &stdout
            } else {
                &*stderr
            };
            return Err(Error::SpotifyCli(format!(
                "{}: {}",
                args.join(" "),
                message.trim()
            )));
        }
        Ok(stdout)
    }
}

fn parse<T: for<'de> Deserialize<'de>>(json: &str) -> Result<T> {
    serde_json::from_str(json).map_err(|e| Error::SpotifyCli(format!("unexpected answer: {e}")))
}

fn parse_now_playing(json: &str) -> Result<Option<NowPlaying>> {
    #[derive(Deserialize)]
    struct Answer {
        currently_playing: Option<NowPlaying>,
    }
    Ok(parse::<Answer>(json)?
        .currently_playing
        .filter(|now| !now.uri.is_empty()))
}

fn parse_lookup(json: &str) -> Result<HashMap<String, TrackInfo>> {
    #[derive(Deserialize)]
    struct Answer {
        #[serde(default)]
        entities: Vec<Entity>,
    }
    #[derive(Deserialize)]
    struct Entity {
        uri: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        contributors: Vec<Named>,
        #[serde(default)]
        metadata: Vec<Field>,
    }
    #[derive(Deserialize)]
    struct Named {
        name: String,
    }
    #[derive(Deserialize)]
    struct Field {
        field: String,
        value: String,
    }
    Ok(parse::<Answer>(json)?
        .entities
        .into_iter()
        .map(|entity| {
            let duration = entity
                .metadata
                .iter()
                .find(|f| f.field == "duration")
                .and_then(|f| f.value.parse().ok())
                .map(Duration::from_millis);
            let info = TrackInfo {
                name: entity.name,
                artists: entity.contributors.into_iter().map(|c| c.name).collect(),
                duration,
            };
            (entity.uri, info)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Answers captured from spotify_cli 1.3.1.234 on 2026-10-04.

    #[test]
    fn reads_a_private_playlist() {
        let json = r#"{"uri":"spotify:playlist:2nJLB0SB27TZfxfgvJDgXt","name":"BEST JP","description":"","owner":"3y3canzd8f1w1bype9a1fqu9u","is_owned":true,"collaborative":false,"is_public":false,"track_count":5,"snapshot_id":"","tracks":[{"type":"track","uri":"spotify:track:0cN6iBeCR7NgeBeTIKjLml","name":"阿修羅ちゃん","artists":["Ado"],"album":"狂言","position":0},{"type":"track","uri":"spotify:track:0FOVGBW0RpVRuDuUuN4ZVw","name":"ヨワネハキ","artists":["MAISONdes","和ぬか","asmi"],"album":"ヨワネハキ","position":2}]}"#;
        let playlist: Collection = parse(json).unwrap();
        assert_eq!(playlist.name, "BEST JP");
        assert_eq!(playlist.track_count, 5);
        assert_eq!(playlist.tracks[1].artists, ["MAISONdes", "和ぬか", "asmi"]);
        assert!(playlist.tracks[0].is_track());
    }

    #[test]
    fn reads_what_plays() {
        let json = r#"{"currently_playing":{"description":"阿修羅ちゃん — Ado","uri":"spotify:track:0cN6iBeCR7NgeBeTIKjLml","context_description":"BEST JP","is_playing":true}}"#;
        let now = parse_now_playing(json).unwrap().unwrap();
        assert_eq!(now.uri, "spotify:track:0cN6iBeCR7NgeBeTIKjLml");
        assert_eq!(now.context_description, "BEST JP");
        assert!(now.is_playing);
        assert_eq!(
            parse_now_playing(r#"{"currently_playing":null}"#).unwrap(),
            None
        );
        assert_eq!(parse_now_playing("{}").unwrap(), None);
    }

    #[test]
    fn reads_lookups() {
        let json = r#"{"entities":[{"uri":"spotify:track:6Si3ppdntTNAEaBUokB4Yv","type":"Song","name":"Flow","parent":{"name":"PLASMA","uri":"spotify:album:4gqRmcXiuzlxB9nEnFiK4y"},"contributors":[{"name":"Perfume","uri":"spotify:artist:2XMxWKPKCxoLkSdpCViCnr"}],"metadata":[{"field":"duration","value":"184000"},{"field":"formats","value":"FORMAT_AUDIO, "}]},{"uri":"spotify:track:x","metadata":[]}]}"#;
        let found = parse_lookup(json).unwrap();
        let flow = &found["spotify:track:6Si3ppdntTNAEaBUokB4Yv"];
        assert_eq!(flow.name, "Flow");
        assert_eq!(flow.artists, ["Perfume"]);
        assert_eq!(flow.duration, Some(Duration::from_secs(184)));
        assert_eq!(found["spotify:track:x"].duration, None);
    }

    #[test]
    fn reads_the_status() {
        let status: Status = parse(r#"{"running":true,"logged_in":true}"#).unwrap();
        assert!(status.running && status.logged_in);
    }
}

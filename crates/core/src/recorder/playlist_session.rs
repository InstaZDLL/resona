//! Recording a playlist, an album or a list of tracks from a link: loads
//! the list through `spotify_cli`, starts it in Spotify (shuffle and repeat
//! off, volume at 100 %), records, and pauses Spotify once the list is
//! over. A [`Plan`] decides; this module talks to Spotify and the recorder.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};

use super::engine::{Recorder, RecorderConfig, RecorderEvent};
use super::library::ExistingTracks;
use super::playlist::{Mode, Plan, Step, entry_for};
use crate::spotify::cli::{NowPlaying, SpotifyCli};
use crate::spotify::link::{LinkKind, SpotifyLink};
use crate::spotify::title::TrackTitle;
use crate::{Error, Result};

/// How often Spotify is asked what it plays.
const POLL: Duration = Duration::from_secs(1);
/// Spotify has this long to start the list after being asked.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// The capture has this long to start before the list is played.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A playlist, album or track link.
    Link(SpotifyLink),
    /// Tracks started one by one: the ones a previous run missed.
    Entries { name: String, entries: Vec<Entry> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub uri: String,
    pub title: String,
    pub artists: Vec<String>,
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryState {
    Waiting,
    Recording,
    Saved,
    AlreadyRecorded,
    /// Recorded but not kept (partial, too short), or failed.
    Missed,
}

impl EntryState {
    /// Saved now or before: nothing left to do for this entry.
    pub const fn is_done(self) -> bool {
        matches!(self, Self::Saved | Self::AlreadyRecorded)
    }
}

#[derive(Debug, Clone)]
pub enum PlaylistEvent {
    Loaded {
        name: String,
        entries: Vec<Entry>,
    },
    Entry {
        index: usize,
        state: EntryState,
    },
    Recorder(Box<RecorderEvent>),
    /// The run is over (list done, stopped, or failed): the entries not
    /// recorded, to try again.
    Finished {
        name: String,
        missed: Vec<Entry>,
    },
    Error(String),
}

pub struct PlaylistSession {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl PlaylistSession {
    pub fn start(
        config: RecorderConfig,
        source: Source,
    ) -> Result<(Self, Receiver<PlaylistEvent>)> {
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let (stop_tx, stop_rx) = crossbeam_channel::bounded(1);
        let thread = thread::Builder::new()
            .name("resona-playlist".into())
            .spawn(move || {
                let _ = wasapi::initialize_mta();
                run(config, source, &events_tx, &stop_rx);
            })?;
        Ok((
            Self {
                stop: stop_tx,
                thread: Some(thread),
            },
            events_rx,
        ))
    }

    /// Stops before the end of the list: pauses Spotify and keeps what was
    /// recorded. Waits for the last track to be encoded.
    pub fn stop(mut self) {
        let _ = self.stop.try_send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for PlaylistSession {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
    }
}

fn run(
    config: RecorderConfig,
    source: Source,
    events: &Sender<PlaylistEvent>,
    stop: &Receiver<()>,
) {
    let (name, missed) = match Run::new(config, source, events) {
        Ok(mut run) => {
            if let Err(e) = run.record(stop) {
                let _ = events.send(PlaylistEvent::Error(e.to_string()));
            }
            run.finish()
        }
        Err(e) => {
            let _ = events.send(PlaylistEvent::Error(e.to_string()));
            (String::new(), Vec::new())
        }
    };
    let _ = events.send(PlaylistEvent::Finished { name, missed });
}

struct Run<'a> {
    cli: SpotifyCli,
    config: RecorderConfig,
    name: String,
    entries: Vec<Entry>,
    states: Vec<EntryState>,
    /// The entry each recording was taken for when it started, by title:
    /// it ends (and is encoded) while the next one already plays.
    started: HashMap<String, usize>,
    plan: Plan,
    recorder: Option<Recorder>,
    recorder_events: Receiver<RecorderEvent>,
    events: &'a Sender<PlaylistEvent>,
}

impl<'a> Run<'a> {
    fn new(
        mut config: RecorderConfig,
        source: Source,
        events: &'a Sender<PlaylistEvent>,
    ) -> Result<Self> {
        let cli = SpotifyCli::find()?;
        let status = cli.status()?;
        if !status.running {
            return Err(Error::SpotifyNotRunning);
        }
        let (name, entries, mode) = load(&cli, source)?;
        let _ = events.send(PlaylistEvent::Loaded {
            name: name.clone(),
            entries: entries.clone(),
        });
        // Spotify moves past a track already in the folder instead of
        // playing it through for nothing.
        if config.existing == ExistingTracks::Skip {
            config.skip_existing_in_spotify = true;
        }
        let uris = entries.iter().map(|e| e.uri.clone()).collect();
        Ok(Self {
            cli,
            config,
            name,
            states: vec![EntryState::Waiting; entries.len()],
            started: HashMap::new(),
            entries,
            plan: Plan::new(uris, mode),
            recorder: None,
            recorder_events: crossbeam_channel::never(),
            events,
        })
    }

    fn record(&mut self, stop: &Receiver<()>) -> Result<()> {
        if self.plan.is_empty() {
            return Ok(());
        }
        // In order, as listed, and bit-perfect: Spotify's volume scales
        // the signal.
        for (what, result) in [
            ("shuffle off", self.cli.shuffle(false)),
            ("repeat off", self.cli.repeat_off()),
            ("volume 100 %", self.cli.volume(1.0)),
        ] {
            if let Err(e) = result {
                tracing::warn!("{what}: {e}");
            }
        }

        let (recorder, recorder_events) = Recorder::start(self.config.clone())?;
        self.recorder = Some(recorder);
        self.recorder_events = recorder_events;
        self.wait_for_capture(stop)?;

        let first = self.plan.first_uri().map(str::to_owned);
        if let Some(uri) = first {
            self.cli.play(&uri)?;
            self.plan.requested(0, Instant::now());
        }
        let asked_at = Instant::now();
        // Held until the run ends, however it ends: stops the watch.
        let (seen, _watch) = watch()?;
        let ticks = crossbeam_channel::tick(POLL);
        loop {
            select! {
                recv(stop) -> _ => return Ok(()),
                recv(self.recorder_events) -> event => match event {
                    Ok(event) => self.recorder_event(event),
                    Err(_) => return Ok(()),
                },
                recv(seen) -> playing => {
                    let Ok(playing) = playing else {
                        return Err(Error::SpotifyCli("lost track of Spotify".into()));
                    };
                    let now = Instant::now();
                    match self.plan.observe(playing.as_ref(), now) {
                        Step::Wait => {}
                        Step::Play(index) => {
                            self.cli.play(&self.entries[index].uri)?;
                            self.plan.requested(index, now);
                        }
                        Step::Finish => return Ok(()),
                    }
                }
                recv(ticks) -> _ => {
                    if self.plan.current().is_none() && asked_at.elapsed() > START_TIMEOUT {
                        return Err(Error::SpotifyCli("Spotify did not start the list".into()));
                    }
                }
            }
        }
    }

    fn wait_for_capture(&mut self, stop: &Receiver<()>) -> Result<()> {
        let deadline = crossbeam_channel::after(CAPTURE_TIMEOUT);
        loop {
            select! {
                recv(stop) -> _ => return Ok(()),
                recv(deadline) -> _ => return Err(Error::SpotifyCli("the capture did not start".into())),
                recv(self.recorder_events) -> event => match event {
                    Ok(event) => {
                        let started = matches!(event, RecorderEvent::CaptureStarted { .. });
                        self.recorder_event(event);
                        if started {
                            return Ok(());
                        }
                    }
                    Err(_) => return Err(Error::CaptureThread),
                },
            }
        }
    }

    fn recorder_event(&mut self, event: RecorderEvent) {
        let update = match &event {
            RecorderEvent::Recording(title) => Some((title, EntryState::Recording)),
            RecorderEvent::AlreadyRecorded { title, .. } => {
                Some((title, EntryState::AlreadyRecorded))
            }
            RecorderEvent::Saved { title, .. } => Some((title, EntryState::Saved)),
            RecorderEvent::Discarded { title, .. } => Some((title, EntryState::Missed)),
            _ => None,
        };
        if let Some((title, state)) = update {
            let key = title.to_string();
            let index = match state {
                EntryState::Recording | EntryState::AlreadyRecorded => {
                    let index = self.entry_of(title);
                    if let Some(index) = index {
                        self.started.insert(key, index);
                    }
                    index
                }
                _ => self
                    .started
                    .get(&key)
                    .copied()
                    .or_else(|| self.entry_of(title)),
            };
            if let Some(index) = index {
                self.set_state(index, state);
            }
        }
        let _ = self.events.send(PlaylistEvent::Recorder(Box::new(event)));
    }

    fn entry_of(&self, title: &TrackTitle) -> Option<usize> {
        let titles: Vec<String> = self.entries.iter().map(|e| e.title.clone()).collect();
        entry_for(&titles, &title.full_title(), self.plan.current())
            .or_else(|| entry_for(&titles, &title.title, self.plan.current()))
    }

    /// Saved stays saved: the end of a run records a second of whatever
    /// Spotify plays next, which must not undo the last entry.
    fn set_state(&mut self, index: usize, state: EntryState) {
        if self.states[index].is_done() || self.states[index] == state {
            return;
        }
        self.states[index] = state;
        let _ = self.events.send(PlaylistEvent::Entry { index, state });
    }

    /// Pauses Spotify, stops the recorder (the last track is encoded), and
    /// reports what is missing.
    fn finish(mut self) -> (String, Vec<Entry>) {
        if !self.plan.is_empty()
            && let Err(e) = self.cli.pause()
        {
            tracing::warn!("pause: {e}");
        }
        if let Some(recorder) = self.recorder.take() {
            recorder.stop();
        }
        while let Ok(event) = self.recorder_events.try_recv() {
            self.recorder_event(event);
        }
        let missed = self
            .entries
            .iter()
            .zip(&self.states)
            .filter(|(_, state)| !state.is_done())
            .map(|(entry, _)| entry.clone())
            .collect();
        (self.name, missed)
    }
}

/// Asks Spotify what it plays every [`POLL`], on a thread of its own: a
/// call can hang for seconds (seen 2026-10-04), and the session must keep
/// handling the recorder meanwhile. Only answers are sent: a failed call
/// is not "nothing playing", which would end the list. The thread stops
/// when the returned [`Watch`] is dropped, even while every call fails
/// (Spotify closed).
fn watch() -> Result<(Receiver<Option<NowPlaying>>, Watch)> {
    let cli = SpotifyCli::find()?;
    let (seen_tx, seen_rx) = crossbeam_channel::bounded(1);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    thread::Builder::new()
        .name("resona-now-playing".into())
        .spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match cli.now_playing() {
                    Ok(playing) => {
                        if seen_tx.send(playing).is_err() {
                            return;
                        }
                    }
                    Err(e) => tracing::warn!("now-playing: {e}"),
                }
                thread::sleep(POLL);
            }
        })?;
    Ok((seen_rx, Watch { stop }))
}

/// Stops the [`watch`] thread when dropped.
struct Watch {
    stop: Arc<AtomicBool>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The entries to record and how to play them.
fn load(cli: &SpotifyCli, source: Source) -> Result<(String, Vec<Entry>, Mode)> {
    match source {
        Source::Entries { name, entries } => Ok((name, entries, Mode::Tracks)),
        Source::Link(link) if link.kind == LinkKind::Track => {
            let uri = link.uri();
            let info = cli
                .lookup(std::slice::from_ref(&uri))?
                .remove(&uri)
                .ok_or_else(|| Error::SpotifyCli(format!("unknown track {uri}")))?;
            let entry = Entry {
                uri,
                title: info.name.clone(),
                artists: info.artists,
                duration: info.duration,
            };
            Ok((info.name, vec![entry], Mode::Tracks))
        }
        Source::Link(link) => {
            let uri = link.uri();
            let collection = cli.collection(&uri)?;
            let tracks: Vec<_> = collection
                .tracks
                .into_iter()
                .filter(|t| t.is_track())
                .collect();
            let uris: Vec<String> = tracks.iter().map(|t| t.uri.clone()).collect();
            // Durations are for display: a failed lookup only hides them.
            let mut infos = cli.lookup(&uris).unwrap_or_default();
            let entries = tracks
                .into_iter()
                .map(|track| Entry {
                    duration: infos.remove(&track.uri).and_then(|i| i.duration),
                    uri: track.uri,
                    title: track.name,
                    artists: track.artists,
                })
                .collect();
            Ok((collection.name, entries, Mode::Context(uri)))
        }
    }
}

//! Polling Spotify and publishing [`Event`]s, the equivalent of the C#
//! `SpotifyHandler` timers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};

use super::process::SpotifyProcesses;
use super::smtc::{Smtc, SmtcSnapshot};
use super::state::{self, Content, Event, SmtcView, Status};
use super::window;

/// Same period as the C# watcher.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// One poll's raw inputs, kept for diagnostics (`spotify_probe` example).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub root_pid: Option<u32>,
    pub window_title: Option<String>,
    pub smtc: Option<SmtcSnapshot>,
}

/// Reads Spotify's state. Must live on a thread with COM initialised (MTA).
pub struct Poller {
    processes: Option<SpotifyProcesses>,
    smtc: Option<Smtc>,
}

impl Poller {
    pub fn new() -> Self {
        let smtc = Smtc::new()
            .inspect_err(|e| tracing::warn!("SMTC unavailable, window title only: {e}"))
            .ok();
        Self {
            processes: None,
            smtc,
        }
    }

    pub fn observe(&mut self) -> Observation {
        let mut window = self
            .processes
            .as_ref()
            .and_then(|p| window::find_main_window(&p.all));
        if window.is_none() {
            // Enumerating every process is the costly part of a poll; redo it
            // only when the cached PIDs no longer own a window (Spotify
            // restarted, or was not running).
            self.processes = SpotifyProcesses::find();
            window = self
                .processes
                .as_ref()
                .and_then(|p| window::find_main_window(&p.all));
        }
        let smtc = self.smtc.as_ref().and_then(|smtc| {
            smtc.spotify()
                .inspect_err(|e| tracing::debug!("SMTC read failed: {e}"))
                .ok()
                .flatten()
        });
        Observation {
            root_pid: self.processes.as_ref().map(|p| p.root),
            window_title: window.map(|w| w.title),
            smtc,
        }
    }
}

impl Default for Poller {
    fn default() -> Self {
        Self::new()
    }
}

/// `previous` is the content of the last status, see [`state::classify`].
pub fn status_of(observation: &Observation, audio_active: bool, previous: &Content) -> Status {
    if observation.root_pid.is_none() {
        return Status::NOT_RUNNING;
    }
    let smtc = observation.smtc.as_ref().map(|s| SmtcView {
        title: &s.title,
        album: &s.album,
        album_artist: &s.album_artist,
        track_number: s.track_number,
        duration: s.duration,
        playing: s.playing,
    });
    state::classify(
        observation.window_title.as_deref(),
        smtc.as_ref(),
        audio_active,
        previous,
    )
}

pub struct Monitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    /// `audio_active` tells whether Spotify is producing sound right now;
    /// it is how an ad behind an idle window title is recognised. The
    /// recorder answers it from the capture stream.
    pub fn spawn(
        audio_active: impl Fn() -> bool + Send + 'static,
    ) -> std::io::Result<(Self, Receiver<Event>)> {
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("spytify-monitor".into())
            .spawn(move || run(&events_tx, &thread_stop, &audio_active))?;
        Ok((
            Self {
                stop,
                thread: Some(thread),
            },
            events_rx,
        ))
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run(events: &Sender<Event>, stop: &AtomicBool, audio_active: &dyn Fn() -> bool) {
    let _ = wasapi::initialize_mta();
    let mut poller = Poller::new();
    let mut previous = Status::NOT_RUNNING;
    while !stop.load(Ordering::Relaxed) {
        let current = status_of(&poller.observe(), audio_active(), &previous.content);
        for event in state::diff(&previous, &current) {
            if events.send(event).is_err() {
                return;
            }
        }
        previous = current;
        thread::sleep(POLL_INTERVAL);
    }
}

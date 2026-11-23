//! Polling the music app and publishing [`Event`]s, the equivalent of the
//! C# `SpotifyHandler` timers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender};

use super::process::AppProcesses;
use super::smtc::{Smtc, SmtcSnapshot};
use super::state::{self, Content, Event, SmtcView, Status};
use super::title::TrackTitle;
use super::window;
use crate::provider::{Provider, TitleMode};

/// Same period as the C# watcher.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// One poll's raw inputs, kept for diagnostics (`spotify_probe` example).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub provider: Provider,
    pub root_pid: Option<u32>,
    pub window_title: Option<String>,
    pub smtc: Option<SmtcSnapshot>,
}

/// Reads the app's state. Must live on a thread with COM initialised (MTA).
pub struct Poller {
    provider: Provider,
    processes: Option<AppProcesses>,
    smtc: Option<Smtc>,
}

impl Poller {
    pub fn new(provider: Provider) -> Self {
        let smtc = Smtc::new()
            .inspect_err(|e| tracing::warn!("SMTC unavailable, window title only: {e}"))
            .ok();
        Self {
            provider,
            processes: None,
            smtc,
        }
    }

    pub fn observe(&mut self) -> Observation {
        let class = self.provider.window_class_prefix();
        let mut window = self
            .processes
            .as_ref()
            .and_then(|p| window::find_main_window(&p.all, class));
        if window.is_none() {
            // Enumerating every process is the costly part of a poll; redo it
            // only when the cached PIDs no longer own a window (the app
            // restarted, or was not running).
            self.processes = AppProcesses::find(self.provider);
            window = self
                .processes
                .as_ref()
                .and_then(|p| window::find_main_window(&p.all, class));
        }
        let smtc = self.smtc.as_ref().and_then(|smtc| {
            smtc.session(self.provider)
                .inspect_err(|e| tracing::debug!("SMTC read failed: {e}"))
                .ok()
                .flatten()
        });
        Observation {
            provider: self.provider,
            root_pid: self.processes.as_ref().map(|p| p.root),
            window_title: window.map(|w| w.title),
            smtc,
        }
    }
}

/// `previous` is the content of the last status, see [`state::classify`].
pub fn status_of(observation: &Observation, audio_active: bool, previous: &Content) -> Status {
    if observation.root_pid.is_none() {
        return Status::NOT_RUNNING;
    }
    let smtc = observation.smtc.as_ref().map(|s| SmtcView {
        title: &s.title,
        artist: &s.artist,
        album: &s.album,
        album_artist: &s.album_artist,
        track_number: s.track_number,
        duration: s.duration,
        playing: s.playing,
    });
    match observation.provider.title_mode() {
        TitleMode::WindowTitle => state::classify(
            observation.window_title.as_deref(),
            smtc.as_ref(),
            audio_active,
            previous,
        ),
        TitleMode::Smtc => state::classify_smtc(smtc.as_ref()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant, reason = "a few events per second at most")]
pub enum MonitorEvent {
    /// A status change, observed at `at`. The change itself happened
    /// earlier: up to one poll, plus Spotify's own delay in updating.
    Status { at: Instant, event: Event },
    /// SMTC dated the start of the current track: `started_at` is when its
    /// first sample played, to within a few milliseconds. Sent once per
    /// track, a poll or so after its [`Event::ContentChanged`].
    TrackStart {
        title: TrackTitle,
        started_at: Instant,
    },
}

pub struct Monitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    /// Watches `provider`. `audio_active` tells whether the app is producing
    /// sound right now; it is how a Spotify ad behind an idle window title
    /// is recognised. The recorder answers it from the capture stream.
    pub fn spawn(
        provider: Provider,
        audio_active: impl Fn() -> bool + Send + 'static,
    ) -> std::io::Result<(Self, Receiver<MonitorEvent>)> {
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("resona-monitor".into())
            .spawn(move || run(provider, &events_tx, &thread_stop, &audio_active))?;
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

fn run(
    provider: Provider,
    events: &Sender<MonitorEvent>,
    stop: &AtomicBool,
    audio_active: &dyn Fn() -> bool,
) {
    let _ = wasapi::initialize_mta();
    let mut poller = Poller::new(provider);
    let mut previous = Status::NOT_RUNNING;
    let mut dated: Option<TrackTitle> = None;
    while !stop.load(Ordering::Relaxed) {
        let observation = poller.observe();
        let at = Instant::now();
        let current = status_of(&observation, audio_active(), &previous.content);
        for event in state::diff(&previous, &current) {
            if events.send(MonitorEvent::Status { at, event }).is_err() {
                return;
            }
        }
        if provider.dates_track_start()
            && let Content::Track(track) = &current.content
            && dated.as_ref() != Some(&track.title)
            && let Some(started_at) = observation
                .smtc
                .as_ref()
                .and_then(|s| track_start(s, &track.title))
        {
            dated = Some(track.title.clone());
            let start = MonitorEvent::TrackStart {
                title: track.title.clone(),
                started_at,
            };
            if events.send(start).is_err() {
                return;
            }
        }
        previous = current;
        thread::sleep(POLL_INTERVAL);
    }
}

/// When `title` started playing, from SMTC's position and the time it was
/// published. Only while playing: a paused timeline dates nothing.
fn track_start(smtc: &SmtcSnapshot, title: &TrackTitle) -> Option<Instant> {
    if !smtc.playing || !state::smtc_title_matches(&smtc.title, title) {
        return None;
    }
    let since_update = SystemTime::now()
        .duration_since(smtc.timeline_updated?)
        .unwrap_or_default();
    Instant::now().checked_sub(since_update + smtc.position.unwrap_or_default())
}

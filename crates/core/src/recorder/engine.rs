//! The recording session: capture + monitor in, one FLAC per track out.
//!
//! Three threads besides the caller's: the capture thread (WASAPI), the
//! monitor thread (Spotiframe_at(started_at)y state), and this engine's own, which owns the
//! [`Splitter`] and the temporary WAV of the track being recorded. Encoding
//! runs on a fourth thread so a slow FLAC never holds up the capture.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};

use super::clock::FrameClock;
use super::library::{ExistingTracks, Library};
use super::naming::{self, Layout};
use super::splitter::{Action, Change, Ended, Splitter};
use crate::analysis::{BitAnalysis, Fidelity};
use crate::audio_setup::{self, CableAsDefault, OutputDevice};
use crate::capture::{CaptureConfig, Packet, ProcessCapture};
use crate::device_config;
use crate::encode::{Quantizer, flac, mp3, wav, wav::CaptureWav};
use crate::format::CAPTURE_SAMPLE_RATE;
use crate::format::OutputFormat;
use crate::metadata::deezer::DeezerClient;
use crate::metadata::name_match::title_similarity;
use crate::metadata::{self, TagsOutcome};
use crate::playback::Playback;
use crate::routing;
use crate::spotify::cli::{NowPlaying, SpotifyCli};
use crate::spotify::monitor::{Monitor, MonitorEvent};
use crate::spotify::process::SpotifyProcesses;
use crate::spotify::smtc::Smtc;
use crate::spotify::state::{Content, Event};
use crate::spotify::title::TrackTitle;
use crate::{Error, Result};

/// How often a lost capture (Spotify closed or restarted) is retried.
const CAPTURE_RETRY: Duration = Duration::from_secs(2);
/// A title change is noticed on the next poll: on average half an
/// interval after it happened.
const TITLE_LAG: Duration = Duration::from_millis(250);
/// Shortfall against SMTC's duration still counted as a complete track
/// (silence trimmed at both ends, Spotify's own rounding).
const PARTIAL_TOLERANCE: Duration = Duration::from_secs(3);
const TEMP_DIR: &str = ".resona-tmp";

#[derive(Debug, Clone)]
pub struct RecorderConfig {
    pub output_dir: PathBuf,
    /// Keep tracks recorded only in part (joined late, skipped).
    pub keep_partial: bool,
    /// Tracks shorter than this are dropped (jingles, accidental plays).
    pub min_duration: Duration,
    /// See [`crate::format::SpotifyQuality::default_output`] for the
    /// natural choice per Spotify tier.
    pub format: OutputFormat,
    /// Sub-folders and file name prefix.
    pub layout: Layout,
    /// What to do with a track already in the output folder.
    pub existing: ExistingTracks,
    /// With [`ExistingTracks::Skip`]: also make Spotify move on to the
    /// next track instead of playing one that will not be recorded.
    pub skip_existing_in_spotify: bool,
    /// Mute Spotify in the Windows mixer while an ad plays (Free tier).
    pub mute_ads: bool,
    /// Send Spotify to a virtual audio cable (VB-Cable) instead of the
    /// headset or speakers, so their settings (rate, enhancements,
    /// surround) no longer touch the recording. Off takes Spotify back
    /// off the cable.
    pub virtual_cable: bool,
    /// With [`Self::virtual_cable`]: play what is recorded on the default
    /// device, to hear Spotify. Off, Spotify is silent while recording.
    pub listen: bool,
}

#[derive(Debug, Clone)]
pub enum RecorderEvent {
    CaptureStarted {
        pid: u32,
        /// The device Spotify plays on as found when the capture started,
        /// with [`OutputDevice::lossless_issues`] to warn about.
        device: OutputDevice,
    },
    /// [`RecorderConfig::virtual_cable`] is on but no cable is installed:
    /// Spotify stays on its current device.
    CableMissing,
    /// The Windows default output device is the virtual cable: nothing is
    /// heard (VB-Cable's installer does that).
    CableIsDefault(CableAsDefault),
    /// The track starts with a volume ramp Spotify added: crossfade or
    /// Automix is on (Spotify updates have switched them back on). Its
    /// start, and the end of the track before, are not lossless.
    SpotifyFades(TrackTitle),
    CaptureLost,
    Recording(TrackTitle),
    /// The track is already in the output folder and is not recorded
    /// again ([`ExistingTracks::Skip`]).
    AlreadyRecorded {
        title: TrackTitle,
        existing: PathBuf,
        /// Spotify was asked to move on to the next track.
        skipped_in_spotify: bool,
    },
    /// Spotify was muted or unmuted around an ad.
    AdMuted(bool),
    Saved {
        title: TrackTitle,
        path: PathBuf,
        fidelity: Fidelity,
        duration: Duration,
        /// Where the tags beyond Spotify's own came from, or why not.
        tags: TagsOutcome,
    },
    Discarded {
        title: TrackTitle,
        reason: DiscardReason,
        /// What the recording was worth, for diagnostics.
        fidelity: Fidelity,
        duration: Duration,
    },
    Spotify(Event),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardReason {
    Partial,
    TooShort,
    /// Saved meanwhile under the same name ([`ExistingTracks::Skip`]).
    AlreadyRecorded,
}

pub struct Recorder {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    pub fn start(config: RecorderConfig) -> Result<(Self, Receiver<RecorderEvent>)> {
        std::fs::create_dir_all(config.output_dir.join(TEMP_DIR))?;
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let (stop_tx, stop_rx) = crossbeam_channel::bounded(1);
        let thread = thread::Builder::new()
            .name("resona-recorder".into())
            .spawn(move || {
                if let Err(e) = run(&config, &events_tx, &stop_rx) {
                    let _ = events_tx.send(RecorderEvent::Error(e.to_string()));
                }
            })?;
        Ok((
            Self {
                stop: stop_tx,
                thread: Some(thread),
            },
            events_rx,
        ))
    }

    /// Ends the current track (kept or discarded like any other) and waits
    /// for pending encodes.
    pub fn stop(mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
    }
}

struct Capture {
    pid: u32,
    capture: ProcessCapture,
    packets: Receiver<Packet>,
    device: OutputDevice,
}

fn start_capture(
    config: &RecorderConfig,
    cable: &mut Option<CableRoute>,
    events: &Sender<RecorderEvent>,
) -> Result<Capture> {
    let processes = SpotifyProcesses::find().ok_or(Error::SpotifyNotRunning)?;
    if !config.virtual_cable {
        leave_cable(processes.root);
    } else if cable.is_none() {
        *cable = CableRoute::enter(processes.root, events);
    }
    let device = audio_setup::spotify_output_device(processes.root)?;
    let (capture, packets) = ProcessCapture::start(CaptureConfig {
        pid: processes.root,
        source_channels: device.channels,
        channel_mask: device.channel_mask,
    })?;
    Ok(Capture {
        pid: processes.root,
        capture,
        packets,
        device,
    })
}

/// Spotify sent to the virtual cable for the length of a recording: it
/// plays there silently, out of the headset's chain.
struct CableRoute {
    cable_id: String,
    /// Spotify's own output device before (`None`: the default one).
    previous: Option<String>,
}

impl CableRoute {
    /// Sets the cable up (44.1 kHz) and sends Spotify (root process `pid`)
    /// to it. `None`, with an event saying why, when that fails.
    fn enter(pid: u32, events: &Sender<RecorderEvent>) -> Option<Self> {
        let cable_id = match audio_setup::virtual_cable() {
            Ok(Some((id, _))) => id,
            Ok(None) => {
                let _ = events.send(RecorderEvent::CableMissing);
                return None;
            }
            Err(e) => {
                tracing::warn!("cannot list the output devices: {e}");
                return None;
            }
        };
        if let Ok(Some(default)) = audio_setup::cable_as_default() {
            let _ = events.send(RecorderEvent::CableIsDefault(default));
        }
        // VB-Cable installs at 48 kHz: Spotify would be resampled on its
        // way in. If this fails, the capture's setup warning says so.
        if audio_setup::output_device(&cable_id).is_ok_and(|d| d.resamples())
            && let Err(e) = device_config::set_sample_rate(&cable_id, CAPTURE_SAMPLE_RATE)
        {
            tracing::warn!("cannot set the cable to 44.1 kHz: {e}");
        }
        let current = routing::spotify_endpoint(pid).unwrap_or_else(|e| {
            tracing::warn!("cannot read Spotify's output device: {e}");
            None
        });
        // Already on the cable: left there by a session that did not end
        // cleanly. The default device is the safe way back.
        let previous = current.filter(|id| *id != cable_id);
        if let Err(e) = routing::route_spotify(pid, Some(&cable_id)) {
            let _ = events.send(RecorderEvent::Error(format!(
                "cannot send Spotify to the virtual cable: {e}"
            )));
            return None;
        }
        Some(Self { cable_id, previous })
    }
}

/// Gives Spotify its device back, however the session ends. Windows keeps
/// the choice per executable, so this needs a running Spotify; otherwise
/// the next session puts it back.
impl Drop for CableRoute {
    fn drop(&mut self) {
        if let Some(processes) = SpotifyProcesses::find()
            && let Err(e) = routing::route_spotify(processes.root, self.previous.as_deref())
        {
            tracing::warn!("cannot give Spotify its output device back: {e}");
        }
    }
}

/// Plays the capture on the default device, Spotify itself being silent in
/// the cable. `None`, with an event saying why, when that fails.
fn listen(cable_id: &str, events: &Sender<RecorderEvent>) -> Option<(Playback, Sender<Vec<f32>>)> {
    match Playback::start(cable_id) {
        Ok(playback) => Some(playback),
        // The cable is the default device: [`RecorderEvent::CableIsDefault`]
        // has said so already.
        Err(Error::PlaybackLoop) => None,
        Err(e) => {
            let _ = events.send(RecorderEvent::Error(format!(
                "cannot play Spotify on the headset: {e}"
            )));
            None
        }
    }
}

/// Takes Spotify off the virtual cable if a session left it there.
fn leave_cable(pid: u32) {
    let Ok(Some((cable_id, _))) = audio_setup::virtual_cable() else {
        return;
    };
    if routing::spotify_endpoint(pid).is_ok_and(|id| id.as_deref() == Some(cable_id.as_str())) {
        let _ = routing::route_spotify(pid, None);
    }
}

fn run(config: &RecorderConfig, events: &Sender<RecorderEvent>, stop: &Receiver<()>) -> Result<()> {
    let _ = wasapi::initialize_mta();
    let sound = Arc::new(AtomicBool::new(false));
    let monitor_sound = Arc::clone(&sound);
    let (monitor, monitor_events) = Monitor::spawn(move || monitor_sound.load(Ordering::Relaxed))?;
    let (jobs_tx, jobs_rx) = crossbeam_channel::unbounded::<Job>();
    let library = Arc::new(Mutex::new(Library::scan(&config.output_dir)));
    let encoder = {
        let events = events.clone();
        let config = config.clone();
        let library = Arc::clone(&library);
        thread::Builder::new()
            .name("resona-encoder".into())
            .spawn(move || encode_jobs(&config, &library, &jobs_rx, &events))?
    };

    let mut session = Session {
        splitter: Splitter::new(CAPTURE_SAMPLE_RATE),
        clock: FrameClock::new(CAPTURE_SAMPLE_RATE),
        writer: None,
        skipping: None,
        temp_dir: config.output_dir.join(TEMP_DIR),
        jobs: jobs_tx,
        events: events.clone(),
        config: config.clone(),
        library,
        smtc: None,
        ad_muted: false,
        playing: PlayingWatch::spawn(),
    };
    let mut capture: Option<Capture> = None;
    let mut last_attempt: Option<Instant> = None;
    // Dropped when the session ends, error or not: Spotify leaves the cable.
    let mut cable: Option<CableRoute> = None;
    let mut playback: Option<(Playback, Sender<Vec<f32>>)> = None;

    loop {
        if capture.is_none() && last_attempt.is_none_or(|t| t.elapsed() >= CAPTURE_RETRY) {
            last_attempt = Some(Instant::now());
            if let Ok(started) = start_capture(config, &mut cable, events) {
                if config.listen
                    && playback.is_none()
                    && let Some(route) = &cable
                {
                    playback = listen(&route.cable_id, events);
                }
                let _ = events.send(RecorderEvent::CaptureStarted {
                    pid: started.pid,
                    device: started.device.clone(),
                });
                capture = Some(started);
            }
        }
        let never = crossbeam_channel::never();
        let packets = capture.as_ref().map_or(&never, |c| &c.packets);

        select! {
            recv(stop) -> _ => break,
            recv(packets) -> packet => match packet {
                Ok(packet) => {
                    sound.store(packet.samples.iter().any(|&s| s != 0.0), Ordering::Relaxed);
                    if let Some((_, to_headset)) = &playback {
                        let _ = to_headset.send(packet.samples.clone());
                    }
                    session.audio(&packet)?;
                }
                Err(_) => {
                    capture = None;
                    let _ = events.send(RecorderEvent::CaptureLost);
                }
            },
            recv(monitor_events) -> event => {
                if let Ok(event) = event {
                    session.monitor(event)?;
                }
            },
            default(CAPTURE_RETRY) => {}
        }
    }

    monitor.stop();
    drop(playback);
    if let Some(capture) = capture {
        let _ = capture.capture.stop();
    }
    let actions = session.splitter.finish();
    session.apply(actions)?;
    session.set_ad_muted(false);
    drop(session);
    let _ = encoder.join();
    Ok(())
}

struct Writer {
    title: TrackTitle,
    uri: Option<String>,
    wav: CaptureWav,
    path: PathBuf,
    analysis: BitAnalysis,
}

struct Job {
    ended: Ended,
    wav: PathBuf,
    analysis: BitAnalysis,
    /// The catalogue entry played, for exact tags.
    uri: Option<String>,
}

/// What Spotify plays, as its command-line tool reports it, every
/// [`WATCH_INTERVAL`]: gives each recording the exact catalogue entry
/// (search cannot tell a song's releases apart, and some are not found at
/// all: the album version of 阿修羅ちゃん, 2026-10-04).
struct PlayingWatch {
    latest: Arc<Mutex<Option<NowPlaying>>>,
    stop: Arc<AtomicBool>,
}

const WATCH_INTERVAL: Duration = Duration::from_secs(2);

impl PlayingWatch {
    /// `None` without Spotify's command-line tool.
    fn spawn() -> Option<Self> {
        let cli = SpotifyCli::find().ok()?;
        let latest = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_latest, thread_stop) = (Arc::clone(&latest), Arc::clone(&stop));
        thread::Builder::new()
            .name("resona-catalogue-watch".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    if let Ok(playing) = cli.now_playing()
                        && let Ok(mut latest) = thread_latest.lock()
                    {
                        *latest = playing;
                    }
                    thread::sleep(WATCH_INTERVAL);
                }
            })
            .ok()?;
        Some(Self { latest, stop })
    }

    /// The entry playing, if it is `title` (the watch may lag a track).
    fn uri_for(&self, title: &TrackTitle) -> Option<String> {
        let latest = self.latest.lock().ok()?;
        let playing = latest.as_ref()?;
        // `Title — Artist, Artist`.
        let played = playing.description.split(" — ").next().unwrap_or_default();
        let same = [title.full_title(), title.title.clone()]
            .iter()
            .any(|t| title_similarity(played, t) >= 0.85);
        same.then(|| playing.uri.clone())
    }
}

impl Drop for PlayingWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Session {
    splitter: Splitter,
    clock: FrameClock,
    writer: Option<Writer>,
    /// The current track is already recorded and is not written.
    skipping: Option<TrackTitle>,
    temp_dir: PathBuf,
    jobs: Sender<Job>,
    events: Sender<RecorderEvent>,
    config: RecorderConfig,
    library: Arc<Mutex<Library>>,
    /// Opened on first use, to ask Spotify for the next track.
    smtc: Option<Smtc>,
    ad_muted: bool,
    /// Exact catalogue entries for the tags.
    playing: Option<PlayingWatch>,
}

impl Session {
    fn audio(&mut self, packet: &Packet) -> Result<()> {
        let start = self.splitter.end_frame();
        let end = start + (packet.samples.len() / 2) as u64;
        self.clock.record(packet.received_at, start, end);
        let actions = self.splitter.push_audio(&packet.samples);
        self.apply(actions)
    }

    fn monitor(&mut self, event: MonitorEvent) -> Result<()> {
        let frame_at = |at: Instant| self.clock.frame_at(at);
        match event {
            MonitorEvent::Status { at, event } => {
                let frame = at
                    .checked_sub(TITLE_LAG)
                    .and_then(frame_at)
                    .unwrap_or_else(|| self.splitter.end_frame());
                if let Event::ContentChanged { current, .. } = &event
                    && self.config.mute_ads
                {
                    // Muted as soon as the ad is noticed, for the listener;
                    // the recording does not depend on it.
                    self.set_ad_muted(matches!(current, Content::Ad));
                }
                let change = match &event {
                    Event::ContentChanged { current, .. } => Some(Change::Content(current.clone())),
                    Event::PlayStateChanged { playing } => Some(Change::Playing(*playing)),
                    Event::DetailsUpdated(track) => Some(Change::Details(track.clone())),
                    Event::RunningChanged { .. } => None,
                };
                let _ = self.events.send(RecorderEvent::Spotify(event));
                if let Some(change) = change {
                    self.splitter.change(frame, change);
                }
            }
            MonitorEvent::TrackStart { title, started_at } => {
                let frame = frame_at(started_at);
                let end = self.splitter.end_frame();
                self.splitter
                    .change(end, Change::TrackStart { title, frame });
            }
        }
        Ok(())
    }

    fn apply(&mut self, actions: Vec<Action>) -> Result<()> {
        for action in actions {
            match action {
                Action::Begin { track } => {
                    if self.config.existing == ExistingTracks::Skip
                        && let Some(existing) = self.already_recorded(&track.title)
                    {
                        let skipped_in_spotify =
                            self.config.skip_existing_in_spotify && self.skip_in_spotify();
                        let _ = self.events.send(RecorderEvent::AlreadyRecorded {
                            title: track.title.clone(),
                            existing,
                            skipped_in_spotify,
                        });
                        self.skipping = Some(track.title);
                        continue;
                    }
                    let path = self.temp_dir.join(format!("{}.wav", unique_stamp()));
                    let _ = self
                        .events
                        .send(RecorderEvent::Recording(track.title.clone()));
                    self.writer = Some(Writer {
                        uri: self.playing.as_ref().and_then(|w| w.uri_for(&track.title)),
                        title: track.title,
                        wav: CaptureWav::create(&path)?,
                        path,
                        analysis: BitAnalysis::default(),
                    });
                }
                Action::Write(samples) => {
                    if let Some(writer) = self.writer.as_mut() {
                        writer.analysis.feed(&samples);
                        writer.wav.write(&samples)?;
                    }
                }
                Action::End(ended) => {
                    if self.skipping.take().is_some() {
                        continue;
                    }
                    if let Some(writer) = self.writer.take() {
                        debug_assert_eq!(writer.title, ended.track.title);
                        writer.wav.finalize()?;
                        let _ = self.jobs.send(Job {
                            ended,
                            wav: writer.path,
                            analysis: writer.analysis,
                            uri: writer.uri,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

impl Session {
    fn already_recorded(&self, title: &TrackTitle) -> Option<PathBuf> {
        let library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        library.find(title).map(PathBuf::from)
    }

    /// Asks Spotify for the next track; `false` if it could not be asked.
    fn skip_in_spotify(&mut self) -> bool {
        if self.smtc.is_none() {
            self.smtc = Smtc::new()
                .inspect_err(|e| tracing::warn!("SMTC unavailable, cannot skip: {e}"))
                .ok();
        }
        self.smtc
            .as_ref()
            .and_then(|smtc| smtc.skip_spotify().ok())
            .unwrap_or(false)
    }

    fn set_ad_muted(&mut self, muted: bool) {
        if muted == self.ad_muted {
            return;
        }
        let Some(processes) = SpotifyProcesses::find() else {
            return;
        };
        match audio_setup::set_spotify_muted(&processes.all, muted) {
            Ok(()) => {
                self.ad_muted = muted;
                let _ = self.events.send(RecorderEvent::AdMuted(muted));
            }
            Err(e) => tracing::warn!("cannot mute Spotify: {e}"),
        }
    }
}

fn unique_stamp() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("track-{nanos}")
}

/// Where tags come from.
struct Catalogues {
    deezer: Option<DeezerClient>,
    spotify: Option<SpotifyCli>,
}

fn encode_jobs(
    config: &RecorderConfig,
    library: &Mutex<Library>,
    jobs: &Receiver<Job>,
    events: &Sender<RecorderEvent>,
) {
    let catalogues = Catalogues {
        deezer: DeezerClient::new()
            .inspect_err(|e| tracing::warn!("no Deezer client: {e}"))
            .ok(),
        spotify: SpotifyCli::find()
            .inspect_err(|e| tracing::warn!("no Spotify catalogue: {e}"))
            .ok(),
    };
    // Order of the tracks saved in this session, for `Prefix::OrderNumber`.
    let mut order = 0;
    for job in jobs {
        if job.analysis.fades_in() {
            let _ = events.send(RecorderEvent::SpotifyFades(job.ended.track.title.clone()));
        }
        let event = match encode(config, &catalogues, library, order + 1, &job) {
            Ok(event) => event,
            Err(e) => RecorderEvent::Error(format!("{}: {e}", job.ended.track.title)),
        };
        if matches!(event, RecorderEvent::Saved { .. }) {
            order += 1;
        }
        let _ = std::fs::remove_file(&job.wav);
        let _ = events.send(event);
    }
}

fn encode(
    config: &RecorderConfig,
    catalogues: &Catalogues,
    library: &Mutex<Library>,
    order: u32,
    job: &Job,
) -> Result<RecorderEvent> {
    let title = job.ended.track.title.clone();
    let duration = job.ended.duration(CAPTURE_SAMPLE_RATE);
    let fidelity = job.analysis.fidelity();
    let discard = if duration < config.min_duration {
        Some(DiscardReason::TooShort)
    } else if !config.keep_partial && job.ended.is_partial(CAPTURE_SAMPLE_RATE, PARTIAL_TOLERANCE) {
        Some(DiscardReason::Partial)
    } else {
        None
    };
    if let Some(reason) = discard {
        return Ok(RecorderEvent::Discarded {
            title,
            reason,
            fidelity,
            duration,
        });
    }
    // Encoded and tagged next to the WAV, then moved into place: the
    // output folder never shows a half-written file.
    let format = config.format;
    // Not `with_extension`: for a WAV output that would be the capture
    // file itself, truncated while it is being read.
    let staged = job
        .wav
        .with_extension(format!("out.{}", format.extension()));
    let source_bits = fidelity.lossless_depth();
    match format {
        OutputFormat::Flac { depth } => {
            let quantizer = Quantizer::new(depth.resolve(source_bits), source_bits);
            flac::encode_wav_to_flac(&job.wav, &staged, quantizer)?;
        }
        OutputFormat::Wav { depth } => {
            let quantizer = Quantizer::new(depth.resolve(source_bits), source_bits);
            wav::export_wav(&job.wav, &staged, quantizer)?;
        }
        OutputFormat::Mp3 { kbps } => mp3::encode_wav_to_mp3(&job.wav, &staged, kbps)?,
    }
    let (tags, outcome) = metadata::tags_for(
        catalogues.deezer.as_ref(),
        catalogues.spotify.as_ref(),
        &job.ended.track,
        job.uri.as_deref(),
    );
    let tagged = tags.write(&staged, format);

    // Filed with the tags just found: the album artist and album name
    // from Deezer group an album better than Spotify's artist list.
    let relative = naming::relative_path(
        config.layout,
        &naming::Placement {
            title: &title,
            album_artist: tags.album_artist.as_deref(),
            album: tags.album.as_deref(),
            track_number: tags.track_number,
            order,
        },
        format.extension(),
    );
    let mut library = library.lock().unwrap_or_else(|e| e.into_inner());
    let existing = library.find(&title).map(PathBuf::from);
    let path = match (config.existing, existing) {
        (ExistingTracks::Skip, Some(_)) => {
            let _ = std::fs::remove_file(&staged);
            return Ok(RecorderEvent::Discarded {
                title,
                reason: DiscardReason::AlreadyRecorded,
                fidelity,
                duration,
            });
        }
        (ExistingTracks::Replace, Some(old)) => {
            std::fs::remove_file(&old)?;
            library.remove(&old);
            config.output_dir.join(&relative)
        }
        _ => {
            let target = config.output_dir.join(&relative);
            let dir = target.parent().unwrap_or(&config.output_dir).to_path_buf();
            let name = target.file_name().unwrap_or_default().to_string_lossy();
            naming::unique_path(&dir, &name)
        }
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::rename(&staged, &path)?;
    library.add(path.clone());
    drop(library);
    tagged?;
    Ok(RecorderEvent::Saved {
        title,
        path,
        fidelity,
        duration,
        tags: outcome,
    })
}

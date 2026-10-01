//! The recording session: capture + monitor in, one FLAC per track out.
//!
//! Three threads besides the caller's: the capture thread (WASAPI), the
//! monitor thread (Spotify state), and this engine's own, which owns the
//! [`Splitter`] and the temporary WAV of the track being recorded. Encoding
//! runs on a fourth thread so a slow FLAC never holds up the capture.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};

use super::clock::FrameClock;
use super::naming;
use super::splitter::{Action, Change, Ended, Splitter};
use crate::analysis::{BitAnalysis, Fidelity};
use crate::audio_setup::{self, OutputDevice};
use crate::capture::{CaptureConfig, Packet, ProcessCapture};
use crate::encode::{Quantizer, flac, mp3, wav, wav::CaptureWav};
use crate::format::CAPTURE_SAMPLE_RATE;
use crate::format::OutputFormat;
use crate::metadata::deezer::DeezerClient;
use crate::metadata::tags::TrackTags;
use crate::metadata::{self, TagsOutcome};
use crate::spotify::monitor::{Monitor, MonitorEvent};
use crate::spotify::process::SpotifyProcesses;
use crate::spotify::state::Event;
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
const TEMP_DIR: &str = ".spytify-tmp";

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
}

#[derive(Debug, Clone)]
pub enum RecorderEvent {
    CaptureStarted {
        pid: u32,
        /// The output device as found when the capture started, with
        /// [`OutputDevice::lossless_issues`] to warn about.
        device: OutputDevice,
    },
    CaptureLost,
    Recording(TrackTitle),
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
            .name("spytify-recorder".into())
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

fn start_capture() -> Result<Capture> {
    let processes = SpotifyProcesses::find().ok_or(Error::SpotifyNotRunning)?;
    let device = audio_setup::default_output_device()?;
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

fn run(config: &RecorderConfig, events: &Sender<RecorderEvent>, stop: &Receiver<()>) -> Result<()> {
    let _ = wasapi::initialize_mta();
    let sound = Arc::new(AtomicBool::new(false));
    let monitor_sound = Arc::clone(&sound);
    let (monitor, monitor_events) = Monitor::spawn(move || monitor_sound.load(Ordering::Relaxed))?;
    let (jobs_tx, jobs_rx) = crossbeam_channel::unbounded::<Job>();
    let encoder = {
        let events = events.clone();
        let config = config.clone();
        thread::Builder::new()
            .name("spytify-encoder".into())
            .spawn(move || encode_jobs(&config, &jobs_rx, &events))?
    };

    let mut session = Session {
        splitter: Splitter::new(CAPTURE_SAMPLE_RATE),
        clock: FrameClock::new(CAPTURE_SAMPLE_RATE),
        writer: None,
        temp_dir: config.output_dir.join(TEMP_DIR),
        jobs: jobs_tx,
        events: events.clone(),
    };
    let mut capture: Option<Capture> = None;
    let mut last_attempt: Option<Instant> = None;

    loop {
        if capture.is_none() && last_attempt.is_none_or(|t| t.elapsed() >= CAPTURE_RETRY) {
            last_attempt = Some(Instant::now());
            if let Ok(started) = start_capture() {
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
    if let Some(capture) = capture {
        let _ = capture.capture.stop();
    }
    let actions = session.splitter.finish();
    session.apply(actions)?;
    drop(session);
    let _ = encoder.join();
    Ok(())
}

struct Writer {
    title: TrackTitle,
    wav: CaptureWav,
    path: PathBuf,
    analysis: BitAnalysis,
}

struct Job {
    ended: Ended,
    wav: PathBuf,
    analysis: BitAnalysis,
}

struct Session {
    splitter: Splitter,
    clock: FrameClock,
    writer: Option<Writer>,
    temp_dir: PathBuf,
    jobs: Sender<Job>,
    events: Sender<RecorderEvent>,
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
                let change = match &event {
                    Event::ContentChanged { current, .. } => Some(Change::Content(current.clone())),
                    Event::PlayStateChanged { playing } => Some(Change::Playing(*playing)),
                    Event::DetailsUpdated(track) => Some(Change::Details(track.clone())),
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
                    let path = self.temp_dir.join(format!("{}.wav", unique_stamp()));
                    let _ = self
                        .events
                        .send(RecorderEvent::Recording(track.title.clone()));
                    self.writer = Some(Writer {
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
                    if let Some(writer) = self.writer.take() {
                        debug_assert_eq!(writer.title, ended.track.title);
                        writer.wav.finalize()?;
                        let _ = self.jobs.send(Job {
                            ended,
                            wav: writer.path,
                            analysis: writer.analysis,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

fn unique_stamp() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("track-{nanos}")
}

fn encode_jobs(config: &RecorderConfig, jobs: &Receiver<Job>, events: &Sender<RecorderEvent>) {
    let deezer = DeezerClient::new()
        .inspect_err(|e| tracing::warn!("no Deezer client, Spotify tags only: {e}"))
        .ok();
    for job in jobs {
        let event = match encode(config, deezer.as_ref(), &job) {
            Ok(event) => event,
            Err(e) => RecorderEvent::Error(format!("{}: {e}", job.ended.track.title)),
        };
        let _ = std::fs::remove_file(&job.wav);
        let _ = events.send(event);
    }
}

fn encode(
    config: &RecorderConfig,
    deezer: Option<&DeezerClient>,
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
    let staged = job.wav.with_extension(format.extension());
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
    let (tags, outcome) = match deezer {
        Some(client) => metadata::tags_for(client, &job.ended.track),
        None => (
            TrackTags::from_spotify(&job.ended.track),
            TagsOutcome::Unavailable("no HTTP client".into()),
        ),
    };
    let tagged = tags.write(&staged, format);
    let path = naming::unique_path(
        &config.output_dir,
        &naming::file_name(&title, format.extension()),
    );
    std::fs::rename(&staged, &path)?;
    tagged?;
    Ok(RecorderEvent::Saved {
        title,
        path,
        fidelity,
        duration,
        tags: outcome,
    })
}

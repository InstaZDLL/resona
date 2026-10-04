//! The window and its wiring to the engine.
//!
//! Engine threads never touch the UI: their events are forwarded to the
//! Slint event loop (`upgrade_in_event_loop`), and the UI keeps the only
//! copy of what it shows.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};
use spytify_core::analysis::Fidelity;
use spytify_core::audio_setup::{CableAsDefault, SetupIssue};
use spytify_core::metadata::TagsOutcome;
use spytify_core::recorder::engine::{DiscardReason, Recorder, RecorderEvent};
use spytify_core::recorder::playlist_session::{
    Entry, EntryState, PlaylistEvent, PlaylistSession, Source,
};
use spytify_core::settings::Settings;
use spytify_core::spotify::link::SpotifyLink;
use spytify_core::spotify::monitor::{Monitor, MonitorEvent};
use spytify_core::spotify::state::{Content, Event};
use spytify_core::spotify::title::TrackTitle;

use crate::cover::CoverLoader;
use crate::mapping::{self, at, index_of};
use crate::tray::Tray;
use crate::{AppWindow, Notice, Quality, Reason, SpotifyState, TrackRow, TrackState, Warning};

pub fn run() -> anyhow::Result<()> {
    // lofty warns on every FLAC it tags that it adds a padding block: our
    // encoder writes none, which is fine.
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::filter::Targets::new()
                .with_default(tracing::Level::WARN)
                .with_target("lofty", tracing::Level::ERROR),
        )
        .init();
    let settings = Rc::new(RefCell::new(Settings::load()));
    let window = AppWindow::new()?;
    select_language(settings.borrow().language);
    show_settings(&window, &settings.borrow());

    let tracks = Rc::new(VecModel::<TrackRow>::default());
    window.set_tracks(ModelRc::from(tracks.clone()));
    let notices = Rc::new(VecModel::<Notice>::default());
    window.set_notices(ModelRc::from(notices.clone()));

    // Spotify's state is shown whether recording or not.
    let (monitor, monitor_events) = Monitor::spawn(|| false)?;
    // The cover art follows the track the monitor reports.
    let cover = CoverLoader::spawn(window.as_weak());
    cover.refresh();
    let (shown_tx, shown_rx) = crossbeam_channel::unbounded();
    thread::spawn(move || {
        for event in monitor_events {
            if matches!(
                event,
                MonitorEvent::Status {
                    event: Event::ContentChanged { .. }
                        | Event::DetailsUpdated(_)
                        | Event::RunningChanged { .. },
                    ..
                }
            ) {
                cover.refresh();
            }
            if shown_tx.send(event).is_err() {
                break;
            }
        }
    });
    forward(shown_rx, window.as_weak(), on_monitor_event);

    let running: Rc<RefCell<Option<Running>>> = Rc::default();
    let started: Rc<Cell<Option<Instant>>> = Rc::default();

    let timer = slint::Timer::default();
    {
        let weak = window.as_weak();
        let started = Rc::clone(&started);
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                if let (Some(window), Some(since)) = (weak.upgrade(), started.get()) {
                    window.set_elapsed(mapping::clock(since.elapsed()).into());
                }
            },
        );
    }
    let session = Session {
        settings: Rc::clone(&settings),
        running: Rc::clone(&running),
        started: Rc::clone(&started),
        notices: Rc::clone(&notices),
    };

    window.on_toggle_recording({
        let weak = window.as_weak();
        let session = session.clone();
        move || {
            if let Some(window) = weak.upgrade() {
                if window.get_recording() {
                    session.stop(&window);
                } else {
                    session.start(&window, None);
                }
            }
        }
    });

    window.on_record_playlist({
        let weak = window.as_weak();
        let session = session.clone();
        move || {
            let Some(window) = weak.upgrade() else { return };
            if window.get_recording() {
                return;
            }
            match SpotifyLink::parse(&window.get_playlist_link()) {
                Ok(link) => session.start(&window, Some(Source::Link(link))),
                Err(e) => session.notices.push(Notice {
                    kind: Warning::Error,
                    detail: e.to_string().into(),
                    ..Notice::default()
                }),
            }
        }
    });

    window.on_retry_missed({
        let weak = window.as_weak();
        let session = session.clone();
        move || {
            let Some(window) = weak.upgrade() else { return };
            let (name, entries) = MISSED.with_borrow(Clone::clone);
            if !window.get_recording() && !entries.is_empty() {
                session.start(&window, Some(Source::Entries { name, entries }));
            }
        }
    });

    window.on_settings_changed({
        let weak = window.as_weak();
        let settings = Rc::clone(&settings);
        move || {
            if let Some(window) = weak.upgrade() {
                let mut settings = settings.borrow_mut();
                read_settings(&window, &mut settings);
                save(&settings);
            }
        }
    });

    window.on_quality_changed({
        let weak = window.as_weak();
        let settings = Rc::clone(&settings);
        move |index| {
            let Some(window) = weak.upgrade() else { return };
            // The natural format for the tier, which the user may still
            // change afterwards.
            let quality = at(&mapping::QUALITIES, index);
            let format = spytify_core::format::SpotifyQuality::from(quality).default_output();
            window.set_format_index(index_of(&mapping::FORMATS, &format));
            let mut settings = settings.borrow_mut();
            read_settings(&window, &mut settings);
            save(&settings);
        }
    });

    window.on_language_changed({
        let settings = Rc::clone(&settings);
        move |index| {
            let mut settings = settings.borrow_mut();
            settings.language = at(&mapping::LANGUAGES, index);
            select_language(settings.language);
            save(&settings);
        }
    });

    window.on_browse_output({
        let weak = window.as_weak();
        let settings = Rc::clone(&settings);
        move || {
            let Some(window) = weak.upgrade() else { return };
            let current = settings.borrow().output_dir.clone();
            if let Some(folder) = rfd::FileDialog::new().set_directory(&current).pick_folder() {
                window.set_output_dir(folder.display().to_string().into());
                let mut settings = settings.borrow_mut();
                settings.output_dir = folder;
                save(&settings);
            }
        }
    });

    window.on_open_output({
        let settings = Rc::clone(&settings);
        move || {
            let dir = settings.borrow().output_dir.clone();
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::process::Command::new("explorer").arg(dir).spawn();
        }
    });

    window.on_open_cable_site(|| {
        let _ = std::process::Command::new("explorer")
            .arg("https://vb-audio.com/Cable/")
            .spawn();
    });

    // Looked up off the UI thread, which is not in COM's MTA.
    {
        let weak = window.as_weak();
        thread::spawn(move || {
            let _ = wasapi::initialize_mta();
            let cable = spytify_core::audio_setup::virtual_cable()
                .ok()
                .flatten()
                .map(|(_, name)| name)
                .unwrap_or_default();
            let as_default = spytify_core::audio_setup::cable_as_default().ok().flatten();
            let _ = weak.upgrade_in_event_loop(move |w| {
                w.set_cable_name(cable.into());
                if let Some(as_default) = as_default {
                    show_cable_as_default(&w, &as_default);
                }
            });
        });
    }

    window.on_restore_default_device({
        let weak = window.as_weak();
        move |id| {
            let weak = weak.clone();
            thread::spawn(move || {
                let _ = wasapi::initialize_mta();
                let result = spytify_core::device_config::set_default_device(&id);
                let _ = weak.upgrade_in_event_loop(move |w| {
                    let notices = w.get_notices();
                    let Some(notices) = notices.as_any().downcast_ref::<VecModel<Notice>>() else {
                        return;
                    };
                    let mut kept: Vec<Notice> = notices
                        .iter()
                        .filter(|n| n.kind != Warning::CableDefault)
                        .collect();
                    if let Err(e) = result {
                        kept.push(Notice {
                            kind: Warning::Error,
                            detail: e.to_string().into(),
                            ..Notice::default()
                        });
                    }
                    notices.set_vec(kept);
                });
            });
        }
    });

    // Closing the window during a recording sends it to the notification
    // area; otherwise it quits.
    let tray = match Tray::new(&window.as_weak()) {
        Ok(tray) => Some(Rc::new(RefCell::new(tray))),
        Err(e) => {
            tracing::warn!("no notification area icon: {e}");
            None
        }
    };
    window.window().on_close_requested({
        let weak = window.as_weak();
        let in_tray = tray.is_some();
        move || {
            let recording = weak.upgrade().is_some_and(|w| w.get_recording());
            if !(recording && in_tray) {
                let _ = slint::quit_event_loop();
            }
            slint::CloseRequestResponse::HideWindow
        }
    });
    let tray_timer = slint::Timer::default();
    if let Some(tray) = &tray {
        let weak = window.as_weak();
        let settings = Rc::clone(&settings);
        let tray = Rc::clone(tray);
        tray_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(250),
            move || {
                if let Some(window) = weak.upgrade() {
                    tray.borrow_mut()
                        .update(window.get_recording(), settings.borrow().language);
                }
            },
        );
    }

    window.show()?;
    slint::run_event_loop_until_quit()?;

    // Quitting while recording keeps what was recorded.
    if let Some(running) = running.borrow_mut().take() {
        running.stop();
    }
    monitor.stop();
    Ok(())
}

/// What the record button stops.
enum Running {
    Free(Recorder),
    Playlist(PlaylistSession),
}

impl Running {
    fn stop(self) {
        match self {
            Self::Free(recorder) => recorder.stop(),
            Self::Playlist(session) => session.stop(),
        }
    }
}

thread_local! {
    /// The list entry the latest playlist event was about: the recorder
    /// event that follows it (saved, discarded) belongs to that row.
    static LAST_ENTRY: Cell<Option<usize>> = const { Cell::new(None) };
    /// The entries the last playlist run missed, for "retry".
    static MISSED: RefCell<(String, Vec<Entry>)> = RefCell::default();
}

/// Starting and stopping a recording, free or of a playlist.
#[derive(Clone)]
struct Session {
    settings: Rc<RefCell<Settings>>,
    running: Rc<RefCell<Option<Running>>>,
    started: Rc<Cell<Option<Instant>>>,
    notices: Rc<VecModel<Notice>>,
}

impl Session {
    /// Records everything Spotify plays, or the playlist `source`.
    fn start(&self, window: &AppWindow, source: Option<Source>) {
        // A playlist run that ended on its own is still held here.
        if let Some(finished) = self.running.borrow_mut().take() {
            thread::spawn(move || finished.stop());
        }
        self.notices.set_vec(Vec::new());
        window.set_saved_count(0);
        window.set_missed_count(0);
        window.set_playlist_name(SharedString::new());
        let config = self.settings.borrow().recorder_config();
        let started = match source {
            None => Recorder::start(config).map(|(recorder, events)| {
                forward(events, window.as_weak(), on_recorder_event);
                Running::Free(recorder)
            }),
            Some(source) => PlaylistSession::start(config, source).map(|(session, events)| {
                LAST_ENTRY.set(None);
                window.set_playlist_running(true);
                forward(events, window.as_weak(), on_playlist_event);
                Running::Playlist(session)
            }),
        };
        match started {
            Ok(running) => {
                *self.running.borrow_mut() = Some(running);
                self.started.set(Some(Instant::now()));
                window.set_elapsed(mapping::clock(Duration::ZERO).into());
                window.set_recording(true);
            }
            Err(e) => self.notices.push(Notice {
                kind: Warning::Error,
                detail: e.to_string().into(),
                ..Notice::default()
            }),
        }
    }

    /// Stopping waits for the last track to be encoded and tagged: off the
    /// UI thread.
    fn stop(&self, window: &AppWindow) {
        let Some(running) = self.running.borrow_mut().take() else {
            window.set_recording(false);
            return;
        };
        window.set_finishing(true);
        self.started.set(None);
        let weak = window.as_weak();
        thread::spawn(move || {
            running.stop();
            let _ = weak.upgrade_in_event_loop(|w| {
                w.set_finishing(false);
                w.set_recording(false);
                w.set_playlist_running(false);
            });
        });
    }
}

fn on_playlist_event(window: &AppWindow, event: PlaylistEvent) {
    let tracks = window.get_tracks();
    let Some(tracks) = tracks.as_any().downcast_ref::<VecModel<TrackRow>>() else {
        return;
    };
    match event {
        PlaylistEvent::Loaded { name, entries } => {
            window.set_playlist_name(name.into());
            window.set_playlist_total(entries.len() as i32);
            window.set_playlist_done(0);
            tracks.set_vec(
                entries
                    .iter()
                    .map(|entry| TrackRow {
                        title: format!("{} - {}", entry.artists.join(", "), entry.title).into(),
                        length: entry
                            .duration
                            .map(mapping::length)
                            .unwrap_or_default()
                            .into(),
                        state: TrackState::Waiting,
                        ..TrackRow::default()
                    })
                    .collect::<Vec<_>>(),
            );
        }
        PlaylistEvent::Entry { index, state } => {
            LAST_ENTRY.set(Some(index));
            if let Some(mut row) = tracks.row_data(index) {
                row.state = match state {
                    EntryState::Waiting => TrackState::Waiting,
                    EntryState::Recording => TrackState::Recording,
                    EntryState::Saved => TrackState::Saved,
                    EntryState::AlreadyRecorded => TrackState::AlreadyRecorded,
                    EntryState::Missed => TrackState::Failed,
                };
                tracks.set_row_data(index, row);
            }
            let done = tracks
                .iter()
                .filter(|r| matches!(r.state, TrackState::Saved | TrackState::AlreadyRecorded))
                .count();
            window.set_playlist_done(done as i32);
        }
        PlaylistEvent::Recorder(event) => {
            let row = LAST_ENTRY
                .get()
                .and_then(|i| Some((i, tracks.row_data(i)?)));
            match (*event, row) {
                (
                    RecorderEvent::Saved {
                        fidelity,
                        duration,
                        tags,
                        path,
                        ..
                    },
                    Some((index, mut row)),
                ) if row.state == TrackState::Saved => {
                    describe(&mut row, fidelity, duration);
                    row.tagged = tags == TagsOutcome::Deezer;
                    row.format = extension(&path).into();
                    tracks.set_row_data(index, row);
                    window.set_saved_count(window.get_saved_count() + 1);
                }
                (
                    RecorderEvent::Discarded {
                        reason,
                        fidelity,
                        duration,
                        ..
                    },
                    Some((index, mut row)),
                ) if row.state == TrackState::Failed => {
                    describe(&mut row, fidelity, duration);
                    row.state = TrackState::Discarded;
                    row.reason = reason_of(reason);
                    tracks.set_row_data(index, row);
                }
                // The list rows stand for these.
                (
                    RecorderEvent::Recording(_)
                    | RecorderEvent::AlreadyRecorded { .. }
                    | RecorderEvent::Saved { .. }
                    | RecorderEvent::Discarded { .. },
                    _,
                ) => {}
                (event, _) => on_recorder_event(window, event),
            }
        }
        PlaylistEvent::Error(message) => {
            let notices = window.get_notices();
            if let Some(notices) = notices.as_any().downcast_ref::<VecModel<Notice>>() {
                notices.push(Notice {
                    kind: Warning::Error,
                    detail: message.into(),
                    ..Notice::default()
                });
            }
        }
        PlaylistEvent::Finished { name, missed } => {
            window.set_missed_count(missed.len() as i32);
            MISSED.set((name, missed));
            window.set_recording(false);
            window.set_playlist_running(false);
        }
    }
}

fn extension(path: &std::path::Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_uppercase())
        .unwrap_or_default()
}

fn reason_of(reason: DiscardReason) -> Reason {
    match reason {
        DiscardReason::Partial => Reason::Partial,
        DiscardReason::TooShort => Reason::TooShort,
        DiscardReason::AlreadyRecorded => Reason::AlreadyRecorded,
    }
}

fn select_language(language: spytify_core::settings::Language) {
    if let Err(e) = slint::select_bundled_translation(mapping::language_code(language)) {
        tracing::warn!("translation: {e}");
    }
}

fn save(settings: &Settings) {
    if let Err(e) = settings.save() {
        tracing::warn!("settings not saved: {e}");
    }
}

fn show_settings(window: &AppWindow, settings: &Settings) {
    window.set_output_dir(settings.output_dir.display().to_string().into());
    window.set_quality_index(index_of(&mapping::QUALITIES, &settings.spotify_quality));
    window.set_format_index(index_of(&mapping::FORMATS, &settings.format));
    window.set_min_duration(settings.min_duration_secs.min(600) as i32);
    window.set_keep_partial(settings.keep_partial);
    window.set_folders_index(index_of(&mapping::FOLDERS, &settings.layout.folders));
    window.set_prefix_index(index_of(&mapping::PREFIXES, &settings.layout.prefix));
    window.set_existing_index(index_of(&mapping::EXISTING, &settings.existing));
    window.set_skip_in_spotify(settings.skip_existing_in_spotify);
    window.set_mute_ads(settings.mute_ads);
    window.set_virtual_cable(settings.virtual_cable);
    window.set_listen(settings.listen);
    window.set_language_index(index_of(&mapping::LANGUAGES, &settings.language));
}

fn read_settings(window: &AppWindow, settings: &mut Settings) {
    settings.spotify_quality = at(&mapping::QUALITIES, window.get_quality_index());
    settings.format = at(&mapping::FORMATS, window.get_format_index());
    settings.min_duration_secs = window.get_min_duration().max(0) as u64;
    settings.keep_partial = window.get_keep_partial();
    settings.layout.folders = at(&mapping::FOLDERS, window.get_folders_index());
    settings.layout.prefix = at(&mapping::PREFIXES, window.get_prefix_index());
    settings.existing = at(&mapping::EXISTING, window.get_existing_index());
    settings.skip_existing_in_spotify = window.get_skip_in_spotify();
    settings.mute_ads = window.get_mute_ads();
    settings.virtual_cable = window.get_virtual_cable();
    settings.listen = window.get_listen();
}

/// Hands every event of `events` to `handle` on the UI thread, until the
/// sender is dropped.
fn forward<E: Send + 'static>(
    events: crossbeam_channel::Receiver<E>,
    window: Weak<AppWindow>,
    handle: fn(&AppWindow, E),
) {
    thread::spawn(move || {
        for event in events {
            if window
                .upgrade_in_event_loop(move |w| handle(&w, event))
                .is_err()
            {
                break;
            }
        }
    });
}

fn on_monitor_event(window: &AppWindow, event: MonitorEvent) {
    let MonitorEvent::Status { event, .. } = event else {
        return;
    };
    match event {
        Event::RunningChanged { running } => {
            if !running {
                window.set_spotify_state(SpotifyState::NotRunning);
                show_now_playing(window, None, None);
            }
        }
        Event::ContentChanged { current, .. } => match current {
            Content::Track(track) => {
                show_now_playing(window, Some(&track.title), track.details.album.as_deref());
                window.set_spotify_state(SpotifyState::Playing);
            }
            Content::Ad => {
                show_now_playing(window, None, None);
                window.set_spotify_state(SpotifyState::Ad);
            }
            Content::Nothing => {
                show_now_playing(window, None, None);
                window.set_spotify_state(SpotifyState::Idle);
            }
        },
        Event::PlayStateChanged { playing } => {
            let state = window.get_spotify_state();
            if state == SpotifyState::Ad || state == SpotifyState::NotRunning {
                return;
            }
            window.set_spotify_state(if playing {
                SpotifyState::Playing
            } else if window.get_now_title().is_empty() {
                SpotifyState::Idle
            } else {
                SpotifyState::Paused
            });
        }
        Event::DetailsUpdated(track) => {
            window.set_now_album(track.details.album.unwrap_or_default().into());
        }
    }
}

fn show_now_playing(window: &AppWindow, title: Option<&TrackTitle>, album: Option<&str>) {
    window.set_now_title(title.map(TrackTitle::full_title).unwrap_or_default().into());
    window.set_now_artist(title.map(|t| t.artist.clone()).unwrap_or_default().into());
    window.set_now_album(album.unwrap_or_default().into());
}

fn on_recorder_event(window: &AppWindow, event: RecorderEvent) {
    let tracks = window.get_tracks();
    let Some(tracks) = tracks.as_any().downcast_ref::<VecModel<TrackRow>>() else {
        return;
    };
    let notices = window.get_notices();
    let Some(notices) = notices.as_any().downcast_ref::<VecModel<Notice>>() else {
        return;
    };
    match event {
        RecorderEvent::CaptureStarted { device, .. } => {
            // The device may have changed since the last capture (Spotify
            // moved to the cable): its warnings are redone, others kept.
            let mut kept: Vec<Notice> = notices
                .iter()
                .filter(|n| !matches!(n.kind, Warning::Enhancements | Warning::SampleRate))
                .collect();
            let name: SharedString = device.name.as_str().into();
            for issue in device.lossless_issues() {
                kept.push(match issue {
                    SetupIssue::Enhancements => Notice {
                        kind: Warning::Enhancements,
                        device: name.clone(),
                        detail: SharedString::new(),
                    },
                    SetupIssue::SampleRate(rate) => Notice {
                        kind: Warning::SampleRate,
                        device: name.clone(),
                        detail: rate.to_string().into(),
                    },
                });
            }
            notices.set_vec(kept);
        }
        RecorderEvent::Recording(title) => {
            tracks.push(row(&title, TrackState::Recording));
        }
        RecorderEvent::AlreadyRecorded { title, .. } => {
            tracks.push(row(&title, TrackState::AlreadyRecorded));
        }
        RecorderEvent::Saved {
            title,
            path,
            fidelity,
            duration,
            tags,
        } => {
            let mut saved = row(&title, TrackState::Saved);
            describe(&mut saved, fidelity, duration);
            saved.tagged = tags == TagsOutcome::Deezer;
            saved.format = extension(&path).into();
            replace_recording(tracks, &title, saved);
            window.set_saved_count(window.get_saved_count() + 1);
            if fidelity == Fidelity::Processed {
                notices.push(Notice {
                    kind: Warning::Processed,
                    detail: title.to_string().into(),
                    ..Notice::default()
                });
            }
        }
        RecorderEvent::Discarded {
            title,
            reason,
            fidelity,
            duration,
        } => {
            let mut discarded = row(&title, TrackState::Discarded);
            describe(&mut discarded, fidelity, duration);
            discarded.reason = reason_of(reason);
            replace_recording(tracks, &title, discarded);
        }
        RecorderEvent::Error(message) => {
            // Encoding errors start with the track's display title.
            for i in 0..tracks.row_count() {
                let mut row = tracks.row_data(i).expect("index in range");
                if row.state == TrackState::Recording && message.starts_with(row.title.as_str()) {
                    row.state = TrackState::Failed;
                    tracks.set_row_data(i, row);
                }
            }
            notices.push(Notice {
                kind: Warning::Error,
                detail: message.into(),
                ..Notice::default()
            });
        }
        RecorderEvent::CableIsDefault(as_default) => show_cable_as_default(window, &as_default),
        RecorderEvent::SpotifyFades(title) => {
            // Once: the setting is the same for every track.
            if !notices.iter().any(|n| n.kind == Warning::SpotifyFades) {
                notices.push(Notice {
                    kind: Warning::SpotifyFades,
                    detail: title.to_string().into(),
                    ..Notice::default()
                });
            }
        }
        RecorderEvent::CableMissing => notices.push(Notice {
            kind: Warning::CableMissing,
            ..Notice::default()
        }),
        RecorderEvent::CaptureLost | RecorderEvent::AdMuted(_) | RecorderEvent::Spotify(_) => {}
    }
}

/// Warns that nothing can be heard, with a button to go back to a real
/// device. Shown once.
fn show_cable_as_default(window: &AppWindow, as_default: &CableAsDefault) {
    let notices = window.get_notices();
    let Some(notices) = notices.as_any().downcast_ref::<VecModel<Notice>>() else {
        return;
    };
    if notices.iter().any(|n| n.kind == Warning::CableDefault) {
        return;
    }
    let (id, name) = as_default.replacement.clone().unwrap_or_default();
    notices.push(Notice {
        kind: Warning::CableDefault,
        device: name.into(),
        detail: id.into(),
    });
}

fn row(title: &TrackTitle, state: TrackState) -> TrackRow {
    TrackRow {
        title: title.to_string().into(),
        state,
        ..TrackRow::default()
    }
}

fn describe(row: &mut TrackRow, fidelity: Fidelity, duration: Duration) {
    row.length = mapping::length(duration).into();
    (row.quality, row.depth, row.touched) = match fidelity {
        Fidelity::BitPerfect { depth, touched } => (
            Quality::BitPerfect,
            i32::from(depth),
            touched.min(i32::MAX as u64) as i32,
        ),
        Fidelity::NearTransparent => (Quality::NearTransparent, 24, 0),
        Fidelity::PeakLimited => (Quality::PeakLimited, 24, 0),
        Fidelity::Processed => (Quality::Processed, 0, 0),
        Fidelity::Silent => (Quality::Silent, 0, 0),
    };
}

/// Replaces the latest "recording" row of `title`, or adds `row` when
/// there is none (a track discarded before it was announced).
fn replace_recording(tracks: &VecModel<TrackRow>, title: &TrackTitle, row: TrackRow) {
    let name = title.to_string();
    let index = (0..tracks.row_count()).rev().find(|&i| {
        tracks
            .row_data(i)
            .is_some_and(|r| r.state == TrackState::Recording && r.title == name.as_str())
    });
    match index {
        Some(i) => tracks.set_row_data(i, row),
        None => tracks.push(row),
    }
}

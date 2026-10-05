//! Sample content for screenshots (README, release notes), without
//! Spotify: `RESONA_DEMO=session` (a recording in progress), `playlist`
//! (a playlist half recorded) or `settings-0` to `settings-3` (a settings
//! tab). Nothing is recorded; the content is set once the window settled.

use std::time::Duration;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, Quality, Reason, SpotifyState, TrackRow, TrackState};

/// After the monitor's first report (Spotify not running), which would
/// otherwise overwrite the sample "now playing".
const SETTLE: Duration = Duration::from_millis(1500);

pub fn schedule(window: &AppWindow) {
    let Ok(mode) = std::env::var("RESONA_DEMO") else {
        return;
    };
    let weak = window.as_weak();
    slint::Timer::single_shot(SETTLE, move || {
        if let Some(window) = weak.upgrade() {
            apply(&window, &mode);
        }
    });
}

fn apply(window: &AppWindow, mode: &str) {
    // Not the machine's own user name.
    window.set_output_dir(r"C:\Users\You\Music\Resona".into());
    if let Some(tab) = mode.strip_prefix("settings-") {
        // Four tabs: Recording, Files, Audio, Interface.
        window.set_settings_tab(tab.parse::<i32>().unwrap_or(0).clamp(0, 3));
        window.set_settings_open(true);
        return;
    }
    window.set_spotify_state(SpotifyState::Playing);
    window.set_now_title("Midnight City".into());
    window.set_now_artist("M83".into());
    window.set_now_album("Hurry Up, We're Dreaming".into());
    window.set_recording(true);
    window.set_elapsed("00:17:42".into());

    let saved = |title: &str, length: &str, quality: Quality, depth: i32| TrackRow {
        title: title.into(),
        length: length.into(),
        state: TrackState::Saved,
        quality,
        depth,
        tagged: true,
        format: "FLAC".into(),
        ..TrackRow::default()
    };
    let with_state = |title: &str, length: &str, state: TrackState| TrackRow {
        title: title.into(),
        length: length.into(),
        state,
        ..TrackRow::default()
    };
    let rows = if mode == "playlist" {
        window.set_playlist_running(true);
        window.set_playlist_name("Indie Night".into());
        window.set_playlist_done(4);
        window.set_playlist_total(7);
        vec![
            saved("Phoenix - 1901", "3:13", Quality::BitPerfect, 16),
            saved("The Strokes - Reptilia", "3:40", Quality::BitPerfect, 24),
            saved(
                "Arctic Monkeys - Do I Wanna Know?",
                "4:32",
                Quality::PeakLimited,
                24,
            ),
            saved("Foals - My Number", "3:59", Quality::BitPerfect, 16),
            with_state("M83 - Midnight City", "4:03", TrackState::Recording),
            with_state("MGMT - Electric Feel", "3:49", TrackState::Waiting),
            with_state(
                "Two Door Cinema Club - What You Know",
                "3:11",
                TrackState::Waiting,
            ),
        ]
    } else {
        vec![
            saved("Daft Punk - Instant Crush", "5:37", Quality::BitPerfect, 16),
            saved("Phoenix - 1901", "3:13", Quality::BitPerfect, 24),
            saved(
                "Tame Impala - The Less I Know the Better",
                "3:36",
                Quality::PeakLimited,
                24,
            ),
            TrackRow {
                reason: Reason::TooShort,
                ..with_state("Spotify - Advertisement", "0:15", TrackState::Discarded)
            },
            saved("Justice - D.A.N.C.E.", "4:02", Quality::BitPerfect, 16),
            with_state("M83 - Midnight City", "", TrackState::Recording),
        ]
    };
    let saved_count = rows.iter().filter(|r| r.state == TrackState::Saved).count();
    window.set_saved_count(saved_count as i32);
    window.set_tracks(ModelRc::new(VecModel::from(rows)));
}

/// Demo content stays as set: the live monitor must not replace it.
pub fn active() -> bool {
    // The same reading as `schedule`: a value that is not UTF-8 is no demo.
    std::env::var("RESONA_DEMO").is_ok()
}

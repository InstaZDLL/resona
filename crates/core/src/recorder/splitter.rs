//! Turning the continuous capture into one recording per track.
//!
//! The monitor reports changes late: after the next poll, and after
//! Spotify updated its title. So audio is held back for [`HORIZON`] before
//! being committed to a track, and every change is placed at the frame it
//! actually happened, which is still inside the held-back audio by the
//! time the change is known. Pure logic: the caller feeds audio and
//! changes, and carries out the returned [`Action`]s.

use std::collections::VecDeque;
use std::time::Duration;

use super::boundary::snap_to_silence;
use crate::spotify::state::{Content, Track};
use crate::spotify::title::TrackTitle;

/// How long audio is held back. Covers a poll interval, Spotify's title
/// delay, the next poll for SMTC's dating of the track start, and
/// [`DETAILS_SETTLE`].
pub const HORIZON: Duration = Duration::from_secs(3);
/// SMTC details received this close to the end of a track are dropped:
/// SMTC updates field by field, and the next track's fields have been
/// seen next to the previous title.
pub const DETAILS_SETTLE: Duration = Duration::from_millis(1500);
/// How far a cut may move to land on silence: a title-based estimate is
/// off by up to a poll, a SMTC-dated one by Spotify's output latency.
const SNAP_ESTIMATED: Duration = Duration::from_millis(300);
const SNAP_DATED: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Sent with the track's first sound: a track that never makes a
    /// sound (skipped right away) produces no actions at all.
    Begin {
        track: Track,
    },
    /// Interleaved stereo samples for the current track.
    Write(Vec<f32>),
    End(Ended),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    /// With the last settled SMTC details.
    pub track: Track,
    pub frames: u64,
    /// The track was already playing when recording started.
    pub joined_late: bool,
}

impl Ended {
    pub fn duration(&self, sample_rate: u32) -> Duration {
        Duration::from_secs_f64(self.frames as f64 / f64::from(sample_rate))
    }

    /// Shorter than SMTC said by more than `tolerance`: skipped, or joined
    /// late. Unknown without SMTC's duration.
    pub fn is_partial(&self, sample_rate: u32, tolerance: Duration) -> bool {
        self.joined_late
            || self
                .track
                .details
                .duration
                .is_some_and(|full| self.duration(sample_rate) + tolerance < full)
    }
}

#[derive(Debug, Clone)]
pub enum Change {
    Content(Content),
    Playing(bool),
    /// SMTC details for the current track arrived or changed.
    Details(Track),
    /// SMTC dated the start of `title`: at `frame`, or before the capture
    /// began (`None`).
    TrackStart {
        title: TrackTitle,
        frame: Option<u64>,
    },
}

#[derive(Debug, Clone)]
struct Pending {
    frame: u64,
    change: Change,
    /// For a content change: the frame comes from SMTC, not from when the
    /// title changed.
    dated: bool,
    joined_late: bool,
}

struct Active {
    track: Track,
    frames: u64,
    joined_late: bool,
    /// Nothing but digital silence written so far (leading silence is
    /// trimmed).
    started: bool,
    /// Digital silence held back: written if the track goes on, dropped
    /// if it ends (trailing silence).
    held_zeros: u64,
}

pub struct Splitter {
    sample_rate: u32,
    horizon: u64,
    settle: u64,
    /// Held-back audio, interleaved stereo, starting at `buffer_start`.
    buffer: VecDeque<f32>,
    buffer_start: u64,
    pending: Vec<Pending>,
    active: Option<Active>,
    paused: bool,
}

impl Splitter {
    pub fn new(sample_rate: u32) -> Self {
        let frames = |d: Duration| (d.as_secs_f64() * f64::from(sample_rate)) as u64;
        Self {
            sample_rate,
            horizon: frames(HORIZON),
            settle: frames(DETAILS_SETTLE),
            buffer: VecDeque::new(),
            buffer_start: 0,
            pending: Vec::new(),
            active: None,
            paused: false,
        }
    }

    /// Frame index just past the last audio received.
    pub fn end_frame(&self) -> u64 {
        self.buffer_start + (self.buffer.len() / 2) as u64
    }

    pub fn push_audio(&mut self, samples: &[f32]) -> Vec<Action> {
        self.buffer.extend(samples);
        let upto = self.end_frame().saturating_sub(self.horizon);
        let mut actions = Vec::new();
        self.commit(upto, &mut actions);
        actions
    }

    /// Records `change` as happening at `frame` (moved up to the oldest
    /// audio not committed yet if it is older).
    pub fn change(&mut self, frame: u64, change: Change) {
        let frame = frame.max(self.buffer_start);
        match change {
            Change::TrackStart {
                title,
                frame: dated,
            } => self.date_start(&title, dated),
            Change::Content(content) => {
                // Details just before a track change may already describe
                // the next track.
                let settle_from = frame.saturating_sub(self.settle);
                self.pending.retain(|p| {
                    !(matches!(p.change, Change::Details(_)) && p.frame >= settle_from)
                });
                self.insert(Pending {
                    frame,
                    change: Change::Content(content),
                    dated: false,
                    joined_late: false,
                });
            }
            change => self.insert(Pending {
                frame,
                change,
                dated: false,
                joined_late: false,
            }),
        }
    }

    /// Commits everything and ends the current track.
    pub fn finish(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        self.commit(self.end_frame(), &mut actions);
        self.end_active(&mut actions);
        actions
    }

    fn insert(&mut self, pending: Pending) {
        let at = self.pending.partition_point(|p| p.frame <= pending.frame);
        self.pending.insert(at, pending);
    }

    fn date_start(&mut self, title: &TrackTitle, frame: Option<u64>) {
        let is_switch_to = |p: &Pending| matches!(&p.change, Change::Content(Content::Track(t)) if &t.title == title);
        if let Some(index) = self.pending.iter().position(is_switch_to) {
            let mut pending = self.pending.remove(index);
            match frame {
                Some(frame) if frame >= self.buffer_start => {
                    pending.frame = frame;
                    pending.dated = true;
                }
                // Started before the audio we still hold: before the
                // capture, or so long ago the cut is already written.
                _ => pending.joined_late = frame.is_none(),
            }
            self.insert(pending);
        } else if frame.is_none()
            && let Some(active) = self.active.as_mut()
            && &active.track.title == title
        {
            active.joined_late = true;
        }
    }

    fn commit(&mut self, upto: u64, actions: &mut Vec<Action>) {
        while let Some(next) = self.pending.first() {
            if next.frame > upto {
                break;
            }
            let mut pending = self.pending.remove(0);
            if matches!(pending.change, Change::Content(_)) {
                let (front, back) = self.buffer.as_slices();
                let held: Vec<f32> = front.iter().chain(back).copied().collect();
                let window = self.snap_window(&pending);
                pending.frame = snap_to_silence(&held, self.buffer_start, pending.frame, window)
                    .max(self.buffer_start);
                self.write_until(pending.frame, actions);
            } else {
                // Applying a playback or details change slightly early is
                // harmless; writing into a pending cut's search window is not.
                self.write_until(pending.frame.min(self.write_limit()), actions);
            }
            self.apply(pending, actions);
        }
        self.write_until(upto.min(self.write_limit()), actions);
    }

    fn snap_window(&self, pending: &Pending) -> u64 {
        let window = if pending.dated {
            SNAP_DATED
        } else {
            SNAP_ESTIMATED
        };
        (window.as_secs_f64() * f64::from(self.sample_rate)) as u64
    }

    /// Audio may be written up to the search window of the next pending
    /// cut, so the cut can still move back onto a silence.
    fn write_limit(&self) -> u64 {
        self.pending
            .iter()
            .filter(|p| matches!(p.change, Change::Content(_)))
            .map(|p| p.frame.saturating_sub(self.snap_window(p)))
            .min()
            .unwrap_or(u64::MAX)
    }

    fn apply(&mut self, pending: Pending, actions: &mut Vec<Action>) {
        match pending.change {
            Change::Content(content) => {
                self.end_active(actions);
                if let Content::Track(track) = content {
                    self.active = Some(Active {
                        track,
                        frames: 0,
                        joined_late: pending.joined_late,
                        started: false,
                        held_zeros: 0,
                    });
                }
            }
            Change::Playing(playing) => self.paused = !playing,
            Change::Details(track) => {
                if let Some(active) = self.active.as_mut()
                    && active.track.title == track.title
                {
                    active.track.details = track.details;
                }
            }
            Change::TrackStart { .. } => {}
        }
    }

    fn end_active(&mut self, actions: &mut Vec<Action>) {
        if let Some(active) = self.active.take()
            && active.started
        {
            actions.push(Action::End(Ended {
                track: active.track,
                frames: active.frames,
                joined_late: active.joined_late,
            }));
        }
    }

    /// Moves held-back audio up to `frame` into the current track.
    fn write_until(&mut self, frame: u64, actions: &mut Vec<Action>) {
        if frame <= self.buffer_start {
            return;
        }
        let count = ((frame - self.buffer_start) * 2) as usize;
        let samples: Vec<f32> = self.buffer.drain(..count.min(self.buffer.len())).collect();
        self.buffer_start = frame.min(self.buffer_start + (samples.len() / 2) as u64);
        let Some(active) = self.active.as_mut() else {
            return;
        };

        let mut out = Vec::with_capacity(samples.len());
        for frame in samples.as_chunks::<2>().0 {
            let silent = frame[0] == 0.0 && frame[1] == 0.0;
            if silent {
                // Paused: Spotify sends digital silence, which is not part
                // of the track. Otherwise silence is kept only between
                // sounds (trimmed at both ends).
                if !self.paused && active.started {
                    active.held_zeros += 1;
                }
                continue;
            }
            if !active.started {
                active.started = true;
                actions.push(Action::Begin {
                    track: active.track.clone(),
                });
            }
            for _ in 0..active.held_zeros {
                out.extend_from_slice(&[0.0, 0.0]);
            }
            active.frames += active.held_zeros + 1;
            active.held_zeros = 0;
            out.extend_from_slice(frame);
        }
        if !out.is_empty() {
            actions.push(Action::Write(out));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::state::TrackDetails;
    use crate::spotify::title::TitleSeparator;

    const RATE: u32 = 44_100;
    const SECOND: u64 = RATE as u64;

    fn track(name: &str) -> Track {
        Track {
            title: TrackTitle {
                artist: "Artist".into(),
                title: name.into(),
                title_extended: None,
                separator: TitleSeparator::None,
            },
            details: TrackDetails::default(),
        }
    }

    /// `frames` of stereo "music": a constant non-zero level, identifying
    /// the track by its value.
    fn music(level: f32, frames: u64) -> Vec<f32> {
        vec![level; (frames * 2) as usize]
    }

    struct Run {
        splitter: Splitter,
        actions: Vec<Action>,
    }

    impl Run {
        fn new() -> Self {
            Self {
                splitter: Splitter::new(RATE),
                actions: Vec::new(),
            }
        }
        fn audio(&mut self, samples: &[f32]) {
            // Fed in 10 ms packets, as the capture does.
            for packet in samples.chunks(882) {
                let actions = self.splitter.push_audio(packet);
                self.actions.extend(actions);
            }
        }
        fn change(&mut self, frame: u64, change: Change) {
            self.splitter.change(frame, change);
        }
        fn finish(mut self) -> Vec<Action> {
            let actions = self.splitter.finish();
            self.actions.extend(actions);
            self.actions
        }
    }

    /// Per recorded track: its title, the levels written (each once, in
    /// order) and the frame count.
    fn summary(actions: &[Action]) -> Vec<(String, Vec<f32>, u64)> {
        let mut tracks: Vec<(String, Vec<f32>, u64)> = Vec::new();
        for action in actions {
            match action {
                Action::Begin { track } => tracks.push((track.title.title.clone(), vec![], 0)),
                Action::Write(samples) => {
                    let current = tracks.last_mut().expect("write outside a track");
                    for &s in samples.iter().step_by(2) {
                        if current.1.last() != Some(&s) {
                            current.1.push(s);
                        }
                    }
                }
                Action::End(ended) => {
                    let current = tracks.last_mut().unwrap();
                    assert_eq!(current.0, ended.track.title.title);
                    current.2 = ended.frames;
                }
            }
        }
        tracks
    }

    #[test]
    fn splits_at_the_reported_frame() {
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(track("One"))));
        run.audio(&music(0.1, 5 * SECOND));
        // "Two" starts at 5 s and the change is noticed half a second later.
        run.audio(&music(0.2, SECOND / 2));
        run.change(5 * SECOND, Change::Content(Content::Track(track("Two"))));
        run.audio(&music(0.2, 5 * SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0], ("One".into(), vec![0.1], 5 * SECOND));
        assert_eq!(tracks[1].1, vec![0.2]);
        assert_eq!(tracks[1].2, 5 * SECOND + SECOND / 2);
    }

    #[test]
    fn a_late_estimate_snaps_back_to_the_gap() {
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(track("One"))));
        run.audio(&music(0.1, 5 * SECOND));
        run.audio(&music(0.0, SECOND / 10)); // 100 ms gap between tracks
        run.audio(&music(0.2, SECOND / 2));
        // Title noticed 200 ms after "Two" started.
        let estimate = 5 * SECOND + SECOND / 10 + SECOND / 5;
        run.change(estimate, Change::Content(Content::Track(track("Two"))));
        run.audio(&music(0.2, 5 * SECOND + SECOND / 2));
        let tracks = summary(&run.finish());
        // No sound of "Two" leaked into "One", none was lost from "Two".
        assert_eq!(tracks[0].1, vec![0.1]);
        assert_eq!(tracks[0].2, 5 * SECOND);
        assert_eq!(tracks[1].2, 6 * SECOND);
    }

    #[test]
    fn smtc_dating_moves_the_cut() {
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(track("One"))));
        run.audio(&music(0.1, 5 * SECOND));
        run.audio(&music(0.2, SECOND));
        // Title noticed 400 ms late, gapless (no silence to snap to)…
        run.change(
            5 * SECOND + 2 * SECOND / 5,
            Change::Content(Content::Track(track("Two"))),
        );
        // …then SMTC dates the start exactly.
        run.change(
            6 * SECOND,
            Change::TrackStart {
                title: track("Two").title,
                frame: Some(5 * SECOND),
            },
        );
        run.audio(&music(0.2, 4 * SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks[0].1, vec![0.1]);
        assert_eq!(tracks[1].1, vec![0.2]);
        assert_eq!(tracks[1].2, 5 * SECOND);
    }

    #[test]
    fn pause_drops_silence_and_keeps_one_file() {
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(track("One"))));
        run.audio(&music(0.1, 2 * SECOND));
        run.change(2 * SECOND, Change::Playing(false));
        run.audio(&music(0.0, 10 * SECOND)); // paused: Spotify sends zeros
        run.change(12 * SECOND, Change::Playing(true));
        run.audio(&music(0.1, 2 * SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].2, 4 * SECOND);
    }

    #[test]
    fn silence_is_trimmed_at_both_ends_but_kept_inside() {
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(track("One"))));
        run.audio(&music(0.0, SECOND));
        run.audio(&music(0.1, SECOND));
        run.audio(&music(0.0, SECOND)); // a break inside the track
        run.audio(&music(0.1, SECOND));
        run.audio(&music(0.0, SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks[0].1, vec![0.1, 0.0, 0.1]);
        assert_eq!(tracks[0].2, 3 * SECOND);
    }

    #[test]
    fn ads_are_not_recorded() {
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(track("One"))));
        run.audio(&music(0.1, 4 * SECOND));
        run.change(4 * SECOND, Change::Content(Content::Ad));
        run.audio(&music(0.5, 4 * SECOND));
        run.change(8 * SECOND, Change::Content(Content::Track(track("Two"))));
        run.audio(&music(0.2, 4 * SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks.len(), 2);
        assert!(tracks.iter().all(|t| !t.1.contains(&0.5)));
    }

    #[test]
    fn details_right_before_a_change_are_dropped() {
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(track("One"))));
        let mut good = track("One");
        good.details.album = Some("Right album".into());
        run.change(SECOND, Change::Details(good));
        run.audio(&music(0.1, 5 * SECOND));
        // SMTC half-switched to the next track, 1 s before the title did.
        let mut stale = track("One");
        stale.details.album = Some("Next track's album".into());
        run.change(4 * SECOND, Change::Details(stale));
        run.change(5 * SECOND, Change::Content(Content::Track(track("Two"))));
        run.audio(&music(0.2, 4 * SECOND));
        let actions = run.finish();
        let Some(Action::End(ended)) = actions.iter().find(|a| matches!(a, Action::End(_))) else {
            panic!()
        };
        assert_eq!(ended.track.details.album.as_deref(), Some("Right album"));
    }

    #[test]
    fn track_started_before_the_capture_is_partial() {
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(track("One"))));
        run.change(
            0,
            Change::TrackStart {
                title: track("One").title,
                frame: None,
            },
        );
        run.audio(&music(0.1, 4 * SECOND));
        let actions = run.finish();
        let Some(Action::End(ended)) = actions.last() else {
            panic!()
        };
        assert!(ended.joined_late);
        assert!(ended.is_partial(RATE, Duration::from_secs(2)));
    }

    #[test]
    fn skipped_track_is_partial() {
        let mut one = track("One");
        one.details.duration = Some(Duration::from_secs(200));
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(one)));
        run.audio(&music(0.1, 30 * SECOND));
        let actions = run.finish();
        let Some(Action::End(ended)) = actions.last() else {
            panic!()
        };
        assert!(!ended.joined_late);
        assert!(ended.is_partial(RATE, Duration::from_secs(2)));
    }

    #[test]
    fn a_track_without_sound_leaves_nothing() {
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(track("One"))));
        run.audio(&music(0.0, 4 * SECOND));
        run.change(4 * SECOND, Change::Content(Content::Track(track("Two"))));
        run.audio(&music(0.2, 4 * SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].0, "Two");
    }
}

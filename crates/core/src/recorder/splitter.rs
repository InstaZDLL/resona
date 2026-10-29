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

use super::boundary::{Kind, SPOTIFY_GAP, Search, place_cut};
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
/// How far a cut may move to land on silence (before, after): a
/// title-based estimate is off by up to a poll, a SMTC-dated one by
/// Spotify's output latency. Wider windows were tried on 2026-10-03 and
/// were wrong: the drift they chased came from Spotify's crossfade and
/// Automix, switched back on by a Spotify update. They also risk cutting
/// into a quiet intro. After a skip, SMTC dates the next track at the
/// click, while Spotify still plays the old one's 125 ms fade-out, then
/// 250 ms of silence (2026-10-29): a dated cut reaches 300 ms after.
const SNAP_DATED: (Duration, Duration) = (Duration::from_millis(100), Duration::from_millis(300));
const SNAP_ESTIMATED: (Duration, Duration) =
    (Duration::from_millis(300), Duration::from_millis(300));
/// How far a cut may move around the end SMTC's length gives a track (its
/// start plus its duration). That end is trusted when the change agrees
/// with it, both within reach of each other: the start it counts from is
/// itself a cut, placed to within Spotify's output latency.
const LENGTH_REACH: Duration = Duration::from_millis(200);
/// Digital silence kept at either end of a track, at most. Spotify's
/// tracks include their own leading and trailing silence: keeping it gives
/// the file the catalogue's length and keeps synced lyrics on time
/// (2026-10-14: trimmed, a file lost up to 1.5 s and its lyrics ran early).
/// Longer is not part of the track (Spotify idle, the list over).
const EDGE_SILENCE: Duration = Duration::from_secs(10);

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
    /// Where the track was cut from the previous one.
    origin: u64,
    /// Paused at some point: its start plus its length no longer tell
    /// where it ends.
    paused: bool,
    frames: u64,
    joined_late: bool,
    /// The track made a sound: a track that never does leaves no file.
    started: bool,
    /// Digital silence held back (frames), written before the next sound
    /// or, up to [`EDGE_SILENCE`], at either end of the track.
    held_zeros: u64,
}

pub struct Splitter {
    sample_rate: u32,
    horizon: u64,
    settle: u64,
    edge_silence: u64,
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
            edge_silence: frames(EDGE_SILENCE),
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
                let search = self.search(&pending);
                let cut = place_cut(&held, self.buffer_start, search);
                self.write_until(cut.frame, actions);
                self.skip_until(cut.resume);
                pending.frame = cut.resume.max(self.buffer_start);
            } else {
                // Applying a playback or details change slightly early is
                // harmless; writing into a pending cut's search window is not.
                self.write_until(pending.frame.min(self.write_limit()), actions);
            }
            self.apply(pending, actions);
        }
        self.write_until(upto.min(self.write_limit()), actions);
    }

    /// Where to look for the cut of a content change: around the current
    /// track's end when its length is known and the change agrees with it
    /// (SMTC may date the next track as late as the end of Spotify's gap),
    /// around the change otherwise.
    fn search(&self, pending: &Pending) -> Search {
        let (before, after) = if pending.dated {
            SNAP_DATED
        } else {
            SNAP_ESTIMATED
        };
        let (before, after) = (self.frames(before), self.frames(after));
        let estimate = pending.frame;
        let reach = self.frames(LENGTH_REACH);
        let spotify_gap = self.changes_album(pending);
        if let Some(end) = self.expected_end()
            && estimate + after + reach >= end
            && end + SPOTIFY_GAP + reach + before >= estimate
        {
            return Search {
                from: end.saturating_sub(reach),
                to: end + reach,
                target: end,
                kind: Kind::Ending,
                spotify_gap,
            };
        }
        Search {
            from: estimate.saturating_sub(before),
            to: estimate + after,
            target: estimate,
            kind: Kind::Interrupted,
            spotify_gap,
        }
    }

    /// The change goes to a track of another album, or the albums are
    /// unknown: Spotify plays its gap between them. The next track's album
    /// comes with its title, or with the SMTC details that follow.
    fn changes_album(&self, pending: &Pending) -> bool {
        let Some(active) = &self.active else {
            return true;
        };
        let Change::Content(Content::Track(next)) = &pending.change else {
            return true;
        };
        let next_album = next.details.album.as_ref().or_else(|| {
            self.pending.iter().find_map(|p| match &p.change {
                Change::Details(track) if track.title == next.title => track.details.album.as_ref(),
                _ => None,
            })
        });
        match (&active.track.details.album, next_album) {
            (Some(album), Some(next_album)) => album != next_album,
            _ => true,
        }
    }

    /// The current track's start plus the length SMTC gave it.
    fn expected_end(&self) -> Option<u64> {
        let active = self.active.as_ref()?;
        if active.joined_late || active.paused {
            return None;
        }
        Some(active.origin + self.frames(active.track.details.duration?))
    }

    fn frames(&self, duration: Duration) -> u64 {
        (duration.as_secs_f64() * f64::from(self.sample_rate)) as u64
    }

    /// Audio may be written up to the search window of the next pending
    /// cut, so the cut can still move back onto a silence, and up to half a
    /// second before it: Spotify's gap is only recognised whole.
    fn write_limit(&self) -> u64 {
        self.pending
            .iter()
            .filter(|p| matches!(p.change, Change::Content(_)))
            .map(|p| self.search(p).from.saturating_sub(SPOTIFY_GAP))
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
                        origin: pending.frame,
                        paused: self.paused,
                        frames: 0,
                        joined_late: pending.joined_late,
                        started: false,
                        held_zeros: 0,
                    });
                }
            }
            Change::Playing(playing) => {
                self.paused = !playing;
                if let Some(active) = self.active.as_mut() {
                    active.paused |= !playing;
                }
            }
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
        if let Some(mut active) = self.active.take()
            && active.started
        {
            // The track's trailing silence.
            let trailing = active.held_zeros.min(self.edge_silence);
            if trailing > 0 {
                actions.push(Action::Write(vec![0.0; (trailing * 2) as usize]));
                active.frames += trailing;
            }
            actions.push(Action::End(Ended {
                track: active.track,
                frames: active.frames,
                joined_late: active.joined_late,
            }));
        }
    }

    /// Drops held-back audio up to `frame`: Spotify's gap between two
    /// tracks, part of neither.
    fn skip_until(&mut self, frame: u64) {
        let count = frame
            .saturating_sub(self.buffer_start)
            .min((self.buffer.len() / 2) as u64);
        self.buffer.drain(..(count * 2) as usize);
        self.buffer_start += count;
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
                // of the track. Otherwise it is the track's own silence.
                if !self.paused {
                    active.held_zeros += 1;
                }
                continue;
            }
            if !active.started {
                active.started = true;
                actions.push(Action::Begin {
                    track: active.track.clone(),
                });
                // The track's leading silence.
                active.held_zeros = active.held_zeros.min(self.edge_silence);
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
        // No sound of "Two" leaked into "One", none was lost, and the gap
        // between them is kept.
        assert_eq!(tracks[0].1, vec![0.1, 0.0]);
        assert!(!tracks[1].1.contains(&0.1));
        assert_eq!(tracks[0].2 + tracks[1].2, 11 * SECOND + SECOND / 10);
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

    fn lasting(name: &str, duration: Duration) -> Track {
        let mut track = track(name);
        track.details.duration = Some(duration);
        track
    }

    #[test]
    fn gapless_cuts_at_the_declared_length() {
        let mut run = Run::new();
        run.change(
            0,
            Change::Content(Content::Track(lasting("One", Duration::from_secs(5)))),
        );
        run.audio(&music(0.1, 5 * SECOND));
        run.audio(&music(0.2, SECOND / 2));
        // Title noticed 400 ms late, no silence to snap to.
        run.change(
            5 * SECOND + 2 * SECOND / 5,
            Change::Content(Content::Track(track("Two"))),
        );
        run.audio(&music(0.2, 5 * SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks[0], ("One".into(), vec![0.1], 5 * SECOND));
        assert_eq!(tracks[1].1, vec![0.2]);
    }

    #[test]
    fn a_gap_is_split_at_the_declared_length() {
        // One lasts 5.05 s: its last 50 ms are silence, the next 50 ms are
        // Two's.
        let one = lasting("One", Duration::from_millis(5_050));
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(one)));
        run.audio(&music(0.1, 5 * SECOND));
        run.audio(&music(0.0, SECOND / 10));
        run.audio(&music(0.2, SECOND / 2));
        run.change(
            5 * SECOND + 3 * SECOND / 10,
            Change::Content(Content::Track(track("Two"))),
        );
        run.audio(&music(0.2, 5 * SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks[0].1, vec![0.1, 0.0]);
        assert_eq!(tracks[0].2, 5 * SECOND + SECOND / 20);
        assert_eq!(tracks[1].1, vec![0.0, 0.2]);
    }

    /// One (5 s of sound), Spotify's gap, then Two with `lead` frames of
    /// its own silence, which SMTC dates near the end of the gap, as seen
    /// on 2026-10-29.
    fn dated_late_in_the_gap(one: Track, lead: u64) -> Vec<(String, Vec<f32>, u64)> {
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(one)));
        run.audio(&music(0.1, 5 * SECOND));
        run.audio(&music(0.0, SECOND / 2 + lead));
        run.audio(&music(0.2, SECOND));
        let dated = 5 * SECOND + 47 * SECOND / 100;
        run.change(
            dated + SECOND / 3,
            Change::Content(Content::Track(track("Two"))),
        );
        run.change(
            dated + SECOND,
            Change::TrackStart {
                title: track("Two").title,
                frame: Some(dated),
            },
        );
        run.audio(&music(0.2, 5 * SECOND));
        summary(&run.finish())
    }

    #[test]
    fn a_start_dated_at_the_end_of_spotifys_gap_still_ends_at_the_length() {
        // Two's own 200 ms of silence stay Two's: One ends at its length.
        let tracks = dated_late_in_the_gap(lasting("One", Duration::from_secs(5)), SECOND / 5);
        assert_eq!(tracks[0], ("One".into(), vec![0.1], 5 * SECOND));
        assert_eq!(tracks[1].1, vec![0.0, 0.2]);
    }

    #[test]
    fn spotifys_gap_is_found_whole_far_from_the_estimate() {
        // Without a length, the cut is searched 100 ms around the dated
        // start, 470 ms into the gap: the gap is still recognised.
        let tracks = dated_late_in_the_gap(track("One"), 0);
        assert_eq!(tracks[0], ("One".into(), vec![0.1], 5 * SECOND));
        assert_eq!(tracks[1].1, vec![0.2]);
    }

    #[test]
    fn spotifys_gap_belongs_to_neither_track() {
        // As measured on 2026-10-29: Spotify plays 500 ms of digital
        // silence between tracks, and SMTC dates the next one a little
        // after the gap begins. Two opens with 200 ms of its own silence.
        let five = Duration::from_secs(5);
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(lasting("One", five))));
        let dated = |run: &mut Run, name: &str, at: u64| {
            run.change(
                at + SECOND / 3,
                Change::Content(Content::Track(lasting(name, five))),
            );
            run.change(
                at + SECOND,
                Change::TrackStart {
                    title: track(name).title,
                    frame: Some(at),
                },
            );
        };
        run.audio(&music(0.1, 5 * SECOND));
        run.audio(&music(0.0, SECOND / 2 + SECOND / 5));
        run.audio(&music(0.2, SECOND));
        dated(&mut run, "Two", 5 * SECOND + SECOND / 12);
        run.audio(&music(0.2, 3 * SECOND + 4 * SECOND / 5));
        run.audio(&music(0.0, SECOND / 2));
        run.audio(&music(0.3, SECOND));
        dated(&mut run, "Three", 10 * SECOND + SECOND / 2 + SECOND / 12);
        run.audio(&music(0.3, 5 * SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks[0], ("One".into(), vec![0.1], 5 * SECOND));
        assert_eq!(tracks[1], ("Two".into(), vec![0.0, 0.2], 5 * SECOND));
        assert_eq!(tracks[2].1, vec![0.3]);
    }

    fn on(album: &str, mut track: Track) -> Track {
        track.details.album = Some(album.into());
        track
    }

    /// SMTC dates `name` at `at`, after its title showed a second later.
    fn dated_start(run: &mut Run, next: Track, at: u64) {
        let title = next.title.clone();
        run.change(at + SECOND / 3, Change::Content(Content::Track(next)));
        run.change(
            at + SECOND,
            Change::TrackStart {
                title,
                frame: Some(at),
            },
        );
    }

    #[test]
    fn within_an_album_the_tracks_own_silence_is_kept() {
        // A live album: One ends on a second of its own digital silence,
        // and Spotify plays Two right after it, without its gap.
        let one = on("Live", lasting("One", Duration::from_secs(5)));
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(one)));
        run.audio(&music(0.1, 4 * SECOND));
        run.audio(&music(0.0, SECOND));
        run.audio(&music(0.2, SECOND + SECOND / 2));
        // Two's album comes with the SMTC details, after its title.
        dated_start(&mut run, track("Two"), 5 * SECOND + SECOND / 12);
        run.change(
            5 * SECOND + SECOND,
            Change::Details(on("Live", track("Two"))),
        );
        run.audio(&music(0.2, 5 * SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks[0], ("One".into(), vec![0.1, 0.0], 5 * SECOND));
        assert_eq!(tracks[1].1, vec![0.2]);
    }

    #[test]
    fn a_skip_cuts_after_the_old_tracks_fade_out() {
        // Skipped at 3 s: Spotify fades One out over 125 ms, then plays
        // 250 ms of silence before Two. SMTC dates Two at the click.
        let one = on("A", lasting("One", Duration::from_secs(200)));
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(one)));
        run.audio(&music(0.1, 3 * SECOND));
        run.audio(&music(0.05, SECOND / 8));
        run.audio(&music(0.0, SECOND / 4));
        run.audio(&music(0.2, SECOND + SECOND / 2));
        dated_start(&mut run, on("B", track("Two")), 3 * SECOND);
        run.audio(&music(0.2, 5 * SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(
            tracks[0],
            ("One".into(), vec![0.1, 0.05], 3 * SECOND + SECOND / 8)
        );
        assert_eq!(tracks[1].1, vec![0.0, 0.2]);
    }

    #[test]
    fn a_skip_cuts_at_the_next_tracks_first_sample() {
        let one = lasting("One", Duration::from_secs(200));
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(one)));
        run.audio(&music(0.1, 3 * SECOND));
        // Skipped mid-note: Two opens with 5 ms near silence (−80 dBFS),
        // too short to be a gap.
        run.audio(&music(0.0001, 220));
        run.audio(&music(0.2, SECOND / 2));
        run.change(
            3 * SECOND + 3 * SECOND / 20,
            Change::Content(Content::Track(track("Two"))),
        );
        run.audio(&music(0.2, 5 * SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks[0].1, vec![0.1, 0.0001]);
        assert!(
            (3 * SECOND..3 * SECOND + 220).contains(&tracks[0].2),
            "{}",
            tracks[0].2
        );
        assert!(!tracks[1].1.contains(&0.1));
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
    fn the_tracks_own_silence_is_kept() {
        // Leading, inner and trailing silence are part of the track, as in
        // Spotify's catalogue length: synced lyrics stay on time.
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(track("One"))));
        run.audio(&music(0.0, SECOND));
        run.audio(&music(0.1, SECOND));
        run.audio(&music(0.0, SECOND)); // a break inside the track
        run.audio(&music(0.1, SECOND));
        run.audio(&music(0.0, SECOND));
        let tracks = summary(&run.finish());
        assert_eq!(tracks[0].1, vec![0.0, 0.1, 0.0, 0.1, 0.0]);
        assert_eq!(tracks[0].2, 5 * SECOND);
    }

    #[test]
    fn long_silence_at_the_ends_is_capped() {
        let mut run = Run::new();
        run.change(0, Change::Content(Content::Track(track("One"))));
        run.audio(&music(0.0, 30 * SECOND));
        run.audio(&music(0.1, SECOND));
        run.audio(&music(0.0, 30 * SECOND)); // Spotify idle, not paused
        let tracks = summary(&run.finish());
        assert_eq!(tracks[0].2, SECOND + 2 * EDGE_SILENCE.as_secs() * SECOND);
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

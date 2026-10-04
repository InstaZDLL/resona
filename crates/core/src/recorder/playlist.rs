//! Recording a whole playlist or album: which of its tracks Spotify plays,
//! when to start the next one, when the list is done. Pure logic, fed with
//! what `spotify_cli now-playing` reports; the session in
//! `playlist_session` carries the steps out.

use std::time::{Duration, Instant};

use crate::metadata::name_match::title_similarity;
use crate::spotify::cli::NowPlaying;

/// After asking Spotify for a track, how long to wait for it to show up
/// before asking again.
const PLAY_GRACE: Duration = Duration::from_secs(5);
/// Title similarity above which a recording is taken for a list entry.
const SAME_TITLE: f64 = 0.85;

/// How Spotify gets from one track of the list to the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Spotify plays the playlist or album itself (`spotify:playlist:…`).
    Context(String),
    /// Each track is started on its own: retrying the ones missed.
    Tracks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Wait,
    /// Ask Spotify to play this entry of the list.
    Play(usize),
    /// The list is over: pause Spotify and stop recording.
    Finish,
}

pub struct Plan {
    uris: Vec<String>,
    mode: Mode,
    current: Option<usize>,
    requested: Option<(usize, Instant)>,
}

impl Plan {
    pub fn new(uris: Vec<String>, mode: Mode) -> Self {
        Self {
            uris,
            mode,
            current: None,
            requested: None,
        }
    }

    /// What to ask Spotify for first.
    pub fn first_uri(&self) -> Option<&str> {
        match &self.mode {
            Mode::Context(uri) => Some(uri),
            Mode::Tracks => self.uris.first().map(String::as_str),
        }
    }

    /// The entry playing, as last seen.
    pub const fn current(&self) -> Option<usize> {
        self.current
    }

    pub fn len(&self) -> usize {
        self.uris.len()
    }

    pub fn is_empty(&self) -> bool {
        self.uris.is_empty()
    }

    /// Call after starting [`Self::first_uri`] or a [`Step::Play`].
    pub fn requested(&mut self, index: usize, at: Instant) {
        self.requested = Some((index, at));
    }

    /// Looks at what Spotify plays at `now` and says what to do.
    pub fn observe(&mut self, playing: Option<&NowPlaying>, now: Instant) -> Step {
        // From the entry playing on: a track listed twice is the later one.
        let from = self.current.unwrap_or(0);
        let seen = playing.and_then(|p| {
            (from..self.uris.len())
                .chain(0..from)
                .find(|&i| self.uris[i] == p.uri)
        });
        if let Some(index) = seen {
            if self.requested.is_some_and(|(asked, _)| asked == index) {
                self.requested = None;
            }
            return match self.current {
                // Back to an earlier entry: Spotify reached the end of the
                // playlist and rewound to its start, paused.
                Some(current) if index < current && matches!(self.mode, Mode::Context(_)) => {
                    Step::Finish
                }
                _ => {
                    self.current = Some(index);
                    Step::Wait
                }
            };
        }
        // Spotify plays something else, or nothing.
        if let Some((_, asked_at)) = self.requested
            && now.duration_since(asked_at) < PLAY_GRACE
        {
            return Step::Wait;
        }
        let Some(current) = self.current else {
            // Not started yet (or never will: the caller times out).
            return match (&self.mode, self.requested) {
                (Mode::Tracks, Some((asked, _))) => Step::Play(asked),
                _ => Step::Wait,
            };
        };
        match self.mode {
            // Past the last entry, Spotify's autoplay took over.
            Mode::Context(_) => Step::Finish,
            Mode::Tracks if current + 1 < self.uris.len() => Step::Play(current + 1),
            Mode::Tracks => Step::Finish,
        }
    }
}

/// The list entry a recording is of: by title (as Spotify's window shows
/// it) among the entries, the one playing breaking ties.
pub fn entry_for(titles: &[String], recorded: &str, current: Option<usize>) -> Option<usize> {
    let mut best: Option<(f64, usize)> = None;
    for (index, title) in titles.iter().enumerate() {
        let score = title_similarity(title, recorded);
        let nearer = |other: usize| current.is_some_and(|c| index.abs_diff(c) < other.abs_diff(c));
        if score >= SAME_TITLE && best.is_none_or(|(s, i)| score > s || (score == s && nearer(i))) {
            best = Some((score, index));
        }
    }
    best.map(|(_, index)| index).or(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing(uri: &str) -> NowPlaying {
        NowPlaying {
            uri: uri.into(),
            description: String::new(),
            context_description: String::new(),
            is_playing: true,
        }
    }

    fn uris(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("spotify:track:{i}")).collect()
    }

    #[test]
    fn context_runs_to_the_end_then_finishes_on_autoplay() {
        let t0 = Instant::now();
        let mut plan = Plan::new(uris(3), Mode::Context("spotify:playlist:p".into()));
        assert_eq!(plan.first_uri(), Some("spotify:playlist:p"));
        // Spotify still on the previous thing right after `play`.
        assert_eq!(
            plan.observe(Some(&playing("spotify:track:x")), t0),
            Step::Wait
        );
        for i in 0..3 {
            let uri = format!("spotify:track:{i}");
            assert_eq!(plan.observe(Some(&playing(&uri)), t0), Step::Wait);
            assert_eq!(plan.current(), Some(i));
        }
        assert_eq!(
            plan.observe(Some(&playing("spotify:track:radio")), t0),
            Step::Finish
        );
    }

    #[test]
    fn context_finishes_when_spotify_rewinds() {
        let t0 = Instant::now();
        let mut plan = Plan::new(uris(2), Mode::Context("spotify:album:a".into()));
        plan.observe(Some(&playing("spotify:track:0")), t0);
        plan.observe(Some(&playing("spotify:track:1")), t0);
        let mut rewound = playing("spotify:track:0");
        rewound.is_playing = false;
        assert_eq!(plan.observe(Some(&rewound), t0), Step::Finish);
    }

    #[test]
    fn skipped_entries_are_fine() {
        // Spotify skipped entry 1 (already recorded).
        let t0 = Instant::now();
        let mut plan = Plan::new(uris(3), Mode::Context("spotify:playlist:p".into()));
        plan.observe(Some(&playing("spotify:track:0")), t0);
        assert_eq!(
            plan.observe(Some(&playing("spotify:track:2")), t0),
            Step::Wait
        );
        assert_eq!(plan.current(), Some(2));
    }

    #[test]
    fn a_track_listed_twice_is_not_a_rewind() {
        let t0 = Instant::now();
        let list = vec![
            "spotify:track:a".into(),
            "spotify:track:b".into(),
            "spotify:track:a".into(),
        ];
        let mut plan = Plan::new(list, Mode::Context("spotify:playlist:p".into()));
        plan.observe(Some(&playing("spotify:track:a")), t0);
        plan.observe(Some(&playing("spotify:track:b")), t0);
        assert_eq!(
            plan.observe(Some(&playing("spotify:track:a")), t0),
            Step::Wait
        );
        assert_eq!(plan.current(), Some(2));
    }

    #[test]
    fn tracks_mode_starts_each_entry() {
        let t0 = Instant::now();
        let later = t0 + PLAY_GRACE;
        let mut plan = Plan::new(uris(2), Mode::Tracks);
        assert_eq!(plan.first_uri(), Some("spotify:track:0"));
        plan.requested(0, t0);
        assert_eq!(
            plan.observe(Some(&playing("spotify:track:0")), t0),
            Step::Wait
        );
        // Entry 0 over, autoplay moves on: start entry 1.
        assert_eq!(
            plan.observe(Some(&playing("spotify:track:radio")), t0),
            Step::Play(1)
        );
        plan.requested(1, t0);
        // Not there yet: no second request within the grace period.
        assert_eq!(
            plan.observe(Some(&playing("spotify:track:radio")), t0),
            Step::Wait
        );
        assert_eq!(
            plan.observe(Some(&playing("spotify:track:1")), t0),
            Step::Wait
        );
        assert_eq!(
            plan.observe(Some(&playing("spotify:track:radio")), later),
            Step::Finish
        );
    }

    #[test]
    fn tracks_mode_asks_again_when_ignored() {
        let t0 = Instant::now();
        let mut plan = Plan::new(uris(1), Mode::Tracks);
        plan.requested(0, t0);
        assert_eq!(plan.observe(None, t0), Step::Wait);
        assert_eq!(plan.observe(None, t0 + PLAY_GRACE), Step::Play(0));
    }

    #[test]
    fn recordings_are_matched_to_entries() {
        let titles = ["Flow".to_owned(), "Hi-Five".to_owned(), "Flow".to_owned()];
        assert_eq!(entry_for(&titles, "Hi-Five", None), Some(1));
        // Twice in the list: the entry playing wins.
        assert_eq!(entry_for(&titles, "Flow", Some(2)), Some(2));
        assert_eq!(entry_for(&titles, "Something else", Some(1)), Some(1));
        assert_eq!(entry_for(&titles, "Something else", None), None);
    }
}

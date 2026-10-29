//! Placing the cut between two tracks on the audio itself.
//!
//! The splitter knows roughly where a track changed: when the monitor
//! noticed it (off by up to a poll interval, or by Spotify's output latency
//! when SMTC dated it), or, at the end of a track, its start plus the
//! length SMTC declared. [`place_cut`] looks around that frame for, in
//! order: a silence; a dip well under the surrounding level, where two
//! tracks fade or flow into each other; on a skip, the millisecond where
//! the peak level collapses, the next track's first sample; and otherwise
//! keeps the frame it was given, the declared length being the best guess
//! for a gapless transition.
//!
//! Adapted from Spytify v2 (Go, `internal/core/session.go` `tryResolve`
//! and `internal/core/ring.go`, MIT, see `THIRD_PARTY_NOTICES.md`). Unlike
//! v2, equally quiet moments go to the one nearest the target rather than
//! the earliest: a gap is split where the declared length or the dated
//! start puts it, so each file keeps its own leading and trailing silence.

/// Length of the blocks compared for silence and dips, 10 ms at 44.1 kHz.
const BLOCK_FRAMES: usize = 441;
/// A block quieter than this (RMS, about −60 dBFS) counts as a gap.
const QUIET_RMS: f32 = 0.001;
/// A block this far under the median block of the search (RMS, −15 dB) is
/// a boundary even when it is not silent: a crossfade, or tracks that flow
/// into each other.
const DIP_RATIO: f32 = 0.177_827_94;
/// Windows compared for the exact cut and for a skip's edge, 1 ms.
const PEAK_FRAMES: usize = 44;
/// On a skip, the next track starts where the peak of a 1 ms window falls
/// this far under the median of all of them (−20 dB).
const EDGE_RATIO: f32 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The track reached the length SMTC declared: `target` is its start
    /// plus that length, the cut kept for a gapless transition.
    Ending,
    /// The track changed before its end (a skip), or its length is
    /// unknown: `target` is when the change was noticed.
    Interrupted,
}

/// Where to look for the cut, in frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Search {
    pub from: u64,
    pub to: u64,
    /// The best estimate: the cut when the audio shows nothing better, and
    /// the preferred one among equally quiet moments.
    pub target: u64,
    pub kind: Kind,
}

/// What placed the cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutBy {
    Silence,
    Dip,
    /// The first sample of the next track, after a skip.
    Edge,
    /// The declared length (gapless).
    Length,
    /// When the change was noticed.
    Estimate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    pub frame: u64,
    pub by: CutBy,
}

/// `samples` is interleaved stereo starting at frame `first_frame`. Only
/// audio inside both `samples` and the search is looked at.
pub fn place_cut(samples: &[f32], first_frame: u64, search: Search) -> Cut {
    let audio = Audio {
        samples,
        first_frame,
    };
    let target = search.target;
    let fallback = Cut {
        frame: target,
        by: match search.kind {
            Kind::Ending => CutBy::Length,
            Kind::Interrupted => CutBy::Estimate,
        },
    };
    let from = search.from.max(first_frame);
    let to = search.to.min(audio.end());
    if to <= from {
        return fallback;
    }

    if let Some((rms, at)) = audio.quietest_block(from, to, target) {
        if rms < QUIET_RMS {
            return Cut {
                frame: audio.refine(at, target),
                by: CutBy::Silence,
            };
        }
        if rms <= audio.median_rms(from, to) * DIP_RATIO {
            return Cut {
                frame: audio.refine(at, target),
                by: CutBy::Dip,
            };
        }
    }
    // Only after a skip: at the end of a track, a percussive break would
    // pass for an edge.
    if search.kind == Kind::Interrupted
        && let Some((peak, at, median)) = audio.quietest_peak(from, to, target)
        && peak <= median * EDGE_RATIO
    {
        return Cut {
            frame: at,
            by: CutBy::Edge,
        };
    }
    fallback
}

struct Audio<'a> {
    samples: &'a [f32],
    first_frame: u64,
}

impl Audio<'_> {
    fn end(&self) -> u64 {
        self.first_frame + (self.samples.len() / 2) as u64
    }

    /// Samples of `frames` frames from frame `at`.
    fn frames(&self, at: u64, frames: usize) -> &[f32] {
        let offset = ((at - self.first_frame) * 2) as usize;
        &self.samples[offset..offset + frames * 2]
    }

    fn rms(&self, at: u64) -> f32 {
        let block = self.frames(at, BLOCK_FRAMES);
        (block.iter().map(|s| s * s).sum::<f32>() / block.len() as f32).sqrt()
    }

    fn peak(&self, at: u64) -> f32 {
        self.frames(at, PEAK_FRAMES)
            .iter()
            .fold(0.0, |peak, s| peak.max(s.abs()))
    }

    /// The quietest block starting in `from..to` (RMS, start), in steps of
    /// half a block. Ties (digital silence above all) go to the block
    /// nearest `target`.
    fn quietest_block(&self, from: u64, to: u64, target: u64) -> Option<(f32, u64)> {
        let starts = (from..)
            .step_by(BLOCK_FRAMES / 2)
            .take_while(|at| at + BLOCK_FRAMES as u64 <= to);
        quietest(starts.map(|at| (self.rms(at), at)), target)
    }

    /// The median RMS of consecutive blocks in `from..to`.
    fn median_rms(&self, from: u64, to: u64) -> f32 {
        let starts = (from..)
            .step_by(BLOCK_FRAMES)
            .take_while(|at| at + BLOCK_FRAMES as u64 <= to);
        median(starts.map(|at| self.rms(at)).collect())
    }

    /// The 1 ms window with the lowest peak (peak, start, ties nearest
    /// `target`), with the median peak of consecutive 1 ms windows, in
    /// `from..to`.
    fn quietest_peak(&self, from: u64, to: u64, target: u64) -> Option<(f32, u64, f32)> {
        let last = to.checked_sub(PEAK_FRAMES as u64)?;
        let (peak, at) = quietest((from..=last).map(|at| (self.peak(at), at)), target)?;
        let tiles = (from..=last).step_by(PEAK_FRAMES);
        Some((peak, at, median(tiles.map(|at| self.peak(at)).collect())))
    }

    /// Moves a cut onto the 1 ms window with the lowest peak within a
    /// block of it: where a gap begins, or where a fade-out meets a
    /// fade-in. Ties go to the window nearest `target`.
    fn refine(&self, cut: u64, target: u64) -> u64 {
        let reach = BLOCK_FRAMES as u64;
        let from = cut.saturating_sub(reach).max(self.first_frame);
        let Some(last) = self.end().checked_sub(PEAK_FRAMES as u64) else {
            return cut;
        };
        let to = (cut + reach).min(last);
        quietest((from..=to).map(|at| (self.peak(at), at)), target).map_or(cut, |(_, at)| at)
    }
}

/// The lowest `(level, frame)`, ties going to the frame nearest `target`.
fn quietest(levels: impl Iterator<Item = (f32, u64)>, target: u64) -> Option<(f32, u64)> {
    levels.fold(None, |best, (level, at)| match best {
        Some((low, near))
            if low < level || (low == level && near.abs_diff(target) <= at.abs_diff(target)) =>
        {
            best
        }
        _ => Some((level, at)),
    })
}

fn median(mut levels: Vec<f32>) -> f32 {
    if levels.is_empty() {
        return 0.0;
    }
    levels.sort_by(f32::total_cmp);
    levels[levels.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn around(target: u64, reach: u64, kind: Kind) -> Search {
        Search {
            from: target - reach,
            to: target + reach,
            target,
            kind,
        }
    }

    /// Noise-like level for frame `f`, between 0.1 and 0.6.
    fn noise(f: usize) -> f32 {
        ((f * 7_919) % 1000) as f32 / 2000.0 + 0.1
    }

    /// Stereo noise scaled per frame by `gain`.
    fn shaped(frames: usize, gain: impl Fn(usize) -> f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|f| {
                let v = noise(f) * gain(f);
                [v, -v]
            })
            .collect()
    }

    /// Stereo noise with digital silence over `quiet` frames.
    fn signal(frames: usize, quiet: std::ops::Range<usize>) -> Vec<f32> {
        shaped(frames, |f| if quiet.contains(&f) { 0.0 } else { 1.0 })
    }

    fn cut(samples: &[f32], first_frame: u64, search: Search) -> u64 {
        place_cut(samples, first_frame, search).frame
    }

    #[test]
    fn moves_onto_the_gap() {
        let samples = signal(44_100, 20_000..21_000);
        let placed = place_cut(&samples, 0, around(22_000, 4_410, Kind::Interrupted));
        assert_eq!(placed.by, CutBy::Silence);
        // In the gap, towards its end: nearest the estimate.
        assert!(
            (20_450..=21_000 - PEAK_FRAMES as u64).contains(&placed.frame),
            "{placed:?}"
        );
    }

    #[test]
    fn a_gap_is_split_at_the_target() {
        let samples = signal(44_100, 20_000..21_000);
        let placed = place_cut(&samples, 0, around(20_300, 4_410, Kind::Ending));
        assert_eq!(
            placed,
            Cut {
                frame: 20_300,
                by: CutBy::Silence
            }
        );
    }

    #[test]
    fn keeps_the_estimate_without_a_gap() {
        let samples = signal(44_100, 0..0);
        let placed = place_cut(&samples, 0, around(22_000, 4_410, Kind::Interrupted));
        assert_eq!(
            placed,
            Cut {
                frame: 22_000,
                by: CutBy::Estimate
            }
        );
    }

    #[test]
    fn gapless_cuts_at_the_declared_length() {
        let samples = signal(44_100, 0..0);
        let placed = place_cut(&samples, 0, around(22_000, 8_820, Kind::Ending));
        assert_eq!(
            placed,
            Cut {
                frame: 22_000,
                by: CutBy::Length
            }
        );
    }

    #[test]
    fn a_gap_out_of_reach_is_ignored() {
        let samples = signal(44_100, 1_000..2_000);
        assert_eq!(
            cut(&samples, 0, around(22_000, 4_410, Kind::Interrupted)),
            22_000
        );
    }

    #[test]
    fn reaches_further_after_the_estimate() {
        // An uneven window: the gap 1.2 s after the estimate is in reach.
        let samples = signal(88_200, 60_000..61_000);
        let search = |target: u64| Search {
            from: target.saturating_sub(13_230),
            to: target + 77_175,
            target,
            kind: Kind::Interrupted,
        };
        let placed = cut(&samples, 0, search(8_000));
        assert!((60_000..61_000).contains(&placed), "{placed}");
        // The same gap 1.2 s before is out of reach.
        assert_eq!(cut(&samples, 0, search(80_000)), 80_000);
    }

    #[test]
    fn digital_silence_beats_a_quiet_passage() {
        let mut samples = signal(88_200, 60_000..61_000);
        // A quiet fade (−66 dBFS) right at the estimate.
        for s in &mut samples[2 * 10_000..2 * 12_000] {
            *s = s.signum() * 0.0005;
        }
        let search = Search {
            from: 0,
            to: 87_675,
            target: 10_500,
            kind: Kind::Interrupted,
        };
        let placed = cut(&samples, 0, search);
        assert!((60_000..61_000).contains(&placed), "{placed}");
    }

    #[test]
    fn a_crossfade_cuts_in_its_dip() {
        // Two tracks crossfading: the level drops 25 dB around frame 22 050
        // without reaching silence.
        let samples = shaped(44_100, |f| {
            let distance = (f as f32 - 22_050.0).abs();
            (distance / 1_323.0).clamp(0.056, 1.0)
        });
        let placed = place_cut(&samples, 0, around(22_500, 6_615, Kind::Ending));
        assert_eq!(placed.by, CutBy::Dip);
        assert!(placed.frame.abs_diff(22_050) < 100, "{placed:?}");
    }

    #[test]
    fn a_skip_cuts_at_the_next_tracks_first_sample() {
        // Cut mid-note at frame 20 000; the next track opens with 5 ms near
        // silence (−60 dBFS) before its sound: too short for a quiet block.
        let samples = shaped(44_100, |f| match f {
            20_000..20_220 => 0.002,
            _ => 1.0,
        });
        let placed = place_cut(&samples, 0, around(23_000, 13_230, Kind::Interrupted));
        assert_eq!(placed.by, CutBy::Edge);
        assert!((20_000..20_220).contains(&placed.frame), "{placed:?}");
        // At a track's end, the same moment is not trusted as an edge.
        let ending = place_cut(&samples, 0, around(23_000, 13_230, Kind::Ending));
        assert_eq!(
            ending,
            Cut {
                frame: 23_000,
                by: CutBy::Length
            }
        );
    }

    #[test]
    fn works_with_an_offset_buffer() {
        let samples = signal(44_100, 20_000..21_000);
        let placed = cut(&samples, 100_000, around(122_000, 4_410, Kind::Interrupted));
        assert!((120_000..121_000).contains(&placed), "{placed}");
    }

    #[test]
    fn a_search_outside_the_audio_keeps_the_target() {
        let samples = signal(4_410, 0..0);
        let placed = place_cut(&samples, 0, around(20_000, 1_000, Kind::Ending));
        assert_eq!(
            placed,
            Cut {
                frame: 20_000,
                by: CutBy::Length
            }
        );
    }
}

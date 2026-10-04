//! Moving an estimated track boundary onto the quietest moment nearby.
//!
//! The estimate comes from when the monitor noticed the change, off by up
//! to a poll interval (or by Spotify's output latency when SMTC dated it).
//! Most tracks start and end on near-silence, so cutting
//! there keeps a stray bit of the previous track out of the next file. A
//! gapless transition has no quiet moment: the estimate is then kept as is.

/// Length of the blocks compared, 10 ms at 44.1 kHz.
const BLOCK_FRAMES: usize = 441;
/// A block quieter than this (RMS, about −60 dBFS) counts as a gap.
const QUIET_RMS: f32 = 0.001;

/// How far a cut may move, in frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub before: u64,
    pub after: u64,
}

/// `samples` is interleaved stereo starting at frame `first_frame`;
/// `estimate` is the frame to refine. Returns the start of the quietest
/// block in reach, digital silence first (the gap Spotify leaves between
/// most tracks, where a quiet intro or fade is not), or `estimate` if none
/// is quiet.
pub fn snap_to_silence(samples: &[f32], first_frame: u64, estimate: u64, window: Window) -> u64 {
    let frames = (samples.len() / 2) as u64;
    let from = estimate.saturating_sub(window.before).max(first_frame);
    let to = (estimate + window.after).min(first_frame + frames);
    if to <= from {
        return estimate;
    }

    let mut quietest: Option<(f32, u64)> = None;
    let mut start = from;
    while start + BLOCK_FRAMES as u64 <= to {
        let offset = ((start - first_frame) * 2) as usize;
        let block = &samples[offset..offset + BLOCK_FRAMES * 2];
        let rms = (block.iter().map(|s| s * s).sum::<f32>() / block.len() as f32).sqrt();
        // Ties (digital silence above all) go to the block nearest the
        // estimate.
        let better = quietest.is_none_or(|(best, at)| {
            rms < best || (rms == best && start.abs_diff(estimate) < at.abs_diff(estimate))
        });
        if rms < QUIET_RMS && better {
            quietest = Some((rms, start));
        }
        start += (BLOCK_FRAMES / 2) as u64;
    }
    quietest.map_or(estimate, |(_, at)| at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn both(frames: u64) -> Window {
        Window {
            before: frames,
            after: frames,
        }
    }

    /// Stereo noise-like signal with digital silence over `quiet` frames.
    fn signal(frames: usize, quiet: std::ops::Range<usize>) -> Vec<f32> {
        (0..frames)
            .flat_map(|f| {
                let v = if quiet.contains(&f) {
                    0.0
                } else {
                    ((f * 7_919) % 1000) as f32 / 2000.0 + 0.1
                };
                [v, -v]
            })
            .collect()
    }

    #[test]
    fn moves_onto_the_gap() {
        let samples = signal(44_100, 20_000..21_000);
        let cut = snap_to_silence(&samples, 0, 22_000, both(4_410));
        assert!((20_000..=20_600).contains(&cut), "{cut}");
    }

    #[test]
    fn keeps_the_estimate_without_a_gap() {
        let samples = signal(44_100, 0..0);
        assert_eq!(snap_to_silence(&samples, 0, 22_000, both(4_410)), 22_000);
    }

    #[test]
    fn a_gap_out_of_reach_is_ignored() {
        let samples = signal(44_100, 1_000..2_000);
        assert_eq!(snap_to_silence(&samples, 0, 22_000, both(4_410)), 22_000);
    }

    #[test]
    fn reaches_further_after_the_estimate() {
        // An uneven window: the gap 1.2 s after the estimate is in reach.
        let samples = signal(88_200, 60_000..61_000);
        let window = Window {
            before: 13_230,
            after: 77_175,
        };
        let cut = snap_to_silence(&samples, 0, 8_000, window);
        assert!((60_000..=60_600).contains(&cut), "{cut}");
        // The same gap 1.2 s before is out of reach.
        assert_eq!(snap_to_silence(&samples, 0, 80_000, window), 80_000);
    }

    #[test]
    fn digital_silence_beats_a_quiet_passage() {
        let mut samples = signal(88_200, 60_000..61_000);
        // A quiet fade (−66 dBFS) right at the estimate.
        for s in &mut samples[2 * 10_000..2 * 12_000] {
            *s = s.signum() * 0.0005;
        }
        let window = Window {
            before: 13_230,
            after: 77_175,
        };
        let cut = snap_to_silence(&samples, 0, 10_500, window);
        assert!((60_000..=60_600).contains(&cut), "{cut}");
    }

    #[test]
    fn works_with_an_offset_buffer() {
        let samples = signal(44_100, 20_000..21_000);
        let cut = snap_to_silence(&samples, 100_000, 122_000, both(4_410));
        assert!((120_000..=120_600).contains(&cut), "{cut}");
    }
}

//! Moving an estimated track boundary onto the quietest moment nearby.
//!
//! The estimate comes from when the monitor noticed the change, off by up
//! to a poll interval (or by Spotify's output latency when SMTC dated it).
//! Most tracks start and end on near-silence, so cutting there instead
//! keeps a stray bit of the previous track out of the next file. A gapless
//! transition has no quiet moment: the estimate is then kept as is.

/// Length of the blocks compared, 10 ms at 44.1 kHz.
const BLOCK_FRAMES: usize = 441;
/// A block quieter than this (RMS, about −60 dBFS) counts as a gap.
const QUIET_RMS: f32 = 0.001;

/// `samples` is interleaved stereo starting at frame `first_frame`;
/// `estimate` is the frame to refine and `window` how far (in frames) the
/// cut may move either way. Returns the start of the quietest block in
/// reach, or `estimate` if none is quiet.
pub fn snap_to_silence(samples: &[f32], first_frame: u64, estimate: u64, window: u64) -> u64 {
    let frames = (samples.len() / 2) as u64;
    let from = estimate.saturating_sub(window).max(first_frame);
    let to = (estimate + window).min(first_frame + frames);
    if to <= from {
        return estimate;
    }

    let mut quietest: Option<(f32, u64)> = None;
    let mut start = from;
    while start + BLOCK_FRAMES as u64 <= to {
        let offset = ((start - first_frame) * 2) as usize;
        let block = &samples[offset..offset + BLOCK_FRAMES * 2];
        let rms = (block.iter().map(|s| s * s).sum::<f32>() / block.len() as f32).sqrt();
        // Ties go to the block nearest the estimate.
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
        let cut = snap_to_silence(&samples, 0, 22_000, 4_410);
        assert!((20_000..=20_600).contains(&cut), "{cut}");
    }

    #[test]
    fn keeps_the_estimate_without_a_gap() {
        let samples = signal(44_100, 0..0);
        assert_eq!(snap_to_silence(&samples, 0, 22_000, 4_410), 22_000);
    }

    #[test]
    fn a_gap_out_of_reach_is_ignored() {
        let samples = signal(44_100, 1_000..2_000);
        assert_eq!(snap_to_silence(&samples, 0, 22_000, 4_410), 22_000);
    }

    #[test]
    fn works_with_an_offset_buffer() {
        let samples = signal(44_100, 20_000..21_000);
        let cut = snap_to_silence(&samples, 100_000, 122_000, 4_410);
        assert!((120_000..=120_600).contains(&cut), "{cut}");
    }
}

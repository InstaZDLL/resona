//! Offline look at a raw float capture kept by `capture_spotify`
//! (`RESONA_KEEP_WAV=1`): where the samples off the 24-bit grid are.
//!
//! ```text
//! cargo run -p resona-core --example analyze_wav -- capture.capture.wav
//! ```
//!
//! - by level: a peak limiter only touches the loudest samples;
//! - left / right together: a gain stage hits both channels of a frame,
//!   per-channel conversion errors do not;
//! - by residual: f32 keeps 24 significant bits, so a loud sample can only
//!   be off by half a 24-bit step, a quieter one by finer amounts.

use resona_core::analysis::BitAnalysis;

const SCALE_24: f64 = 8_388_608.0;

fn residual_24(sample: f32) -> f64 {
    let scaled = f64::from(sample) * SCALE_24;
    (scaled - scaled.round()).abs()
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: analyze_wav <file.wav>"))?;
    let samples: Vec<f32> = hound::WavReader::open(&path)?
        .samples::<f32>()
        .collect::<Result<_, _>>()?;
    let mut analysis = BitAnalysis::default();
    analysis.feed(&samples);
    println!(
        "{} samples, peak {:.1} dBFS, off the 24-bit grid {:.2} %",
        samples.len(),
        analysis.peak_dbfs(),
        analysis.off_grid_ratio() * 100.0
    );

    println!("\nby level (dBFS band: samples, off grid, mean residual of those off):");
    for band in 0..10 {
        let (high, low) = (-6.0 * band as f32, -6.0 * (band + 1) as f32);
        let in_band: Vec<f32> = samples
            .iter()
            .copied()
            .filter(|s| {
                let db = 20.0 * s.abs().log10();
                s.abs() > 0.0 && db <= high && (db > low || band == 9)
            })
            .collect();
        let off: Vec<f64> = in_band
            .iter()
            .map(|&s| residual_24(s))
            .filter(|&r| r > 0.0)
            .collect();
        println!(
            "  {high:>4.0} to {:>4} dB: {:>9} samples, off {:>5.1} %, mean residual {:.3}",
            if band == 9 {
                "-inf".to_string()
            } else {
                format!("{low:.0}")
            },
            in_band.len(),
            100.0 * off.len() as f64 / in_band.len().max(1) as f64,
            off.iter().sum::<f64>() / off.len().max(1) as f64,
        );
    }

    let frames: Vec<(bool, bool)> = samples
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[l, r]| (residual_24(l) > 0.0, residual_24(r) > 0.0))
        .collect();
    let left = frames.iter().filter(|f| f.0).count();
    let right = frames.iter().filter(|f| f.1).count();
    let both = frames.iter().filter(|f| f.0 && f.1).count();
    let expected_if_independent = left as f64 * right as f64 / frames.len() as f64;
    println!(
        "\nleft off {left}, right off {right}, both off in the same frame {both} \
         (≈ {expected_if_independent:.0} if unrelated)"
    );

    // Signed deviations of frames off on both sides: a mid/side stereo
    // decode done in float puts the same error on L and R (mid) or opposite
    // ones (side); an unrelated added signal has no such link.
    let deviation = |s: f32| {
        let scaled = f64::from(s) * SCALE_24;
        scaled - scaled.round()
    };
    let (mut same, mut opposite, mut other, mut both_off) = (0_u64, 0_u64, 0_u64, 0_u64);
    let mut dot = 0.0_f64;
    let (mut norm_l, mut norm_r) = (0.0_f64, 0.0_f64);
    for &[l, r] in samples.as_chunks::<2>().0 {
        let (dl, dr) = (deviation(l), deviation(r));
        if dl == 0.0 || dr == 0.0 {
            continue;
        }
        both_off += 1;
        if (dl - dr).abs() < 1e-9 {
            same += 1;
        } else if (dl + dr).abs() < 1e-9 {
            opposite += 1;
        } else {
            other += 1;
        }
        dot += dl * dr;
        norm_l += dl * dl;
        norm_r += dr * dr;
    }
    let share = |n: u64| 100.0 * n as f64 / both_off.max(1) as f64;
    println!(
        "frames off on both sides: L = R deviation {:.1} %, L = −R {:.1} %, unrelated {:.1} %, \
         correlation {:+.3}",
        share(same),
        share(opposite),
        share(other),
        dot / (norm_l.sqrt() * norm_r.sqrt()).max(f64::MIN_POSITIVE),
    );

    // Off-grid samples grouped in time (a gain changing over a stretch) or
    // scattered (a per-sample effect)?
    let mut runs = Vec::new();
    let mut current = 0_usize;
    for &(l, _) in &frames {
        if l {
            current += 1;
        } else if current > 0 {
            runs.push(current);
            current = 0;
        }
    }
    let mean_run = runs.iter().sum::<usize>() as f64 / runs.len().max(1) as f64;
    println!(
        "left channel: {} runs of off-grid samples, mean length {mean_run:.1}",
        runs.len()
    );

    let off: Vec<f64> = samples
        .iter()
        .map(|&s| residual_24(s))
        .filter(|&r| r > 0.0)
        .collect();
    let halves = off.iter().filter(|&&r| (r - 0.5).abs() < 1e-9).count();
    let quarters = off
        .iter()
        .filter(|&&r| (r - 0.25).abs() < 1e-9 || (r - 0.75).abs() < 1e-9)
        .count();
    println!(
        "residuals of off-grid samples: exactly 1/2 step {:.1} %, 1/4 step {:.1} %, finer {:.1} %",
        100.0 * halves as f64 / off.len().max(1) as f64,
        100.0 * quarters as f64 / off.len().max(1) as f64,
        100.0 * (off.len() - halves - quarters) as f64 / off.len().max(1) as f64,
    );
    Ok(())
}

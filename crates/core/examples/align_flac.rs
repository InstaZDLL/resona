//! Finds where the start of one recording sits in another of the same
//! track: how much audio a cut lost or gained at the start.
//!
//! ```text
//! cargo run -p resona-core --example align_flac -- <reference.flac> <other.flac> [seconds]
//! ```
//!
//! Positive: `other` starts that much later in the track than `reference`
//! (its first part is missing). Gains may differ (volume), the comparison
//! is normalized.

use std::path::Path;

fn mono(path: &Path, seconds: f64) -> anyhow::Result<(Vec<f32>, u32)> {
    let mut reader = claxon::FlacReader::open(path)?;
    let info = reader.streaminfo();
    let channels = info.channels as usize;
    let scale = 1.0 / f32::from(1_u16 << (info.bits_per_sample - 1).min(15)).max(1.0);
    let limit = (seconds * f64::from(info.sample_rate)) as usize * channels;
    let mut samples = Vec::new();
    let mut frame = Vec::with_capacity(channels);
    for sample in reader.samples().take(limit) {
        frame.push(sample? as f32 * scale);
        if frame.len() == channels {
            samples.push(frame.iter().sum::<f32>() / channels as f32);
            frame.clear();
        }
    }
    Ok((samples, info.sample_rate))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [reference, other] = [&args[0], &args[1]].map(Path::new);
    let seconds: f64 = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(10.0);
    // A 2 s probe from `other`, searched over `seconds` of `reference`
    // and the other way round, so both a lost and a gained start show.
    let probe = 2.0;
    let (a, rate) = mono(reference, seconds + probe)?;
    let (b, _) = mono(other, seconds + probe)?;
    let best = |hay: &[f32], needle: &[f32]| {
        let norm = |s: &[f32]| s.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        let needle_norm = norm(needle);
        (0..hay.len().saturating_sub(needle.len()))
            .step_by(1)
            .map(|offset| {
                let window = &hay[offset..offset + needle.len()];
                let dot: f32 = window.iter().zip(needle).map(|(x, y)| x * y).sum();
                (offset, dot / (norm(window) * needle_norm))
            })
            .fold((0, f32::MIN), |acc, c| if c.1 > acc.1 { c } else { acc })
    };
    let n = (probe * f64::from(rate)) as usize;
    let skip = rate as usize; // past any trimmed silence
    let (in_ref, score_a) = best(&a, &b[skip..skip + n]);
    let (in_other, score_b) = best(&b, &a[skip..skip + n]);
    let ms = |frames: f64| frames * 1000.0 / f64::from(rate);
    println!(
        "other[1 s] found in reference at {:.0} ms (correlation {score_a:.3}): other starts {:+.0} ms later",
        ms(in_ref as f64),
        ms(in_ref as f64 - skip as f64)
    );
    println!(
        "reference[1 s] found in other at {:.0} ms (correlation {score_b:.3}): other starts {:+.0} ms later",
        ms(in_other as f64),
        ms(skip as f64 - in_other as f64)
    );
    Ok(())
}

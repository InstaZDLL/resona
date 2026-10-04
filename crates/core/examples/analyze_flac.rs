//! Where a saved FLAC departs from integer 16-bit audio: overall, and per
//! second, to tell a processed whole track from processed moments (a fade
//! at a pause, a cut).
//!
//! ```text
//! cargo run -p resona-core --example analyze_flac -- "Artist - Title.flac"
//! ```

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: analyze_flac <file.flac>"))?;
    let mut reader = claxon::FlacReader::open(&path)?;
    let info = reader.streaminfo();
    let rate = info.sample_rate as usize;
    let channels = info.channels as usize;
    let bits = info.bits_per_sample;
    let samples: Vec<i32> = reader.samples().collect::<Result<_, _>>()?;
    println!(
        "{path}: {bits}-bit, {rate} Hz, {channels} ch, {:.1} s",
        samples.len() as f64 / (rate * channels) as f64
    );
    if bits != 24 {
        println!("not a 24-bit file: nothing finer than its own grid to look for");
        return Ok(());
    }

    // A 16-bit source stored at 24 bits: every value is a multiple of 256.
    let off = |block: &[i32]| block.iter().filter(|&&s| s % 256 != 0).count();
    let total_off = off(&samples);
    println!(
        "off the 16-bit grid: {total_off} of {} samples ({:.3} %)",
        samples.len(),
        100.0 * total_off as f64 / samples.len() as f64
    );
    let second = rate * channels;
    let busy: Vec<(usize, usize)> = samples
        .chunks(second)
        .enumerate()
        .map(|(i, block)| (i, off(block)))
        .filter(|&(_, n)| n > 0)
        .collect();
    println!("seconds with off-grid samples: {}", busy.len());
    for (second, count) in busy.iter().take(40) {
        println!(
            "  {:>2}:{:02}  {count:>6} samples ({:.1} %)",
            second / 60,
            second % 60,
            100.0 * *count as f64 / (rate * channels) as f64
        );
    }
    // How far off-grid samples are from the nearest 16-bit value, in 24-bit
    // steps (a 16-bit step is 256 of them). Tiny distances mean a 16-bit
    // source with noise on top, which rounding to 16 bits removes exactly.
    let mut histogram = std::collections::BTreeMap::<u32, usize>::new();
    for &s in &samples {
        let rem = s.rem_euclid(256);
        let distance = rem.min(256 - rem) as u32;
        if distance > 0 {
            *histogram.entry(distance).or_default() += 1;
        }
    }
    println!("distance to the nearest 16-bit value (in 24-bit steps: count):");
    for (distance, count) in histogram.iter().take(12) {
        println!("  {distance:>3}: {count}");
    }
    if let Some((max, _)) = histogram.iter().next_back() {
        println!("  largest distance: {max} of 128");
    }
    // Near a loud moment or not? A peak limiter only touches samples within
    // a few milliseconds of a loud peak. Local level: the largest magnitude
    // within ±5 ms on the same channel.
    let full = f64::from((1_u32 << (bits - 1)) - 1);
    let window = rate / 200 * channels;
    let mut by_local_peak = [(0_usize, 0_usize); 6]; // (samples, off grid) per 3 dB band
    for (i, &s) in samples.iter().enumerate() {
        let from = i.saturating_sub(window);
        let to = (i + window).min(samples.len());
        let local = samples[from..to]
            .iter()
            .skip((i - from) % channels)
            .step_by(channels)
            .map(|v| v.unsigned_abs())
            .max()
            .unwrap_or(0);
        let db = 20.0 * (f64::from(local) / full).log10();
        let band = ((-db / 3.0).floor() as usize).min(5);
        by_local_peak[band].0 += 1;
        if s % 256 != 0 {
            by_local_peak[band].1 += 1;
        }
    }
    println!("off-grid share by loudest level within ±5 ms:");
    for (band, (count, off)) in by_local_peak.iter().enumerate() {
        let label = if band == 5 {
            "below -15 dB".to_string()
        } else {
            format!(
                "{:>3} to {:>3} dB",
                -3 * band as i32,
                -3 * (band as i32 + 1)
            )
        };
        println!(
            "  {label}: {:>5.1} % of {count}",
            100.0 * *off as f64 / (*count).max(1) as f64
        );
    }
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    println!("peak {peak} of {} (full scale)", (1_u32 << (bits - 1)) - 1);
    Ok(())
}

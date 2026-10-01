//! Mapping wall-clock instants (when the monitor saw something) to frame
//! positions in the capture stream.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far back instants can be resolved. Must cover the splitter's
/// horizon.
const HISTORY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy)]
struct Packet {
    received_at: Instant,
    start: u64,
    end: u64,
}

/// Remembers when recent packets arrived and which frames they held.
///
/// Packets are not evenly spaced in time: while Spotify is paused some
/// builds send nothing at all. Each instant is therefore resolved against
/// the packet received just after it, never extrapolated across a gap.
pub struct FrameClock {
    sample_rate: u32,
    packets: VecDeque<Packet>,
}

impl FrameClock {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            packets: VecDeque::new(),
        }
    }

    /// A packet holding frames `start..end` was received at `received_at`.
    pub fn record(&mut self, received_at: Instant, start: u64, end: u64) {
        self.packets.push_back(Packet {
            received_at,
            start,
            end,
        });
        while self
            .packets
            .front()
            .is_some_and(|p| received_at.duration_since(p.received_at) > HISTORY)
        {
            self.packets.pop_front();
        }
    }

    /// The frame playing at `at`, or `None` if `at` predates the history
    /// (for instance a track that started before the capture did).
    pub fn frame_at(&self, at: Instant) -> Option<u64> {
        let first = self.packets.front()?;
        if at
            < first
                .received_at
                .checked_sub(self.duration_of(first.end - first.start))?
        {
            return None;
        }
        let Some(packet) = self.packets.iter().find(|p| p.received_at >= at) else {
            return self.packets.back().map(|p| p.end);
        };
        let back = (packet.received_at - at).as_secs_f64() * f64::from(self.sample_rate);
        Some(packet.end.saturating_sub(back as u64).max(packet.start))
    }

    fn duration_of(&self, frames: u64) -> Duration {
        Duration::from_secs_f64(frames as f64 / f64::from(self.sample_rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// 10 ms packets of 441 frames, received every 10 ms from `t0`.
    fn steady(t0: Instant, packets: u64) -> FrameClock {
        let mut clock = FrameClock::new(RATE);
        for i in 0..packets {
            let received = t0 + Duration::from_millis(10 * (i + 1));
            clock.record(received, i * 441, (i + 1) * 441);
        }
        clock
    }

    #[test]
    fn resolves_instants_inside_the_stream() {
        let t0 = Instant::now();
        let clock = steady(t0, 100);
        let frame = clock.frame_at(t0 + Duration::from_millis(505)).unwrap();
        assert!((22_000..=22_300).contains(&frame), "{frame}");
    }

    #[test]
    fn instants_before_the_capture_are_unknown() {
        let t0 = Instant::now() + Duration::from_secs(1);
        let clock = steady(t0, 10);
        assert_eq!(clock.frame_at(t0 - Duration::from_millis(500)), None);
    }

    #[test]
    fn instants_after_the_last_packet_map_to_its_end() {
        let t0 = Instant::now();
        let clock = steady(t0, 10);
        assert_eq!(clock.frame_at(t0 + Duration::from_secs(5)), Some(4_410));
    }

    #[test]
    fn a_gap_without_packets_is_not_extrapolated() {
        // Paused for 2 s with no packets, then playback resumes.
        let t0 = Instant::now();
        let mut clock = steady(t0, 10); // frames 0..4410 by t0 + 100 ms
        let resumed = t0 + Duration::from_millis(2_110);
        clock.record(resumed, 4_410, 4_851);
        // Anything during the gap resolves to the start of the next packet.
        assert_eq!(clock.frame_at(t0 + Duration::from_secs(1)), Some(4_410));
    }
}

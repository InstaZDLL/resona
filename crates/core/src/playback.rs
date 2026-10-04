//! Plays the captured audio on the Windows default device, so Spotify can
//! still be heard while it plays silently into the virtual cable. Takes
//! the place of Windows' "Listen to this device", which would need setting
//! up by hand.
//!
//! What is heard goes through the headset's own chain (resampling,
//! enhancements): it never reaches the recording.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

use crate::format::{CAPTURE_CHANNELS, CAPTURE_SAMPLE_RATE};
use crate::{Error, Result};

const EVENT_TIMEOUT_MS: u32 = 250;
/// Audio queued beyond this is dropped: the cable and the headset run on
/// separate clocks, and the delay would otherwise grow without bound.
const MAX_QUEUED_SECS: usize = 1;

pub struct Playback {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Playback {
    /// Starts playing on the default device what is sent to the returned
    /// channel (interleaved stereo `f32` at [`CAPTURE_SAMPLE_RATE`]).
    /// Refuses `avoid`, the device the captured audio comes from: playing
    /// it back there would loop. Needs nothing from the caller's thread.
    pub fn start(avoid: &str) -> Result<(Self, Sender<Vec<f32>>)> {
        let (samples_tx, samples_rx) = crossbeam_channel::unbounded();
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let stop = Arc::new(AtomicBool::new(false));
        let avoid = avoid.to_owned();
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("resona-playback".into())
            .spawn(move || {
                if let Err(e) = play(&avoid, &samples_rx, &ready_tx, &thread_stop) {
                    // Before the stream started, `ready` carries the error.
                    let _ = ready_tx.send(Err(e));
                }
            })?;
        let playback = Self {
            stop,
            thread: Some(thread),
        };
        match ready_rx.recv() {
            Ok(Ok(())) => Ok((playback, samples_tx)),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::PlaybackThread),
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn play(
    avoid: &str,
    samples: &Receiver<Vec<f32>>,
    ready: &Sender<Result<()>>,
    stop: &AtomicBool,
) -> Result<()> {
    let _ = wasapi::initialize_mta();
    let device = DeviceEnumerator::new()?.get_default_device(&Direction::Render)?;
    if device.get_id()? == avoid {
        return Err(Error::PlaybackLoop);
    }
    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        CAPTURE_SAMPLE_RATE as usize,
        usize::from(CAPTURE_CHANNELS),
        None,
    );
    let mut client = device.get_iaudioclient()?;
    let (period, _) = client.get_device_period()?;
    // The headset may run at another rate: Windows converts.
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: period,
    };
    client.initialize_client(&format, &Direction::Render, &mode)?;
    let event = client.set_get_eventhandle()?;
    let render = client.get_audiorenderclient()?;
    client.start_stream()?;
    let _ = ready.send(Ok(()));

    let channels = usize::from(CAPTURE_CHANNELS);
    let max_queued = MAX_QUEUED_SECS * CAPTURE_SAMPLE_RATE as usize * channels;
    let mut queue: VecDeque<f32> = VecDeque::new();
    while !stop.load(Ordering::Relaxed) {
        loop {
            match samples.try_recv() {
                Ok(packet) => queue.extend(packet),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(client.stop_stream()?),
            }
        }
        if queue.len() > max_queued {
            let excess = (queue.len() - max_queued) / channels * channels;
            queue.drain(..excess);
        }
        if event.wait_for_event(EVENT_TIMEOUT_MS).is_err() {
            continue;
        }
        let frames = client.get_available_space_in_frames()? as usize;
        // Short of audio (a pause, the capture starting): silence.
        let bytes: Vec<u8> = (0..frames * channels)
            .flat_map(|_| queue.pop_front().unwrap_or(0.0).to_le_bytes())
            .collect();
        render.write_to_device(frames, &bytes, None)?;
    }
    Ok(client.stop_stream()?)
}

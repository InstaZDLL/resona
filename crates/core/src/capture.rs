//! Per-process loopback capture (WASAPI `PROCESS_LOOPBACK`).
//!
//! Captures only Spotify's process tree, so other apps, system sounds and
//! the master volume never reach the recording. This replaces the C#
//! version's VB-Cable driver and undocumented audio-policy rerouting.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};

use crate::format::{CAPTURE_SAMPLE_RATE, front_stereo};
use crate::{Error, Result};

/// How long the capture thread blocks waiting for audio before checking
/// the stop flag. Paused Spotify sends nothing or silence, by build.
const EVENT_TIMEOUT_MS: u32 = 250;

/// Interleaved stereo `f32` samples at [`CAPTURE_SAMPLE_RATE`].
#[derive(Debug)]
pub struct Packet {
    pub samples: Vec<f32>,
    /// WASAPI dropped audio before this packet (the reader fell behind).
    pub discontinuity: bool,
    /// Peak of the channels beyond front left / right, when the device is
    /// multichannel. Non-zero means Spotify spread the music over them and
    /// the front pair alone is not the whole signal.
    pub other_channels_peak: f32,
    /// When the packet was read: maps frames to wall-clock time
    /// ([`crate::recorder::clock::FrameClock`]).
    pub received_at: Instant,
}

pub struct ProcessCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

#[derive(Debug, Clone, Copy)]
pub struct CaptureConfig {
    /// Captured with its whole process tree: Windows offers no "this
    /// process alone" mode. (wasapi's `include_tree: false` is not that: it
    /// captures everything *except* the tree.)
    pub pid: u32,
    /// The output device's channel count (see
    /// [`crate::audio_setup::OutputDevice::channels`]): the stream is
    /// captured in that layout, so Windows never downmixes it, and reduced
    /// to the front pair here.
    pub source_channels: u16,
    /// The device's speaker layout, so that no remapping between layouts
    /// happens either.
    pub channel_mask: u32,
}

impl ProcessCapture {
    /// Starts capturing on a dedicated MTA thread. Returns once the stream
    /// is running, or with the initialisation error.
    pub fn start(config: CaptureConfig) -> Result<(Self, Receiver<Packet>)> {
        let (packets_tx, packets_rx) = crossbeam_channel::unbounded();
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let stop = Arc::new(AtomicBool::new(false));

        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("spytify-capture".into())
            .spawn(move || capture_thread(config, &packets_tx, &ready_tx, &thread_stop))?;

        let mut capture = Self {
            stop,
            thread: Some(thread),
        };
        match ready_rx.recv() {
            Ok(Ok(())) => Ok((capture, packets_rx)),
            Ok(Err(error)) => {
                capture.join()?;
                Err(error)
            }
            Err(_) => {
                capture.join()?;
                Err(Error::CaptureThread)
            }
        }
    }

    pub fn stop(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        self.join()
    }

    fn join(&mut self) -> Result<()> {
        match self.thread.take() {
            Some(thread) => thread.join().map_err(|_| Error::CaptureThread)?,
            None => Ok(()),
        }
    }
}

impl Drop for ProcessCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn capture_thread(
    config: CaptureConfig,
    packets: &Sender<Packet>,
    ready: &Sender<Result<()>>,
    stop: &AtomicBool,
) -> Result<()> {
    // S_FALSE (already initialised) is fine; a real failure surfaces on
    // the first COM call below.
    let _ = wasapi::initialize_mta();

    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        CAPTURE_SAMPLE_RATE as usize,
        usize::from(config.source_channels),
        Some(config.channel_mask),
    );
    let started = (|| -> Result<_> {
        let mut client = AudioClient::new_application_loopback_client(config.pid, true)?;
        // Process loopback has no mix format of its own: `autoconvert` makes
        // Windows deliver the requested format whatever Spotify renders.
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 0,
        };
        client.initialize_client(&format, &Direction::Capture, &mode)?;
        let event = client.set_get_eventhandle()?;
        let capture = client.get_audiocaptureclient()?;
        client.start_stream()?;
        Ok((client, event, capture))
    })();
    let (client, event, capture) = match started {
        Ok(parts) => {
            let _ = ready.send(Ok(()));
            parts
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return Ok(());
        }
    };

    let mut bytes = VecDeque::new();
    while !stop.load(Ordering::Relaxed) {
        if event.wait_for_event(EVENT_TIMEOUT_MS).is_err() {
            continue;
        }
        while capture
            .get_next_packet_size()?
            .is_some_and(|frames| frames > 0)
        {
            let info = capture.read_from_device_to_deque(&mut bytes)?;
            let interleaved: Vec<f32> = if info.flags.silent {
                let count = bytes.len() / 4;
                bytes.clear();
                vec![0.0; count]
            } else {
                let raw: Vec<u8> = bytes.drain(..).collect();
                raw.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&b| f32::from_le_bytes(b))
                    .collect()
            };
            let (samples, other_channels_peak) =
                front_stereo(&interleaved, usize::from(config.source_channels));
            let packet = Packet {
                samples,
                discontinuity: info.flags.data_discontinuity,
                other_channels_peak,
                received_at: Instant::now(),
            };
            if packets.send(packet).is_err() {
                // Receiver gone: nobody is recording any more.
                return Ok(client.stop_stream()?);
            }
        }
    }
    Ok(client.stop_stream()?)
}

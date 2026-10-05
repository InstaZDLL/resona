//! The current track's cover art, as Spotify gives it to Windows' media
//! overlay (SMTC). Fetched on its own thread: SMTC calls block.

use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use resona_core::spotify::smtc::Smtc;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer, Weak};

use crate::AppWindow;

/// SMTC updates the art a moment after the title, and sometimes later
/// still: looked at after each of these delays.
const CHECKS: [Duration; 2] = [Duration::from_millis(500), Duration::from_millis(2000)];

pub struct CoverLoader {
    requests: Sender<()>,
}

impl CoverLoader {
    pub fn spawn(window: Weak<AppWindow>) -> Self {
        let (requests, received) = crossbeam_channel::unbounded();
        // Screenshots in demo mode must not show the cover of whatever the
        // machine's Spotify plays: no loader, requests go nowhere.
        if crate::demo::active() {
            return Self { requests };
        }
        thread::Builder::new()
            .name("resona-cover".into())
            .spawn(move || {
                let _ = wasapi::initialize_mta();
                match Smtc::new() {
                    Ok(smtc) => run(&smtc, &received, &window),
                    Err(e) => tracing::warn!("no cover art: {e}"),
                }
            })
            .expect("spawning a thread");
        Self { requests }
    }

    /// The track may have changed: look at the cover again.
    pub fn refresh(&self) {
        let _ = self.requests.send(());
    }
}

fn run(smtc: &Smtc, requests: &Receiver<()>, window: &Weak<AppWindow>) {
    let mut shown: Option<Vec<u8>> = None;
    while requests.recv().is_ok() {
        let mut checks = CHECKS.iter().peekable();
        let mut waited = Duration::ZERO;
        while let Some(&at) = checks.next() {
            // A newer request starts the checks over.
            match requests.recv_timeout(at - waited) {
                Ok(()) => {
                    checks = CHECKS.iter().peekable();
                    waited = Duration::ZERO;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => waited = at,
            }
            let bytes = smtc.spotify_thumbnail().ok().flatten();
            if bytes == shown {
                continue;
            }
            let pixels = bytes.as_deref().and_then(decode);
            shown = bytes;
            let _ = window.upgrade_in_event_loop(move |w| match pixels {
                Some(pixels) => {
                    w.set_now_cover(Image::from_rgba8(pixels));
                    w.set_has_cover(true);
                }
                None => w.set_has_cover(false),
            });
        }
    }
}

fn decode(bytes: &[u8]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let image = image::load_from_memory(bytes)
        .inspect_err(|e| tracing::warn!("cover art: {e}"))
        .ok()?
        .into_rgba8();
    Some(SharedPixelBuffer::clone_from_slice(
        image.as_raw(),
        image.width(),
        image.height(),
    ))
}

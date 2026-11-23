//! The current track's cover art, as the music app gives it to Windows'
//! media overlay (SMTC). Fetched on its own thread: SMTC calls block.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use resona_core::provider::Provider;
use resona_core::spotify::smtc::Smtc;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer, Weak};

use crate::AppWindow;

/// SMTC updates the art a moment after the title, and sometimes later
/// still: looked at after each of these delays.
const CHECKS: [Duration; 2] = [Duration::from_millis(500), Duration::from_millis(2000)];

enum Request {
    /// The track may have changed.
    Refresh,
    /// Another app was selected and the window cleared its cover: whatever
    /// was shown must be shown again, even the same image.
    Reset,
}

#[derive(Clone)]
pub struct CoverLoader {
    requests: Sender<Request>,
    /// The app selected: covers of any other are neither fetched nor shown.
    selected: Arc<Mutex<Provider>>,
}

impl CoverLoader {
    pub fn spawn(window: Weak<AppWindow>, provider: Provider) -> Self {
        let (requests, received) = crossbeam_channel::unbounded();
        let selected = Arc::new(Mutex::new(provider));
        let loader = Self {
            requests,
            selected: Arc::clone(&selected),
        };
        // Screenshots in demo mode must not show the cover of whatever the
        // machine's Spotify plays: no loader, requests go nowhere.
        if crate::demo::active() {
            return loader;
        }
        thread::Builder::new()
            .name("resona-cover".into())
            .spawn(move || {
                let _ = wasapi::initialize_mta();
                match Smtc::new() {
                    Ok(smtc) => run(&smtc, &received, &selected, &window),
                    Err(e) => tracing::warn!("no cover art: {e}"),
                }
            })
            .expect("spawning a thread");
        loader
    }

    /// Follows `provider` from now on.
    pub fn select(&self, provider: Provider) {
        *self.selected.lock().unwrap_or_else(|e| e.into_inner()) = provider;
        let _ = self.requests.send(Request::Reset);
    }

    /// The track of `provider` may have changed: look at the cover again.
    /// Ignored for an app no longer selected (its monitor still stopping).
    pub fn refresh(&self, provider: Provider) {
        if selected(&self.selected) == provider {
            let _ = self.requests.send(Request::Refresh);
        }
    }
}

fn selected(provider: &Mutex<Provider>) -> Provider {
    *provider.lock().unwrap_or_else(|e| e.into_inner())
}

fn run(
    smtc: &Smtc,
    requests: &Receiver<Request>,
    provider: &Mutex<Provider>,
    window: &Weak<AppWindow>,
) {
    let mut shown: Option<Vec<u8>> = None;
    while let Ok(request) = requests.recv() {
        if matches!(request, Request::Reset) {
            shown = None;
        }
        let mut checks = CHECKS.iter().peekable();
        let mut waited = Duration::ZERO;
        while let Some(&at) = checks.next() {
            // A newer request starts the checks over.
            match requests.recv_timeout(at - waited) {
                Ok(newer) => {
                    if matches!(newer, Request::Reset) {
                        shown = None;
                    }
                    checks = CHECKS.iter().peekable();
                    waited = Duration::ZERO;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => waited = at,
            }
            let fetched_for = selected(provider);
            let bytes = smtc.thumbnail(fetched_for).ok().flatten();
            // Another app was selected during the fetch: its own request
            // follows.
            if bytes == shown || selected(provider) != fetched_for {
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

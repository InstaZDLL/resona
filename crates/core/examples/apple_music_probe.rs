//! Apple Music spike: what Resona would see of the Apple Music app for
//! Windows, before any of it is built. Prints the process tree, every SMTC
//! session, the tree's top-level windows (class and title) and the output
//! device, then captures the tree's audio while logging every change of
//! SMTC session or window title, and reports whether the capture was
//! bit-transparent.
//!
//! Questions it answers: is Apple Music's window title static, which window
//! class is its main window, does its audio reach a process-loopback capture
//! of the `AppleMusic.exe` tree (or is it protected), and at which rate and
//! depth does it arrive.
//!
//! ```text
//! cargo run -p resona-core --example apple_music_probe -- [seconds]
//! RESONA_PROBE_CABLE=1: moves the agent to the virtual cable 5 s in, back 30 s in
//! ```

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use crossbeam_channel::RecvTimeoutError;
    use resona_core::analysis::BitAnalysis;
    use resona_core::audio_setup;
    use resona_core::capture::{CaptureConfig, ProcessCapture};
    use resona_core::format::CAPTURE_CHANNELS;

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .init();
    let seconds: u64 = std::env::args()
        .nth(1)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(60);
    let _ = wasapi::initialize_mta();

    let tree = probe::apple_music_tree().ok_or_else(|| {
        anyhow::anyhow!("AppleMusic.exe is not running: start Apple Music and play a track")
    })?;
    println!("process tree (root PID {}):", tree.root);
    for (pid, parent, name) in &tree.members {
        println!("  {pid:>6}  parent {parent:>6}  {name}");
    }
    let pids: HashSet<u32> = tree.members.iter().map(|(pid, _, _)| *pid).collect();

    println!("\nSMTC sessions:");
    for session in probe::smtc_sessions()? {
        println!("  {session:#?}");
    }

    println!("\ntop-level windows of the tree:");
    for window in probe::windows_of(&pids) {
        println!(
            "  {:>6}  {:<40}  visible {:<5}  {:?}",
            window.pid, window.class, window.visible, window.title
        );
    }

    // Apple Music plays through its library agent, an out-of-process COM
    // server started by svchost (`-Embedding`), outside AppleMusic.exe's
    // tree (measured 2026-11-23): the device and the capture are its.
    let target = probe::pid_of("AMPLibraryAgent.exe").unwrap_or(tree.root);
    let device = audio_setup::spotify_output_device(target)?;
    println!(
        "\noutput device: {} — {} Hz, {} bits, {} channels (mask {:#x}), volume {:.0} %{}{}",
        device.name,
        device.sample_rate,
        device.bits_per_sample,
        device.channels,
        device.channel_mask,
        device.volume_scalar * 100.0,
        if device.muted { ", MUTED" } else { "" },
        if device.enhancements == Some(true) {
            "  ⚠ audio enhancements ON"
        } else {
            ""
        },
    );
    for session in audio_setup::session_volumes(&pids)? {
        println!(
            "audio session (PID {}): volume {:.0} %{}",
            session.pid,
            session.volume * 100.0,
            if session.muted { ", MUTED" } else { "" },
        );
    }

    // Which process actually renders the sound: the meter of every audio
    // session on every device, whatever its owner.
    println!("\naudio sessions of all processes (peak meter over 1 s):");
    for session in probe::all_audio_sessions()? {
        println!(
            "  {:>6}  {:<28}  {:<9}  peak {:.4}  on {}",
            session.pid, session.process, session.state, session.peak, session.device
        );
    }

    println!(
        "\ncapturing at the device rate for {seconds} s; \
         change track, pause and resume meanwhile…"
    );
    println!("capture target PID {target}");
    let (capture, packets) = ProcessCapture::start(CaptureConfig {
        pid: target,
        source_channels: device.channels,
        channel_mask: device.channel_mask,
        sample_rate: device.sample_rate,
    })?;
    let mut analysis = BitAnalysis::default();
    let mut all_samples: Vec<f32> = Vec::new();
    let mut per_second = BitAnalysis::default();
    let mut second_frames = 0_usize;
    let mut packet_count = 0_u64;
    let mut last_change: Option<(Option<probe::Session>, Option<String>)> = None;
    // RESONA_PROBE_CABLE=1: the agent is sent to the virtual cable 5 s in
    // and back 30 s in, to see what the capture gets around each move.
    let cable = if std::env::var_os("RESONA_PROBE_CABLE").is_some() {
        audio_setup::virtual_cable()?.map(|(id, _)| id)
    } else {
        None
    };
    let previous = resona_core::routing::spotify_endpoint(target)?;
    let mut moves = vec![(30, previous.clone()), (5, cable.clone())];
    if cable.is_none() {
        moves.clear();
    }
    // Sends the agent back however the probe ends (short run, error).
    let mut route_back = probe::RouteBack {
        pid: target,
        previous,
        armed: false,
    };
    let started = Instant::now();
    let deadline = started + Duration::from_secs(seconds);
    let mut next_poll = started;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if let Some(&(at, ref device)) = moves.last()
            && started.elapsed() >= Duration::from_secs(at)
        {
            let back = *device == route_back.previous;
            // Armed before leaving the previous device, cleared only once
            // the agent is back on it: a failed return is retried on exit.
            if !back {
                route_back.armed = true;
            }
            let result = resona_core::routing::route_spotify(target, device.as_deref());
            if back && result.is_ok() {
                route_back.armed = false;
            }
            println!(
                "[{:>6.1} s] routed to {:?}: {result:?}",
                started.elapsed().as_secs_f32(),
                device.as_deref().unwrap_or("default device")
            );
            moves.pop();
        }
        if Instant::now() >= next_poll {
            next_poll += Duration::from_millis(500);
            let session = probe::smtc_sessions()?
                .into_iter()
                .find(|s| s.app_id.to_ascii_lowercase().contains("applemusic"))
                .map(|mut s| {
                    // Moves on every poll.
                    s.position_ms = None;
                    s
                });
            let title = probe::windows_of(&pids)
                .into_iter()
                .find(|w| w.visible && !w.title.is_empty())
                .map(|w| w.title);
            let change = (session, title);
            if last_change.as_ref() != Some(&change) {
                println!(
                    "[{:>6.1} s] window {:?}\n           smtc {:?}",
                    started.elapsed().as_secs_f32(),
                    change.1,
                    change.0
                );
                last_change = Some(change);
            }
        }
        match packets.recv_timeout(left.min(Duration::from_millis(100))) {
            Ok(packet) => {
                packet_count += 1;
                analysis.feed(&packet.samples);
                all_samples.extend_from_slice(&packet.samples);
                per_second.feed(&packet.samples);
                second_frames += packet.samples.len() / usize::from(CAPTURE_CHANNELS);
                if second_frames >= device.sample_rate as usize {
                    println!(
                        "[{:>6.1} s] audio peak {:>6.1} dBFS, {}",
                        started.elapsed().as_secs_f32(),
                        per_second.peak_dbfs(),
                        match per_second.effective_depth() {
                            Some(bits) => format!("{bits}-bit exact"),
                            None if per_second.is_silent() => "silent".into(),
                            None => "NOT bit-transparent".into(),
                        }
                    );
                    per_second = BitAnalysis::default();
                    second_frames = 0;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    capture.stop()?;

    let frames = analysis.samples() / u64::from(CAPTURE_CHANNELS);
    println!(
        "\ncaptured {frames} frames ({:.1} s) in {packet_count} packets, peak {:.1} dBFS",
        frames as f64 / f64::from(device.sample_rate),
        analysis.peak_dbfs(),
    );
    match analysis.effective_depth() {
        Some(bits) => println!("bit-transparent: yes, {bits}-bit"),
        None if analysis.is_silent() => println!(
            "only silence: either nothing played, or Apple Music's audio does not \
             reach a loopback capture of its tree"
        ),
        None => println!(
            "bit-transparent: NO ({:.2} % off the 24-bit grid) — Sound Check, \
             volume below 100 %, or a track at another rate than the device",
            analysis.off_grid_ratio() * 100.0
        ),
    }
    if analysis.effective_depth().is_none() && !analysis.is_silent() {
        match resona_core::analysis::estimate_gain(&all_samples) {
            Some(gain) => println!(
                "constant gain ({:.1} % of samples on a regular step): {:.2} dB if \
                 the source is 16-bit, {:.2} dB if 24-bit",
                gain.score * 100.0,
                gain.gain_db_16(),
                gain.gain_db_24(),
            ),
            None => println!("no constant gain: resampled or processed"),
        }
        // How far off, second by second: a processed signal sits about 0.25
        // step from the grid, a float rounding far below one step.
        println!("distance to the 24-bit grid per second (in 24-bit steps):");
        let second = device.sample_rate as usize * usize::from(CAPTURE_CHANNELS);
        for (index, block) in all_samples.chunks(second).enumerate() {
            let r = resona_core::analysis::grid_residual(block);
            println!(
                "  {index:>3} s  off {:>5.1} %   mean {:.4}   16-bit mean {:.4}",
                r.off_grid_24 * 100.0,
                r.mean_lsb_24,
                r.mean_lsb_16,
            );
        }
        // Where the off-grid samples are: only near full scale points to a
        // peak limiter, everywhere to a gain or a resampler.
        println!("off the 24-bit grid, by amplitude:");
        let bounds = [0.0_f32, 0.25, 0.5, 0.75, 0.9, 0.97, 1.01];
        for range in bounds.windows(2) {
            let (low, high) = (range[0], range[1]);
            let (mut total, mut off) = (0_u64, 0_u64);
            for &s in all_samples
                .iter()
                .filter(|s| (low..high).contains(&s.abs()))
            {
                total += 1;
                let grid = resona_core::analysis::quantize(s, 24) as f32 / 8_388_608.0;
                off += u64::from(grid != s);
            }
            if total > 0 {
                println!(
                    "  |x| {low:.2}..{high:.2}  {total:>9} samples  {:>6.2} % off",
                    off as f64 * 100.0 / total as f64
                );
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
mod probe {
    use std::collections::HashSet;
    use std::ffi::OsStr;

    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager as SessionManager;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    };
    use windows::core::BOOL;

    pub struct Tree {
        pub root: u32,
        /// `(pid, parent pid, name)` of the root and every descendant.
        pub members: Vec<(u32, u32, String)>,
    }

    /// The root `AppleMusic.exe` and all its descendants, whatever their
    /// name (WebView2, helpers).
    pub fn apple_music_tree() -> Option<Tree> {
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        )
        .ok()?;
        let apple: HashSet<u32> = system
            .processes_by_exact_name(OsStr::new("AppleMusic.exe"))
            .map(|p| p.pid().as_u32())
            .collect();
        let root = system
            .processes_by_exact_name(OsStr::new("AppleMusic.exe"))
            .find(|p| {
                p.parent()
                    .is_none_or(|parent| !apple.contains(&parent.as_u32()))
            })?
            .pid()
            .as_u32();
        let mut members = vec![(root, 0, "AppleMusic.exe".to_owned())];
        let mut frontier = vec![root];
        while let Some(parent) = frontier.pop() {
            for (pid, process) in system.processes() {
                if process.parent().map(|p| p.as_u32()) == Some(parent) {
                    let pid = pid.as_u32();
                    members.push((pid, parent, process.name().to_string_lossy().into_owned()));
                    frontier.push(pid);
                }
            }
        }
        Some(Tree { root, members })
    }

    pub fn pid_of(name: &str) -> Option<u32> {
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        )
        .ok()?;
        let pid = system
            .processes_by_exact_name(OsStr::new(name))
            .next()?
            .pid()
            .as_u32();
        Some(pid)
    }

    /// Gives the agent its output device back when dropped, if `armed`.
    pub struct RouteBack {
        pub pid: u32,
        pub previous: Option<String>,
        pub armed: bool,
    }

    impl Drop for RouteBack {
        fn drop(&mut self) {
            if self.armed {
                let result =
                    resona_core::routing::route_spotify(self.pid, self.previous.as_deref());
                println!("routed back to {:?}: {result:?}", self.previous);
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Session {
        pub app_id: String,
        pub status: String,
        pub title: String,
        pub artist: String,
        pub album: String,
        pub album_artist: String,
        pub track_number: i32,
        pub genres: Vec<String>,
        pub position_ms: Option<i64>,
        pub duration_ms: i64,
    }

    pub fn smtc_sessions() -> anyhow::Result<Vec<Session>> {
        let manager = SessionManager::RequestAsync()?.join()?;
        let mut sessions = Vec::new();
        for session in manager.GetSessions()? {
            let properties = session.TryGetMediaPropertiesAsync()?.join()?;
            let timeline = session.GetTimelineProperties()?;
            sessions.push(Session {
                app_id: session.SourceAppUserModelId()?.to_string(),
                status: format!("{:?}", session.GetPlaybackInfo()?.PlaybackStatus()?),
                title: properties.Title()?.to_string(),
                artist: properties.Artist()?.to_string(),
                album: properties.AlbumTitle()?.to_string(),
                album_artist: properties.AlbumArtist()?.to_string(),
                track_number: properties.TrackNumber()?,
                genres: properties
                    .Genres()?
                    .into_iter()
                    .map(|g| g.to_string())
                    .collect(),
                position_ms: Some(timeline.Position()?.Duration / 10_000),
                duration_ms: timeline.EndTime()?.Duration / 10_000,
            });
        }
        Ok(sessions)
    }

    pub struct AudioSession {
        pub device: String,
        pub pid: u32,
        pub process: String,
        pub state: String,
        pub peak: f32,
    }

    /// Every audio session on every active render device, with the highest
    /// peak its meter showed over one second.
    pub fn all_audio_sessions() -> anyhow::Result<Vec<AudioSession>> {
        use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
        use windows::Win32::Media::Audio::{
            DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
            MMDeviceEnumerator, eRender,
        };
        use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
        use windows::core::Interface;

        let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        )?;
        let mut meters = Vec::new();
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            for d in 0..devices.GetCount()? {
                let device = devices.Item(d)?;
                let device_id = device.GetId()?.to_string()?;
                let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
                let sessions = manager.GetSessionEnumerator()?;
                for s in 0..sessions.GetCount()? {
                    let control = sessions.GetSession(s)?;
                    let control2: IAudioSessionControl2 = control.cast()?;
                    let pid = control2.GetProcessId().unwrap_or(0);
                    let process = system.process(sysinfo::Pid::from_u32(pid)).map_or_else(
                        || "?".to_owned(),
                        |p| p.name().to_string_lossy().into_owned(),
                    );
                    let state = format!("{:?}", control.GetState()?);
                    let meter: IAudioMeterInformation = control.cast()?;
                    meters.push((
                        AudioSession {
                            device: device_id.clone(),
                            pid,
                            process,
                            state,
                            peak: 0.0,
                        },
                        meter,
                    ));
                }
            }
            for _ in 0..20 {
                for (session, meter) in &mut meters {
                    session.peak = session.peak.max(meter.GetPeakValue().unwrap_or(0.0));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        Ok(meters.into_iter().map(|(s, _)| s).collect())
    }

    pub struct Window {
        pub pid: u32,
        pub class: String,
        pub title: String,
        pub visible: bool,
    }

    /// Every top-level window owned by `pids`, titled or not.
    pub fn windows_of(pids: &HashSet<u32>) -> Vec<Window> {
        let mut search = Search {
            pids,
            found: Vec::new(),
        };
        let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut search as isize)) };
        search.found
    }

    struct Search<'a> {
        pids: &'a HashSet<u32>,
        found: Vec<Window>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `&mut Search` passed to `EnumWindows`,
        // alive for the whole synchronous enumeration.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
        if !search.pids.contains(&pid) {
            return true.into();
        }
        let mut class = [0u16; 128];
        let class_len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        let mut title = [0u16; 512];
        let title_len = unsafe { GetWindowTextW(hwnd, &mut title) } as usize;
        search.found.push(Window {
            pid,
            class: String::from_utf16_lossy(&class[..class_len]),
            title: String::from_utf16_lossy(&title[..title_len]),
            visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
        });
        true.into()
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("apple_music_probe needs Windows.");
}

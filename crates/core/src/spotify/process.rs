//! Locating the music app's processes: the one its window belongs to, and
//! the one that renders its sound (not the same for Apple Music).

use std::collections::HashSet;
use std::ffi::OsStr;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

use crate::provider::Provider;

/// The app's running processes, found by name.
#[derive(Debug, Clone)]
pub struct AppProcesses {
    /// The [`Provider::process_name`] whose parent is not one of them.
    pub root: u32,
    /// All of them, used to find the app's windows.
    pub all: HashSet<u32>,
    /// The root [`Provider::audio_process_name`]: the capture targets it
    /// with its tree (Spotify is a Chromium app whose audio is rendered by
    /// a child process, which Spotify may restart). `None` when Apple
    /// Music's library agent is not running (yet).
    pub audio_root: Option<u32>,
    /// All the processes of that name: their audio sessions are the app's
    /// in the Windows mixer.
    pub audio_all: HashSet<u32>,
}

impl AppProcesses {
    pub fn find(provider: Provider) -> Option<Self> {
        // A failed process listing reads as "not running": callers poll
        // this, so the next call simply tries again.
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        )
        .ok()?;
        let (root, all) = family(&system, provider.process_name())?;
        let (audio_root, audio_all) = if provider.audio_process_name() == provider.process_name() {
            (Some(root), all.clone())
        } else {
            family(&system, provider.audio_process_name())
                .map_or((None, HashSet::new()), |(root, all)| (Some(root), all))
        };
        Some(Self {
            root,
            all,
            audio_root,
            audio_all,
        })
    }
}

/// The processes named `name` and the root one among them.
fn family(system: &System, name: &str) -> Option<(u32, HashSet<u32>)> {
    let name = OsStr::new(name);
    let all: HashSet<u32> = system
        .processes_by_exact_name(name)
        .map(|p| p.pid().as_u32())
        .collect();
    let root = system
        .processes_by_exact_name(name)
        .find(|p| {
            p.parent()
                .is_none_or(|parent| !all.contains(&parent.as_u32()))
        })?
        .pid()
        .as_u32();
    Some((root, all))
}

/// PID of the process to capture for `provider`, see
/// [`AppProcesses::audio_root`].
pub fn find_audio_pid(provider: Provider) -> Option<u32> {
    AppProcesses::find(provider).and_then(|p| p.audio_root)
}

/// PID of the root `Spotify.exe`.
pub fn find_root_pid() -> Option<u32> {
    AppProcesses::find(Provider::Spotify).map(|p| p.root)
}

/// Whether process `pid` still runs. Apple Music's library agent is
/// restarted by COM (seen when its streaming quality changed): a capture of
/// the old one hears nothing.
/// A failed process listing answers yes: dropping a working capture would
/// be worse than keeping a dead one a little longer.
pub fn is_running(pid: u32) -> bool {
    let Ok(mut system) = System::new() else {
        return true;
    };
    let pid = Pid::from_u32(pid);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system.process(pid).is_some()
}

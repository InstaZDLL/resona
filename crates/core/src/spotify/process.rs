//! Locating the Spotify desktop client's processes.

use std::collections::HashSet;
use std::ffi::OsStr;

use sysinfo::{ProcessRefreshKind, RefreshKind, System};

const PROCESS_NAME: &str = "Spotify.exe";

/// Every running `Spotify.exe`, and the root one among them.
#[derive(Debug, Clone)]
pub struct SpotifyProcesses {
    /// The `Spotify.exe` whose parent is not Spotify. Capture targets it
    /// with `include_tree`: Spotify is a Chromium app and audio is rendered
    /// by a child process, which Spotify may restart.
    pub root: u32,
    /// All Spotify PIDs, used to find its windows.
    pub all: HashSet<u32>,
}

impl SpotifyProcesses {
    pub fn find() -> Option<Self> {
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        );
        let name = OsStr::new(PROCESS_NAME);
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
        Some(Self { root, all })
    }
}

/// PID of the root `Spotify.exe`, see [`SpotifyProcesses::root`].
pub fn find_root_pid() -> Option<u32> {
    SpotifyProcesses::find().map(|p| p.root)
}

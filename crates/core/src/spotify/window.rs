//! Spotify's main window title, via `EnumWindows`.

use std::collections::HashSet;

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId,
};
use windows::core::BOOL;

/// Chromium top-level window class. Spotify also owns IME and message-only
/// windows with titles of their own, which this filters out.
const MAIN_WINDOW_CLASS_PREFIX: &str = "Chrome_WidgetWin";

pub struct SpotifyWindow {
    pub hwnd: HWND,
    pub title: String,
}

/// The titled Chromium window of any of `pids`, visible or not: the title
/// stays up to date while Spotify is minimized to the tray, unlike .NET's
/// `MainWindowTitle` which the C# version relied on.
pub fn find_main_window(pids: &HashSet<u32>) -> Option<SpotifyWindow> {
    let mut search = Search { pids, found: None };
    // EnumWindows reports the early stop (callback returning FALSE) as an
    // error; there is nothing else to report.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut search as isize)) };
    search.found
}

struct Search<'a> {
    pids: &'a HashSet<u32>,
    found: Option<SpotifyWindow>,
}

unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Search` passed to `EnumWindows` above,
    // alive for the whole synchronous enumeration.
    let search = unsafe { &mut *(lparam.0 as *mut Search) };

    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
    if !search.pids.contains(&pid) {
        return true.into();
    }
    let mut class = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
    if !String::from_utf16_lossy(&class[..len]).starts_with(MAIN_WINDOW_CLASS_PREFIX) {
        return true.into();
    }
    let mut title = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut title) } as usize;
    if len == 0 {
        return true.into();
    }
    search.found = Some(SpotifyWindow {
        hwnd,
        title: String::from_utf16_lossy(&title[..len]),
    });
    false.into()
}

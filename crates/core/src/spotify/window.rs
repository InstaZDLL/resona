//! The music app's main window title, via `EnumWindows`.

use std::collections::HashSet;

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId,
};
use windows::core::BOOL;

pub struct AppWindow {
    pub hwnd: HWND,
    pub title: String,
}

/// The titled window of any of `pids` whose class starts with
/// `class_prefix` ([`crate::provider::Provider::window_class_prefix`]),
/// visible or not: the title stays up to date while Spotify is minimized
/// to the tray, unlike .NET's `MainWindowTitle` which the C# version relied
/// on. The class filters out the IME and message-only windows the app also
/// owns, titled too.
pub fn find_main_window(pids: &HashSet<u32>, class_prefix: &str) -> Option<AppWindow> {
    let mut search = Search {
        pids,
        class_prefix,
        found: None,
    };
    // EnumWindows reports the early stop (callback returning FALSE) as an
    // error; there is nothing else to report.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut search as isize)) };
    search.found
}

struct Search<'a> {
    pids: &'a HashSet<u32>,
    class_prefix: &'a str,
    found: Option<AppWindow>,
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
    if !String::from_utf16_lossy(&class[..len]).starts_with(search.class_prefix) {
        return true.into();
    }
    let mut title = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut title) } as usize;
    if len == 0 {
        return true.into();
    }
    search.found = Some(AppWindow {
        hwnd,
        title: String::from_utf16_lossy(&title[..len]),
    });
    false.into()
}

//! Focus navigation — cycle through tilable windows
//! Exposed to keybinds via `focus_next()` / `focus_prev()` etc and Rhai `focus_next()`
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow};

use crate::manager::collect_windows;
use crate::taskbar;

/// Get tilable windows in tiling order (same as manager)
fn tilable_windows() -> Vec<HWND> {
    let tb = taskbar::get_taskbar_hwnd();
    let mut wins = collect_windows(tb);
    // filter floating as manager does (so focus doesn't jump to floating? but allow floating focus via config)
    wins.retain(|hwnd| !crate::rules::is_floating(*hwnd));
    // filter virtual desktop if enabled
    wins.retain(|hwnd| crate::virtual_desktop::is_on_current_desktop(*hwnd));
    wins
}

fn set_foreground(hwnd: HWND) {
    unsafe {
        // AttachThreadInput dance to allow SetForegroundWindow from background
        let fg = GetForegroundWindow();
        let mut fg_pid = 0;
        let mut cur_pid = 0;
        let fg_tid = GetWindowThreadProcessId(fg, Some(&mut fg_pid));
        let cur_tid = GetWindowThreadProcessId(hwnd, Some(&mut cur_pid));
        let cur_thread = windows::Win32::System::Threading::GetCurrentThreadId();
        // try attach
        let attached = if fg_tid != cur_tid && fg_tid != 0 {
            AttachThreadInput(fg_tid, cur_thread, true).as_bool()
        } else { false };
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        if attached {
            let _ = AttachThreadInput(fg_tid, cur_thread, false);
        }
        println!("[focus] -> {:?} {}", hwnd.0, crate::util::get_window_title(hwnd));
    }
}

pub fn focus_next() {
    let wins = tilable_windows();
    if wins.is_empty() { return; }
    let fg = unsafe { GetForegroundWindow() };
    // find current index
    let idx = wins.iter().position(|w| w.0 == fg.0).unwrap_or(usize::MAX);
    let next = if idx == usize::MAX || idx + 1 >= wins.len() { 0 } else { idx + 1 };
    set_foreground(wins[next]);
}

pub fn focus_prev() {
    let wins = tilable_windows();
    if wins.is_empty() { return; }
    let fg = unsafe { GetForegroundWindow() };
    let idx = wins.iter().position(|w| w.0 == fg.0).unwrap_or(0);
    let prev = if idx == 0 { wins.len() - 1 } else { idx - 1 };
    set_foreground(wins[prev]);
}

pub fn focus_direction(dir: &str) {
    // simple: left/right maps to prev/next; up/down also
    match dir.to_lowercase().as_str() {
        "left" | "up" | "prev" | "h" | "k" => focus_prev(),
        "right" | "down" | "next" | "l" | "j" => focus_next(),
        _ => focus_next(),
    }
}

/// Called from scripting: `focus_next()`
pub fn focus_window_by_title_substr(substr: &str) {
    let wins = tilable_windows();
    for hwnd in wins {
        let title = crate::util::get_window_title(hwnd);
        if title.to_lowercase().contains(&substr.to_lowercase()) {
            set_foreground(hwnd);
            break;
        }
    }
}

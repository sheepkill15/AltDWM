//! Focus navigation — cycle through tilable windows
//! Exposed to keybinds via `focus_next()` / `focus_prev()` etc and Rhai `focus_next()`
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow, SetWindowPos, HWND_TOP, SWP_NOSIZE, SWP_NOZORDER, SWP_FRAMECHANGED};

use crate::manager::collect_windows;
use crate::taskbar;

static RUNTIME_FLOATING: LazyLock<Mutex<HashSet<isize>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn is_runtime_floating(hwnd: HWND) -> bool {
    RUNTIME_FLOATING.lock().unwrap_or_else(|e| e.into_inner()).contains(&(hwnd.0 as isize))
}
pub fn toggle_floating_focused() {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() { return; }
    let key = hwnd.0 as isize;
    let mut set = RUNTIME_FLOATING.lock().unwrap_or_else(|e| e.into_inner());
    if set.contains(&key) {
        set.remove(&key);
        println!("[focus] untiled (floating off) {:?}", hwnd.0);
    } else {
        set.insert(key);
        println!("[focus] floated {:?}", hwnd.0);
    }
    crate::request_retile();
}
pub fn move_focused_to_monitor(dir: &str) {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() { return; }
    // get all monitors
    let mons = crate::manager::get_all_monitors();
    if mons.len() <= 1 { return; }
    let cur = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let idx = mons.iter().position(|&h| h.0 == cur.0).unwrap_or(0);
    let target_idx = match dir.to_lowercase().as_str() {
        "next" | "right" | "down" | "l" | "j" => (idx + 1) % mons.len(),
        "prev" | "left" | "up" | "h" | "k" => (idx + mons.len() - 1) % mons.len(),
        _ => (idx + 1) % mons.len(),
    };
    let target = mons[target_idx];
    // center on target monitor work area
    unsafe {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(target, &mut mi as *mut _ as *mut _).as_bool() {
            let work = mi.rcWork;
            let mut rect = RECT::default();
            if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect).is_ok() {
                let w = rect.right - rect.left;
                let h = rect.bottom - rect.top;
                let x = work.left + (work.right - work.left - w) / 2;
                let y = work.top + (work.bottom - work.top - h) / 2;
                let _ = SetWindowPos(hwnd, Some(HWND_TOP), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
                // also set foreground
                set_foreground(hwnd);
                crate::request_retile();
                println!("[focus] move {:?} to monitor {} (0x{:x})", hwnd.0, target_idx+1, target.0 as usize);
            }
        }
    }
}

/// Get tilable windows in tiling order (same as manager)
fn tilable_windows() -> Vec<HWND> {
    let tb = taskbar::get_taskbar_hwnd();
    let mut wins = collect_windows(tb);
    wins.retain(|hwnd| !crate::rules::is_floating(*hwnd) && !is_runtime_floating(*hwnd));
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

pub fn focus_hwnd(hwnd: HWND) {
    set_foreground(hwnd);
}

//! Native AltDWM command center.
//!
//! This is deliberately small and dependency-free: it gives the shell a useful,
//! discoverable surface while keeping the existing Win32/GDI architecture.

use std::sync::{LazyLock, Mutex};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetMonitorInfoW,
    InvalidateRect, MonitorFromWindow, RoundRect, SelectObject, SetBkMode, SetTextColor, TextOutW,
    HDC, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, VK_BACK, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowRect,
    SetForegroundWindow, SetWindowPos, ShowWindow, HMENU, HWND_TOPMOST, SWP_SHOWWINDOW, SW_SHOW,
    WM_CHAR, WM_CLOSE, WM_DESTROY, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_PAINT,
    WS_EX_APPWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

const WIDTH: i32 = 520;
const HEIGHT: i32 = 548;
const ITEM_TOP: i32 = 178;
const ITEM_HEIGHT: i32 = 58;
const MAX_VISIBLE_ITEMS: usize = 5;
const KEY_BACK: u16 = VK_BACK.0;
const KEY_DOWN: u16 = VK_DOWN.0;
const KEY_ESCAPE: u16 = VK_ESCAPE.0;
const KEY_RETURN: u16 = VK_RETURN.0;
const KEY_UP: u16 = VK_UP.0;

#[derive(Clone, Copy)]
enum CommandAction {
    Launch(&'static str),
    OpenConfig,
    Reload,
    ToggleTiling,
    Layout(&'static str),
}

#[derive(Clone, Copy)]
struct CommandItem {
    badge: &'static str,
    title: &'static str,
    description: &'static str,
    keywords: &'static str,
    action: CommandAction,
}

const ITEMS: &[CommandItem] = &[
    CommandItem {
        badge: "FI",
        title: "Files",
        description: "Browse folders and files",
        keywords: "explorer folders documents",
        action: CommandAction::Launch("explorer.exe"),
    },
    CommandItem {
        badge: ">_",
        title: "Terminal",
        description: "Open Windows Terminal",
        keywords: "console shell powershell wt",
        action: CommandAction::Launch("wt.exe"),
    },
    CommandItem {
        badge: "CF",
        title: "Configure AltDWM",
        description: "Edit the active configuration",
        keywords: "settings preferences toml theme keys",
        action: CommandAction::OpenConfig,
    },
    CommandItem {
        badge: "RE",
        title: "Reload configuration",
        description: "Apply saved settings without restarting",
        keywords: "refresh config apply",
        action: CommandAction::Reload,
    },
    CommandItem {
        badge: "PA",
        title: "Pause or resume tiling",
        description: "Temporarily release managed windows",
        keywords: "toggle stop start floating",
        action: CommandAction::ToggleTiling,
    },
    CommandItem {
        badge: "MS",
        title: "Master stack layout",
        description: "Primary window with a secondary stack",
        keywords: "layout master stack tile",
        action: CommandAction::Layout("MasterStack"),
    },
    CommandItem {
        badge: "GR",
        title: "Grid layout",
        description: "Arrange windows in a balanced grid",
        keywords: "layout columns rows tile",
        action: CommandAction::Layout("Grid"),
    },
    CommandItem {
        badge: "MO",
        title: "Monocle layout",
        description: "Focus one window at a time",
        keywords: "layout fullscreen single focus",
        action: CommandAction::Layout("Monocle"),
    },
];

#[derive(Default)]
struct CenterState {
    hwnd: isize,
    query: String,
    selected: usize,
}

static STATE: LazyLock<Mutex<CenterState>> = LazyLock::new(|| Mutex::new(CenterState::default()));

fn matching_indices(query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    ITEMS
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            needle.is_empty()
                || item.title.to_lowercase().contains(&needle)
                || item.description.to_lowercase().contains(&needle)
                || item.keywords.contains(&needle)
        })
        .map(|(index, _)| index)
        .collect()
}

unsafe fn fill_rect(hdc: HDC, rect: RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    FillRect(hdc, &rect, brush);
    let _ = DeleteObject(brush.into());
}

unsafe fn fill_round_rect(hdc: HDC, rect: RECT, radius: i32, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let old = SelectObject(hdc, brush.into());
    let _ = RoundRect(
        hdc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        radius,
        radius,
    );
    let _ = SelectObject(hdc, old);
    let _ = DeleteObject(brush.into());
}

unsafe fn text(hdc: HDC, value: &str, x: i32, y: i32, color: COLORREF) {
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, color);
    let wide: Vec<u16> = value.encode_utf16().collect();
    let _ = TextOutW(hdc, x, y, &wide);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let theme = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .theme
        .clone();

    fill_rect(hdc, client, theme.panel_bg("top"));

    let title_font = crate::theme::get_cached_font_variant(&theme, 23, 600);
    let body_font = crate::theme::get_cached_font_variant(&theme, 14, 400);
    let small_font = crate::theme::get_cached_font_variant(&theme, 12, 400);
    let badge_font = crate::theme::get_cached_font_variant(&theme, 12, 600);

    let previous = SelectObject(hdc, title_font.into());
    text(hdc, "AltDWM", 28, 22, theme.text_color());
    let _ = SelectObject(hdc, small_font.into());
    text(
        hdc,
        "COMMAND CENTER  •  TYPE TO FILTER",
        29,
        54,
        theme.text_dim_color(),
    );

    let query = STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .query
        .clone();
    let search_rect = RECT {
        left: 26,
        top: 88,
        right: client.right - 26,
        bottom: 142,
    };
    fill_round_rect(hdc, search_rect, 14, theme.surface_color());
    let _ = SelectObject(hdc, body_font.into());
    text(hdc, "⌕", 44, 105, theme.accent_active_color());
    let search_text = if query.is_empty() {
        "Search apps, layouts, and actions…"
    } else {
        &query
    };
    let search_color = if query.is_empty() {
        theme.text_dim_color()
    } else {
        theme.text_color()
    };
    text(hdc, search_text, 76, 105, search_color);

    let state = STATE.lock().unwrap_or_else(|error| error.into_inner());
    let matches = matching_indices(&state.query);
    let selected = state.selected.min(matches.len().saturating_sub(1));
    drop(state);

    let visible = matches.iter().take(MAX_VISIBLE_ITEMS);
    for (row, item_index) in visible.enumerate() {
        let item = ITEMS[*item_index];
        let top = ITEM_TOP + row as i32 * ITEM_HEIGHT;
        let row_rect = RECT {
            left: 18,
            top,
            right: client.right - 18,
            bottom: top + ITEM_HEIGHT - 6,
        };
        if row == selected {
            fill_round_rect(hdc, row_rect, 12, theme.surface_hover_color());
        }
        let badge_rect = RECT {
            left: 30,
            top: top + 8,
            right: 68,
            bottom: top + 46,
        };
        fill_round_rect(
            hdc,
            badge_rect,
            12,
            if row == selected {
                theme.accent_active_color()
            } else {
                theme.surface_color()
            },
        );
        let _ = SelectObject(hdc, badge_font.into());
        text(hdc, item.badge, 40, top + 19, theme.text_color());
        let _ = SelectObject(hdc, body_font.into());
        text(hdc, item.title, 82, top + 9, theme.text_color());
        let _ = SelectObject(hdc, small_font.into());
        text(hdc, item.description, 82, top + 31, theme.text_dim_color());
    }

    if matches.is_empty() {
        let _ = SelectObject(hdc, body_font.into());
        text(
            hdc,
            "No matching commands",
            30,
            ITEM_TOP + 18,
            theme.text_color(),
        );
        let _ = SelectObject(hdc, small_font.into());
        text(
            hdc,
            "Try “files”, “layout”, or “reload”.",
            30,
            ITEM_TOP + 46,
            theme.text_dim_color(),
        );
    }

    let footer = RECT {
        left: 0,
        top: client.bottom - 42,
        right: client.right,
        bottom: client.bottom,
    };
    fill_rect(hdc, footer, theme.surface_color());
    let _ = SelectObject(hdc, small_font.into());
    text(
        hdc,
        "↑ ↓  SELECT     ENTER  OPEN     ESC  CLOSE",
        28,
        client.bottom - 27,
        theme.text_dim_color(),
    );
    let _ = SelectObject(hdc, previous);
    let _ = EndPaint(hwnd, &ps);
}

fn run_action(action: CommandAction) {
    match action {
        CommandAction::Launch(command) => {
            crate::scripting::dispatch_action(&format!("launch('{command}')"));
        }
        CommandAction::OpenConfig => {
            if let Some(path) = crate::CONFIG_PATH
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
            {
                let _ = std::process::Command::new("notepad.exe").arg(path).spawn();
            }
        }
        CommandAction::Reload => crate::reload_config_async(),
        CommandAction::ToggleTiling => crate::toggle_tiling(),
        CommandAction::Layout(name) => crate::set_layout_by_name(name),
    }
}

fn invoke_selected(hwnd: HWND) {
    let action = {
        let state = STATE.lock().unwrap_or_else(|error| error.into_inner());
        let matches = matching_indices(&state.query);
        matches
            .get(state.selected.min(matches.len().saturating_sub(1)))
            .map(|index| ITEMS[*index].action)
    };
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    if let Some(action) = action {
        run_action(action);
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_CHAR => {
            let character = char::from_u32(wparam.0 as u32);
            if let Some(character) = character.filter(|character| !character.is_control()) {
                let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
                if state.query.chars().count() < 64 {
                    state.query.push(character);
                    state.selected = 0;
                }
                drop(state);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            match wparam.0 as u16 {
                KEY_ESCAPE => {
                    let _ = DestroyWindow(hwnd);
                }
                KEY_RETURN => invoke_selected(hwnd),
                KEY_BACK => {
                    let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
                    state.query.pop();
                    state.selected = 0;
                    drop(state);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                KEY_UP | KEY_DOWN => {
                    let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
                    let count = matching_indices(&state.query).len().min(MAX_VISIBLE_ITEMS);
                    if count > 0 {
                        if wparam.0 as u16 == KEY_UP {
                            state.selected = state.selected.checked_sub(1).unwrap_or(count - 1);
                        } else {
                            state.selected = (state.selected + 1) % count;
                        }
                    }
                    drop(state);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            if y >= ITEM_TOP {
                let row = ((y - ITEM_TOP) / ITEM_HEIGHT) as usize;
                let count = {
                    let state = STATE.lock().unwrap_or_else(|error| error.into_inner());
                    matching_indices(&state.query).len().min(MAX_VISIBLE_ITEMS)
                };
                if row < count {
                    STATE
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .selected = row;
                    invoke_selected(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_KILLFOCUS | WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
            if state.hwnd == hwnd.0 as isize {
                state.hwnd = 0;
                state.query.clear();
                state.selected = 0;
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn ensure_class() -> Result<(), String> {
    crate::util::register_window_class(w!("AltDWM_CommandCenter"), wndproc, "Command center")
}

fn place_near(anchor: HWND) -> (i32, i32) {
    unsafe {
        let monitor = MonitorFromWindow(anchor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let mut anchor_rect = RECT::default();
        let _ = GetWindowRect(anchor, &mut anchor_rect);
        let monitor_width = info.rcWork.right - info.rcWork.left;
        let x = (anchor_rect.left + 8)
            .max(info.rcWork.left + 12)
            .min(info.rcWork.right - WIDTH - 12)
            .min(info.rcWork.left + monitor_width - WIDTH - 12);
        let anchor_center = (anchor_rect.top + anchor_rect.bottom) / 2;
        let monitor_center = (info.rcMonitor.top + info.rcMonitor.bottom) / 2;
        let y = if anchor_center > monitor_center {
            anchor_rect.top - HEIGHT - 10
        } else {
            anchor_rect.bottom + 10
        };
        (
            x,
            y.max(info.rcWork.top + 12)
                .min(info.rcWork.bottom - HEIGHT - 12),
        )
    }
}

pub fn toggle(anchor: HWND) {
    let existing = STATE.lock().unwrap_or_else(|error| error.into_inner()).hwnd;
    if existing != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(existing as *mut std::ffi::c_void));
        }
        return;
    }
    if let Err(error) = ensure_class() {
        eprintln!("[command-center] {error}");
        return;
    }
    let (x, y) = place_near(anchor);
    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_APPWINDOW,
            w!("AltDWM_CommandCenter"),
            w!("AltDWM Command Center"),
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            WIDTH,
            HEIGHT,
            Some(anchor),
            Some(HMENU(std::ptr::null_mut())),
            Some(HINSTANCE(std::ptr::null_mut())),
            None,
        )
    };
    let Ok(hwnd) = created else {
        eprintln!(
            "[command-center] CreateWindowExW failed: {:?}",
            created.err()
        );
        return;
    };
    STATE.lock().unwrap_or_else(|error| error.into_inner()).hwnd = hwnd.0 as isize;
    unsafe {
        const DWMWA_WINDOW_CORNER_PREFERENCE_RAW: i32 = 33;
        const DWMWCP_ROUND: u32 = 2;
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(DWMWA_WINDOW_CORNER_PREFERENCE_RAW),
            &corner as *const _ as _,
            size_of_val(&corner) as u32,
        );
        let border = crate::CURRENT_CONFIG
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .theme
            .border_color();
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border.0 as *const _ as _,
            size_of_val(&border.0) as u32,
        );
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            WIDTH,
            HEIGHT,
            SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
    }
}

pub fn toggle_from_keyboard() {
    let anchor = crate::panel::first_handle().unwrap_or_else(|| unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow()
    });
    if !anchor.0.is_null() {
        toggle(anchor);
    }
}

pub fn close() {
    let hwnd = STATE.lock().unwrap_or_else(|error| error.into_inner()).hwnd;
    if hwnd != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(hwnd as *mut std::ffi::c_void));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::matching_indices;

    #[test]
    fn command_search_matches_titles_descriptions_and_keywords() {
        assert!(!matching_indices("terminal").is_empty());
        assert!(!matching_indices("settings").is_empty());
        assert!(!matching_indices("balanced").is_empty());
        assert!(matching_indices("definitely-not-a-command").is_empty());
    }
}

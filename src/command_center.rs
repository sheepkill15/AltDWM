//! Native AltDWM command center.
//!
//! This is deliberately small and dependency-free: it gives the shell a useful,
//! discoverable surface while keeping the existing Win32/GDI architecture.

use std::sync::{LazyLock, Mutex};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromWindow, HDC, HFONT,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
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

// All measurements below are device-independent pixels at 96 DPI, scaled for
// the display the window opens on. They used to be raw pixels, so the panel was
// physically half-size on a 200% display and its text did not fit its rows.
const WIDTH: i32 = 520;
const HEIGHT: i32 = 548;
const ITEM_TOP: i32 = 178;
const ITEM_HEIGHT: i32 = 58;
const EDGE: i32 = 26;
const HEADER_TOP: i32 = 18;
const SEARCH_TOP: i32 = 88;
const SEARCH_HEIGHT: i32 = 54;
const FOOTER_HEIGHT: i32 = 42;

/// DPI scale of the display this window is on.
fn scale(hwnd: HWND) -> f32 {
    crate::ui::scale_for_window(hwnd)
}
const MAX_VISIBLE_ITEMS: usize = 5;
const KEY_BACK: u16 = VK_BACK.0;
const KEY_DOWN: u16 = VK_DOWN.0;
const KEY_ESCAPE: u16 = VK_ESCAPE.0;
const KEY_RETURN: u16 = VK_RETURN.0;
const KEY_UP: u16 = VK_UP.0;

#[derive(Clone, Copy)]
enum CommandAction {
    ShowShortcuts,
    Launch(&'static str),
    OpenConfig,
    Reload,
    ToggleTiling,
    Layout(&'static str),
    QuickSettings,
    RefreshApps,
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
        badge: "KB",
        title: "Active shortcuts",
        description: "View every registered AltDWM shortcut",
        keywords: "keyboard keys keybinds hotkeys help controls",
        action: CommandAction::ShowShortcuts,
    },
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
    CommandItem {
        badge: "QS",
        title: "Quick settings",
        description: "Volume, brightness, network, and battery",
        keywords: "sound audio wifi bluetooth power display input language",
        action: CommandAction::QuickSettings,
    },
    CommandItem {
        badge: "AP",
        title: "Rescan applications",
        description: "Rebuild the app index after installing something",
        keywords: "apps refresh reindex search launcher",
        action: CommandAction::RefreshApps,
    },
];

/// One row of the result list: either a built-in command or an installed
/// application. Commands are offered first because they are few and named
/// exactly; applications fill the rest of the list.
enum ResultRow {
    Command(usize),
    App(crate::apps::AppEntry),
}

impl ResultRow {
    fn badge(&self) -> String {
        match self {
            ResultRow::Command(index) => ITEMS[*index].badge.to_string(),
            ResultRow::App(entry) => entry
                .name
                .chars()
                .filter(|c| c.is_alphanumeric())
                .take(2)
                .collect::<String>()
                .to_uppercase(),
        }
    }

    fn title(&self) -> String {
        match self {
            ResultRow::Command(index) => ITEMS[*index].title.to_string(),
            ResultRow::App(entry) => entry.name.clone(),
        }
    }

    fn description(&self) -> String {
        match self {
            ResultRow::Command(index) => ITEMS[*index].description.to_string(),
            ResultRow::App(entry) => describe_app(entry),
        }
    }
}

/// A short, honest hint about what an indexed entry actually is.
fn describe_app(entry: &crate::apps::AppEntry) -> String {
    if entry.id.contains('!') {
        return "Store app".into();
    }
    if let Some(scheme) = entry.id.split_once("://") {
        return format!("{} link", scheme.0);
    }
    let tail = entry.id.rsplit(['\\', '/']).next().unwrap_or(&entry.id);
    if tail.eq_ignore_ascii_case(&entry.name) {
        return "Application".into();
    }
    tail.to_string()
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CenterView {
    #[default]
    Commands,
    Shortcuts,
}

#[derive(Default)]
struct CenterState {
    hwnd: isize,
    query: String,
    selected: usize,
    view: CenterView,
}

static STATE: LazyLock<Mutex<CenterState>> = LazyLock::new(|| Mutex::new(CenterState::default()));

/// Repaint the command center if it is open. Called when the application index
/// finishes building, so results appear as soon as they are available.
pub fn invalidate() {
    let hwnd = STATE.lock().unwrap_or_else(|error| error.into_inner()).hwnd;
    if hwnd != 0 {
        unsafe {
            let _ = InvalidateRect(Some(HWND(hwnd as *mut std::ffi::c_void)), None, false);
        }
    }
}

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

/// The rows to show for `query`: matching built-in commands, then the best
/// matching installed applications, capped at what the list can display.
fn results(query: &str) -> Vec<ResultRow> {
    let mut rows: Vec<ResultRow> = matching_indices(query)
        .into_iter()
        .map(ResultRow::Command)
        .collect();
    // With no query the palette shows its own commands; applications appear as
    // soon as the user starts typing, which is when they are being searched for.
    if !query.trim().is_empty() && rows.len() < MAX_VISIBLE_ITEMS {
        let room = MAX_VISIBLE_ITEMS - rows.len();
        rows.extend(
            crate::apps::search(query, room)
                .into_iter()
                .map(ResultRow::App),
        );
    }
    rows.truncate(MAX_VISIBLE_ITEMS);
    rows
}

fn fill_rect(hdc: HDC, rect: RECT, color: COLORREF) {
    crate::ui::fill_rect(hdc, &rect, color);
}

fn fill_round_rect(hdc: HDC, rect: RECT, radius: i32, color: COLORREF) {
    crate::ui::fill_round_rect(hdc, &rect, radius, color);
}

/// One line, vertically centred in `rect` and ellipsised to fit. Every label in
/// this window goes through here, which is what keeps the rows aligned; they
/// were previously placed with per-call constants like `top + 9`, `top + 19`,
/// and `top + 31`.
fn label(hdc: HDC, rect: RECT, value: &str, font: HFONT, color: COLORREF) {
    crate::ui::draw_label(hdc, &rect, value, font, color);
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
    let scale = scale(hwnd);
    let px = |value: i32| crate::ui::px(value, scale);
    let font = |size: i32, weight: i32| {
        crate::theme::get_cached_font_variant(&theme, px(size), weight)
    };

    fill_rect(hdc, client, theme.panel_bg("top"));

    let title_font = font(23, 600);
    let body_font = font(14, 400);
    let small_font = font(12, 400);
    let badge_font = font(12, 600);

    let edge = px(EDGE);
    let header = RECT {
        left: edge,
        top: px(HEADER_TOP),
        right: client.right - edge,
        bottom: px(HEADER_TOP) + px(34),
    };
    label(hdc, header, "AltDWM", title_font, theme.text_color());
    let view = STATE.lock().unwrap_or_else(|error| error.into_inner()).view;
    let subtitle = RECT {
        top: header.bottom,
        bottom: header.bottom + px(22),
        ..header
    };
    label(
        hdc,
        subtitle,
        if view == CenterView::Shortcuts {
            "ACTIVE SHORTCUTS"
        } else {
            "COMMAND CENTER  •  TYPE TO FILTER"
        },
        small_font,
        theme.text_dim_color(),
    );

    if view == CenterView::Shortcuts {
        paint_shortcuts(hdc, client, &theme, scale, body_font, small_font);
        let _ = EndPaint(hwnd, &ps);
        return;
    }

    let query = STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .query
        .clone();
    let search_rect = RECT {
        left: edge,
        top: px(SEARCH_TOP),
        right: client.right - edge,
        bottom: px(SEARCH_TOP) + px(SEARCH_HEIGHT),
    };
    fill_round_rect(hdc, search_rect, px(14), theme.surface_color());
    // A small drawn caret instead of `⌕`, which is not in Segoe UI and rendered
    // as a fallback glyph or a missing-character box.
    let caret_side = px(9);
    let caret_left = search_rect.left + px(18);
    let caret_top = search_rect.top + (search_rect.bottom - search_rect.top - caret_side) / 2;
    let caret = RECT {
        left: caret_left,
        top: caret_top,
        right: caret_left + caret_side,
        bottom: caret_top + caret_side,
    };
    fill_round_rect(hdc, caret, caret_side / 2, theme.accent_active_color());
    let search_text = if !query.is_empty() {
        query.as_str()
    } else if crate::apps::is_ready() {
        "Search applications, layouts, and actions…"
    } else {
        "Indexing applications…"
    };
    let search_color = if query.is_empty() {
        theme.text_dim_color()
    } else {
        theme.text_color()
    };
    label(
        hdc,
        RECT {
            left: caret.right + px(12),
            right: search_rect.right - px(12),
            ..search_rect
        },
        search_text,
        body_font,
        search_color,
    );

    let state = STATE.lock().unwrap_or_else(|error| error.into_inner());
    let rows = results(&state.query);
    let selected = state.selected.min(rows.len().saturating_sub(1));
    drop(state);

    let item_height = px(ITEM_HEIGHT);
    for (row, item) in rows.iter().enumerate() {
        let top = px(ITEM_TOP) + row as i32 * item_height;
        let row_rect = RECT {
            left: px(18),
            top,
            right: client.right - px(18),
            bottom: top + item_height - px(6),
        };
        if row == selected {
            fill_round_rect(hdc, row_rect, px(12), theme.surface_hover_color());
        }
        let badge_side = row_rect.bottom - row_rect.top - px(8);
        let badge_rect = RECT {
            left: row_rect.left + px(12),
            top: row_rect.top + px(4),
            right: row_rect.left + px(12) + badge_side,
            bottom: row_rect.top + px(4) + badge_side,
        };
        fill_round_rect(
            hdc,
            badge_rect,
            px(12),
            if row == selected {
                theme.accent_active_color()
            } else {
                theme.surface_color()
            },
        );
        label(
            hdc,
            RECT {
                left: badge_rect.left + px(10),
                ..badge_rect
            },
            &item.badge(),
            badge_font,
            theme.text_color(),
        );
        let text_left = badge_rect.right + px(14);
        let split = row_rect.top + (row_rect.bottom - row_rect.top) / 2;
        label(
            hdc,
            RECT {
                left: text_left,
                top: row_rect.top + px(4),
                right: row_rect.right - px(12),
                bottom: split,
            },
            &item.title(),
            body_font,
            theme.text_color(),
        );
        label(
            hdc,
            RECT {
                left: text_left,
                top: split,
                right: row_rect.right - px(12),
                bottom: row_rect.bottom - px(4),
            },
            &item.description(),
            small_font,
            theme.text_dim_color(),
        );
    }

    if rows.is_empty() {
        let empty = RECT {
            left: px(30),
            top: px(ITEM_TOP),
            right: client.right - px(30),
            bottom: px(ITEM_TOP) + px(28),
        };
        label(
            hdc,
            empty,
            "No matches",
            body_font,
            theme.text_color(),
        );
        let hint = if crate::apps::is_ready() {
            format!(
                "Searched {} commands and {} applications.",
                ITEMS.len(),
                crate::apps::count()
            )
        } else {
            "Still indexing applications…".to_string()
        };
        label(
            hdc,
            RECT {
                top: empty.bottom,
                bottom: empty.bottom + px(24),
                ..empty
            },
            &hint,
            small_font,
            theme.text_dim_color(),
        );
    }

    paint_footer(
        hdc,
        client,
        &theme,
        scale,
        small_font,
        "↑ ↓  SELECT     ENTER  OPEN     ESC  CLOSE",
    );
    let _ = EndPaint(hwnd, &ps);
}

/// Footer strip, shared by both views.
fn paint_footer(
    hdc: HDC,
    client: RECT,
    theme: &crate::theme::Theme,
    scale: f32,
    font: HFONT,
    hint: &str,
) {
    let height = crate::ui::px(FOOTER_HEIGHT, scale);
    let footer = RECT {
        left: 0,
        top: client.bottom - height,
        right: client.right,
        bottom: client.bottom,
    };
    fill_rect(hdc, footer, theme.surface_color());
    label(
        hdc,
        RECT {
            left: crate::ui::px(EDGE, scale),
            right: client.right - crate::ui::px(EDGE, scale),
            ..footer
        },
        hint,
        font,
        theme.text_dim_color(),
    );
}

fn paint_shortcuts(
    hdc: HDC,
    client: RECT,
    theme: &crate::theme::Theme,
    scale: f32,
    body_font: HFONT,
    small_font: HFONT,
) {
    let px = |value: i32| crate::ui::px(value, scale);
    let keybinds = crate::ACTIVE_KEYBINDS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let count = keybinds.len();
    let columns = if count > 7 { 2 } else { 1 };
    let rows = count.div_ceil(columns).max(1);
    let edge = px(EDGE);
    let column_width = (client.right - edge * 2) / columns as i32;
    let card_height = px(46);

    if keybinds.is_empty() {
        label(
            hdc,
            RECT {
                left: edge,
                top: px(92),
                right: client.right - edge,
                bottom: px(92) + px(28),
            },
            "No shortcuts are currently registered.",
            body_font,
            theme.text_color(),
        );
    } else {
        for (index, keybind) in keybinds.iter().enumerate() {
            let column = index / rows;
            let row = index % rows;
            let left = edge + column as i32 * column_width;
            let top = px(92) + row as i32 * (card_height + px(8));
            if top + card_height > client.bottom - px(FOOTER_HEIGHT) {
                continue;
            }
            let card = RECT {
                left,
                top,
                right: left + column_width - px(10),
                bottom: top + card_height,
            };
            fill_round_rect(hdc, card, px(10), theme.surface_color());
            let split = card.top + (card.bottom - card.top) / 2;
            label(
                hdc,
                RECT {
                    left: card.left + px(12),
                    right: card.right - px(10),
                    top: card.top + px(3),
                    bottom: split,
                },
                &keybind.keys,
                body_font,
                theme.accent_active_color(),
            );
            let description = keybind.description.as_deref().unwrap_or(&keybind.action);
            label(
                hdc,
                RECT {
                    left: card.left + px(12),
                    right: card.right - px(10),
                    top: split,
                    bottom: card.bottom - px(3),
                },
                description,
                small_font,
                theme.text_dim_color(),
            );
        }
    }

    paint_footer(
        hdc,
        client,
        theme,
        scale,
        small_font,
        "ESC / BACKSPACE  BACK TO COMMANDS",
    );
}

fn run_action(action: CommandAction) {
    match action {
        CommandAction::ShowShortcuts => {}
        CommandAction::QuickSettings => crate::quick_settings::toggle(),
        CommandAction::RefreshApps => crate::apps::refresh(),
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
    enum Chosen {
        Command(CommandAction),
        App(crate::apps::AppEntry),
    }
    let chosen = {
        let state = STATE.lock().unwrap_or_else(|error| error.into_inner());
        let rows = results(&state.query);
        let index = state.selected.min(rows.len().saturating_sub(1));
        match rows.into_iter().nth(index) {
            Some(ResultRow::Command(item)) => Some(Chosen::Command(ITEMS[item].action)),
            Some(ResultRow::App(entry)) => Some(Chosen::App(entry)),
            None => None,
        }
    };
    let action = match chosen {
        Some(Chosen::App(entry)) => {
            // Close first: launching can take a moment, and leaving the palette
            // on screen over the new window looks like it failed.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            crate::apps::launch(&entry);
            return;
        }
        Some(Chosen::Command(action)) => Some(action),
        None => None,
    };
    if let Some(action) = action {
        if matches!(action, CommandAction::ShowShortcuts) {
            let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
            state.view = CenterView::Shortcuts;
            state.query.clear();
            state.selected = 0;
            drop(state);
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        } else {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            run_action(action);
        }
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
            if STATE.lock().unwrap_or_else(|error| error.into_inner()).view == CenterView::Shortcuts
            {
                return LRESULT(0);
            }
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
            let shortcuts_view = STATE.lock().unwrap_or_else(|error| error.into_inner()).view
                == CenterView::Shortcuts;
            if shortcuts_view && matches!(wparam.0 as u16, KEY_ESCAPE | KEY_BACK) {
                let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
                state.view = CenterView::Commands;
                drop(state);
                let _ = InvalidateRect(Some(hwnd), None, false);
                return LRESULT(0);
            }
            if shortcuts_view {
                return LRESULT(0);
            }
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
                    let count = results(&state.query).len();
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
            if STATE.lock().unwrap_or_else(|error| error.into_inner()).view == CenterView::Shortcuts
            {
                return LRESULT(0);
            }
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            // Row geometry is scaled when painted, so hit-testing has to scale
            // identically or a click lands on a different row than the one under
            // the pointer on any display above 100%.
            let scale = scale(hwnd);
            let item_top = crate::ui::px(ITEM_TOP, scale);
            let item_height = crate::ui::px(ITEM_HEIGHT, scale).max(1);
            if y >= item_top {
                let row = ((y - item_top) / item_height) as usize;
                let count = {
                    let state = STATE.lock().unwrap_or_else(|error| error.into_inner());
                    results(&state.query).len()
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
                state.view = CenterView::Commands;
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn ensure_class() -> Result<(), String> {
    crate::util::register_window_class(w!("AltDWM_CommandCenter"), wndproc, "Command center")
}

fn place_near(anchor: HWND) -> Placement {
    unsafe {
        let monitor = MonitorFromWindow(anchor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let mut anchor_rect = RECT::default();
        let _ = GetWindowRect(anchor, &mut anchor_rect);
        // Physical size for the display the panel will appear on, so the window
        // is the same apparent size everywhere instead of shrinking as scaling
        // rises.
        let scale = crate::ui::scale_for_monitor(monitor);
        let width = crate::ui::px(WIDTH, scale);
        let height = crate::ui::px(HEIGHT, scale);
        let margin = crate::ui::px(12, scale);
        let monitor_width = info.rcWork.right - info.rcWork.left;
        let x = (anchor_rect.left + crate::ui::px(8, scale))
            .max(info.rcWork.left + margin)
            .min(info.rcWork.right - width - margin)
            .min(info.rcWork.left + monitor_width - width - margin);
        let anchor_center = (anchor_rect.top + anchor_rect.bottom) / 2;
        let monitor_center = (info.rcMonitor.top + info.rcMonitor.bottom) / 2;
        let y = if anchor_center > monitor_center {
            anchor_rect.top - height - crate::ui::px(10, scale)
        } else {
            anchor_rect.bottom + crate::ui::px(10, scale)
        };
        Placement {
            x,
            y: y.max(info.rcWork.top + margin)
                .min(info.rcWork.bottom - height - margin),
            width,
            height,
        }
    }
}

/// Where and how large the command center should open.
struct Placement {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

pub fn toggle(anchor: HWND) {
    // Idempotent: covers the case where indexing failed or has not run yet, so
    // opening the palette a second time can still recover the app list.
    crate::apps::begin_indexing();
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
    let placement = place_near(anchor);
    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_APPWINDOW,
            w!("AltDWM_CommandCenter"),
            w!("AltDWM Command Center"),
            WS_POPUP | WS_VISIBLE,
            placement.x,
            placement.y,
            placement.width,
            placement.height,
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
            placement.x,
            placement.y,
            placement.width,
            placement.height,
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
        assert_eq!(matching_indices("shortcut"), vec![0]);
        assert!(!matching_indices("terminal").is_empty());
        assert!(!matching_indices("settings").is_empty());
        assert!(!matching_indices("balanced").is_empty());
        assert!(matching_indices("definitely-not-a-command").is_empty());
    }
}

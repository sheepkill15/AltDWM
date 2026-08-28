//! The notification area's overflow — Windows' chevron, as a flyout.
//!
//! Two kinds of icon end up here. Applications that set `NIS_HIDDEN` are asking
//! for it, and on a normal desktop that is most of them. The rest are simply the
//! ones a bar of finite width could not fit; dropping those silently is how the
//! old tray managed to be both empty-looking and wrong at the same time.
//!
//! Rows carry their icon *and* the application's own tooltip text, so this is
//! also the only surface in the shell that says which icon is which.

use std::sync::{LazyLock, Mutex};

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, SelectObject, HDC, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DrawIconEx, GetClientRect, SetForegroundWindow,
    SetWindowPos, ShowWindow, DI_NORMAL, HICON, HMENU, HWND_TOPMOST, SWP_SHOWWINDOW, SW_SHOW,
    WM_DESTROY, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::tray::{self, Button, TrayEntry};
use crate::ui::{self, draw_label, fill_round_rect, inset_rect, point_in_rect, px, rect_height};

/// Device-independent pixels at 96 DPI, scaled per display.
const WIDTH: i32 = 268;
const ROW: i32 = 34;
const EDGE: i32 = 8;
const ICON: i32 = 16;

#[derive(Default)]
struct State {
    hwnd: isize,
    /// The rows as they were when the flyout opened.
    ///
    /// A snapshot rather than a live read: a tray icon can appear or vanish
    /// between the pointer moving and the button coming up, and a menu whose
    /// rows renumber under the cursor activates the wrong application.
    items: Vec<TrayEntry>,
    pointer: Option<(i32, i32)>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

fn handle() -> Option<HWND> {
    let raw = STATE.lock().unwrap_or_else(|error| error.into_inner()).hwnd;
    (raw != 0).then_some(HWND(raw as *mut std::ffi::c_void))
}

pub fn is_open() -> bool {
    handle().is_some()
}

pub fn close() {
    if let Some(hwnd) = handle() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

fn items() -> Vec<TrayEntry> {
    STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .items
        .clone()
}

/// Row rectangles, shared by paint and hit-testing so a click can only ever
/// land on something that was drawn.
fn row_rects(client: RECT, count: usize, scale: f32) -> Vec<RECT> {
    let edge = px(EDGE, scale);
    let row = px(ROW, scale);
    (0..count)
        .map(|index| RECT {
            left: client.left + edge,
            top: client.top + edge + row * index as i32,
            right: client.right - edge,
            bottom: client.top + edge + row * (index as i32 + 1),
        })
        .collect()
}

fn content_height(count: usize, scale: f32) -> i32 {
    px(EDGE, scale) * 2 + px(ROW, scale) * count.max(1) as i32
}

fn paint(hwnd: HWND, hdc: HDC, client: RECT) {
    let _antialias = ui::begin_antialiased_paint(hdc);
    let theme = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .theme
        .clone();
    let scale = ui::scale_for_window(hwnd);
    let scaled = |value: i32| px(value, scale);
    let body_font = crate::theme::get_cached_font_variant(&theme, scaled(theme.font_size), 400);
    ui::fill_rect(hdc, &client, theme.panel_bg("top"));

    let items = items();
    let pointer = STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .pointer;
    let rects = row_rects(client, items.len(), scale);
    let radius = scaled(theme.rounding);
    for (entry, rect) in items.iter().zip(&rects) {
        let hovered = pointer.is_some_and(|(x, y)| point_in_rect(x, y, rect));
        if hovered {
            fill_round_rect(hdc, rect, radius, theme.surface_hover_color());
        }
        let icon_side = scaled(ICON);
        let icon_left = rect.left + scaled(10);
        let icon_top = rect.top + (rect_height(rect) - icon_side) / 2;
        if entry.icon != 0 {
            unsafe {
                let _ = DrawIconEx(
                    hdc,
                    icon_left,
                    icon_top,
                    HICON(entry.icon as *mut std::ffi::c_void),
                    icon_side,
                    icon_side,
                    0,
                    None,
                    DI_NORMAL,
                );
            }
        } else {
            // The Explorer bridge has no bitmap to give us. A mark keeps the
            // rows aligned instead of letting the labels slide left.
            let dot = RECT {
                left: icon_left + icon_side / 4,
                top: icon_top + icon_side / 4,
                right: icon_left + icon_side * 3 / 4,
                bottom: icon_top + icon_side * 3 / 4,
            };
            fill_round_rect(hdc, &dot, scaled(4), theme.accent_color());
        }
        let text = RECT {
            left: icon_left + icon_side + scaled(10),
            right: rect.right - scaled(8),
            ..*rect
        };
        let color = if hovered {
            theme.text_color()
        } else {
            theme.text_dim_color()
        };
        draw_label(hdc, &text, &tray::title_line(&entry.name), body_font, color);
    }
    if items.is_empty() {
        let area = inset_rect(client, scaled(16), 0);
        draw_label(
            hdc,
            &area,
            "Nothing hidden",
            body_font,
            theme.text_dim_color(),
        );
    }
}

fn handle_click(hwnd: HWND, x: i32, y: i32, button: Button) {
    let items = items();
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let scale = ui::scale_for_window(hwnd);
    let Some(index) = row_rects(client, items.len(), scale)
        .iter()
        .position(|rect| point_in_rect(x, y, rect))
    else {
        return;
    };
    let id = items[index].id;
    // Close first: the application is about to be given the foreground so it can
    // show a context menu, and a flyout still on screen would fight it for
    // focus and dismiss the menu again.
    close();
    tray::invoke(id, button);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let point = |lparam: LPARAM| {
        (
            (lparam.0 & 0xFFFF) as i16 as i32,
            ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
        )
    };
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            let width = (client.right - client.left).max(1);
            let height = (client.bottom - client.top).max(1);
            let buffer_dc = CreateCompatibleDC(Some(hdc));
            let buffer_bitmap = CreateCompatibleBitmap(hdc, width, height);
            let buffered = !buffer_dc.0.is_null() && !buffer_bitmap.0.is_null();
            let old_bitmap = buffered.then(|| SelectObject(buffer_dc, buffer_bitmap.into()));
            let draw_dc = if buffered { buffer_dc } else { hdc };
            paint(hwnd, draw_dc, client);
            if let Some(old_bitmap) = old_bitmap {
                let _ = BitBlt(hdc, 0, 0, width, height, Some(buffer_dc), 0, 0, SRCCOPY);
                let _ = SelectObject(buffer_dc, old_bitmap);
            }
            if !buffer_bitmap.0.is_null() {
                let _ = DeleteObject(buffer_bitmap.into());
            }
            if !buffer_dc.0.is_null() {
                let _ = DeleteDC(buffer_dc);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let position = point(lparam);
            let changed = {
                let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
                let changed = state.pointer != Some(position);
                state.pointer = Some(position);
                changed
            };
            if changed {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = point(lparam);
            handle_click(hwnd, x, y, Button::Left);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = point(lparam);
            handle_click(hwnd, x, y, Button::Right);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 as u16 == VK_ESCAPE.0 {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            // Dismiss on focus loss, like every other flyout in the shell.
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
            state.hwnd = 0;
            state.items.clear();
            state.pointer = None;
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Centre of `anchor`, which is what decides the display the flyout belongs to.
fn anchor_centre(anchor: RECT) -> windows::Win32::Foundation::POINT {
    windows::Win32::Foundation::POINT {
        x: (anchor.left + anchor.right) / 2,
        y: (anchor.top + anchor.bottom) / 2,
    }
}

/// Place the flyout beside `anchor`, inside `work`.
///
/// The anchor is the chevron's own rectangle in screen coordinates, so the
/// flyout opens above a bottom bar and below a top one without either edge
/// being special-cased, and its right edge lines up with the chevron's.
fn placement(anchor: RECT, work: RECT, width: i32, height: i32, margin: i32) -> (i32, i32) {
    let above = anchor.top - height - margin;
    let below = anchor.bottom + margin;
    let y = if above >= work.top { above } else { below }
        .clamp(work.top, (work.bottom - height).max(work.top));
    let x = (anchor.right - width).clamp(work.left, (work.right - width).max(work.left));
    (x, y)
}

/// The work area of the display holding `anchor`, falling back to the anchor
/// itself so placement still produces a point if the query fails.
fn work_area(anchor: RECT) -> RECT {
    let monitor = unsafe { MonitorFromPoint(anchor_centre(anchor), MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            anchor
        }
    }
}

/// Open the flyout for `items`, anchored to `anchor` in screen coordinates.
/// Calling it while open closes it, so the chevron toggles.
pub fn toggle(anchor: RECT, items: Vec<TrayEntry>) {
    if is_open() {
        close();
        return;
    }
    if let Err(error) =
        crate::util::register_window_class(w!("AltDWM_TrayOverflow"), wndproc, "TrayOverflow")
    {
        eprintln!("[tray-overflow] {error}");
        return;
    }
    let scale = ui::scale_for_monitor(unsafe {
        MonitorFromPoint(anchor_centre(anchor), MONITOR_DEFAULTTONEAREST)
    });
    let width = px(WIDTH, scale);
    let height = content_height(items.len(), scale);
    let (x, y) = placement(anchor, work_area(anchor), width, height, px(6, scale));
    {
        let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
        state.items = items;
        state.pointer = None;
    }
    let created = unsafe {
        CreateWindowExW(
            // See the command center: a flyout does not belong in Alt+Tab.
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("AltDWM_TrayOverflow"),
            w!("AltDWM Tray Overflow"),
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            width,
            height,
            None,
            Some(HMENU(std::ptr::null_mut())),
            Some(HINSTANCE(std::ptr::null_mut())),
            None,
        )
    };
    let Ok(hwnd) = created else {
        eprintln!(
            "[tray-overflow] CreateWindowExW failed: {:?}",
            created.err()
        );
        STATE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .items
            .clear();
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
        if let Ok(cfg) = crate::CURRENT_CONFIG.try_lock() {
            let border = cfg.theme.border_color();
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                &border.0 as *const _ as _,
                size_of_val(&border.0) as u32,
            );
        }
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
    }
}

#[cfg(test)]
mod tests {
    use super::{content_height, placement, row_rects, ROW, WIDTH};
    use windows::Win32::Foundation::RECT;

    #[test]
    fn rows_fit_inside_the_window_the_flyout_sizes_itself_to() {
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            for count in [1usize, 3, 12] {
                let height = content_height(count, scale);
                let client = RECT {
                    left: 0,
                    top: 0,
                    right: crate::ui::px(WIDTH, scale),
                    bottom: height,
                };
                let rects = row_rects(client, count, scale);
                assert_eq!(rects.len(), count);
                for rect in &rects {
                    assert!(rect.top >= client.top, "row above the client area");
                    assert!(rect.bottom <= client.bottom, "row past the bottom");
                    assert!(rect.right > rect.left);
                }
                // Rows must not overlap, or one would swallow its neighbour's
                // clicks.
                for pair in rects.windows(2) {
                    assert_eq!(pair[1].top, pair[0].bottom);
                }
            }
        }
        assert_eq!(content_height(0, 1.0), content_height(1, 1.0));
        assert_eq!(crate::ui::px(ROW, 2.0), 68);
    }

    /// A bar at the bottom of the display opens the flyout upwards; one at the
    /// top opens it downwards. Neither may leave the work area.
    #[test]
    fn the_flyout_opens_away_from_the_bar_and_stays_on_screen() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let bottom_bar = RECT {
            left: 1600,
            top: 1040,
            right: 1640,
            bottom: 1080,
        };
        let (x, y) = placement(bottom_bar, work, 268, 200, 6);
        assert!(y + 200 <= 1040, "flyout should sit above a bottom bar");
        assert!(x >= work.left && x + 268 <= work.right);

        let top_bar = RECT {
            left: 0,
            top: 0,
            right: 40,
            bottom: 40,
        };
        let (x, y) = placement(top_bar, work, 268, 200, 6);
        assert!(y >= 40, "flyout should sit below a top bar");
        assert_eq!(
            x, work.left,
            "a left-edge anchor clamps rather than going off-screen"
        );

        // A display that cannot fit the flyout at all still yields a point
        // inside it rather than a negative one.
        let cramped = RECT {
            left: 0,
            top: 0,
            right: 200,
            bottom: 150,
        };
        let (x, y) = placement(cramped, cramped, 268, 200, 6);
        assert_eq!((x, y), (0, 0));
    }
}

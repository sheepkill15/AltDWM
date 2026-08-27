//! Quick settings — the shell's control surface for volume, brightness,
//! network, keyboard layout, and power.
//!
//! State comes from `crate::system`, which polls off the UI thread, so this
//! module only draws and routes input. Rows are described once and both drawn
//! and hit-tested from that description, so a click can never land on something
//! that was not rendered.
//!
//! Where AltDWM cannot reasonably own the whole interaction — choosing a Wi-Fi
//! network, pairing a Bluetooth device — the row opens the matching Windows
//! settings page rather than pretending to a capability it does not have.

use std::sync::{LazyLock, Mutex};

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MonitorFromWindow, SelectObject,
    HDC, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, PAINTSTRUCT,
    SRCCOPY,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, SetForegroundWindow,
    SetWindowPos, ShowWindow, HMENU, HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_SHOW,
    WM_DESTROY,
    WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::system::{self, NetworkStatus};
use crate::ui::{self, draw_label, fill_round_rect, inset_rect, px, rect_height, rect_width};

/// Device-independent pixels at 96 DPI, scaled per display.
const WIDTH: i32 = 344;
const EDGE: i32 = 20;
const HEADER: i32 = 56;
const SLIDER_ROW: i32 = 62;
const CONTROL_ROW: i32 = 46;
const TRACK: i32 = 6;
const KNOB: i32 = 14;
const FOOTER: i32 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slider {
    Volume,
    Brightness,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Control {
    Mute,
    WiFiRadio,
    WiFiSettings,
    BluetoothSettings,
    KeyboardLayout,
    SoundSettings,
}

enum Row {
    /// A draggable 0–100 control.
    Slider {
        which: Slider,
        label: &'static str,
        value: u8,
        /// A row for a capability the machine does not expose is shown greyed
        /// out rather than hidden, so its absence is legible.
        available: bool,
        detail: String,
    },
    /// A two-state control AltDWM can actually flip.
    Toggle {
        which: Control,
        label: &'static str,
        detail: String,
        on: bool,
    },
    /// A row that hands off to Windows, or steps through a list.
    Action {
        which: Control,
        label: &'static str,
        detail: String,
    },
    /// Read-only status.
    Info {
        label: &'static str,
        detail: String,
    },
}

impl Row {
    fn height(&self) -> i32 {
        match self {
            Row::Slider { .. } => SLIDER_ROW,
            _ => CONTROL_ROW,
        }
    }
}

#[derive(Default)]
struct State {
    hwnd: isize,
    /// Slider currently being dragged, with the track it was laid out on.
    ///
    /// Keeping the track means a drag does not re-derive every row — which reads
    /// live system state and enumerates keyboard layouts — on each of the many
    /// `WM_MOUSEMOVE` messages a drag produces.
    dragging: Option<(Slider, RECT)>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

fn handle() -> Option<HWND> {
    let raw = STATE.lock().unwrap_or_else(|error| error.into_inner()).hwnd;
    (raw != 0).then_some(HWND(raw as *mut std::ffi::c_void))
}

/// Repaint if the surface is open. Called by the system poller when anything it
/// watches changes, so an external volume change is reflected here immediately.
pub fn invalidate() {
    if let Some(hwnd) = handle() {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
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

// ------------------------------------------------------------------ rows ----

fn describe_network(status: &NetworkStatus) -> String {
    match status {
        NetworkStatus::WiFi { ssid, signal } if !ssid.is_empty() => {
            format!("{ssid} · {signal}%")
        }
        other => other.label(),
    }
}

fn rows() -> Vec<Row> {
    let status = system::status();
    let mut rows = Vec::new();

    let volume = status.volume;
    rows.push(Row::Slider {
        which: Slider::Volume,
        label: "Volume",
        value: volume.map(|volume| volume.percent()).unwrap_or(0),
        available: volume.is_some(),
        detail: match volume {
            Some(volume) if volume.muted => "Muted".into(),
            Some(volume) => format!("{}%", volume.percent()),
            None => "No output device".into(),
        },
    });
    if let Some(volume) = volume {
        rows.push(Row::Toggle {
            which: Control::Mute,
            label: "Mute",
            detail: if volume.muted { "On" } else { "Off" }.into(),
            on: volume.muted,
        });
    }

    rows.push(Row::Slider {
        which: Slider::Brightness,
        label: "Brightness",
        value: status.brightness.map(|value| value.percent).unwrap_or(0),
        available: status.brightness.is_some(),
        // Internal laptop panels answer WMI rather than DDC/CI, so say so
        // instead of showing a control that silently does nothing.
        detail: match status.brightness {
            Some(value) => format!("{}%", value.percent),
            None => "Display does not report DDC/CI".into(),
        },
    });

    match status.wifi_radio_on {
        Some(on) => rows.push(Row::Toggle {
            which: Control::WiFiRadio,
            label: "Wi-Fi",
            detail: if on {
                describe_network(&status.network)
            } else {
                "Off".into()
            },
            on,
        }),
        None => rows.push(Row::Info {
            label: "Network",
            detail: describe_network(&status.network),
        }),
    }

    rows.push(Row::Action {
        which: Control::WiFiSettings,
        label: "Networks",
        detail: "Choose a network".into(),
    });
    rows.push(Row::Action {
        which: Control::BluetoothSettings,
        label: "Bluetooth",
        detail: "Devices and pairing".into(),
    });

    let layouts = crate::input::installed();
    let current = crate::input::current();
    rows.push(match (&current, layouts.len()) {
        (Some(current), count) if count > 1 => Row::Action {
            which: Control::KeyboardLayout,
            label: "Keyboard",
            detail: format!("{} · {} installed", current.name, count),
        },
        (Some(current), _) => Row::Info {
            label: "Keyboard",
            detail: current.name.clone(),
        },
        (None, _) => Row::Info {
            label: "Keyboard",
            detail: "Unknown".into(),
        },
    });

    if let Some(battery) = status.battery {
        let percent = battery
            .percent
            .map(|percent| format!("{percent}%"))
            .unwrap_or_else(|| "Unknown".into());
        let state = if battery.charging {
            " · charging".to_string()
        } else if battery.on_ac {
            " · plugged in".to_string()
        } else if let Some(minutes) = battery.minutes_remaining {
            format!(" · {}h {:02}m left", minutes / 60, minutes % 60)
        } else {
            String::new()
        };
        rows.push(Row::Info {
            label: "Battery",
            detail: format!("{percent}{state}"),
        });
    }

    rows.push(Row::Action {
        which: Control::SoundSettings,
        label: "Sound",
        detail: "Output devices and mixer".into(),
    });

    rows
}

fn content_height(rows: &[Row], scale: f32) -> i32 {
    let body: i32 = rows.iter().map(|row| px(row.height(), scale)).sum();
    px(HEADER, scale) + body + px(FOOTER, scale)
}

/// Rectangles for each row, in client coordinates. Drawing and hit-testing both
/// read this, so they cannot disagree.
fn row_rects(client: RECT, rows: &[Row], scale: f32) -> Vec<RECT> {
    let edge = px(EDGE, scale);
    let mut top = client.top + px(HEADER, scale);
    rows.iter()
        .map(|row| {
            let height = px(row.height(), scale);
            let rect = RECT {
                left: client.left + edge,
                top,
                right: client.right - edge,
                bottom: top + height,
            };
            top += height;
            rect
        })
        .collect()
}

/// The draggable track inside a slider row.
fn track_rect(row: RECT, scale: f32) -> RECT {
    let track = px(TRACK, scale).max(2);
    let knob = px(KNOB, scale).max(6);
    // Inset by the knob radius so the knob's centre can reach both ends without
    // the knob itself leaving the row.
    let center = row.bottom - px(16, scale);
    RECT {
        left: row.left + knob / 2,
        top: center - track / 2,
        right: row.right - knob / 2,
        bottom: center + track / 2,
    }
}

fn value_from_x(track: RECT, x: i32) -> u8 {
    let width = rect_width(&track).max(1);
    let offset = (x - track.left).clamp(0, width);
    ((offset as f32 / width as f32) * 100.0).round().clamp(0.0, 100.0) as u8
}

// --------------------------------------------------------------- painting ----

/// Resize the window if the rows no longer fit it.
///
/// The height is computed when the surface opens, but the row list is derived
/// from live state: an audio endpoint appearing adds a Mute row, a battery
/// appearing adds a status row. Without this a row could be laid out past the
/// bottom of the window, drawn nowhere and clickable nowhere.
fn fit_window_to_rows(hwnd: HWND, client: RECT, rows: &[Row], scale: f32) -> bool {
    let wanted = content_height(rows, scale);
    if (client.bottom - client.top) == wanted {
        return false;
    }
    let mut frame = RECT::default();
    unsafe {
        if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut frame).is_err() {
            return false;
        }
        // Grow upward: the surface is anchored to the bottom-right corner.
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            frame.left,
            frame.bottom - wanted,
            frame.right - frame.left,
            wanted,
            SWP_NOACTIVATE,
        );
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
    true
}

fn paint(hwnd: HWND, hdc: HDC, client: RECT) {
    let theme = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .theme
        .clone();
    let scale = ui::scale_for_window(hwnd);
    let px = |value: i32| ui::px(value, scale);
    let font = |size: i32, weight: i32| {
        crate::theme::get_cached_font_variant(&theme, px(size), weight)
    };
    let title_font = font(17, 600);
    let body_font = font(theme.font_size, 500);
    let small_font = font((theme.font_size - 2).max(8), 400);

    ui::fill_rect(hdc, &client, theme.panel_bg("top"));

    let header = RECT {
        left: client.left + px(EDGE),
        top: client.top + px(14),
        right: client.right - px(EDGE),
        bottom: client.top + px(14) + px(26),
    };
    draw_label(hdc, &header, "Quick settings", title_font, theme.text_color());

    let rows = rows();
    if fit_window_to_rows(hwnd, client, &rows, scale) {
        // The resize repaints; this pass would draw against stale bounds.
        return;
    }
    let rects = row_rects(client, &rows, scale);
    for (row, rect) in rows.iter().zip(&rects) {
        match row {
            Row::Slider {
                label,
                value,
                available,
                detail,
                ..
            } => {
                let track = track_rect(*rect, scale);
                // The label band is whatever the track leaves, rather than a
                // fixed height that clips at a larger theme font size.
                let text = RECT {
                    bottom: track.top - px(2),
                    ..*rect
                };
                let color = if *available {
                    theme.text_color()
                } else {
                    theme.text_dim_color()
                };
                draw_label(hdc, &text, label, body_font, color);
                draw_right(hdc, &text, detail, small_font, theme.text_dim_color(), scale);

                fill_round_rect(
                    hdc,
                    &track,
                    rect_height(&track) / 2,
                    theme.surface_color(),
                );
                if *available {
                    let width = rect_width(&track);
                    let filled = width * i32::from(*value) / 100;
                    let done = RECT {
                        right: track.left + filled,
                        ..track
                    };
                    fill_round_rect(
                        hdc,
                        &done,
                        rect_height(&track) / 2,
                        theme.accent_active_color(),
                    );
                    let knob_side = px(KNOB).max(6);
                    let center_x = track.left + filled;
                    let center_y = (track.top + track.bottom) / 2;
                    let knob = RECT {
                        left: center_x - knob_side / 2,
                        top: center_y - knob_side / 2,
                        right: center_x + knob_side / 2,
                        bottom: center_y + knob_side / 2,
                    };
                    fill_round_rect(hdc, &knob, knob_side / 2, theme.text_color());
                }
            }
            Row::Toggle {
                label, detail, on, ..
            } => {
                let body = draw_control_row(hdc, rect, label, detail, &theme, body_font, small_font, scale);
                // A pill switch: the same affordance Windows uses, so the state
                // reads at a glance rather than needing the label.
                let switch_w = px(CONTROL_WIDTH - 6);
                let switch_h = px(20);
                let switch = RECT {
                    left: body.right - switch_w,
                    top: (body.top + body.bottom - switch_h) / 2,
                    right: body.right,
                    bottom: (body.top + body.bottom + switch_h) / 2,
                };
                fill_round_rect(
                    hdc,
                    &switch,
                    switch_h / 2,
                    if *on {
                        theme.accent_active_color()
                    } else {
                        theme.surface_color()
                    },
                );
                let knob_side = switch_h - px(6);
                let knob_left = if *on {
                    switch.right - knob_side - px(3)
                } else {
                    switch.left + px(3)
                };
                let knob = RECT {
                    left: knob_left,
                    top: switch.top + px(3),
                    right: knob_left + knob_side,
                    bottom: switch.top + px(3) + knob_side,
                };
                fill_round_rect(hdc, &knob, knob_side / 2, theme.text_color());
            }
            Row::Action { label, detail, .. } => {
                let body = draw_control_row(hdc, rect, label, detail, &theme, body_font, small_font, scale);
                // A chevron drawn from two blocks: no icon font to depend on.
                let arrow = px(4);
                let center_y = (body.top + body.bottom) / 2;
                for step in 0..2 {
                    let offset = step * arrow;
                    let mark = RECT {
                        left: body.right - arrow * 2 + offset,
                        top: center_y - arrow + offset,
                        right: body.right - arrow * 2 + offset + arrow,
                        bottom: center_y + offset,
                    };
                    ui::fill_rect(hdc, &mark, theme.text_dim_color());
                }
            }
            Row::Info { label, detail } => {
                let text = inset_rect(*rect, 0, px(6));
                draw_label(hdc, &text, label, body_font, theme.text_dim_color());
                draw_right(hdc, &text, detail, small_font, theme.text_dim_color(), scale);
            }
        }
    }
}

/// Width reserved at the right of a control row for its switch or chevron.
const CONTROL_WIDTH: i32 = 44;

/// Shared chrome for toggle and action rows: a hoverable surface with a label on
/// the left and a detail line on the right. Returns the inner content box.
#[allow(clippy::too_many_arguments)]
fn draw_control_row(
    hdc: HDC,
    rect: &RECT,
    label: &str,
    detail: &str,
    theme: &crate::theme::Theme,
    body_font: HFONT,
    small_font: HFONT,
    scale: f32,
) -> RECT {
    let px = |value: i32| ui::px(value, scale);
    let surface = inset_rect(*rect, 0, px(3));
    fill_round_rect(hdc, &surface, px(theme.rounding), theme.surface_color());
    let body = inset_rect(surface, px(12), 0);
    draw_label(hdc, &body, label, body_font, theme.text_color());
    let detail_box = RECT {
        right: body.right - px(CONTROL_WIDTH),
        ..body
    };
    draw_right(hdc, &detail_box, detail, small_font, theme.text_dim_color(), scale);
    body
}

/// Right-align a short value against the end of `rect`.
fn draw_right(
    hdc: HDC,
    rect: &RECT,
    text: &str,
    font: HFONT,
    color: windows::Win32::Foundation::COLORREF,
    scale: f32,
) {
    if text.is_empty() {
        return;
    }
    let width = ui::measure_label(hdc, text, font) + ui::px(2, scale);
    let left = (rect.right - width).max(rect.left);
    draw_label(
        hdc,
        &RECT {
            left,
            ..*rect
        },
        text,
        font,
        color,
    );
}

// ------------------------------------------------------------------ input ----

fn open_settings_page(page: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide: Vec<u16> = page.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        ShellExecuteW(
            None,
            PCWSTR::null(),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn activate_control(which: Control, on: bool) -> bool {
    match which {
        Control::Mute => {
            system::toggle_mute();
            false
        }
        Control::WiFiRadio => {
            system::set_wifi_radio(!on);
            false
        }
        Control::WiFiSettings => {
            open_settings_page("ms-settings:network-wifi");
            true
        }
        Control::BluetoothSettings => {
            open_settings_page("ms-settings:bluetooth");
            true
        }
        Control::SoundSettings => {
            open_settings_page("ms-settings:sound");
            true
        }
        Control::KeyboardLayout => {
            // Switching targets the foreground window, so the surface has to go
            // away first or it would be the foreground window itself.
            true
        }
    }
}

fn handle_click(hwnd: HWND, x: i32, y: i32) {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let scale = ui::scale_for_window(hwnd);
    let rows = rows();
    let rects = row_rects(client, &rows, scale);
    for (row, rect) in rows.iter().zip(&rects) {
        if !ui::point_in_rect(x, y, rect) {
            continue;
        }
        match row {
            Row::Slider {
                which, available, ..
            } => {
                if !available {
                    return;
                }
                let track = track_rect(*rect, scale);
                // Clicking anywhere in the row jumps to that value and starts a
                // drag, which is what every slider does.
                apply_slider(*which, value_from_x(track, x));
                STATE
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .dragging = Some((*which, track));
                unsafe {
                    SetCapture(hwnd);
                }
            }
            Row::Toggle { which, on, .. } => {
                if activate_control(*which, *on) {
                    close();
                }
            }
            Row::Action { which, .. } => {
                if *which == Control::KeyboardLayout {
                    close();
                    crate::input::cycle();
                } else if activate_control(*which, false) {
                    close();
                }
            }
            Row::Info { .. } => {}
        }
        invalidate();
        return;
    }
}

fn apply_slider(which: Slider, value: u8) {
    match which {
        Slider::Volume => system::set_volume(f32::from(value) / 100.0),
        Slider::Brightness => system::set_brightness(value),
    }
}

fn handle_drag(x: i32) {
    let dragging = STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .dragging;
    let Some((which, track)) = dragging else {
        return;
    };
    apply_slider(which, value_from_x(track, x));
    invalidate();
}

fn handle_wheel(hwnd: HWND, x: i32, y: i32, delta: i16) {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let scale = ui::scale_for_window(hwnd);
    let rows = rows();
    let rects = row_rects(client, &rows, scale);
    let step: i32 = if delta > 0 { 5 } else { -5 };
    for (row, rect) in rows.iter().zip(&rects) {
        if !ui::point_in_rect(x, y, rect) {
            continue;
        }
        if let Row::Slider {
            which, available, ..
        } = row
        {
            if !*available {
                return;
            }
            match which {
                Slider::Volume => system::adjust_volume(step as f32 / 100.0),
                Slider::Brightness => system::adjust_brightness(step),
            }
            invalidate();
        }
        return;
    }
}

// ----------------------------------------------------------------- window ----

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
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
        WM_LBUTTONDOWN => {
            let (x, y) = point(lparam);
            handle_click(hwnd, x, y);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, _) = point(lparam);
            handle_drag(x);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            STATE
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .dragging = None;
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Wheel coordinates are screen-relative; the rows are not.
            let mut converted = windows::Win32::Foundation::POINT {
                x: (lparam.0 & 0xFFFF) as i16 as i32,
                y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
            };
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut converted);
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
            handle_wheel(hwnd, converted.x, converted.y, delta);
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
            state.dragging = None;
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn ensure_class() -> Result<(), String> {
    crate::util::register_window_class(w!("AltDWM_QuickSettings"), wndproc, "QuickSettings")
}

/// Bottom-right of the work area on the monitor holding `anchor`, matching where
/// Windows puts its own quick settings.
fn placement(anchor: Option<HWND>, scale_of: &mut f32) -> (i32, i32, i32, i32) {
    let monitor = match anchor {
        Some(anchor) if !anchor.0.is_null() => unsafe {
            MonitorFromWindow(anchor, MONITOR_DEFAULTTONEAREST)
        },
        _ => unsafe {
            MonitorFromPoint(
                windows::Win32::Foundation::POINT { x: 0, y: 0 },
                MONITOR_DEFAULTTOPRIMARY,
            )
        },
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let work = unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }
        }
    };
    let scale = ui::scale_for_monitor(monitor);
    *scale_of = scale;
    let rows = rows();
    let width = px(WIDTH, scale);
    let height = content_height(&rows, scale);
    let margin = px(12, scale);
    let x = (work.right - width - margin).max(work.left + margin);
    let y = (work.bottom - height - margin).max(work.top + margin);
    (x, y, width, height)
}

/// Open the surface, or close it if it is already open.
///
/// Anchored on the foreground window so it appears on the display the user is
/// working on rather than always on the primary.
pub fn toggle() {
    let foreground = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    toggle_near((!foreground.0.is_null()).then_some(foreground));
}

pub fn toggle_near(anchor: Option<HWND>) {
    if is_open() {
        close();
        return;
    }
    if let Err(error) = ensure_class() {
        eprintln!("[quick-settings] {error}");
        return;
    }
    // Ask for a fresh reading so the surface opens with current values rather
    // than whatever the last poll saw.
    system::refresh();
    let mut scale = 1.0f32;
    let (x, y, width, height) = placement(anchor, &mut scale);
    let created = unsafe {
        CreateWindowExW(
            // See the command center: a flyout does not belong in Alt+Tab.
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("AltDWM_QuickSettings"),
            w!("AltDWM Quick Settings"),
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
            "[quick-settings] CreateWindowExW failed: {:?}",
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
    println!("[quick-settings] open at {x},{y} {width}x{height} scale={scale:.2}");
}

#[cfg(test)]
mod tests {
    use super::{content_height, describe_network, row_rects, rows, track_rect, value_from_x};
    use crate::system::NetworkStatus;
    use windows::Win32::Foundation::RECT;

    #[test]
    fn slider_value_tracks_the_pointer_across_the_whole_track() {
        let track = RECT {
            left: 100,
            top: 0,
            right: 300,
            bottom: 6,
        };
        assert_eq!(value_from_x(track, 100), 0);
        assert_eq!(value_from_x(track, 200), 50);
        assert_eq!(value_from_x(track, 300), 100);
        // A drag that leaves the track clamps rather than wrapping.
        assert_eq!(value_from_x(track, 40), 0);
        assert_eq!(value_from_x(track, 5000), 100);
    }

    #[test]
    fn a_degenerate_track_does_not_divide_by_zero() {
        let track = RECT {
            left: 50,
            top: 0,
            right: 50,
            bottom: 6,
        };
        assert_eq!(value_from_x(track, 50), 0);
    }

    #[test]
    fn network_detail_shows_signal_alongside_the_ssid() {
        assert_eq!(
            describe_network(&NetworkStatus::WiFi {
                ssid: "Kitchen".into(),
                signal: 82
            }),
            "Kitchen · 82%"
        );
        assert_eq!(describe_network(&NetworkStatus::Offline), "Offline");
    }

    /// Every row must be reachable inside the window the surface sizes itself to,
    /// or a control would be drawn where it cannot be clicked.
    #[test]
    fn rows_fit_inside_the_computed_window_height() {
        let rows = rows();
        assert!(!rows.is_empty());
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            let height = content_height(&rows, scale);
            let client = RECT {
                left: 0,
                top: 0,
                right: 344,
                bottom: height,
            };
            let rects = row_rects(client, &rows, scale);
            assert_eq!(rects.len(), rows.len());
            for rect in &rects {
                assert!(rect.top >= client.top, "row above the client area");
                assert!(
                    rect.bottom <= client.bottom,
                    "row past the bottom at scale {scale}"
                );
                assert!(rect.right > rect.left);
            }
            // Slider tracks must also stay inside their own row.
            for (row, rect) in rows.iter().zip(&rects) {
                if matches!(row, super::Row::Slider { .. }) {
                    let track = track_rect(*rect, scale);
                    assert!(track.top >= rect.top && track.bottom <= rect.bottom);
                    assert!(track.right > track.left);
                }
            }
        }
    }
}

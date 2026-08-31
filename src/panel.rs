//! Panel manager — turns `PanelConfig` + `WidgetConfig` into Win32 windows.
//!
//! Each panel is a `WS_POPUP | WS_EX_TOPMOST` window that owns its own widget
//! strip. Geometry is expressed in device-independent pixels in configuration
//! and scaled per monitor, so a bar is the same apparent size on a 100% and a
//! 150% display and survives being dragged between them.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    EndPaint, GetMonitorInfoW, HMONITOR, MONITORINFO, MONITORINFOEXW, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetForegroundWindow, IsWindowVisible,
    SetWindowPos, ShowWindow, HMENU, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    SW_SHOWNOACTIVATE, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_TIMER, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::config::{Config, PanelConfig};
use crate::ui::{self, fill_rect, fill_round_rect, resolve_track_sizes};
use crate::widgets::{self, HoverPaint, PanelCtx, Widget};

type PanelCollection = Arc<Mutex<Vec<Panel>>>;

static PANELS: std::sync::LazyLock<Mutex<Option<PanelCollection>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));
/// Pointer position per panel, in client coordinates, while the pointer is over
/// it. Widgets use this to highlight only the item under the cursor.
static POINTERS: std::sync::LazyLock<Mutex<HashMap<isize, (i32, i32)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
const WM_MOUSELEAVE_RAW: u32 = 0x02A3;
const WM_DPICHANGED_RAW: u32 = 0x02E0;
const TIMER_TICK: usize = 1;
const TIMER_FAST: usize = 2;

/// Schedule a repaint without blocking if a paint/config callback already owns
/// the panel collection. WinEvent callbacks can be COM-reentrant on this thread.
pub fn invalidate_all() {
    let Ok(panels_guard) = PANELS.try_lock() else {
        return;
    };
    let Some(panel_arc) = panels_guard.as_ref().cloned() else {
        return;
    };
    drop(panels_guard);
    let Ok(panels) = panel_arc.try_lock() else {
        return;
    };
    for panel in panels.iter() {
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(panel.hwnd), None, false);
        }
    }
}

pub fn first_handle() -> Option<HWND> {
    let panels_guard = PANELS.try_lock().ok()?;
    let panels = panels_guard.as_ref()?.try_lock().ok()?;
    panels.first().map(|panel| panel.hwnd)
}

fn panel_should_show(fullscreen_monitor: Option<HMONITOR>, panel_monitor: HMONITOR) -> bool {
    fullscreen_monitor.is_none_or(|monitor| monitor != panel_monitor)
}

/// Hide shell chrome only on the display occupied by the foreground fullscreen
/// application. Panels on every other monitor remain visible and interactive.
pub fn sync_fullscreen(foreground: HWND) {
    let fullscreen_monitor = if crate::manager::is_exclusive_fullscreen(foreground) {
        Some(unsafe {
            windows::Win32::Graphics::Gdi::MonitorFromWindow(
                foreground,
                windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST,
            )
        })
    } else {
        None
    };
    let Some(panel_arc) = panel_collection() else {
        return;
    };
    let Ok(panels) = panel_arc.try_lock() else {
        return;
    };
    for panel in panels.iter() {
        let should_show = panel_should_show(fullscreen_monitor, panel.monitor);
        let visible = unsafe { IsWindowVisible(panel.hwnd).as_bool() };
        if should_show == visible {
            continue;
        }
        unsafe {
            let _ = ShowWindow(
                panel.hwnd,
                if should_show {
                    SW_SHOWNOACTIVATE
                } else {
                    SW_HIDE
                },
            );
        }
    }
}

pub struct Panel {
    pub cfg: PanelConfig,
    pub hwnd: HWND,
    /// Behind `Arc` so a hit widget can be cloned out and invoked *after* the
    /// panel collection is unlocked. Opening the command center or quick
    /// settings from a click pumps messages, which can re-enter `WM_PAINT` on
    /// this same thread — and that would deadlock on a lock still held.
    pub widgets: Vec<Arc<dyn Widget>>,
    pub background: COLORREF,
    /// Monitor this panel belongs to, kept so the panel can be re-placed after a
    /// DPI or resolution change without rebuilding the whole configuration.
    monitor: HMONITOR,
    /// Stacking offset from its edge, in device-independent pixels.
    edge_offset: i32,
}
unsafe impl Send for Panel {}
unsafe impl Sync for Panel {}

fn apply_panel_chrome(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
    // A bar spans its monitor edge to edge. Rounding its corners left notches
    // showing the desktop through them, and the Mica backdrop that used to be
    // requested here was covered by the opaque background fill in WM_PAINT — so
    // neither did anything but cost a DWM call. Square corners, explicit fill.
    const DWMWA_WINDOW_CORNER_PREFERENCE_RAW: u32 = 33;
    const DWMWCP_DONOTROUND: u32 = 1;
    unsafe {
        let corner: u32 = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(
                DWMWA_WINDOW_CORNER_PREFERENCE_RAW as i32,
            ),
            &corner as *const _ as _,
            size_of_val(&corner) as u32,
        );
        if let Ok(cfg) = crate::CURRENT_CONFIG.try_lock() {
            let border = cfg.theme.border_color();
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                &border.0 as *const _ as _,
                size_of_val(&border) as u32,
            );
        }
    }
}

/// A copy of the panel configuration with every length converted from
/// device-independent to physical pixels for `scale`.
pub fn scaled_panel_config(cfg: &PanelConfig, scale: f32) -> PanelConfig {
    let mut scaled = cfg.clone();
    scaled.height = ui::px(cfg.height, scale).max(1);
    scaled.margin = cfg
        .margin
        .map(|margin| margin.map(|value| ui::px(value, scale)));
    scaled
}

fn build_ctx(panel: &Panel, rect: RECT, hwnd: HWND, windows: Vec<HWND>) -> PanelCtx {
    let vertical = matches!(panel.cfg.position.as_str(), "left" | "right");
    let theme = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .theme
        .clone();
    PanelCtx {
        panel_name: panel.cfg.position.clone(),
        monitor: panel.cfg.monitor.clone(),
        monitor_key: panel.monitor.0 as isize,
        width: if vertical {
            rect.bottom - rect.top
        } else {
            rect.right - rect.left
        },
        height: panel.cfg.height,
        hwnd,
        windows,
        scale: ui::scale_for_window(hwnd),
        vertical,
        pointer: POINTERS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&(hwnd.0 as isize))
            .copied(),
        theme,
    }
}

/// Widget rectangles along the panel's long axis, in client coordinates.
fn widget_rects(panel: &Panel, rect: RECT, ctx: &PanelCtx) -> Vec<RECT> {
    let vertical = ctx.vertical;
    let (start, extent) = if vertical {
        (rect.top, rect.bottom - rect.top)
    } else {
        (rect.left, rect.right - rect.left)
    };
    let requested: Vec<i32> = panel
        .widgets
        .iter()
        .map(|widget| {
            let width = widget.width(ctx);
            if width > 0 {
                ctx.px(width)
            } else {
                0
            }
        })
        .collect();
    let sizes = resolve_track_sizes(&requested, extent, 0);
    let mut offset = start;
    sizes
        .into_iter()
        .map(|size| {
            let item = if vertical {
                RECT {
                    left: rect.left,
                    top: offset,
                    right: rect.right,
                    bottom: offset + size,
                }
            } else {
                RECT {
                    left: offset,
                    top: rect.top,
                    right: offset + size,
                    bottom: rect.bottom,
                }
            };
            offset += size;
            item
        })
        .collect()
}

fn paint_panel(
    panel: &Panel,
    hwnd: HWND,
    hdc: windows::Win32::Graphics::Gdi::HDC,
    rect: RECT,
    windows: Vec<HWND>,
) {
    let _antialias = ui::begin_antialiased_paint(hdc);
    let ctx = build_ctx(panel, rect, hwnd, windows);
    fill_rect(hdc, &rect, panel.background);
    let rects = widget_rects(panel, rect, &ctx);
    for (widget, item) in panel.widgets.iter().zip(rects) {
        if widget.hover_paint() == HoverPaint::Whole
            && ctx
                .pointer
                .is_some_and(|(x, y)| ui::point_in_rect(x, y, &item))
        {
            let highlight = widgets::widget_content_rect(item, &ctx);
            fill_round_rect(
                hdc,
                &highlight,
                ctx.radius(),
                ctx.theme.surface_hover_color(),
            );
        }
        widget.draw(hdc, item, &ctx);
    }
}

unsafe extern "system" fn panel_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(
                Some(hwnd),
                TIMER_TICK,
                1000,
                None,
            );
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_TICK || wparam.0 == TIMER_FAST {
                // Foreground/location hooks handle this immediately; the timer
                // is a backstop for games that switch display mode without
                // emitting a normal WinEvent transition.
                sync_fullscreen(GetForegroundWindow());
                // Widgets refresh here, never in WM_PAINT: a Rhai script or a
                // file read in the paint handler stalled the whole shell.
                let mut changed = false;
                // Snapshot before taking the panel lock. Window discovery can
                // enter virtual-desktop COM, which is allowed to pump paint
                // messages on this thread.
                let snapshot = crate::manager::window_snapshot();
                let mut refreshes = Vec::new();
                if let Some(panel_arc) = panel_collection() {
                    if let Ok(panels) = panel_arc.try_lock() {
                        for panel in panels.iter().filter(|panel| panel.hwnd == hwnd) {
                            let mut client = RECT::default();
                            let _ = GetClientRect(hwnd, &mut client);
                            let ctx = build_ctx(panel, client, hwnd, snapshot.clone());
                            let rects = widget_rects(panel, client, &ctx);
                            for (widget, rect) in panel.widgets.iter().zip(rects) {
                                refreshes.push((widget.clone(), rect, ctx.clone()));
                            }
                        }
                    }
                }
                // Scripts and their context APIs can launch COM/window queries
                // and pump messages too. Invoke them only after releasing the
                // panel collection lock.
                for (widget, rect, ctx) in refreshes {
                    // Every widget refreshes; `||` would short-circuit and
                    // starve the ones after the first change.
                    changed |= widget.refresh(&ctx, rect);
                }
                // Only repaint when something moved. A sub-second widget used to
                // force the whole bar — window enumeration and all — to redraw
                // at its interval whether or not anything had changed.
                //
                // The paint path fills every pixel itself, so erasing first
                // would expose a blank frame.
                if changed {
                    let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            let width = (rect.right - rect.left).max(1);
            let height = (rect.bottom - rect.top).max(1);
            let buffer_dc = CreateCompatibleDC(Some(hdc));
            let buffer_bitmap = CreateCompatibleBitmap(hdc, width, height);
            let buffered = !buffer_dc.0.is_null() && !buffer_bitmap.0.is_null();
            let old_bitmap = if buffered {
                Some(windows::Win32::Graphics::Gdi::SelectObject(
                    buffer_dc,
                    buffer_bitmap.into(),
                ))
            } else {
                None
            };
            let draw_dc = if buffered { buffer_dc } else { hdc };
            // Gathered before the lock: the shared snapshot can issue a COM
            // virtual-desktop query, and COM may pump a paint message on this
            // thread, which would then block on the lock we already hold.
            let snapshot = crate::manager::window_snapshot();
            if let Some(panel_arc) = panel_collection() {
                let panels = panel_arc.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(panel) = panels.iter().find(|panel| panel.hwnd == hwnd) {
                    paint_panel(panel, hwnd, draw_dc, rect, snapshot);
                }
            }
            if let Some(old_bitmap) = old_bitmap {
                let _ = BitBlt(hdc, 0, 0, width, height, Some(buffer_dc), 0, 0, SRCCOPY);
                let _ = windows::Win32::Graphics::Gdi::SelectObject(buffer_dc, old_bitmap);
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
            let point = client_point(lparam);
            // The action is resolved under the lock and dispatched after it is
            // released. Dispatching while holding the panel collection would
            // deadlock the moment an action touched the panels themselves.
            // Resolve which widget was hit under the lock, then release it
            // before invoking anything: a widget may open a window, set the
            // foreground window, or run a script, all of which pump messages.
            let hit = resolve_hit(hwnd, point);
            if let Some((panel_name, widget, item, ctx)) = hit {
                if let Some(action) = widget.on_click(point, item, &ctx) {
                    println!(
                        "[panel {panel_name}] widget '{}' click -> {action}",
                        widget.name()
                    );
                    crate::scripting::dispatch_action_on_monitor(&action, ctx.monitor_key);
                }
            }
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            // Right click reaches the widget and stops there: a widget's
            // configured `action` belongs to the left button, and firing it from
            // both would make a tray context menu also launch a program.
            let point = client_point(lparam);
            if let Some((panel_name, widget, item, ctx)) = resolve_hit(hwnd, point) {
                if let Some(action) = widget.on_right_click(point, item, &ctx) {
                    println!(
                        "[panel {panel_name}] widget '{}' right click -> {action}",
                        widget.name()
                    );
                    crate::scripting::dispatch_action_on_monitor(&action, ctx.monitor_key);
                }
            }
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            // The class asks for CS_DBLCLKS, so the second click of a pair
            // arrives here instead of as another WM_LBUTTONDOWN. Widgets with no
            // double-click meaning forward it back to `on_click`, which keeps
            // that second click from being swallowed.
            let point = client_point(lparam);
            if let Some((panel_name, widget, item, ctx)) = resolve_hit(hwnd, point) {
                if let Some(action) = widget.on_double_click(point, item, &ctx) {
                    println!(
                        "[panel {panel_name}] widget '{}' double click -> {action}",
                        widget.name()
                    );
                    crate::scripting::dispatch_action_on_monitor(&action, ctx.monitor_key);
                }
            }
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Wheel messages carry screen coordinates, unlike the button and
            // move messages. Subtracting the window rect's origin happens to work
            // for a borderless popup; ScreenToClient is correct for any window.
            let point = to_client(hwnd, client_point(lparam));
            let notches = ((wparam.0 >> 16) & 0xFFFF) as i16;
            let delta = if notches > 0 { 1 } else { -1 };
            let handled = match resolve_hit(hwnd, point) {
                Some((_, widget, item, ctx)) => widget.on_scroll(delta, point, item, &ctx),
                None => false,
            };
            if handled {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let point = client_point(lparam);
            let changed = {
                let mut pointers = POINTERS.lock().unwrap_or_else(|error| error.into_inner());
                pointers.insert(hwnd.0 as isize, point) != Some(point)
            };
            if changed {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            }
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tracking);
            LRESULT(0)
        }
        WM_MOUSELEAVE_RAW => {
            POINTERS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&(hwnd.0 as isize));
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_DPICHANGED_RAW => {
            // The manifest asks for PerMonitorV2, which obliges us to re-place
            // and repaint at the new scale instead of being silently stretched.
            reposition_panel(hwnd);
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, true);
            crate::request_retile();
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(hwnd), TIMER_TICK);
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(hwnd), TIMER_FAST);
            POINTERS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&(hwnd.0 as isize));
            // don't PostQuitMessage — panels are not main loop; host is
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// The widget under `point`, resolved and cloned out so the caller can invoke it
/// with no lock held. Returns the owning panel's name for logging.
fn resolve_hit(hwnd: HWND, point: (i32, i32)) -> Option<(String, Arc<dyn Widget>, RECT, PanelCtx)> {
    // Gathered before the lock for the same reason as in WM_PAINT.
    let snapshot = crate::manager::window_snapshot();
    let panel_arc = panel_collection()?;
    let panels = panel_arc.lock().unwrap_or_else(|e| e.into_inner());
    let panel = panels.iter().find(|panel| panel.hwnd == hwnd)?;
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    let ctx = build_ctx(panel, rect, hwnd, snapshot);
    let rects = widget_rects(panel, rect, &ctx);
    let name = panel.cfg.name.clone();
    panel
        .widgets
        .iter()
        .zip(rects)
        .find(|(_, item)| ui::point_in_rect(point.0, point.1, item))
        .map(|(widget, item)| (name, widget.clone(), item, ctx))
}

/// Convert a screen point to `hwnd`'s client coordinates.
fn to_client(hwnd: HWND, point: (i32, i32)) -> (i32, i32) {
    let mut converted = windows::Win32::Foundation::POINT {
        x: point.0,
        y: point.1,
    };
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut converted);
    }
    (converted.x, converted.y)
}

fn client_point(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam.0 & 0xFFFF) as i16 as i32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    )
}

fn panel_collection() -> Option<PanelCollection> {
    PANELS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .cloned()
}

fn ensure_class() -> Result<(), String> {
    crate::util::register_window_class(w!("AltDWM_Panel"), panel_wndproc, "Panel")
}

fn monitor_rect(monitor: HMONITOR) -> Option<RECT> {
    unsafe {
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(monitor, &mut mi as *mut _ as *mut _)
            .as_bool()
            .then_some(mi.rcMonitor)
    }
}

fn resolve_panel_monitors(target: &str) -> Vec<(HMONITOR, RECT)> {
    let mons = crate::manager::get_all_monitors();
    if mons.is_empty() {
        return Vec::new();
    }
    let lower = target.to_lowercase();
    if lower == "all" {
        return mons
            .iter()
            .filter_map(|monitor| monitor_rect(*monitor).map(|rect| (*monitor, rect)))
            .collect();
    }
    let single: Option<HMONITOR> = if lower == "primary" || lower == "1" {
        mons.iter()
            .copied()
            .find(|monitor| unsafe {
                let mut mi = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                GetMonitorInfoW(*monitor, &mut mi as *mut _ as *mut _).as_bool()
                    && (mi.dwFlags & 1) != 0
            })
            .or_else(|| mons.first().copied())
    } else if let Ok(idx) = lower.parse::<usize>() {
        (idx >= 1 && idx <= mons.len()).then(|| mons[idx - 1])
    } else {
        mons.iter().copied().find(|monitor| unsafe {
            let mut ex = MONITORINFOEXW {
                monitorInfo: MONITORINFO {
                    cbSize: size_of::<MONITORINFOEXW>() as u32,
                    ..Default::default()
                },
                szDevice: [0; 32],
            };
            GetMonitorInfoW(*monitor, &mut ex as *mut _ as *mut _ as *mut MONITORINFO).as_bool()
                && String::from_utf16_lossy(&ex.szDevice)
                    .trim_matches(char::from(0))
                    .to_lowercase()
                    .contains(&lower)
        })
    };
    single
        .and_then(|monitor| monitor_rect(monitor).map(|rect| vec![(monitor, rect)]))
        .or_else(|| {
            mons.first()
                .and_then(|monitor| monitor_rect(*monitor).map(|rect| vec![(*monitor, rect)]))
        })
        .unwrap_or_default()
}

fn panel_rect(config: &PanelConfig, monitor: RECT, edge_offset: i32) -> RECT {
    let [margin_top, margin_right, margin_bottom, margin_left] = config.margins();
    let monitor_width = monitor.right - monitor.left;
    let monitor_height = monitor.bottom - monitor.top;
    match config.position.as_str() {
        "top" => RECT {
            left: monitor.left + margin_left,
            top: monitor.top + edge_offset + margin_top,
            right: monitor.right - margin_right,
            bottom: monitor.top + edge_offset + margin_top + config.height,
        },
        "bottom" => RECT {
            left: monitor.left + margin_left,
            top: monitor.bottom - edge_offset - margin_bottom - config.height,
            right: monitor.right - margin_right,
            bottom: monitor.bottom - edge_offset - margin_bottom,
        },
        "left" => RECT {
            left: monitor.left + edge_offset + margin_left,
            top: monitor.top + margin_top,
            right: monitor.left + edge_offset + margin_left + config.height,
            bottom: monitor.bottom - margin_bottom,
        },
        "right" => RECT {
            left: monitor.right - edge_offset - margin_right - config.height,
            top: monitor.top + margin_top,
            right: monitor.right - edge_offset - margin_right,
            bottom: monitor.bottom - margin_bottom,
        },
        _ => RECT {
            left: monitor.left,
            top: monitor.top,
            right: monitor.left + monitor_width.max(1),
            bottom: monitor.top + monitor_height.max(1),
        },
    }
}

/// Re-place one panel from its configuration at its monitor's current scale.
fn reposition_panel(hwnd: HWND) {
    let Some(panel_arc) = panel_collection() else {
        return;
    };
    let Ok(panels) = panel_arc.try_lock() else {
        return;
    };
    let Some(panel) = panels.iter().find(|panel| panel.hwnd == hwnd) else {
        return;
    };
    let Some(area) = monitor_rect(panel.monitor) else {
        return;
    };
    let scale = ui::scale_for_monitor(panel.monitor);
    let scaled = scaled_panel_config(&panel.cfg, scale);
    let rect = panel_rect(&scaled, area, ui::px(panel.edge_offset, scale));
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            rect.left,
            rect.top,
            (rect.right - rect.left).max(1),
            (rect.bottom - rect.top).max(1),
            SWP_NOACTIVATE,
        );
    }
}

/// Re-place every panel. Called after a resolution change, a monitor being
/// attached or removed, or a DPI change on any display.
pub fn reposition_all() {
    let Some(panel_arc) = panel_collection() else {
        return;
    };
    let handles: Vec<HWND> = {
        let Ok(panels) = panel_arc.try_lock() else {
            return;
        };
        panels.iter().map(|panel| panel.hwnd).collect()
    };
    for hwnd in handles {
        reposition_panel(hwnd);
    }
    invalidate_all();
}

pub fn create_panels(cfg: &Config) -> Result<Vec<HWND>, String> {
    if cfg.panels.is_empty() {
        return Ok(vec![]);
    }

    ensure_class()?;
    let mut handles = Vec::new();

    let widget_map: HashMap<String, crate::config::WidgetConfig> = cfg
        .widgets
        .iter()
        .map(|w| (w.name.clone(), w.clone()))
        .collect();

    let panels_arc = Arc::new(Mutex::new(Vec::<Panel>::new()));
    *PANELS.lock().unwrap_or_else(|e| e.into_inner()) = Some(panels_arc.clone());
    // Stacking offsets accumulate in device-independent pixels so the same
    // configuration stacks identically on displays with different scales.
    let mut edge_offsets: HashMap<(isize, String), i32> = HashMap::new();

    for pc in &cfg.panels {
        if !["top", "right", "bottom", "left"].contains(&pc.position.as_str()) {
            eprintln!(
                "[panel] invalid position '{}' for '{}' — skipped",
                pc.position, pc.name
            );
            continue;
        }
        let targets = resolve_panel_monitors(&pc.monitor);
        if targets.is_empty() {
            eprintln!(
                "[panel] no monitor resolved for '{}' monitor='{}' — skipped",
                pc.name, pc.monitor
            );
            continue;
        }
        let bg = pc
            .background
            .as_deref()
            .map(|s| cfg.theme.color(s))
            .unwrap_or_else(|| cfg.theme.panel_bg(&pc.position));

        for (mon_idx, (hmon, mon_rect)) in targets.iter().enumerate() {
            let offset = edge_offsets
                .entry((hmon.0 as isize, pc.position.clone()))
                .or_default();
            let edge_offset = *offset;
            *offset += pc.edge_consumption();

            let scale = ui::scale_for_monitor(*hmon);
            let scaled = scaled_panel_config(pc, scale);
            let rect = panel_rect(&scaled, *mon_rect, ui::px(edge_offset, scale));
            let x = rect.left;
            let y = rect.top;
            let w = (rect.right - rect.left).max(1);
            let h = (rect.bottom - rect.top).max(1);

            let mut widgets_inst: Vec<Arc<dyn Widget>> = Vec::new();
            for wname in &pc.widgets {
                if let Some(wcfg) = widget_map.get(wname) {
                    widgets_inst.push(Arc::from(widgets::create_widget(wcfg)));
                } else if let Some(wcfg) = crate::config::builtin_widget_config(wname) {
                    widgets_inst.push(Arc::from(widgets::create_widget(&wcfg)));
                } else {
                    eprintln!(
                        "[panel] unknown widget '{}' for panel '{}' — custom fallback",
                        wname, pc.name
                    );
                    let wcfg = crate::config::WidgetConfig {
                        widget_type: "custom".into(),
                        name: wname.clone(),
                        format: None,
                        interval: None,
                        script: None,
                        action: None,
                        command: None,
                        label: Some(wname.clone()),
                        icon: None,
                        width: Some(80),
                        extra: HashMap::new(),
                    };
                    widgets_inst.push(Arc::from(widgets::create_widget(&wcfg)));
                }
            }
            let hwnd = unsafe {
                let hinstance = HINSTANCE(std::ptr::null_mut());
                CreateWindowExW(
                    // WS_EX_NOACTIVATE keeps clicking the bar from stealing
                    // focus from the window the click is about to act on.
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("AltDWM_Panel"),
                    w!("AltDWM Panel"),
                    WS_POPUP | WS_VISIBLE,
                    x,
                    y,
                    w,
                    h,
                    None,
                    Some(HMENU(std::ptr::null_mut())),
                    Some(hinstance),
                    None,
                )
                .map_err(|e| format!("Panel {} CreateWindowExW: {:?}", pc.name, e))?
            };
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    x,
                    y,
                    w,
                    h,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                let _ = ShowWindow(hwnd, SW_SHOW);
                apply_panel_chrome(hwnd);
                // Keep sub-second widgets responsive without polling every panel
                // at that rate.
                let scripted = widgets_inst.iter().any(|widget| widget.kind() == "script");
                let fast_interval = if scripted {
                    // A script may change its own interval on disk. Keep a cheap
                    // scheduler heartbeat so that change can take effect without
                    // recreating the panel; the widget itself skips evaluation
                    // until it is due.
                    Some(50)
                } else {
                    widgets_inst
                        .iter()
                        .filter_map(|widget| widget.interval_ms())
                        .min()
                        .filter(|interval| *interval < 1000)
                };
                if let Some(interval) = fast_interval {
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(
                        Some(hwnd),
                        TIMER_FAST,
                        interval.max(50),
                        None,
                    );
                }
            }
            let panel = Panel {
                cfg: pc.clone(),
                hwnd,
                widgets: widgets_inst,
                background: bg,
                monitor: *hmon,
                edge_offset,
            };
            // Populate widget state once up front so the first paint is not
            // blank while waiting for the first timer tick. Script widgets need
            // the actual panel context and their resolved rectangle.
            let client = RECT {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            };
            let initial_ctx = build_ctx(&panel, client, hwnd, crate::manager::window_snapshot());
            let initial_rects = widget_rects(&panel, client, &initial_ctx);
            for (widget, item) in panel.widgets.iter().zip(initial_rects) {
                widget.refresh(&initial_ctx, item);
            }
            let widget_summary = panel
                .widgets
                .iter()
                .map(|widget| format!("{}:{}", widget.name(), widget.kind()))
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "[panel] '{}' @ {},{} {}x{} monitor={} (target {}/{}) scale={:.2} widgets={}",
                pc.name,
                x,
                y,
                w,
                h,
                pc.monitor,
                mon_idx + 1,
                targets.len(),
                scale,
                widget_summary
            );
            handles.push(hwnd);
            panels_arc
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(panel);
        }
    }

    if handles.is_empty() {
        *PANELS.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Err("configuration did not produce any panel windows".to_string())
    } else {
        Ok(handles)
    }
}

pub fn destroy_panels() {
    if let Some(arc) = PANELS.lock().unwrap_or_else(|e| e.into_inner()).take() {
        for p in arc.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(p.hwnd);
            }
        }
    }
    POINTERS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

#[cfg(test)]
mod tests {
    use super::{panel_rect, panel_should_show, scaled_panel_config};
    use crate::config::PanelConfig;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::HMONITOR;

    const MONITOR: RECT = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };

    #[test]
    fn fullscreen_hides_only_the_panel_on_its_monitor() {
        let primary = HMONITOR::default();
        let secondary = HMONITOR(std::ptr::dangling_mut::<std::ffi::c_void>());
        assert!(!panel_should_show(Some(secondary), secondary));
        assert!(panel_should_show(Some(secondary), primary));
        assert!(panel_should_show(None, primary));
        assert!(panel_should_show(None, secondary));
    }

    #[test]
    fn same_edge_panels_stack_instead_of_overlap() {
        let panel = PanelConfig {
            position: "top".into(),
            height: 30,
            margin: Some([2, 4, 3, 5]),
            ..PanelConfig::default()
        };
        let monitor = RECT {
            left: 100,
            top: 50,
            right: 1100,
            bottom: 850,
        };
        let first = panel_rect(&panel, monitor, 0);
        let second = panel_rect(&panel, monitor, panel.edge_consumption());
        assert_eq!(
            (first.left, first.top, first.right, first.bottom),
            (105, 52, 1096, 82)
        );
        assert_eq!(second.top, 87);
    }

    #[test]
    fn vertical_panels_use_height_as_thickness() {
        let panel = PanelConfig {
            position: "left".into(),
            height: 42,
            margin: Some([3, 4, 5, 6]),
            ..PanelConfig::default()
        };
        let rect = panel_rect(&panel, MONITOR, 0);
        assert_eq!(
            (rect.left, rect.top, rect.right, rect.bottom),
            (6, 3, 48, 1075)
        );
    }

    /// A bar configured as 40 device-independent pixels must occupy 60 physical
    /// pixels on a 150% display, not 40.
    #[test]
    fn panel_geometry_scales_with_display_dpi() {
        let panel = PanelConfig {
            position: "bottom".into(),
            height: 40,
            margin: Some([0, 8, 8, 8]),
            ..PanelConfig::default()
        };
        let scaled = scaled_panel_config(&panel, 1.5);
        assert_eq!(scaled.height, 60);
        assert_eq!(scaled.margins(), [0, 12, 12, 12]);
        let rect = panel_rect(&scaled, MONITOR, 0);
        assert_eq!(rect.bottom - rect.top, 60);
        assert_eq!(rect.left, 12);
        assert_eq!(MONITOR.bottom - rect.bottom, 12);
    }

    #[test]
    fn unscaled_geometry_is_unchanged() {
        let panel = PanelConfig {
            height: 40,
            margin: Some([1, 2, 3, 4]),
            ..PanelConfig::default()
        };
        let scaled = scaled_panel_config(&panel, 1.0);
        assert_eq!(scaled.height, 40);
        assert_eq!(scaled.margins(), [1, 2, 3, 4]);
    }
}

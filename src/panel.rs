//! Panel manager — turns `PanelConfig` + `WidgetConfig` into Win32 windows.
//! Each panel is a `WS_POPUP | WS_EX_TOPMOST` with its own WNDCLASS and widget layout.
//! For MVP, panels repaint on WM_PAINT and 1s WM_TIMER; custom widgets can request faster interval.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC,
    DeleteObject, EndPaint, FillRect, GetMonitorInfoW, HMONITOR, MONITORINFO, MONITORINFOEXW,
    PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, SetWindowPos, ShowWindow, HMENU, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_MOUSEMOVE, WM_PAINT, WM_TIMER, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::config::{Config, PanelConfig};
use crate::widgets::{self, PanelCtx, Widget};

type PanelCollection = Arc<Mutex<Vec<Panel>>>;

static PANELS: std::sync::LazyLock<Mutex<Option<PanelCollection>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));
static HOVERED_WIDGETS: std::sync::LazyLock<Mutex<HashMap<isize, Option<usize>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
const WM_MOUSELEAVE_RAW: u32 = 0x02A3;

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

pub struct Panel {
    pub cfg: PanelConfig,
    pub hwnd: HWND,
    pub widgets: Vec<Box<dyn Widget>>,
    pub background: COLORREF,
}
unsafe impl Send for Panel {}
unsafe impl Sync for Panel {}

fn apply_panel_chrome(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR,
    };
    // DWMWA constants that may not be in Windows 0.61 as typed enums — use raw values
    const DWMWA_WINDOW_CORNER_PREFERENCE_RAW: u32 = 33;
    const DWMWA_SYSTEMBACKDROP_TYPE_RAW: u32 = 38;
    const DWMWCP_ROUND: u32 = 2;
    const DWMSBT_MAINWINDOW: u32 = 2; // Mica
    unsafe {
        // rounded corners
        let corner: u32 = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(
                DWMWA_WINDOW_CORNER_PREFERENCE_RAW as i32,
            ),
            &corner as *const _ as _,
            size_of_val(&corner) as u32,
        );
        // Mica backdrop (Win11 22621+)
        let backdrop: u32 = DWMSBT_MAINWINDOW;
        let _ = DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(DWMWA_SYSTEMBACKDROP_TYPE_RAW as i32),
            &backdrop as *const _ as _,
            size_of_val(&backdrop) as u32,
        );
        // subtle border color from theme if available
        if let Ok(cfg) = crate::CURRENT_CONFIG.try_lock() {
            let border = cfg.theme.border_color();
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                &border.0 as *const _ as _,
                size_of_val(&border) as u32,
            );
            let caption = cfg.theme.panel_bg("bottom");
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_CAPTION_COLOR,
                &caption.0 as *const _ as _,
                size_of_val(&caption) as u32,
            );
        }
    }
}

fn resolve_widget_layout(panel: &Panel, rect: RECT, hwnd: HWND) -> (PanelCtx, bool, Vec<i32>) {
    let vertical = matches!(panel.cfg.position.as_str(), "left" | "right");
    let total_extent = if vertical {
        rect.bottom - rect.top
    } else {
        rect.right - rect.left
    };
    let mut windows =
        crate::manager::collect_windows_including_minimized(crate::taskbar::get_taskbar_hwnd());
    windows.retain(|window| crate::virtual_desktop::is_on_current_desktop(*window));
    let ctx = PanelCtx {
        panel_name: panel.cfg.name.clone(),
        monitor: panel.cfg.monitor.clone(),
        width: total_extent,
        height: panel.cfg.height,
        hwnd,
        windows,
    };
    let requested: Vec<i32> = panel
        .widgets
        .iter()
        .map(|widget| widget.width(&ctx))
        .collect();
    let fixed: i32 = requested.iter().copied().filter(|width| *width != 0).sum();
    let flex_count = requested.iter().filter(|width| **width == 0).count() as i32;
    let flex_width = if flex_count > 0 {
        (total_extent - fixed).max(0) / flex_count
    } else {
        0
    };
    let widths = requested
        .into_iter()
        .map(|width| if width == 0 { flex_width } else { width })
        .collect();
    (ctx, vertical, widths)
}

unsafe extern "system" fn panel_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(hwnd), 1, 1000, None);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == 1 || wparam.0 == 2 {
                // The paint path fills every pixel itself. Asking Windows to erase
                // first exposes a blank frame on every clock/widget tick.
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            // find panel
            let panels_guard = PANELS.lock().unwrap_or_else(|e| e.into_inner());
            let panel_arc = panels_guard.as_ref().cloned();
            drop(panels_guard);
            if let Some(panel_arc) = panel_arc {
                let panels = panel_arc.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(p) = panels.iter().find(|p| p.hwnd == hwnd) {
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
                    let brush = CreateSolidBrush(p.background);
                    FillRect(draw_dc, &rect, brush);
                    let _ = DeleteObject(brush.into());

                    // Layout along the panel's long axis: horizontal bars and vertical docks.
                    let (ctx, vertical, widths) = resolve_widget_layout(p, rect, hwnd);
                    let mut x = if vertical { rect.top } else { rect.left };
                    let hovered = HOVERED_WIDGETS
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .get(&(hwnd.0 as isize))
                        .copied()
                        .flatten();
                    for (index, (widget, width)) in p.widgets.iter().zip(widths).enumerate() {
                        let r = if vertical {
                            RECT {
                                left: rect.left,
                                top: x,
                                right: rect.right,
                                bottom: x + width,
                            }
                        } else {
                            RECT {
                                left: x,
                                top: rect.top,
                                right: x + width,
                                bottom: rect.bottom,
                            }
                        };
                        if hovered == Some(index) && widget.name() != "spacer" {
                            let inset = RECT {
                                left: r.left + 3,
                                top: r.top + 3,
                                right: r.right - 3,
                                bottom: r.bottom - 3,
                            };
                            let theme = crate::CURRENT_CONFIG
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .theme
                                .clone();
                            let region = windows::Win32::Graphics::Gdi::CreateRoundRectRgn(
                                inset.left,
                                inset.top,
                                inset.right,
                                inset.bottom,
                                theme.rounding.max(8),
                                theme.rounding.max(8),
                            );
                            let brush = CreateSolidBrush(theme.surface_hover_color());
                            let _ = windows::Win32::Graphics::Gdi::FillRgn(draw_dc, region, brush);
                            let _ = DeleteObject(region.into());
                            let _ = DeleteObject(brush.into());
                        }
                        widget.draw(draw_dc, r, &ctx);
                        x += width;
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
                    return LRESULT(0);
                }
            }
            let mut ps = PAINTSTRUCT::default();
            let _hdc = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            // route to widget
            let panels_guard = PANELS.lock().unwrap_or_else(|e| e.into_inner());
            let panel_arc = panels_guard.as_ref().cloned();
            drop(panels_guard);
            if let Some(panel_arc) = panel_arc {
                let panels = panel_arc.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(p) = panels.iter().find(|p| p.hwnd == hwnd) {
                    let mut rect = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rect);
                    let (ctx, vertical, widths) = resolve_widget_layout(p, rect, hwnd);
                    let mut cur = 0;
                    let click_main = if vertical { y } else { x };
                    for (widget, width) in p.widgets.iter().zip(widths) {
                        if click_main >= cur && click_main < cur + width {
                            if let Some(action) = widget.on_click(
                                click_main - cur,
                                if vertical { x } else { y },
                                &ctx,
                            ) {
                                println!(
                                    "[panel {}] widget '{}' click -> {}",
                                    p.cfg.name,
                                    widget.name(),
                                    action
                                );
                                crate::scripting::dispatch_action(&action);
                            }
                            break;
                        }
                        cur += width;
                    }
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let panels_guard = PANELS.lock().unwrap_or_else(|e| e.into_inner());
            let panel_arc = panels_guard.as_ref().cloned();
            drop(panels_guard);
            if let Some(panel_arc) = panel_arc {
                let panels = panel_arc.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(p) = panels.iter().find(|p| p.hwnd == hwnd) {
                    let mut rect = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rect);
                    let (_, vertical, widths) = resolve_widget_layout(p, rect, hwnd);
                    let point = if vertical { y } else { x };
                    let mut edge = 0;
                    let mut next = None;
                    for (index, width) in widths.iter().enumerate() {
                        if point >= edge && point < edge + *width {
                            next = Some(index);
                            break;
                        }
                        edge += *width;
                    }
                    let mut hovered = HOVERED_WIDGETS
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if hovered.get(&(hwnd.0 as isize)).copied().flatten() != next {
                        hovered.insert(hwnd.0 as isize, next);
                        let _ =
                            windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
                    }
                    let mut tracking = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tracking);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE_RAW => {
            HOVERED_WIDGETS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&(hwnd.0 as isize));
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(hwnd), 1);
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(hwnd), 2);
            HOVERED_WIDGETS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&(hwnd.0 as isize));
            // don't PostQuitMessage — panels are not main loop; host is
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn ensure_class() -> Result<(), String> {
    crate::util::register_window_class(w!("AltDWM_Panel"), panel_wndproc, "Panel")
}

fn resolve_panel_monitors(target: &str) -> Vec<(HMONITOR, RECT)> {
    let mons = crate::manager::get_all_monitors();
    if mons.is_empty() {
        return Vec::new();
    }
    let lower = target.to_lowercase();
    if lower == "all" {
        let mut out = Vec::new();
        for &h in &mons {
            unsafe {
                let mut mi = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(h, &mut mi as *mut _ as *mut _).as_bool() {
                    out.push((h, mi.rcMonitor));
                }
            }
        }
        return out;
    }
    // single monitor resolution - reuse manager logic but return rect
    let single: Option<HMONITOR> = if lower == "primary" || lower == "1" {
        let mut prim = None;
        for &h in &mons {
            unsafe {
                let mut mi = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(h, &mut mi as *mut _ as *mut _).as_bool()
                    && (mi.dwFlags & 1) != 0
                {
                    prim = Some(h);
                    break;
                }
            }
        }
        prim.or(Some(mons[0]))
    } else if let Ok(idx) = lower.parse::<usize>() {
        if idx >= 1 && idx <= mons.len() {
            Some(mons[idx - 1])
        } else {
            None
        }
    } else {
        // device name substring
        let mut found = None;
        for &h in &mons {
            unsafe {
                let mut ex = MONITORINFOEXW {
                    monitorInfo: MONITORINFO {
                        cbSize: size_of::<MONITORINFOEXW>() as u32,
                        ..Default::default()
                    },
                    szDevice: [0; 32],
                };
                if GetMonitorInfoW(h, &mut ex as *mut _ as *mut _ as *mut MONITORINFO).as_bool() {
                    let dev = String::from_utf16_lossy(&ex.szDevice)
                        .trim_matches(char::from(0))
                        .to_string();
                    if dev.to_lowercase().contains(&lower) {
                        found = Some(h);
                        break;
                    }
                }
            }
        }
        found
    };
    if let Some(h) = single {
        unsafe {
            let mut mi = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(h, &mut mi as *mut _ as *mut _).as_bool() {
                return vec![(h, mi.rcMonitor)];
            }
        }
    }
    // fallback: primary
    unsafe {
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(mons[0], &mut mi as *mut _ as *mut _).as_bool() {
            return vec![(mons[0], mi.rcMonitor)];
        }
    }
    Vec::new()
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

pub fn create_panels(cfg: &Config) -> Result<Vec<HWND>, String> {
    if cfg.panels.is_empty() {
        return Ok(vec![]);
    }

    ensure_class()?;
    let mut handles = Vec::new();

    // Map widget names to configs for lookup
    let widget_map: HashMap<String, crate::config::WidgetConfig> = cfg
        .widgets
        .iter()
        .map(|w| (w.name.clone(), w.clone()))
        .collect();

    let panels_arc = Arc::new(Mutex::new(Vec::<Panel>::new()));
    *PANELS.lock().unwrap_or_else(|e| e.into_inner()) = Some(panels_arc.clone());
    let mut edge_offsets: HashMap<(isize, String), i32> = HashMap::new();

    for pc in &cfg.panels {
        // position — per-monitor aware (fixes SM_CXSCREEN single-monitor bug)
        let targets = resolve_panel_monitors(&pc.monitor);
        if targets.is_empty() {
            eprintln!(
                "[panel] no monitor resolved for '{}' monitor='{}' — skipped",
                pc.name, pc.monitor
            );
            continue;
        }
        let theme = crate::CURRENT_CONFIG
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .theme
            .clone();
        let bg = pc
            .background
            .as_deref()
            .map(|s| theme.color(s))
            .unwrap_or_else(|| theme.panel_bg(&pc.position));

        for (mon_idx, (hmon, mon_rect)) in targets.iter().enumerate() {
            let offset = edge_offsets
                .entry((hmon.0 as isize, pc.position.clone()))
                .or_default();
            if !["top", "right", "bottom", "left"].contains(&pc.position.as_str()) {
                eprintln!(
                    "[panel] invalid position '{}' for '{}' — skipped",
                    pc.position, pc.name
                );
                continue;
            }
            let rect = panel_rect(pc, *mon_rect, *offset);
            let x = rect.left;
            let y = rect.top;
            let w = (rect.right - rect.left).max(1);
            let h = (rect.bottom - rect.top).max(1);
            *offset += pc.edge_consumption();
            let mut widgets_inst: Vec<Box<dyn Widget>> = Vec::new();
            for wname in &pc.widgets {
                if let Some(wcfg) = widget_map.get(wname) {
                    widgets_inst.push(widgets::create_widget(wcfg));
                } else if let Some(wcfg) = crate::config::builtin_widget_config(wname) {
                    widgets_inst.push(widgets::create_widget(&wcfg));
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
                    widgets_inst.push(widgets::create_widget(&wcfg));
                }
            }
            let hwnd = unsafe {
                let hinstance = HINSTANCE(std::ptr::null_mut());
                CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
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
                // Keep sub-second widgets responsive without polling every panel at 250 ms.
                if let Some(interval) = widgets_inst
                    .iter()
                    .filter_map(|widget| widget.interval_ms())
                    .min()
                    .filter(|interval| *interval < 1000)
                {
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(
                        Some(hwnd),
                        2,
                        interval,
                        None,
                    );
                }
            }
            println!(
                "[panel] '{}' @ {},{} {}x{} monitor={} (target {}/{}) widgets={}",
                pc.name,
                x,
                y,
                w,
                h,
                pc.monitor,
                mon_idx + 1,
                targets.len(),
                pc.widgets.join(",")
            );
            handles.push(hwnd);
            panels_arc
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Panel {
                    cfg: pc.clone(),
                    hwnd,
                    widgets: widgets_inst,
                    background: bg,
                });
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
}

#[cfg(test)]
mod tests {
    use super::panel_rect;
    use crate::config::PanelConfig;
    use windows::Win32::Foundation::RECT;

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
        let rect = panel_rect(
            &panel,
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            0,
        );
        assert_eq!(
            (rect.left, rect.top, rect.right, rect.bottom),
            (6, 3, 48, 1075)
        );
    }
}

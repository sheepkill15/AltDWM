//! Panel manager — turns `PanelConfig` + `WidgetConfig` into Win32 windows.
//! Each panel is a `WS_POPUP | WS_EX_TOPMOST` with its own WNDCLASS and widget layout.
//! For MVP, panels repaint on WM_PAINT and 1s WM_TIMER; custom widgets can request faster interval.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect, HBRUSH, BeginPaint, EndPaint, PAINTSTRUCT, GetMonitorInfoW, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, RegisterClassExW, ShowWindow,
    WNDCLASSEXW, CS_HREDRAW, CS_VREDRAW, HMENU, SW_SHOW, WM_CREATE, WM_DESTROY, WM_PAINT, WM_TIMER, WM_LBUTTONDOWN,
    WS_EX_APPWINDOW, WS_EX_TOPMOST, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
    SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, HWND_TOPMOST,
};

use crate::config::{PanelConfig, Config};
use crate::widgets::{self, Widget, PanelCtx};

static PANELS: std::sync::LazyLock<Mutex<Option<Arc<Mutex<Vec<Panel>>>>>> = std::sync::LazyLock::new(|| Mutex::new(None));

pub struct Panel {
    pub cfg: PanelConfig,
    pub hwnd: HWND,
    pub widgets: Vec<Box<dyn Widget>>,
    pub background: COLORREF,
}
unsafe impl Send for Panel {}
unsafe impl Sync for Panel {}

fn apply_panel_chrome(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR};
    // DWMWA constants that may not be in windows 0.61 as typed enums — use raw values
    const DWMWA_WINDOW_CORNER_PREFERENCE_RAW: u32 = 33;
    const DWMWA_SYSTEMBACKDROP_TYPE_RAW: u32 = 38;
    const DWMWCP_ROUND: u32 = 2;
    const DWMSBT_MAINWINDOW: u32 = 2; // Mica
    unsafe {
        // rounded corners
        let corner: u32 = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(DWMWA_WINDOW_CORNER_PREFERENCE_RAW as i32), &corner as *const _ as _, std::mem::size_of_val(&corner) as u32);
        // Mica backdrop (Win11 22621+)
        let backdrop: u32 = DWMSBT_MAINWINDOW;
        let _ = DwmSetWindowAttribute(hwnd, windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(DWMWA_SYSTEMBACKDROP_TYPE_RAW as i32), &backdrop as *const _ as _, std::mem::size_of_val(&backdrop) as u32);
        // subtle border color from theme if available
        if let Ok(cfg) = crate::CURRENT_CONFIG.try_lock() {
            let border = cfg.theme.border_color();
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border.0 as *const _ as _, std::mem::size_of_val(&border) as u32);
            let caption = cfg.theme.panel_bg("bottom");
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &caption.0 as *const _ as _, std::mem::size_of_val(&caption) as u32);
        }
    }
}

unsafe extern "system" fn panel_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(hwnd), 1, 1000, None);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == 1 || wparam.0 == 2 {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, true);
            }
            LRESULT(0)
        }
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
                let brush = CreateSolidBrush(p.background);
                FillRect(hdc, &rect, brush);
                let _ = DeleteObject(brush.into());

                // Layout along the panel's long axis: horizontal bars and vertical docks.
                let vertical = matches!(p.cfg.position.as_str(), "left" | "right");
                let total_w = if vertical { rect.bottom - rect.top } else { rect.right - rect.left };
                let mut fixed: i32 = 0;
                let mut flex_count = 0;
                let mut widths: Vec<i32> = Vec::new();
                let ctx = PanelCtx { panel_name: p.cfg.name.clone(), monitor: p.cfg.monitor.clone(), width: total_w, height: p.cfg.height, hwnd };
                for w in &p.widgets {
                    let wd = w.width(&ctx);
                    widths.push(wd);
                    if wd == 0 { flex_count += 1; } else { fixed += wd; }
                }
                let flex_w = if flex_count > 0 { (total_w - fixed).max(0) / flex_count } else { 0 };
                let mut x = if vertical { rect.top } else { rect.left };
                for (i, w) in p.widgets.iter().enumerate() {
                    let wd = if widths[i]==0 { flex_w } else { widths[i] };
                    let r = if vertical {
                        RECT { left: rect.left, top: x, right: rect.right, bottom: x + wd }
                    } else {
                        RECT { left: x, top: rect.top, right: x + wd, bottom: rect.bottom }
                    };
                    w.draw(hdc, r, &ctx);
                    x += wd;
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
                let vertical = matches!(p.cfg.position.as_str(), "left" | "right");
                let total_w = if vertical { rect.bottom - rect.top } else { rect.right - rect.left };
                let ctx = PanelCtx { panel_name: p.cfg.name.clone(), monitor: p.cfg.monitor.clone(), width: total_w, height: p.cfg.height, hwnd };
                let mut fixed: i32 = 0;
                let mut flex_count = 0;
                let mut widths: Vec<i32> = Vec::new();
                for w in &p.widgets { let wd=w.width(&ctx); widths.push(wd); if wd==0{flex_count+=1;} else{fixed+=wd;} }
                let flex_w = if flex_count>0 { (total_w-fixed).max(0)/flex_count } else {0};
                let mut cur=0;
                let click_main = if vertical { y } else { x };
                for (i,w) in p.widgets.iter().enumerate(){
                    let wd = if widths[i]==0{flex_w}else{widths[i]};
                    if click_main >= cur && click_main < cur+wd {
                        if let Some(action) = w.on_click(click_main-cur, if vertical { x } else { y }, &ctx) {
                            println!("[panel {}] widget '{}' click -> {}", p.cfg.name, w.name(), action);
                            crate::scripting::dispatch_action(&action);
                        }
                        break;
                    }
                    cur+=wd;
                }
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(hwnd), 1);
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(hwnd), 2);
            // don't PostQuitMessage — panels are not main loop; host is
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn ensure_class() -> Result<(), String> {
    unsafe {
        let hinstance = HINSTANCE(std::ptr::null_mut());
        let class_name = w!("AltDWM_Panel");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(panel_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance.into(),
            hIcon: Default::default(),
            hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(Some(hinstance), windows::Win32::UI::WindowsAndMessaging::IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: windows::core::PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: Default::default(),
        };
        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            let err = windows::Win32::Foundation::GetLastError();
            if err.0 != 1410 { return Err(format!("Panel RegisterClassExW: {:?}", err)); }
        }
        Ok(())
    }
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
                let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
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
                let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                if GetMonitorInfoW(h, &mut mi as *mut _ as *mut _).as_bool() && (mi.dwFlags & 1) != 0 {
                    prim = Some(h); break;
                }
            }
        }
        prim.or(Some(mons[0]))
    } else if let Ok(idx) = lower.parse::<usize>() {
        if idx >= 1 && idx <= mons.len() { Some(mons[idx-1]) } else { None }
    } else {
        // device name substring
        let mut found = None;
        for &h in &mons {
            unsafe {
                let mut ex = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, szDevice: [0; 32] };
                if GetMonitorInfoW(h, &mut ex as *mut _ as *mut _ as *mut MONITORINFO).as_bool() {
                    let dev = String::from_utf16_lossy(&ex.szDevice).trim_matches(char::from(0)).to_string();
                    if dev.to_lowercase().contains(&lower) { found = Some(h); break; }
                }
            }
        }
        found
    };
    if let Some(h) = single {
        unsafe {
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(h, &mut mi as *mut _ as *mut _).as_bool() {
                return vec![(h, mi.rcMonitor)];
            }
        }
    }
    // fallback: primary
    unsafe {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mons[0], &mut mi as *mut _ as *mut _).as_bool() {
            return vec![(mons[0], mi.rcMonitor)];
        }
    }
    Vec::new()
}

pub fn create_panels(cfg: &Config) -> Result<Vec<HWND>, String> {
    if cfg.panels.is_empty() { return Ok(vec![]); }

    ensure_class()?;
    let mut handles = Vec::new();

    // Map widget names to configs for lookup
    let widget_map: HashMap<String, crate::config::WidgetConfig> = cfg.widgets.iter().map(|w| (w.name.clone(), w.clone())).collect();

    let panels_arc = Arc::new(Mutex::new(Vec::<Panel>::new()));
    *PANELS.lock().unwrap_or_else(|e| e.into_inner()) = Some(panels_arc.clone());

    for pc in &cfg.panels {
        // position — per-monitor aware (fixes SM_CXSCREEN single-monitor bug)
        let targets = resolve_panel_monitors(&pc.monitor);
        if targets.is_empty() {
            eprintln!("[panel] no monitor resolved for '{}' monitor='{}' — skipped", pc.name, pc.monitor);
            continue;
        }
        let theme = crate::CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()).theme.clone();
        let bg = pc.background.as_deref().map(|s| theme.color(s)).unwrap_or_else(|| theme.panel_bg(&pc.position));

        for (mon_idx, (_, mon_rect)) in targets.iter().enumerate() {
            let (x, y, w, h) = match pc.position.as_str() {
                "top" => (mon_rect.left, mon_rect.top, mon_rect.right - mon_rect.left, pc.height),
                "bottom" => (mon_rect.left, mon_rect.bottom - pc.height, mon_rect.right - mon_rect.left, pc.height),
                "left" => (mon_rect.left, mon_rect.top, pc.height, mon_rect.bottom - mon_rect.top),
                "right" => (mon_rect.right - pc.height, mon_rect.top, pc.height, mon_rect.bottom - mon_rect.top),
                _ => (mon_rect.left, mon_rect.bottom - pc.height, mon_rect.right - mon_rect.left, pc.height),
            };
            let mut widgets_inst: Vec<Box<dyn Widget>> = Vec::new();
            for wname in &pc.widgets {
                if let Some(wcfg) = widget_map.get(wname) {
                    widgets_inst.push(widgets::create_widget(wcfg));
                } else if let Some(wcfg) = crate::config::builtin_widget_config(wname) {
                    widgets_inst.push(widgets::create_widget(&wcfg));
                } else {
                    eprintln!("[panel] unknown widget '{}' for panel '{}' — custom fallback", wname, pc.name);
                    let wcfg = crate::config::WidgetConfig { widget_type: "custom".into(), name: wname.clone(), format: None, interval: None, script: None, action: None, command: None, label: Some(wname.clone()), icon: None, width: Some(80), tooltip: None, extra: HashMap::new() };
                    widgets_inst.push(widgets::create_widget(&wcfg));
                }
            }
            let hwnd = unsafe {
                let hinstance = HINSTANCE(std::ptr::null_mut());
                CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_APPWINDOW,
                    w!("AltDWM_Panel"),
                    w!("AltDWM Panel"),
                    WS_POPUP | WS_VISIBLE,
                    x, y, w, h,
                    None,
                    Some(HMENU(std::ptr::null_mut())),
                    Some(hinstance),
                    None,
                ).map_err(|e| format!("Panel {} CreateWindowExW: {:?}", pc.name, e))?
            };
            unsafe {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_NOZORDER);
                let _ = ShowWindow(hwnd, SW_SHOW);
                apply_panel_chrome(hwnd);
                // Keep sub-second widgets responsive without polling every panel at 250 ms.
                if let Some(interval) = widgets_inst.iter().filter_map(|widget| widget.interval_ms()).min().filter(|interval| *interval < 1000) {
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(hwnd), 2, interval, None);
                }
            }
            println!("[panel] '{}' @ {},{} {}x{} monitor={} (target {}/{}) widgets={}", pc.name, x, y, w, h, pc.monitor, mon_idx+1, targets.len(), pc.widgets.join(","));
            handles.push(hwnd);
            panels_arc.lock().unwrap_or_else(|e| e.into_inner()).push(Panel { cfg: pc.clone(), hwnd, widgets: widgets_inst, background: bg });
        }
    }

    Ok(handles)
}

pub fn destroy_panels() {
    if let Some(arc) = PANELS.lock().unwrap_or_else(|e| e.into_inner()).take() {
        for p in arc.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            unsafe { let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(p.hwnd); }
        }
    }
}

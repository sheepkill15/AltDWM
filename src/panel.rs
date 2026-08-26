//! Panel manager — turns `PanelConfig` + `WidgetConfig` into Win32 windows.
//! Each panel is a `WS_POPUP | WS_EX_TOPMOST` with its own WNDCLASS and widget layout.
//! For MVP, panels repaint on WM_PAINT and 1s WM_TIMER; custom widgets can request faster interval.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect, HBRUSH, BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetSystemMetrics, PostQuitMessage, RegisterClassExW, ShowWindow,
    WNDCLASSEXW, CS_HREDRAW, CS_VREDRAW, HMENU, SW_SHOW, WM_CREATE, WM_DESTROY, WM_PAINT, WM_TIMER, WM_LBUTTONDOWN,
    WS_EX_APPWINDOW, WS_EX_TOPMOST, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE, SM_CXSCREEN, SM_CYSCREEN,
    SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, HWND_TOPMOST,
};

use crate::config::{PanelConfig, Config};
use crate::widgets::{self, Widget, PanelCtx};

static mut PANELS: Option<Arc<Mutex<Vec<Panel>>>> = None;

pub struct Panel {
    pub cfg: PanelConfig,
    pub hwnd: HWND,
    pub widgets: Vec<Box<dyn Widget>>,
    pub background: COLORREF,
}

fn parse_color(s: &str) -> COLORREF {
    let s = s.trim().trim_start_matches('#');
    if let Ok(v) = u32::from_str_radix(s, 16) {
        // TOML "#RRGGBB" -> COLORREF 0x00BBGGRR ? GDI uses 0x00BBGGRR, but we store as 0x00RRGGBB for CreateSolidBrush which expects RGB
        // CreateSolidBrush expects 0x00BBGGRR? Actually RGB macro = (r | g<<8 | b<<16). "#202020" -> 0x202020 is fineEither way.
        COLORREF(v)
    } else {
        COLORREF(0x00202020)
    }
}

unsafe extern "system" fn panel_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(hwnd), 1, 1000, None);
            // also custom widget max interval
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(hwnd), 2, 250, None);
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
            let panels = unsafe { PANELS.as_ref().unwrap().lock().unwrap() };
            let panel = panels.iter().find(|p| p.hwnd == hwnd);
            if let Some(p) = panel {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let brush = CreateSolidBrush(p.background);
                FillRect(hdc, &rect, brush);
                let _ = DeleteObject(brush.into());

                // layout widgets horizontally: fixed widths + flex spacers split remainder
                let total_w = rect.right - rect.left;
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
                let mut x = rect.left;
                for (i, w) in p.widgets.iter().enumerate() {
                    let wd = if widths[i]==0 { flex_w } else { widths[i] };
                    let r = RECT { left: x, top: rect.top, right: x+wd, bottom: rect.bottom };
                    w.draw(hdc, r, &ctx);
                    x += wd;
                }
                let _ = EndPaint(hwnd, &ps);
            } else {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            // route to widget
            let panels = unsafe { PANELS.as_ref().unwrap().lock().unwrap() };
            if let Some(p) = panels.iter().find(|p| p.hwnd == hwnd) {
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let total_w = rect.right - rect.left;
                let ctx = PanelCtx { panel_name: p.cfg.name.clone(), monitor: p.cfg.monitor.clone(), width: total_w, height: p.cfg.height, hwnd };
                let mut fixed: i32 = 0;
                let mut flex_count = 0;
                let mut widths: Vec<i32> = Vec::new();
                for w in &p.widgets { let wd=w.width(&ctx); widths.push(wd); if wd==0{flex_count+=1;} else{fixed+=wd;} }
                let flex_w = if flex_count>0 { (total_w-fixed).max(0)/flex_count } else {0};
                let mut cur=0;
                for (i,w) in p.widgets.iter().enumerate(){
                    let wd = if widths[i]==0{flex_w}else{widths[i]};
                    if x >= cur && x < cur+wd {
                        if let Some(action) = w.on_click(x-cur, y, &ctx) {
                            println!("[panel {}] widget '{}' click -> {}", p.cfg.name, w.name(), action);
                            crate::scripting::dispatch_action(&action);
                        }
                        break;
                    }
                    cur+=wd;
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

pub fn create_panels(cfg: &Config) -> Result<Vec<HWND>, String> {
    if cfg.panels.is_empty() { return Ok(vec![]); }

    ensure_class()?;
    let mut handles = Vec::new();

    // Map widget names to configs for lookup
    let widget_map: HashMap<String, crate::config::WidgetConfig> = cfg.widgets.iter().map(|w| (w.name.clone(), w.clone())).collect();

    let panels_arc = Arc::new(Mutex::new(Vec::<Panel>::new()));
    unsafe { PANELS = Some(panels_arc.clone()); }

    for pc in &cfg.panels {
        // build widget instances in order
        let mut widgets: Vec<Box<dyn Widget>> = Vec::new();
        for wname in &pc.widgets {
            if let Some(wcfg) = widget_map.get(wname) {
                widgets.push(widgets::create_widget(wcfg));
            } else if wname == "spacer" {
                // built-in spacer without config
                let wcfg = crate::config::WidgetConfig {
                    widget_type: "spacer".into(),
                    name: "spacer".into(),
                    format: None, interval: None, script: None, action: None, command: None, label: None, icon: None, width: None, tooltip: None, extra: HashMap::new()
                };
                widgets.push(widgets::create_widget(&wcfg));
            } else {
                eprintln!("[panel] unknown widget '{}' for panel '{}' — spacer fallback", wname, pc.name);
                let wcfg = crate::config::WidgetConfig { widget_type: "custom".into(), name: wname.clone(), format: None, interval: None, script: None, action: None, command: None, label: Some(wname.clone()), icon: None, width: Some(80), tooltip: None, extra: HashMap::new() };
                widgets.push(widgets::create_widget(&wcfg));
            }
        }

        // position
        let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        let (x, y, w, h) = match pc.position.as_str() {
            "top" => (0, 0, screen_w, pc.height),
            "bottom" => (0, screen_h - pc.height, screen_w, pc.height),
            "left" => (0, 0, pc.height, screen_h),
            "right" => (screen_w - pc.height, 0, pc.height, screen_h),
            _ => (0, screen_h - pc.height, screen_w, pc.height),
        };

        let bg = pc.background.as_deref().map(parse_color).unwrap_or(COLORREF(0x00202020));
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
        }
        println!("[panel] '{}' @ {},{} {}x{} monitor={} widgets={}", pc.name, x, y, w, h, pc.monitor, pc.widgets.join(","));

        handles.push(hwnd);
        panels_arc.lock().unwrap().push(Panel { cfg: pc.clone(), hwnd, widgets, background: bg });
    }

    Ok(handles)
}

pub fn destroy_panels() {
    unsafe {
        if let Some(arc) = PANELS.take() {
            for p in arc.lock().unwrap().iter() {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(p.hwnd);
            }
        }
    }
}

pub fn get_panels() -> Option<Arc<Mutex<Vec<Panel>>>> {
    unsafe { PANELS.clone() }
}

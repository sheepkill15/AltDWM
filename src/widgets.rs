//! Widget trait and built-ins — extensible via Rhai `custom` or Rust `cdylib` plugins.
//! See docs/EXTENSIBILITY.md
use windows::Win32::Foundation::{RECT, HWND};
use windows::Win32::Graphics::Gdi::{HDC, SetBkMode, SetTextColor, TextOutW, TRANSPARENT};
use windows::Win32::Foundation::COLORREF;

use crate::config::WidgetConfig;

/// Context passed to every widget during draw / click
#[derive(Debug, Clone)]
pub struct PanelCtx {
    pub panel_name: String,
    pub monitor: String,
    pub width: i32,
    pub height: i32,
    pub hwnd: HWND,
}

/// Core extensibility point — implement this to add a widget.
/// Register via `inventory` or Rhai `custom` script.
pub trait Widget: Send + Sync {
    fn name(&self) -> &str;
    /// 0 = flex (takes remaining space), >0 = fixed pixels
    fn width(&self, _ctx: &PanelCtx) -> i32 { 0 }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx);
    /// return Some(action) to handle click
    fn on_click(&self, _x: i32, _y: i32, _ctx: &PanelCtx) -> Option<String> { None }
    fn interval_ms(&self) -> Option<u32> { None }
    fn tooltip(&self) -> Option<String> { None }
}

// ---- built-ins --------------------------------------------------

pub struct ClockWidget {
    pub cfg: WidgetConfig,
}
impl Widget for ClockWidget {
    fn name(&self) -> &str { &self.cfg.name }
    fn width(&self, _ctx: &PanelCtx) -> i32 { self.cfg.width.unwrap_or(160) }
    fn interval_ms(&self) -> Option<u32> { Some(self.cfg.interval.unwrap_or(1000)) }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00FFFFFF));
            let fmt = self.cfg.format.as_deref().unwrap_or("%H:%M:%S");
            let st = windows::Win32::System::SystemInformation::GetLocalTime();
            // minimal strftime: only handle %H %M %S for MVP; real version uses chrono or rhai
            let mut txt = fmt.to_string();
            txt = txt.replace("%H", &format!("{:02}", st.wHour));
            txt = txt.replace("%M", &format!("{:02}", st.wMinute));
            txt = txt.replace("%S", &format!("{:02}", st.wSecond));
            txt = txt.replace("%Y", &format!("{}", st.wYear));
            txt = txt.replace("%m", &format!("{:02}", st.wMonth));
            txt = txt.replace("%d", &format!("{:02}", st.wDay));
            let wide: Vec<u16> = txt.encode_utf16().collect();
            let x = rect.left + 8;
            let y = rect.top + (rect.bottom - rect.top - 16) / 2;
            let _ = TextOutW(hdc, x, y, &wide);
        }
    }
    fn on_click(&self, _x: i32, _y: i32, _ctx: &PanelCtx) -> Option<String> { self.cfg.action.clone() }
}

pub struct SpacerWidget { pub cfg: WidgetConfig }
impl Widget for SpacerWidget {
    fn name(&self) -> &str { &self.cfg.name }
    fn width(&self, _ctx: &PanelCtx) -> i32 { 0 } // flex
    fn draw(&self, _hdc: HDC, _rect: RECT, _ctx: &PanelCtx) {}
}

pub struct WindowTitleWidget { pub cfg: WidgetConfig }
impl Widget for WindowTitleWidget {
    fn name(&self) -> &str { &self.cfg.name }
    fn width(&self, _ctx: &PanelCtx) -> i32 { 0 } // flex
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00FFFFFF));
            let hwnd = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
            let mut title = crate::util::get_window_title(hwnd);
            let max = self.cfg.extra.get("max_len").and_then(|v| v.as_integer()).unwrap_or(64) as usize;
            if title.len() > max { title.truncate(max); title.push_str("…"); }
            if title.is_empty() { title = "AltDWM".into(); }
            let wide: Vec<u16> = title.encode_utf16().collect();
            let _ = TextOutW(hdc, rect.left + 6, rect.top + 10, &wide);
        }
    }
}

pub struct TrayWidget { pub cfg: WidgetConfig }
impl Widget for TrayWidget {
    fn name(&self) -> &str { &self.cfg.name }
    fn width(&self, _ctx: &PanelCtx) -> i32 { self.cfg.width.unwrap_or(200) }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00808080));
            let txt = "tray: (Shell_NotifyIcon sink TODO)";
            let wide: Vec<u16> = txt.encode_utf16().collect();
            let _ = TextOutW(hdc, rect.left + 4, rect.top + 12, &wide);
        }
    }
}

pub struct WorkspacesWidget { pub cfg: WidgetConfig }
impl Widget for WorkspacesWidget {
    fn name(&self) -> &str { &self.cfg.name }
    fn width(&self, _ctx: &PanelCtx) -> i32 { self.cfg.width.unwrap_or(140) }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00FFFFFF));
            let txt = "WS 1  2  3";
            let wide: Vec<u16> = txt.encode_utf16().collect();
            let _ = TextOutW(hdc, rect.left + 6, rect.top + 12, &wide);
        }
    }
    fn on_click(&self, x: i32, _y: i32, ctx: &PanelCtx) -> Option<String> {
        let idx = (x - 6) / 28 + 1;
        Some(format!("rhai: focus_workspace({})", idx))
    }
}

pub struct LauncherWidget { pub cfg: WidgetConfig }
impl Widget for LauncherWidget {
    fn name(&self) -> &str { &self.cfg.name }
    fn width(&self, _ctx: &PanelCtx) -> i32 { self.cfg.width.unwrap_or(40) }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00FFFFFF));
            let label = self.cfg.label.as_deref().unwrap_or("≡");
            let wide: Vec<u16> = label.encode_utf16().collect();
            let _ = TextOutW(hdc, rect.left + 10, rect.top + 12, &wide);
        }
    }
    fn on_click(&self, _x: i32, _y: i32, _ctx: &PanelCtx) -> Option<String> {
        Some(self.cfg.action.clone().or_else(|| self.cfg.command.clone()).unwrap_or_else(|| "launch('explorer.exe')".into()))
    }
}

/// Custom Rhai-drawn widget — script returns text to draw
pub struct CustomWidget { pub cfg: WidgetConfig }
impl Widget for CustomWidget {
    fn name(&self) -> &str { &self.cfg.name }
    fn width(&self, _ctx: &PanelCtx) -> i32 { self.cfg.width.unwrap_or(120) }
    fn interval_ms(&self) -> Option<u32> { self.cfg.interval }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        let txt = if let Some(script) = &self.cfg.script {
            if script.starts_with("rhai:") {
                let code = script.trim_start_matches("rhai:").trim();
                match crate::scripting::eval_text(code) {
                    Ok(s) => s,
                    Err(e) => format!("err:{}", e),
                }
            } else {
                // path to .rhai file — try load and eval
                match std::fs::read_to_string(script) {
                    Ok(code) => match crate::scripting::eval_text(&code) {
                        Ok(s) => s,
                        Err(e) => format!("rhai:{}", e),
                    },
                    Err(_) => script.clone(),
                }
            }
        } else {
            self.cfg.label.clone().unwrap_or_else(|| "custom".into())
        };
        unsafe {
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00FFFFFF));
            let wide: Vec<u16> = txt.encode_utf16().collect();
            let _ = TextOutW(hdc, rect.left + 6, rect.top + 12, &wide);
        }
    }
    fn on_click(&self, _x: i32, _y: i32, _ctx: &PanelCtx) -> Option<String> { self.cfg.action.clone() }
}

// ---- factory ----------------------------------------------------

pub fn create_widget(cfg: &WidgetConfig) -> Box<dyn Widget> {
    match cfg.widget_type.as_str() {
        "clock" => Box::new(ClockWidget { cfg: cfg.clone() }),
        "spacer" => Box::new(SpacerWidget { cfg: cfg.clone() }),
        "window_title" | "title" => Box::new(WindowTitleWidget { cfg: cfg.clone() }),
        "tray" | "systray" => Box::new(TrayWidget { cfg: cfg.clone() }),
        "workspaces" | "workspaces_pills" => Box::new(WorkspacesWidget { cfg: cfg.clone() }),
        "launcher" | "start" => Box::new(LauncherWidget { cfg: cfg.clone() }),
        "custom" => Box::new(CustomWidget { cfg: cfg.clone() }),
        other => {
            eprintln!("[widgets] unknown type '{}' for '{}' -> custom fallback", other, cfg.name);
            Box::new(CustomWidget { cfg: cfg.clone() })
        }
    }
}

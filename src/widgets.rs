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
            let theme = crate::CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()).theme.clone();
            let font = crate::theme::get_cached_font(&theme);
            let old_font = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, theme.text_color());
            let fmt = self.cfg.format.as_deref().unwrap_or("%H:%M:%S");
            let st = windows::Win32::System::SystemInformation::GetLocalTime();
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
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old_font);
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
            let theme = crate::CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()).theme.clone();
            let font = crate::theme::get_cached_font(&theme);
            let old_font = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, theme.text_color());
            let hwnd = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
            let mut title = crate::util::get_window_title(hwnd);
            let max = self.cfg.extra.get("max_len").and_then(|v| v.as_integer()).unwrap_or(64) as usize;
            if title.len() > max { title.truncate(max); title.push_str("…"); }
            if title.is_empty() { title = "AltDWM".into(); }
            let wide: Vec<u16> = title.encode_utf16().collect();
            let _ = TextOutW(hdc, rect.left + 6, rect.top + 10, &wide);
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old_font);
                    }
    }
}

pub struct TrayWidget { pub cfg: WidgetConfig }
impl Widget for TrayWidget {
    fn name(&self) -> &str { &self.cfg.name }
    fn width(&self, _ctx: &PanelCtx) -> i32 { self.cfg.width.unwrap_or(220) }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            let theme = crate::CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()).theme.clone();
            let icon_sz = 16;
            let mut x = rect.left + 8;
            let y = rect.top + (rect.bottom - rect.top - icon_sz) / 2;
            let colors = [theme.accent_color().0, 0x2AA198 as u32, 0x859900 as u32, theme.border_color().0];
            for col in colors {
                let r = RECT { left: x, top: y, right: x+icon_sz, bottom: y+icon_sz };
                // rounded rect simulation: FillRect with theme
                let br = windows::Win32::Graphics::Gdi::CreateSolidBrush(COLORREF(col));
                // use RoundRect for nicer icons if rounding >0
                if theme.rounding > 0 {
                    let hbr = br;
                    let hrgn = windows::Win32::Graphics::Gdi::CreateRoundRectRgn(r.left, r.top, r.right, r.bottom, theme.rounding, theme.rounding);
                    let _ = windows::Win32::Graphics::Gdi::FillRgn(hdc, hrgn, hbr);
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(hrgn.into());
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(hbr.into());
                } else {
                    windows::Win32::Graphics::Gdi::FillRect(hdc, &r, br);
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(br.into());
                }
            }
            // separator with border color
            let sep = RECT { left: x, top: rect.top+8, right: x+1, bottom: rect.bottom-8 };
            let br = windows::Win32::Graphics::Gdi::CreateSolidBrush(theme.border_color());
            windows::Win32::Graphics::Gdi::FillRect(hdc, &sep, br);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(br.into());
            x += 8;
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, theme.text_dim_color());
            let font = crate::theme::get_cached_font(&theme);
            let old = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            let txt = "tray";
            let wide: Vec<u16> = txt.encode_utf16().collect();
            let _ = TextOutW(hdc, x, rect.top + 12, &wide);
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old);
                    }
    }
}

pub struct WorkspacesWidget { pub cfg: WidgetConfig }
impl Widget for WorkspacesWidget {
    fn name(&self) -> &str { &self.cfg.name }
    fn width(&self, _ctx: &PanelCtx) -> i32 { self.cfg.width.unwrap_or(180) }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            let theme = crate::CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()).theme.clone();
            let font = crate::theme::get_cached_font(&theme);
            let old = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            // real counts: tilable windows + layout
            let tb = crate::taskbar::get_taskbar_hwnd();
            let mut wins = crate::manager::collect_windows(tb);
            wins.retain(|w| !crate::rules::is_floating(*w) && !crate::focus::is_runtime_floating(*w));
            wins.retain(|w| crate::virtual_desktop::is_on_current_desktop(*w));
            let count = wins.len();
            let layout = crate::CURRENT_LAYOUT.lock().unwrap_or_else(|e| e.into_inner()).name();
            let tiling = if crate::TILING_ENABLED.load(std::sync::atomic::Ordering::SeqCst) { "" } else { " [PAUSED]" };
            // include panel monitor context
            let txt = format!("WS {} | {}{} ", count, layout, tiling);
            // pills: highlight first pill as active
            let pill_w = 56;
            let y = rect.top + 6;
            let mut x = rect.left + 4;
            // active pill
            let r_active = RECT { left: x, top: y, right: x+pill_w, bottom: y+20 };
            let hrgn = windows::Win32::Graphics::Gdi::CreateRoundRectRgn(r_active.left, r_active.top, r_active.right, r_active.bottom, theme.rounding, theme.rounding);
            let br = windows::Win32::Graphics::Gdi::CreateSolidBrush(theme.accent_active_color());
            let _ = windows::Win32::Graphics::Gdi::FillRgn(hdc, hrgn, br);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(hrgn.into());
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(br.into());
            SetTextColor(hdc, theme.text_color());
            let wide: Vec<u16> = txt.encode_utf16().collect();
            let _ = TextOutW(hdc, x+6, y+2, &wide);
            x += pill_w + 6;
            // extra info: total manageable (including floating)
            let total = crate::manager::collect_windows(tb).len();
            if total > count {
                SetTextColor(hdc, theme.text_dim_color());
                let extra = format!("+{} float", total - count);
                let wide2: Vec<u16> = extra.encode_utf16().collect();
                let _ = TextOutW(hdc, x, y+4, &wide2);
            }
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old);
                    }
    }
    fn on_click(&self, x: i32, _y: i32, _ctx: &PanelCtx) -> Option<String> {
        let idx = (x - 6) / 28 + 1;
        Some(format!("rhai: focus_workspace({})", idx))
    }
}

pub struct WindowListWidget { pub cfg: WidgetConfig }
impl Widget for WindowListWidget {
    fn name(&self) -> &str { &self.cfg.name }
    fn width(&self, _ctx: &PanelCtx) -> i32 { 0 } // flex
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            let theme = crate::CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()).theme.clone();
            let font = crate::theme::get_cached_font(&theme);
            let old_font = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            let wins = {
                let tb = crate::taskbar::get_taskbar_hwnd();
                let mut v = crate::manager::collect_windows(tb);
                v.retain(|w| !crate::rules::is_floating(*w));
                v.retain(|w| crate::virtual_desktop::is_on_current_desktop(*w));
                v
            };
            let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
            let mut x = rect.left + 4;
            let y = rect.top + 6;
            let max_w = rect.right - rect.left - 8;
            for hwnd in wins {
                let mut title = crate::util::get_window_title(hwnd);
                if title.is_empty() { title = crate::util::get_class_name(hwnd); }
                if title.len() > 16 { title.truncate(16); }
                let is_active = hwnd.0 == fg.0;
                let bg = if is_active { theme.accent_active_color() } else { theme.color("#303030") };
                let fg_col = if is_active { theme.text_color() } else { theme.text_dim_color() };
                let txt = format!(" {} ", title);
                let wide: Vec<u16> = txt.encode_utf16().collect();
                let w = (txt.len() as i32 * 7 + 16).min(140);
                if x + w > rect.right - 4 { break; }
                let r = RECT { left: x, top: y, right: x+w, bottom: y+20 };
                // rounded pill if theme.rounding >0
                if theme.rounding > 0 {
                    let hrgn = windows::Win32::Graphics::Gdi::CreateRoundRectRgn(r.left, r.top, r.right, r.bottom, theme.rounding, theme.rounding);
                    let br = windows::Win32::Graphics::Gdi::CreateSolidBrush(bg);
                    let _ = windows::Win32::Graphics::Gdi::FillRgn(hdc, hrgn, br);
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(hrgn.into());
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(br.into());
                } else {
                    let br = windows::Win32::Graphics::Gdi::CreateSolidBrush(bg);
                    windows::Win32::Graphics::Gdi::FillRect(hdc, &r, br);
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(br.into());
                }
                SetTextColor(hdc, fg_col);
                let _ = TextOutW(hdc, x+6, y+2, &wide);
                x += w + 4;
                if x > rect.right - 20 { break; }
                if x > max_w + rect.left { break; }
            }
            if x == rect.left + 4 {
                SetTextColor(hdc, theme.text_dim_color());
                let txt = "— no windows —";
                let wide: Vec<u16> = txt.encode_utf16().collect();
                let _ = TextOutW(hdc, x, y+2, &wide);
            }
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old_font);
                    }
    }
    fn on_click(&self, x: i32, _y: i32, _ctx: &PanelCtx) -> Option<String> {
        let wins = {
            let tb = crate::taskbar::get_taskbar_hwnd();
            let mut v = crate::manager::collect_windows(tb);
            v.retain(|w| !crate::rules::is_floating(*w));
            v.retain(|w| crate::virtual_desktop::is_on_current_desktop(*w));
            v
        };
        if wins.is_empty() { return None; }
        // avg pill width ~80px, map click to index
        let idx = ((x - 4) / 80).max(0) as usize % wins.len();
        let hwnd = wins[idx];
        crate::focus::focus_hwnd(hwnd);
        None
    }
}

pub struct LauncherWidget { pub cfg: WidgetConfig }
impl Widget for LauncherWidget {
    fn name(&self) -> &str { &self.cfg.name }
    fn width(&self, _ctx: &PanelCtx) -> i32 { self.cfg.width.unwrap_or(40) }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            let theme = crate::CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()).theme.clone();
            let font = crate::theme::get_cached_font(&theme);
            let old = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, theme.text_color());
            let label = self.cfg.label.as_deref().unwrap_or("Menu");
            let wide: Vec<u16> = label.encode_utf16().collect();
            let _ = TextOutW(hdc, rect.left + 10, rect.top + 12, &wide);
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old);
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
            let theme = crate::CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()).theme.clone();
            let font = crate::theme::get_cached_font(&theme);
            let old = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, theme.text_color());
            let wide: Vec<u16> = txt.encode_utf16().collect();
            let _ = TextOutW(hdc, rect.left + 6, rect.top + 12, &wide);
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old);
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
        "window_list" | "tasklist" => Box::new(WindowListWidget { cfg: cfg.clone() }),
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

//! Widget trait and built-ins — extensible via Rhai `custom` or Rust `cdylib` plugins.
//! See docs/EXTENSIBILITY.md
use std::sync::Mutex;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{SetBkMode, SetTextColor, TextOutW, HDC, TRANSPARENT};

use crate::config::WidgetConfig;

fn truncate_chars(value: &mut String, max_chars: usize) {
    if let Some((byte_index, _)) = value.char_indices().nth(max_chars) {
        value.truncate(byte_index);
    }
}

fn window_pill_width(title: &str) -> i32 {
    (title.chars().count() as i32 * 7 + 52).clamp(86, 172)
}

fn window_icon(hwnd: HWND) -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassLongPtrW, GCLP_HICONSM, HICON, ICON_SMALL2, WM_GETICON,
    };
    unsafe {
        let result = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            hwnd,
            WM_GETICON,
            Some(WPARAM(ICON_SMALL2 as usize)),
            Some(LPARAM(0)),
        );
        let raw = if result.0 != 0 {
            result.0
        } else {
            GetClassLongPtrW(hwnd, GCLP_HICONSM) as isize
        };
        (raw != 0).then_some(HICON(raw as *mut std::ffi::c_void))
    }
}

/// Context passed to every widget during draw / click
#[derive(Debug, Clone)]
#[allow(dead_code)] // Public extension context; built-ins do not need every field.
pub struct PanelCtx {
    pub panel_name: String,
    pub monitor: String,
    pub width: i32,
    pub height: i32,
    pub hwnd: HWND,
    /// One per-paint snapshot shared by widgets so a panel does not repeatedly
    /// enumerate HWNDs and query virtual-desktop COM state.
    pub windows: Vec<HWND>,
}

/// Core extensibility point — implement this to add a widget.
/// Register via `inventory` or Rhai `custom` script.
pub trait Widget: Send + Sync {
    fn name(&self) -> &str;
    /// 0 = flex (takes remaining space), >0 = fixed pixels
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        0
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx);
    /// return Some(action) to handle click
    fn on_click(&self, _x: i32, _y: i32, _ctx: &PanelCtx) -> Option<String> {
        None
    }
    fn interval_ms(&self) -> Option<u32> {
        None
    }
}

// ---- built-ins --------------------------------------------------

pub struct ClockWidget {
    pub cfg: WidgetConfig,
}
impl Widget for ClockWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(136)
    }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            let theme = crate::CURRENT_CONFIG
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .theme
                .clone();
            let font = crate::theme::get_cached_font_variant(&theme, 14, 600);
            let small_font = crate::theme::get_cached_font_variant(&theme, 11, 400);
            let old_font = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, theme.text_color());
            let fmt = self.cfg.format.as_deref().unwrap_or("%H:%M");
            let st = windows::Win32::System::SystemInformation::GetLocalTime();
            let mut txt = fmt.to_string();
            txt = txt.replace("%H", &format!("{:02}", st.wHour));
            txt = txt.replace("%M", &format!("{:02}", st.wMinute));
            txt = txt.replace("%S", &format!("{:02}", st.wSecond));
            txt = txt.replace("%Y", &format!("{}", st.wYear));
            txt = txt.replace("%m", &format!("{:02}", st.wMonth));
            txt = txt.replace("%d", &format!("{:02}", st.wDay));
            let wide: Vec<u16> = txt.encode_utf16().collect();
            let x = rect.left + 12;
            let two_line = rect.bottom - rect.top >= 44;
            let y = if two_line {
                rect.top + 7
            } else {
                rect.top + 13
            };
            let _ = TextOutW(hdc, x, y, &wide);
            if two_line {
                let weekdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
                let months = [
                    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
                    "Dec",
                ];
                let date = format!(
                    "{} {} {}",
                    weekdays[st.wDayOfWeek.min(6) as usize],
                    st.wDay,
                    months[st.wMonth.saturating_sub(1).min(11) as usize]
                );
                let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, small_font.into());
                SetTextColor(hdc, theme.text_dim_color());
                let date_wide: Vec<u16> = date.encode_utf16().collect();
                let _ = TextOutW(hdc, x, rect.top + 28, &date_wide);
            }
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old_font);
        }
    }
    fn on_click(&self, _x: i32, _y: i32, _ctx: &PanelCtx) -> Option<String> {
        self.cfg.action.clone()
    }
    fn interval_ms(&self) -> Option<u32> {
        Some(self.cfg.interval.unwrap_or(1000))
    }
}

pub struct SpacerWidget {
    pub cfg: WidgetConfig,
}
impl Widget for SpacerWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        0
    } // flex
    fn draw(&self, _hdc: HDC, _rect: RECT, _ctx: &PanelCtx) {}
}

pub struct WindowTitleWidget {
    pub cfg: WidgetConfig,
}
impl Widget for WindowTitleWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        0
    } // flex
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            let theme = crate::CURRENT_CONFIG
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .theme
                .clone();
            let font = crate::theme::get_cached_font_variant(&theme, 12, 400);
            let old_font = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, theme.text_color());
            let hwnd = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
            let mut title = crate::util::get_window_title(hwnd);
            let max = self
                .cfg
                .extra
                .get("max_len")
                .and_then(|v| v.as_integer())
                .unwrap_or(64) as usize;
            if title.chars().count() > max {
                truncate_chars(&mut title, max);
                title.push('…');
            }
            if title.is_empty() {
                title = "AltDWM".into();
            }
            let wide: Vec<u16> = title.encode_utf16().collect();
            let _ = TextOutW(hdc, rect.left + 6, rect.top + 10, &wide);
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old_font);
        }
    }
}

pub struct TrayWidget {
    pub cfg: WidgetConfig,
}
impl Widget for TrayWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(220)
    }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            let theme = crate::CURRENT_CONFIG
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .theme
                .clone();
            let entries = crate::tray::entries();
            let mut x = rect.left + 8;
            let y = rect.top + 6;
            let font = crate::theme::get_cached_font(&theme);
            let old = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            for entry in entries.iter() {
                let label = crate::tray::compact_name(&entry.name);
                let width = (label.chars().count() as i32 * 7 + 18).clamp(34, 110);
                if x + width > rect.right - 6 {
                    break;
                }
                let r = RECT {
                    left: x,
                    top: y,
                    right: x + width,
                    bottom: y + 24,
                };
                let hrgn = windows::Win32::Graphics::Gdi::CreateRoundRectRgn(
                    r.left,
                    r.top,
                    r.right,
                    r.bottom,
                    theme.rounding.max(4),
                    theme.rounding.max(4),
                );
                let br =
                    windows::Win32::Graphics::Gdi::CreateSolidBrush(theme.color(&theme.tray_bg));
                let _ = windows::Win32::Graphics::Gdi::FillRgn(hdc, hrgn, br);
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(hrgn.into());
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(br.into());
                SetTextColor(hdc, theme.text_dim_color());
                let wide: Vec<u16> = label.encode_utf16().collect();
                let _ = TextOutW(hdc, x + 8, y + 4, &wide);
                x += width + 5;
            }
            if entries.is_empty() {
                SetTextColor(hdc, theme.text_dim_color());
                let wide: Vec<u16> = "No tray items".encode_utf16().collect();
                let _ = TextOutW(hdc, x, rect.top + 12, &wide);
            }
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old);
        }
    }

    fn on_click(&self, x: i32, _y: i32, _ctx: &PanelCtx) -> Option<String> {
        let entries = crate::tray::entries();
        let mut left = 8;
        for (index, entry) in entries.iter().enumerate() {
            let label = crate::tray::compact_name(&entry.name);
            let width = (label.chars().count() as i32 * 7 + 18).clamp(34, 110);
            if x >= left && x < left + width {
                crate::tray::invoke(index);
                break;
            }
            left += width + 5;
        }
        None
    }
}

pub struct WorkspacesWidget {
    pub cfg: WidgetConfig,
}
impl Widget for WorkspacesWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(216)
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        unsafe {
            let theme = crate::CURRENT_CONFIG
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .theme
                .clone();
            let font = crate::theme::get_cached_font_variant(&theme, 13, 600);
            let small_font = crate::theme::get_cached_font_variant(&theme, 10, 400);
            let old = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            // real counts: tilable windows + layout
            let mut wins = ctx.windows.clone();
            wins.retain(|w| {
                !crate::rules::is_floating(*w) && !crate::focus::is_runtime_floating(*w)
            });
            let count = wins.len();
            let layout = crate::CURRENT_LAYOUT
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .name();
            let enabled = crate::TILING_ENABLED.load(std::sync::atomic::Ordering::SeqCst);
            let status = if enabled {
                format!("{} managed", count)
            } else {
                "Tiling paused".to_string()
            };
            let y = rect.top + 5;
            let r_active = RECT {
                left: rect.left + 4,
                top: y,
                right: rect.right - 4,
                bottom: rect.bottom - 5,
            };
            let hrgn = windows::Win32::Graphics::Gdi::CreateRoundRectRgn(
                r_active.left,
                r_active.top,
                r_active.right,
                r_active.bottom,
                theme.rounding.max(8),
                theme.rounding.max(8),
            );
            let br = windows::Win32::Graphics::Gdi::CreateSolidBrush(if enabled {
                theme.surface_color()
            } else {
                theme.accent_color()
            });
            let _ = windows::Win32::Graphics::Gdi::FillRgn(hdc, hrgn, br);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(hrgn.into());
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(br.into());
            SetTextColor(hdc, theme.text_color());
            let layout_label = format!("◇  {}", layout);
            let wide: Vec<u16> = layout_label.encode_utf16().collect();
            let _ = TextOutW(hdc, r_active.left + 12, r_active.top + 7, &wide);
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, small_font.into());
            SetTextColor(hdc, theme.text_dim_color());
            let status_wide: Vec<u16> = format!("{}  •  click to cycle", status)
                .encode_utf16()
                .collect();
            let _ = TextOutW(hdc, r_active.left + 12, r_active.top + 25, &status_wide);
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old);
        }
    }
    fn on_click(&self, _x: i32, _y: i32, _ctx: &PanelCtx) -> Option<String> {
        let current = crate::CURRENT_LAYOUT
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .name()
            .to_lowercase();
        let next = match current.as_str() {
            "masterstack" => "Grid",
            "grid" => "Monocle",
            "monocle" => "Floating",
            _ => "MasterStack",
        };
        crate::set_layout_by_name(next);
        crate::request_retile();
        None
    }
}

pub struct WindowListWidget {
    pub cfg: WidgetConfig,
}

fn window_list_label(hwnd: HWND) -> String {
    let mut title = crate::util::get_window_title(hwnd);
    if title.is_empty() {
        title = crate::util::get_class_name(hwnd);
    }
    truncate_chars(&mut title, 16);
    if unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(hwnd).as_bool() } {
        format!("- {title}")
    } else {
        title
    }
}

impl Widget for WindowListWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        0
    } // flex
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        unsafe {
            let theme = crate::CURRENT_CONFIG
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .theme
                .clone();
            let font = crate::theme::get_cached_font(&theme);
            let old_font = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            // This is a task list, not only a tiling list: include tiled,
            // floating, and minimized managed windows alike.
            let wins = ctx.windows.clone();
            let total = wins.len();
            let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
            let mut x = rect.left + 4;
            let y = rect.top + 5;
            let mut shown = 0usize;
            for hwnd in wins {
                let title = window_list_label(hwnd);
                let is_active = hwnd.0 == fg.0;
                let is_minimized =
                    windows::Win32::UI::WindowsAndMessaging::IsIconic(hwnd).as_bool();
                let bg = if is_active {
                    theme.surface_hover_color()
                } else if is_minimized {
                    theme.panel_bg("top")
                } else {
                    theme.surface_color()
                };
                let fg_col = if is_active {
                    theme.text_color()
                } else {
                    theme.text_dim_color()
                };
                let wide: Vec<u16> = title.encode_utf16().collect();
                let w = window_pill_width(&title);
                let needs_overflow = shown + 1 < total;
                let reserve = if needs_overflow { 50 } else { 4 };
                if x + w > rect.right - reserve {
                    break;
                }
                let r = RECT {
                    left: x,
                    top: y,
                    right: x + w,
                    bottom: rect.bottom - 5,
                };
                // rounded pill if theme.rounding >0
                if theme.rounding > 0 {
                    let hrgn = windows::Win32::Graphics::Gdi::CreateRoundRectRgn(
                        r.left,
                        r.top,
                        r.right,
                        r.bottom,
                        theme.rounding.max(8),
                        theme.rounding.max(8),
                    );
                    let br = windows::Win32::Graphics::Gdi::CreateSolidBrush(bg);
                    let _ = windows::Win32::Graphics::Gdi::FillRgn(hdc, hrgn, br);
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(hrgn.into());
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(br.into());
                } else {
                    let br = windows::Win32::Graphics::Gdi::CreateSolidBrush(bg);
                    windows::Win32::Graphics::Gdi::FillRect(hdc, &r, br);
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(br.into());
                }
                if is_active {
                    let indicator = RECT {
                        left: r.left + 14,
                        top: r.bottom - 3,
                        right: r.right - 14,
                        bottom: r.bottom,
                    };
                    let brush = windows::Win32::Graphics::Gdi::CreateSolidBrush(
                        theme.accent_active_color(),
                    );
                    windows::Win32::Graphics::Gdi::FillRect(hdc, &indicator, brush);
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(brush.into());
                }
                if let Some(icon) = window_icon(hwnd) {
                    let _ = windows::Win32::UI::WindowsAndMessaging::DrawIconEx(
                        hdc,
                        x + 10,
                        y + (r.bottom - y - 16) / 2,
                        icon,
                        16,
                        16,
                        0,
                        None,
                        windows::Win32::UI::WindowsAndMessaging::DI_NORMAL,
                    );
                } else {
                    let badge = title
                        .chars()
                        .next()
                        .unwrap_or('•')
                        .to_uppercase()
                        .to_string();
                    SetTextColor(hdc, theme.accent_active_color());
                    let badge_wide: Vec<u16> = badge.encode_utf16().collect();
                    let _ = TextOutW(hdc, x + 12, y + 9, &badge_wide);
                }
                SetTextColor(hdc, fg_col);
                let _ = TextOutW(hdc, x + 34, y + 9, &wide);
                x += w + 4;
                shown += 1;
            }
            if shown == 0 {
                SetTextColor(hdc, theme.text_dim_color());
                let txt = "Your open windows will appear here";
                let wide: Vec<u16> = txt.encode_utf16().collect();
                let _ = TextOutW(hdc, x + 8, y + 9, &wide);
            } else if shown < total {
                let overflow = format!("+{}", total - shown);
                let wide: Vec<u16> = overflow.encode_utf16().collect();
                SetTextColor(hdc, theme.text_dim_color());
                let _ = TextOutW(hdc, x + 8, y + 9, &wide);
            }
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old_font);
        }
    }
    fn on_click(&self, x: i32, _y: i32, ctx: &PanelCtx) -> Option<String> {
        let wins = ctx.windows.clone();
        if wins.is_empty() {
            return None;
        }
        let mut left = 4;
        for hwnd in wins {
            let title = window_list_label(hwnd);
            let width = window_pill_width(&title);
            if left + width > ctx.width - 4 {
                break;
            }
            if x >= left && x < left + width {
                crate::focus::toggle_window_from_list(hwnd);
                break;
            }
            left += width + 4;
        }
        None
    }
}

pub struct LauncherWidget {
    pub cfg: WidgetConfig,
}
impl Widget for LauncherWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(118)
    }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        unsafe {
            let theme = crate::CURRENT_CONFIG
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .theme
                .clone();
            let font = crate::theme::get_cached_font(&theme);
            let old = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            let button = RECT {
                left: rect.left + 4,
                top: rect.top + 5,
                right: rect.right - 4,
                bottom: rect.bottom - 5,
            };
            let region = windows::Win32::Graphics::Gdi::CreateRoundRectRgn(
                button.left,
                button.top,
                button.right,
                button.bottom,
                theme.rounding.max(8),
                theme.rounding.max(8),
            );
            let brush =
                windows::Win32::Graphics::Gdi::CreateSolidBrush(theme.accent_active_color());
            let _ = windows::Win32::Graphics::Gdi::FillRgn(hdc, region, brush);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(region.into());
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(brush.into());
            SetTextColor(hdc, theme.text_color());
            let label = self
                .cfg
                .label
                .as_deref()
                .or(self.cfg.icon.as_deref())
                .unwrap_or("⌘  AltDWM");
            let wide: Vec<u16> = label.encode_utf16().collect();
            let _ = TextOutW(
                hdc,
                button.left + 12,
                button.top + (button.bottom - button.top - 16) / 2,
                &wide,
            );
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old);
        }
    }
    fn on_click(&self, _x: i32, _y: i32, ctx: &PanelCtx) -> Option<String> {
        // A configured action remains an escape hatch for launch-only widgets;
        // the built-in launcher now opens AltDWM's discoverable command surface.
        if let Some(action) = self.cfg.action.clone().or_else(|| self.cfg.command.clone()) {
            return Some(action);
        }
        crate::command_center::toggle(ctx.hwnd);
        None
    }
}

/// Custom Rhai-drawn widget — script returns text to draw.
/// Evaluation is cached by interval so paints never repeatedly execute scripts or read files.
pub struct CustomWidget {
    pub cfg: WidgetConfig,
    state: Mutex<(Option<Instant>, String)>,
}
impl Widget for CustomWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(120)
    }
    fn draw(&self, hdc: HDC, rect: RECT, _ctx: &PanelCtx) {
        let interval =
            std::time::Duration::from_millis(self.cfg.interval.unwrap_or(1000).max(1) as u64);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let refresh = state.0.is_none_or(|last| last.elapsed() >= interval);
        if refresh {
            state.1 = if let Some(script) = &self.cfg.script {
                let code = if let Some(inline) = script.strip_prefix("rhai:") {
                    Ok(inline.trim().to_string())
                } else {
                    read_widget_script(script)
                };
                code.and_then(|code| crate::scripting::eval_text(&code))
                    .unwrap_or_else(|e| format!("rhai:{}", e))
            } else {
                self.cfg.label.clone().unwrap_or_else(|| "custom".into())
            };
            state.0 = Some(Instant::now());
        }
        let txt = state.1.clone();
        drop(state);
        unsafe {
            let theme = crate::CURRENT_CONFIG
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .theme
                .clone();
            let font = crate::theme::get_cached_font(&theme);
            let old = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, theme.text_color());
            let wide: Vec<u16> = txt.encode_utf16().collect();
            let _ = TextOutW(hdc, rect.left + 6, rect.top + 12, &wide);
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old);
        }
    }
    fn on_click(&self, _x: i32, _y: i32, _ctx: &PanelCtx) -> Option<String> {
        self.cfg.action.clone()
    }
    fn interval_ms(&self) -> Option<u32> {
        self.cfg.interval
    }
}

fn read_widget_script(script: &str) -> Result<String, String> {
    let mut candidates = vec![std::path::PathBuf::from(script)];
    if let Some(dir) = crate::CONFIG_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|path| path.parent())
    {
        candidates.push(dir.join(script));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(script));
        }
    }
    candidates
        .into_iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .ok_or_else(|| format!("script not found: {}", script))
}

// ---- factory ----------------------------------------------------

pub fn create_widget(cfg: &WidgetConfig) -> Box<dyn Widget> {
    match cfg.widget_type.as_str() {
        "clock" => Box::new(ClockWidget { cfg: cfg.clone() }),
        "spacer" => Box::new(SpacerWidget { cfg: cfg.clone() }),
        "window_title" | "title" => Box::new(WindowTitleWidget { cfg: cfg.clone() }),
        "window_list" | "tasklist" => Box::new(WindowListWidget { cfg: cfg.clone() }),
        "tray" | "systray" => Box::new(TrayWidget { cfg: cfg.clone() }),
        "workspaces" | "workspaces_pills" | "layout" | "layout_status" => {
            Box::new(WorkspacesWidget { cfg: cfg.clone() })
        }
        "launcher" | "start" => Box::new(LauncherWidget { cfg: cfg.clone() }),
        "custom" => Box::new(CustomWidget {
            cfg: cfg.clone(),
            state: Mutex::new((None, String::new())),
        }),
        other => {
            eprintln!(
                "[widgets] unknown type '{}' for '{}' -> custom fallback",
                other, cfg.name
            );
            Box::new(CustomWidget {
                cfg: cfg.clone(),
                state: Mutex::new((None, String::new())),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{truncate_chars, window_pill_width};

    #[test]
    fn truncates_unicode_at_character_boundaries() {
        let mut title = "Browser 🌍 тест".to_string();
        truncate_chars(&mut title, 10);
        assert_eq!(title, "Browser 🌍 ");
    }

    #[test]
    fn pill_width_is_character_based_and_capped() {
        assert_eq!(window_pill_width("абв"), 86);
        assert_eq!(window_pill_width(&"x".repeat(100)), 172);
    }
}

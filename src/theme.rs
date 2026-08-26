//! Theme — centralized colors/fonts/rounding for panels + widgets + window chrome
//! Inspired by Hyprland / Polybar theming, but declarative TOML. All values have dark defaults.

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::COLORREF;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    #[serde(default = "def_panel_bg")]
    pub panel_bg: String, // hex "#202020"
    #[serde(default = "def_panel_bg_top")]
    pub panel_bg_top: String,
    #[serde(default = "def_text")]
    pub text: String,
    #[serde(default = "def_text_dim")]
    pub text_dim: String,
    #[serde(default = "def_accent")]
    pub accent: String,
    #[serde(default = "def_accent_active")]
    pub accent_active: String,
    #[serde(default = "def_border")]
    pub border: String,
    #[serde(default = "def_tray_bg")]
    pub tray_bg: String,
    #[serde(default = "def_font_name")]
    pub font_name: String,
    #[serde(default = "def_font_size")]
    pub font_size: i32,
    #[serde(default = "def_rounding")]
    pub rounding: i32, // pill radius for window_list, 0 = square
    #[serde(default = "def_gap")]
    pub gap: i32, // kept in sync with general.gap if not set
}

fn def_panel_bg() -> String {
    "#1e1e1e".into()
}
fn def_panel_bg_top() -> String {
    "#252526".into()
}
fn def_text() -> String {
    "#ffffff".into()
}
fn def_text_dim() -> String {
    "#aaaaaa".into()
}
fn def_accent() -> String {
    "#3a6ea5".into()
}
fn def_accent_active() -> String {
    "#007acc".into()
}
fn def_border() -> String {
    "#404040".into()
}
fn def_tray_bg() -> String {
    "#2d2d30".into()
}
fn def_font_name() -> String {
    "Segoe UI".into()
}
fn def_font_size() -> i32 {
    13
}
fn def_rounding() -> i32 {
    6
}
fn def_gap() -> i32 {
    8
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            panel_bg: def_panel_bg(),
            panel_bg_top: def_panel_bg_top(),
            text: def_text(),
            text_dim: def_text_dim(),
            accent: def_accent(),
            accent_active: def_accent_active(),
            border: def_border(),
            tray_bg: def_tray_bg(),
            font_name: def_font_name(),
            font_size: def_font_size(),
            rounding: def_rounding(),
            gap: def_gap(),
        }
    }
}

impl Theme {
    pub fn color(&self, hex: &str) -> COLORREF {
        parse_hex(hex)
    }
    pub fn panel_bg(&self, position: &str) -> COLORREF {
        if position == "top" {
            self.color(&self.panel_bg_top)
        } else {
            self.color(&self.panel_bg)
        }
    }
    pub fn text_color(&self) -> COLORREF {
        self.color(&self.text)
    }
    pub fn text_dim_color(&self) -> COLORREF {
        self.color(&self.text_dim)
    }
    pub fn accent_color(&self) -> COLORREF {
        self.color(&self.accent)
    }
    pub fn accent_active_color(&self) -> COLORREF {
        self.color(&self.accent_active)
    }
    pub fn border_color(&self) -> COLORREF {
        self.color(&self.border)
    }
}

fn parse_hex(s: &str) -> COLORREF {
    let s = s.trim().trim_start_matches('#');
    // support #RRGGBB and #AARRGGBB (ignore alpha)
    let hex = if s.len() == 8 { &s[2..] } else { s };
    if let Ok(v) = u32::from_str_radix(hex, 16) {
        // GDI COLORREF is 0x00BBGGRR, but CreateSolidBrush expects same layout as RGB macro
        // For "#RRGGBB" we need to swap R and B: 0x00BBGGRR
        let r = (v >> 16) & 0xFF;
        let g = (v >> 8) & 0xFF;
        let b = v & 0xFF;
        COLORREF((r) | (g << 8) | (b << 16))
    } else {
        COLORREF(0x00202020)
    }
}

/// Helper to create a GDI font handle for Segoe UI with theme size (uncached, caller must DeleteObject)
pub fn create_font(theme: &Theme) -> HFONT {
    use windows::Win32::Graphics::Gdi::{
        CreateFontW, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_QUALITY, FF_DONTCARE, FW_NORMAL,
        OUT_DEFAULT_PRECIS, VARIABLE_PITCH,
    };
    let name_wide: Vec<u16> = theme
        .font_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        CreateFontW(
            -theme.font_size,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY,
            VARIABLE_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
            windows::core::PCWSTR(name_wide.as_ptr()),
        )
    }
}

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use windows::Win32::Graphics::Gdi::HFONT;

static FONT_CACHE: LazyLock<Mutex<HashMap<String, isize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Cached version — reuses HFONT per (font_name, size), leaked until exit to avoid GDI churn
pub fn get_cached_font(theme: &Theme) -> HFONT {
    let key = format!("{}:{}", theme.font_name, theme.font_size);
    let mut cache = FONT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&v) = cache.get(&key) {
        return HFONT(v as *mut std::ffi::c_void);
    }
    let h = create_font(theme);
    cache.insert(key, h.0 as isize);
    h
}

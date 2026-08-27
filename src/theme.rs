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
    #[serde(default = "def_border_active")]
    pub border_active: String,
    #[serde(default = "def_border_inactive")]
    pub border_inactive: String,
    #[serde(default = "def_tray_bg")]
    pub tray_bg: String,
    #[serde(default = "def_surface")]
    pub surface: String,
    #[serde(default = "def_surface_hover")]
    pub surface_hover: String,
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
fn def_border_active() -> String {
    "#8b5cf6".into()
}
fn def_border_inactive() -> String {
    "#343842".into()
}
fn def_tray_bg() -> String {
    "#2d2d30".into()
}
fn def_surface() -> String {
    "#292b32".into()
}
fn def_surface_hover() -> String {
    "#353842".into()
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
            border_active: def_border_active(),
            border_inactive: def_border_inactive(),
            tray_bg: def_tray_bg(),
            surface: def_surface(),
            surface_hover: def_surface_hover(),
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
    pub fn active_window_border_color(&self) -> COLORREF {
        self.color(&self.border_active)
    }
    pub fn inactive_window_border_color(&self) -> COLORREF {
        self.color(&self.border_inactive)
    }
    pub fn surface_color(&self) -> COLORREF {
        self.color(&self.surface)
    }
    pub fn surface_hover_color(&self) -> COLORREF {
        self.color(&self.surface_hover)
    }
}

/// Fallback used for any value that is not a colour we can read.
const FALLBACK_COLOR: COLORREF = COLORREF(0x0020_2020);

/// Parse `#RRGGBB` or `#AARRGGBB` (alpha ignored) into a GDI `COLORREF`.
///
/// The value comes straight from the user's TOML, so it is validated before it
/// is indexed. The previous `&s[2..]` sliced by bytes on any eight-byte string
/// and panicked on a multi-byte character — and with `panic = "abort"` in
/// release, a typo in `[theme]` took the whole shell down with the native
/// taskbar still hidden.
pub fn parse_hex(s: &str) -> COLORREF {
    let trimmed = s.trim().trim_start_matches('#');
    if !trimmed.is_ascii() || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        eprintln!("[theme] '{s}' is not a hex colour — using the default");
        return FALLBACK_COLOR;
    }
    let hex = match trimmed.len() {
        6 => trimmed,
        8 => &trimmed[2..],
        _ => {
            eprintln!("[theme] '{s}' must be 6 or 8 hex digits — using the default");
            return FALLBACK_COLOR;
        }
    };
    let Ok(value) = u32::from_str_radix(hex, 16) else {
        return FALLBACK_COLOR;
    };
    // GDI COLORREF is 0x00BBGGRR, so R and B swap relative to #RRGGBB.
    let r = (value >> 16) & 0xFF;
    let g = (value >> 8) & 0xFF;
    let b = value & 0xFF;
    COLORREF(r | (g << 8) | (b << 16))
}

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use windows::Win32::Graphics::Gdi::HFONT;

static FONT_CACHE: LazyLock<Mutex<HashMap<String, isize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Cached font variant used by richer shell surfaces without creating GDI
/// objects during every paint.
pub fn get_cached_font_variant(theme: &Theme, size: i32, weight: i32) -> HFONT {
    use windows::Win32::Graphics::Gdi::{
        CreateFontW, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_QUALITY, FF_DONTCARE,
        OUT_DEFAULT_PRECIS, VARIABLE_PITCH,
    };
    let key = format!("{}:{size}:{weight}", theme.font_name);
    let mut cache = FONT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&v) = cache.get(&key) {
        return HFONT(v as *mut std::ffi::c_void);
    }
    let name_wide: Vec<u16> = theme
        .font_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let h = unsafe {
        CreateFontW(
            -size,
            0,
            0,
            0,
            weight,
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
    };
    cache.insert(key, h.0 as isize);
    h
}

#[cfg(test)]
mod tests {
    use super::{parse_hex, FALLBACK_COLOR};

    #[test]
    fn parses_six_and_eight_digit_colours() {
        // #RRGGBB -> 0x00BBGGRR
        assert_eq!(parse_hex("#8b5cf6").0, 0x00f6_5c8b);
        assert_eq!(parse_hex("8b5cf6").0, 0x00f6_5c8b);
        // Leading alpha pair is ignored.
        assert_eq!(parse_hex("#ff8b5cf6").0, 0x00f6_5c8b);
    }

    #[test]
    fn rejects_malformed_values_without_panicking() {
        // Eight bytes of multi-byte UTF-8 used to panic on a char boundary.
        assert_eq!(parse_hex("#ääää").0, FALLBACK_COLOR.0);
        assert_eq!(parse_hex("#12345").0, FALLBACK_COLOR.0);
        assert_eq!(parse_hex("not a colour").0, FALLBACK_COLOR.0);
        assert_eq!(parse_hex("").0, FALLBACK_COLOR.0);
        assert_eq!(parse_hex("#zzzzzz").0, FALLBACK_COLOR.0);
    }
}

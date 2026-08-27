//! Shared drawing primitives for AltDWM's shell surfaces.
//!
//! Every panel, widget, and command-center row used to position text with
//! hand-tuned pixel constants: `rect.top + 9` here, `rect.top + 13` there, pill
//! widths estimated as `characters * 7`. Two things went wrong with that. Text
//! sat at a different height in every widget of the same bar, and none of it
//! survived a display scaled above 100% — the manifest claims PerMonitorV2 but
//! nothing in the code had ever asked Windows for a DPI.
//!
//! This module is the single answer to both: one scale factor per surface, and
//! text that is measured rather than guessed.

use windows::Win32::Foundation::{COLORREF, HWND, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawTextW, FillRgn, GetTextExtentPoint32W,
    SelectObject, SetBkMode, SetTextColor, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, HDC, HFONT, TRANSPARENT,
};

/// Reference DPI. All constants in this codebase are expressed at this scale.
const BASE_DPI: f32 = 96.0;

/// Design tokens, in device-independent pixels at 96 DPI. Named so the same
/// rhythm can be applied everywhere instead of being re-invented per widget.
pub mod token {
    /// Horizontal breathing room inside a widget or pill.
    pub const PAD: i32 = 10;
    /// Gap between sibling items in a widget (pills, tray icons).
    pub const ITEM_GAP: i32 = 4;
    /// Inset from a panel's top and bottom edge to its content.
    pub const INSET: i32 = 5;
    /// Height of the active-window indicator bar.
    pub const INDICATOR: i32 = 2;
    /// Side of the square status/accent marks drawn instead of font glyphs.
    pub const MARK: i32 = 8;
}

/// DPI scale for the display a window is on. PerMonitorV2 means this can differ
/// between panels in the same session, so it is resolved per window rather than
/// once per process.
pub fn scale_for_window(hwnd: HWND) -> f32 {
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
    if dpi == 0 {
        return 1.0;
    }
    dpi as f32 / BASE_DPI
}

/// DPI scale for a monitor, for surfaces that are positioned before their window
/// exists.
pub fn scale_for_monitor(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> f32 {
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    let mut dpi_x = 0u32;
    let mut dpi_y = 0u32;
    let ok = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_ok();
    if !ok || dpi_x == 0 {
        return 1.0;
    }
    dpi_x as f32 / BASE_DPI
}

/// Convert a device-independent length to physical pixels.
pub fn px(value: i32, scale: f32) -> i32 {
    (value as f32 * scale).round() as i32
}

pub fn rect_width(rect: &RECT) -> i32 {
    rect.right - rect.left
}

pub fn rect_height(rect: &RECT) -> i32 {
    rect.bottom - rect.top
}

/// Shrink a rectangle by independent horizontal and vertical insets, never past
/// the point of inversion.
pub fn inset_rect(rect: RECT, horizontal: i32, vertical: i32) -> RECT {
    let horizontal = horizontal.min(rect_width(rect_ref(&rect)) / 2);
    let vertical = vertical.min(rect_height(rect_ref(&rect)) / 2);
    RECT {
        left: rect.left + horizontal,
        top: rect.top + vertical,
        right: rect.right - horizontal,
        bottom: rect.bottom - vertical,
    }
}

fn rect_ref(rect: &RECT) -> &RECT {
    rect
}

pub fn point_in_rect(x: i32, y: i32, rect: &RECT) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

pub fn fill_rect(hdc: HDC, rect: &RECT, color: COLORREF) {
    if rect_width(rect) <= 0 || rect_height(rect) <= 0 {
        return;
    }
    unsafe {
        let brush = CreateSolidBrush(color);
        windows::Win32::Graphics::Gdi::FillRect(hdc, rect, brush);
        let _ = DeleteObject(brush.into());
    }
}

/// Filled rounded rectangle. The radius is clamped to half the shorter side, so
/// a large `theme.rounding` on a short pill degrades to a capsule instead of
/// producing a distorted shape.
pub fn fill_round_rect(hdc: HDC, rect: &RECT, radius: i32, color: COLORREF) {
    let width = rect_width(rect);
    let height = rect_height(rect);
    if width <= 0 || height <= 0 {
        return;
    }
    let radius = radius.clamp(0, width.min(height) / 2);
    if radius == 0 {
        fill_rect(hdc, rect, color);
        return;
    }
    unsafe {
        let region = CreateRoundRectRgn(
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius * 2,
            radius * 2,
        );
        if region.is_invalid() {
            fill_rect(hdc, rect, color);
            return;
        }
        let brush = CreateSolidBrush(color);
        let _ = FillRgn(hdc, region, brush);
        let _ = DeleteObject(region.into());
        let _ = DeleteObject(brush.into());
    }
}

/// Width of `text` in the DC's current font. Replaces the old
/// `characters * 7` estimate, which was wrong in both directions against a
/// proportional face: wide titles overflowed their pill and drew over the next
/// one, narrow titles left dead space.
pub fn text_width(hdc: HDC, text: &str) -> i32 {
    let wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() {
        return 0;
    }
    let mut size = SIZE::default();
    unsafe {
        if GetTextExtentPoint32W(hdc, &wide, &mut size).as_bool() {
            size.cx
        } else {
            0
        }
    }
}

/// Draw a single line vertically centred in `rect`, clipped to it with an
/// ellipsis. This is the only text entry point the shell uses, which is what
/// keeps baselines aligned across widgets that previously each chose their own
/// vertical offset.
pub fn draw_label(hdc: HDC, rect: &RECT, text: &str, font: HFONT, color: COLORREF) {
    if text.is_empty() || rect_width(rect) <= 0 {
        return;
    }
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut bounds = *rect;
    unsafe {
        let previous = SelectObject(hdc, font.into());
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, color);
        DrawTextW(
            hdc,
            &mut wide,
            &mut bounds,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        let _ = SelectObject(hdc, previous);
    }
}

/// Measured width of `text` in `font`, for widgets that size themselves to
/// their content.
pub fn measure_label(hdc: HDC, text: &str, font: HFONT) -> i32 {
    unsafe {
        let previous = SelectObject(hdc, font.into());
        let width = text_width(hdc, text);
        let _ = SelectObject(hdc, previous);
        width
    }
}

/// Split `span` into `count` tracks separated by `gap`, distributing the
/// remainder so the tracks end exactly on the far edge. Integer division alone
/// left the last widget short of the panel's edge by a few pixels.
pub fn split_span(start: i32, span: i32, count: usize, gap: i32) -> Vec<(i32, i32)> {
    if count == 0 {
        return Vec::new();
    }
    let total_gap = gap * (count as i32 - 1);
    let usable = (span - total_gap).max(count as i32);
    let base = usable / count as i32;
    let remainder = usable % count as i32;
    let mut tracks = Vec::with_capacity(count);
    let mut offset = start;
    for index in 0..count {
        let size = base + i32::from((index as i32) < remainder);
        tracks.push((offset, offset + size));
        offset += size + gap;
    }
    tracks
}

/// Distribute `remaining` pixels across the flex slots of `requested`, where a
/// requested width of zero means "flex". The remainder is spread across the
/// leading flex slots rather than discarded.
pub fn resolve_track_sizes(requested: &[i32], total: i32, gap: i32) -> Vec<i32> {
    let fixed: i32 = requested.iter().copied().filter(|width| *width > 0).sum();
    let flex_count = requested.iter().filter(|width| **width <= 0).count();
    let gaps = gap * (requested.len().max(1) as i32 - 1);
    if flex_count == 0 {
        return requested.iter().map(|width| (*width).max(0)).collect();
    }
    let available = (total - fixed - gaps).max(0);
    let base = available / flex_count as i32;
    let remainder = available % flex_count as i32;
    let mut flex_index = 0;
    requested
        .iter()
        .map(|width| {
            if *width > 0 {
                *width
            } else {
                let size = base + i32::from(flex_index < remainder);
                flex_index += 1;
                size
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{px, resolve_track_sizes, split_span};

    #[test]
    fn scaling_rounds_to_whole_pixels() {
        assert_eq!(px(40, 1.0), 40);
        assert_eq!(px(40, 1.5), 60);
        assert_eq!(px(13, 1.25), 16);
    }

    #[test]
    fn tracks_consume_the_whole_span() {
        let tracks = split_span(0, 101, 4, 5);
        assert_eq!(tracks.len(), 4);
        assert_eq!(tracks[0].0, 0);
        assert_eq!(tracks[3].1, 101);
    }

    #[test]
    fn flex_widths_absorb_the_remainder() {
        // Two flex slots sharing 101px must not lose the odd pixel.
        let sizes = resolve_track_sizes(&[50, 0, 0], 201, 0);
        assert_eq!(sizes[0], 50);
        assert_eq!(sizes.iter().sum::<i32>(), 201);
        assert_eq!(sizes[1] - sizes[2], 1, "remainder goes to the leading slot");
    }

    #[test]
    fn fixed_only_layouts_are_left_alone() {
        assert_eq!(resolve_track_sizes(&[30, 40], 200, 4), vec![30, 40]);
    }
}

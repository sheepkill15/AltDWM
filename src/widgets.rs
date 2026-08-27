//! Widget trait and built-ins — extensible via Rhai `custom` or Rust `cdylib` plugins.
//! See docs/EXTENSIBILITY.md
//!
//! Every widget draws through `crate::ui`, which measures text and scales all
//! lengths for the panel's DPI. Widgets never position text by hand, so a bar's
//! labels share one baseline and one rhythm regardless of panel height or
//! display scaling.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{HDC, HFONT};

use crate::config::WidgetConfig;
use crate::theme::{get_cached_font_variant, Theme};
use crate::ui::{
    draw_label, fill_rect, fill_round_rect, inset_rect, measure_label, point_in_rect, px,
    rect_height, token,
};

fn truncate_chars(value: &mut String, max_chars: usize) {
    if let Some((byte_index, _)) = value.char_indices().nth(max_chars) {
        value.truncate(byte_index);
    }
}

/// Icons are fetched with a timeout and remembered.
///
/// This used to be a blocking `SendMessageW` issued from inside `WM_PAINT`, once
/// per pill, on every paint. A single unresponsive application stopped AltDWM's
/// message loop outright: no panels, no hotkeys, no tiling, and no way to quit.
static ICON_CACHE: LazyLock<Mutex<HashMap<isize, isize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn window_icon(hwnd: HWND) -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassLongPtrW, SendMessageTimeoutW, GCLP_HICONSM, HICON, ICON_SMALL2,
        SMTO_ABORTIFHUNG, WM_GETICON,
    };
    let key = hwnd.0 as isize;
    if let Some(cached) = ICON_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
        .copied()
    {
        return (cached != 0).then_some(HICON(cached as *mut std::ffi::c_void));
    }
    let raw = unsafe {
        let mut result: usize = 0;
        let replied = SendMessageTimeoutW(
            hwnd,
            WM_GETICON,
            WPARAM(ICON_SMALL2 as usize),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            60,
            Some(&mut result),
        );
        if replied.0 != 0 && result != 0 {
            result as isize
        } else {
            GetClassLongPtrW(hwnd, GCLP_HICONSM) as isize
        }
    };
    ICON_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(key, raw);
    (raw != 0).then_some(HICON(raw as *mut std::ffi::c_void))
}

/// Drop cached icons for windows that no longer exist.
pub fn forget_icon(hwnd: HWND) {
    ICON_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&(hwnd.0 as isize));
}

/// How a widget wants pointer hover reflected.
///
/// The panel used to paint a hover slab across the whole widget rectangle.
/// `window_list` is a flex widget occupying most of the bar, so hovering
/// anywhere over the open windows filled a rounded panel nearly the width of
/// the display. Widgets that contain individually clickable items now draw
/// their own hover instead.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HoverPaint {
    /// Not interactive — no hover feedback at all.
    None,
    /// One clickable surface: the panel may highlight the whole rectangle.
    Whole,
    /// Several clickable items: the widget highlights the one under the pointer.
    SelfDrawn,
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
    /// DPI scale of the display this panel is on. Multiply every design constant
    /// by this before drawing.
    pub scale: f32,
    /// True for `left` and `right` panels, where items stack downward.
    pub vertical: bool,
    /// Pointer position in client coordinates while the pointer is over this
    /// panel, so widgets can highlight the item under it.
    pub pointer: Option<(i32, i32)>,
    pub theme: Theme,
}

impl PanelCtx {
    /// Device-independent length in physical pixels for this panel.
    pub fn px(&self, value: i32) -> i32 {
        px(value, self.scale)
    }

    pub fn font(&self, size: i32, weight: i32) -> HFONT {
        get_cached_font_variant(&self.theme, self.px(size), weight)
    }

    /// The panel's body font.
    pub fn body_font(&self) -> HFONT {
        self.font(self.theme.font_size, 400)
    }

    pub fn strong_font(&self) -> HFONT {
        self.font(self.theme.font_size, 600)
    }

    pub fn small_font(&self) -> HFONT {
        self.font((self.theme.font_size - 2).max(8), 400)
    }

    /// Corner radius for pills on this panel, in physical pixels.
    pub fn radius(&self) -> i32 {
        self.px(self.theme.rounding)
    }

    fn pointer_in(&self, rect: &RECT) -> bool {
        self.pointer
            .is_some_and(|(x, y)| point_in_rect(x, y, rect))
    }
}

/// Core extensibility point — implement this to add a widget.
pub trait Widget: Send + Sync {
    fn name(&self) -> &str;
    /// The widget's `type` from configuration, as opposed to its instance name.
    fn kind(&self) -> &'static str;
    /// 0 = flex (takes remaining space), >0 = fixed device-independent pixels
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        0
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx);
    /// return Some(action) to handle click. `point` is in client coordinates and
    /// `rect` is the widget's own rectangle, so hit-testing can reuse exactly
    /// the geometry that `draw` produced.
    fn on_click(&self, _point: (i32, i32), _rect: RECT, _ctx: &PanelCtx) -> Option<String> {
        None
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::None
    }
    fn interval_ms(&self) -> Option<u32> {
        None
    }
    /// Refresh any state that costs real work. Called from the panel's timer,
    /// never from `WM_PAINT`, so a slow script cannot stall painting.
    fn tick(&self) {}
}

/// The content box of a widget: full height minus the panel's inset.
fn content_rect(rect: RECT, ctx: &PanelCtx) -> RECT {
    inset_rect(rect, ctx.px(token::ITEM_GAP), ctx.px(token::INSET))
}

/// A small filled square used in place of decorative font glyphs. `⌘` and `◇`
/// are not present in Segoe UI, so they rendered as fallback or missing-glyph
/// boxes; a drawn shape has no font dependency at all.
fn draw_mark(hdc: HDC, anchor: &RECT, ctx: &PanelCtx, color: COLORREF) -> i32 {
    let side = ctx.px(token::MARK);
    let top = anchor.top + (rect_height(anchor) - side) / 2;
    let mark = RECT {
        left: anchor.left,
        top,
        right: anchor.left + side,
        bottom: top + side,
    };
    fill_round_rect(hdc, &mark, ctx.px(2), color);
    side
}

// ---- built-ins --------------------------------------------------

pub struct ClockWidget {
    pub cfg: WidgetConfig,
}

fn format_time(format: &str) -> String {
    let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let hour12 = match st.wHour % 12 {
        0 => 12,
        other => other,
    };
    let mut out = String::with_capacity(format.len() + 8);
    let mut chars = format.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            // `%%` is a literal percent. The old chained `replace` calls could
            // not express this and would rewrite the escape's own specifier.
            Some('%') => out.push('%'),
            Some('H') => out.push_str(&format!("{:02}", st.wHour)),
            Some('I') => out.push_str(&format!("{:02}", hour12)),
            Some('M') => out.push_str(&format!("{:02}", st.wMinute)),
            Some('S') => out.push_str(&format!("{:02}", st.wSecond)),
            Some('p') => out.push_str(if st.wHour < 12 { "AM" } else { "PM" }),
            Some('Y') => out.push_str(&format!("{}", st.wYear)),
            Some('m') => out.push_str(&format!("{:02}", st.wMonth)),
            Some('d') => out.push_str(&format!("{:02}", st.wDay)),
            Some('a') => out.push_str(WEEKDAYS[usize::from(st.wDayOfWeek).min(6)]),
            Some('b') => {
                out.push_str(MONTHS[usize::from(st.wMonth.saturating_sub(1)).min(11)]);
            }
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

impl Widget for ClockWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "clock"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(112)
    }
    fn hover_paint(&self) -> HoverPaint {
        if self.cfg.action.is_some() {
            HoverPaint::Whole
        } else {
            HoverPaint::None
        }
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let body = content_rect(rect, ctx);
        let time = format_time(self.cfg.format.as_deref().unwrap_or("%H:%M"));
        // A second line only appears when there is genuinely room for two, so a
        // short bar shows a single centred time instead of clipped text.
        let two_line = rect_height(&body) >= ctx.px(34);
        let text_area = inset_rect(body, ctx.px(token::PAD), 0);
        if !two_line {
            draw_label(
                hdc,
                &text_area,
                &time,
                ctx.strong_font(),
                ctx.theme.text_color(),
            );
            return;
        }
        let split = text_area.top + rect_height(&text_area) * 55 / 100;
        let top_half = RECT {
            bottom: split,
            ..text_area
        };
        let bottom_half = RECT {
            top: split,
            ..text_area
        };
        draw_label(
            hdc,
            &top_half,
            &time,
            ctx.strong_font(),
            ctx.theme.text_color(),
        );
        draw_label(
            hdc,
            &bottom_half,
            &format_time("%a %d %b"),
            ctx.small_font(),
            ctx.theme.text_dim_color(),
        );
    }
    fn on_click(&self, _point: (i32, i32), _rect: RECT, _ctx: &PanelCtx) -> Option<String> {
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
    fn kind(&self) -> &'static str {
        "spacer"
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
    fn kind(&self) -> &'static str {
        "window_title"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        0
    } // flex
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let hwnd = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
        let mut title = crate::util::get_window_title(hwnd);
        let max = self
            .cfg
            .extra
            .get("max_len")
            .and_then(|v| v.as_integer())
            .unwrap_or(64)
            .max(0) as usize;
        if title.chars().count() > max {
            truncate_chars(&mut title, max);
            title.push('…');
        }
        if title.is_empty() {
            title = "AltDWM".into();
        }
        // DT_END_ELLIPSIS already clips to the rectangle; max_len only caps how
        // much of a very long title is considered at all.
        let area = inset_rect(content_rect(rect, ctx), ctx.px(token::PAD), 0);
        draw_label(hdc, &area, &title, ctx.body_font(), ctx.theme.text_color());
    }
}

pub struct TrayWidget {
    pub cfg: WidgetConfig,
}

impl TrayWidget {
    /// Item rectangles, shared by `draw` and `on_click`. Previously each
    /// computed its own bounds and the click loop had no cut-off at all, so
    /// clicking past the last visible icon invoked a tray item that had never
    /// been drawn.
    fn entry_layout(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) -> Vec<(usize, RECT)> {
        let body = content_rect(rect, ctx);
        let font = ctx.body_font();
        let gap = ctx.px(token::ITEM_GAP);
        let pad = ctx.px(token::PAD);
        let entries = crate::tray::entries();
        let mut items = Vec::new();
        let mut offset = if ctx.vertical { body.top } else { body.left };
        for (index, entry) in entries.iter().enumerate() {
            let label = crate::tray::compact_name(&entry.name);
            let (item, advance) = if ctx.vertical {
                let height = ctx.px(26);
                if offset + height > body.bottom {
                    break;
                }
                (
                    RECT {
                        left: body.left,
                        top: offset,
                        right: body.right,
                        bottom: offset + height,
                    },
                    height,
                )
            } else {
                let width = measure_label(hdc, &label, font) + pad * 2;
                if offset + width > body.right {
                    break;
                }
                (
                    RECT {
                        left: offset,
                        top: body.top,
                        right: offset + width,
                        bottom: body.bottom,
                    },
                    width,
                )
            };
            items.push((index, item));
            offset += advance + gap;
        }
        items
    }
}

impl Widget for TrayWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "tray"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(196)
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::SelfDrawn
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let items = self.entry_layout(hdc, rect, ctx);
        if items.is_empty() {
            let area = inset_rect(content_rect(rect, ctx), ctx.px(token::PAD), 0);
            draw_label(
                hdc,
                &area,
                "No tray items",
                ctx.small_font(),
                ctx.theme.text_dim_color(),
            );
            return;
        }
        let entries = crate::tray::entries();
        let radius = ctx.radius();
        for (index, item) in items {
            let hovered = ctx.pointer_in(&item);
            let background = if hovered {
                ctx.theme.surface_hover_color()
            } else {
                ctx.theme.color(&ctx.theme.tray_bg)
            };
            fill_round_rect(hdc, &item, radius, background);
            let label = entries
                .get(index)
                .map(|entry| crate::tray::compact_name(&entry.name))
                .unwrap_or_default();
            let text = inset_rect(item, ctx.px(token::PAD), 0);
            let color = if hovered {
                ctx.theme.text_color()
            } else {
                ctx.theme.text_dim_color()
            };
            draw_label(hdc, &text, &label, ctx.body_font(), color);
        }
    }

    fn on_click(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        // Hit-testing needs a DC in the same font the layout was measured with.
        let hdc = unsafe { windows::Win32::Graphics::Gdi::GetDC(Some(ctx.hwnd)) };
        let items = self.entry_layout(hdc, rect, ctx);
        if !hdc.is_invalid() {
            unsafe {
                windows::Win32::Graphics::Gdi::ReleaseDC(Some(ctx.hwnd), hdc);
            }
        }
        for (index, item) in items {
            if point_in_rect(point.0, point.1, &item) {
                crate::tray::invoke(index);
                break;
            }
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
    fn kind(&self) -> &'static str {
        "layout"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(178)
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::Whole
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let mut wins = ctx.windows.clone();
        wins.retain(|w| {
            !crate::rules::is_floating(*w)
                && !crate::focus::is_runtime_floating(*w)
                && !crate::manager::is_auto_floating(*w)
        });
        let layout = crate::CURRENT_LAYOUT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .name();
        let enabled = crate::TILING_ENABLED.load(std::sync::atomic::Ordering::SeqCst);

        let chip = content_rect(rect, ctx);
        // The surface stays calm in both states. Previously the *disabled* state
        // was painted in the accent colour, so a stopped window manager was the
        // one that looked lit up.
        fill_round_rect(hdc, &chip, ctx.radius(), ctx.theme.surface_color());

        let body = inset_rect(chip, ctx.px(token::PAD), 0);
        let mark_color = if enabled {
            ctx.theme.accent_active_color()
        } else {
            ctx.theme.text_dim_color()
        };
        let mark_width = draw_mark(hdc, &body, ctx, mark_color);
        let text = RECT {
            left: body.left + mark_width + ctx.px(token::PAD),
            ..body
        };
        let two_line = rect_height(&chip) >= ctx.px(34);
        let status = if enabled {
            format!("{} managed", wins.len())
        } else {
            "paused".to_string()
        };
        if !two_line {
            draw_label(
                hdc,
                &text,
                &format!("{layout} · {status}"),
                ctx.body_font(),
                ctx.theme.text_color(),
            );
            return;
        }
        let split = text.top + rect_height(&text) * 55 / 100;
        draw_label(
            hdc,
            &RECT {
                bottom: split,
                ..text
            },
            layout,
            ctx.strong_font(),
            ctx.theme.text_color(),
        );
        draw_label(
            hdc,
            &RECT { top: split, ..text },
            &status,
            ctx.small_font(),
            ctx.theme.text_dim_color(),
        );
    }
    fn on_click(&self, _point: (i32, i32), _rect: RECT, _ctx: &PanelCtx) -> Option<String> {
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
        // set_layout_by_name already requests a retile.
        crate::set_layout_by_name(next);
        None
    }
}

pub struct WindowListWidget {
    pub cfg: WidgetConfig,
}

struct WindowPill {
    hwnd: HWND,
    rect: RECT,
    label: String,
    active: bool,
    minimized: bool,
}

impl WindowListWidget {
    fn max_label_chars(&self) -> usize {
        self.cfg
            .extra
            .get("max_len")
            .and_then(|value| value.as_integer())
            .unwrap_or(22)
            .clamp(4, 120) as usize
    }

    /// Lay out one pill per window, sized to its measured label.
    ///
    /// `draw` and `on_click` share this, so a click can never land on a pill
    /// that was not drawn. The two used to disagree: drawing stopped at the
    /// widget's right edge while hit-testing bounded against the whole panel
    /// width, and clicking past the last visible pill raised a hidden window.
    fn pills(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) -> (Vec<WindowPill>, usize) {
        let body = content_rect(rect, ctx);
        let font = ctx.body_font();
        let gap = ctx.px(token::ITEM_GAP);
        let pad = ctx.px(token::PAD);
        let icon = ctx.px(16);
        let foreground = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
        let max_chars = self.max_label_chars();
        let total = ctx.windows.len();
        let mut pills: Vec<WindowPill> = Vec::new();
        let mut offset = if ctx.vertical { body.top } else { body.left };

        for hwnd in &ctx.windows {
            let minimized =
                unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(*hwnd).as_bool() };
            let mut label = crate::util::get_window_title(*hwnd);
            if label.is_empty() {
                label = crate::util::get_class_name(*hwnd);
            }
            if label.chars().count() > max_chars {
                truncate_chars(&mut label, max_chars);
                label.push('…');
            }
            let remaining = total - pills.len();
            // Leave room for the "+N" marker whenever more windows follow.
            let reserve = if remaining > 1 { ctx.px(34) } else { 0 };
            let (item, advance) = if ctx.vertical {
                let height = ctx.px(30);
                if offset + height + reserve > body.bottom {
                    break;
                }
                (
                    RECT {
                        left: body.left,
                        top: offset,
                        right: body.right,
                        bottom: offset + height,
                    },
                    height,
                )
            } else {
                let width = pad + icon + ctx.px(6) + measure_label(hdc, &label, font) + pad;
                if offset + width + reserve > body.right {
                    break;
                }
                (
                    RECT {
                        left: offset,
                        top: body.top,
                        right: offset + width,
                        bottom: body.bottom,
                    },
                    width,
                )
            };
            pills.push(WindowPill {
                hwnd: *hwnd,
                rect: item,
                label,
                active: *hwnd == foreground,
                minimized,
            });
            offset += advance + gap;
        }
        (pills, total)
    }
}

impl Widget for WindowListWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "window_list"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        0
    } // flex
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::SelfDrawn
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let (pills, total) = self.pills(hdc, rect, ctx);
        let radius = ctx.radius();
        if pills.is_empty() {
            let area = inset_rect(content_rect(rect, ctx), ctx.px(token::PAD), 0);
            draw_label(
                hdc,
                &area,
                "No open windows",
                ctx.small_font(),
                ctx.theme.text_dim_color(),
            );
            return;
        }
        for pill in &pills {
            let hovered = ctx.pointer_in(&pill.rect);
            let background = if pill.active || hovered {
                ctx.theme.surface_hover_color()
            } else {
                ctx.theme.surface_color()
            };
            fill_round_rect(hdc, &pill.rect, radius, background);

            if pill.active {
                // Indicator inset by the corner radius so it reads as part of
                // the pill rather than clipping through its rounded corners.
                let indicator = RECT {
                    left: pill.rect.left + radius,
                    top: pill.rect.bottom - ctx.px(token::INDICATOR),
                    right: pill.rect.right - radius,
                    bottom: pill.rect.bottom,
                };
                fill_rect(hdc, &indicator, ctx.theme.accent_active_color());
            }

            let icon_side = ctx.px(16);
            let icon_left = pill.rect.left + ctx.px(token::PAD);
            let icon_top = pill.rect.top + (rect_height(&pill.rect) - icon_side) / 2;
            if let Some(icon) = window_icon(pill.hwnd) {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::DrawIconEx(
                        hdc,
                        icon_left,
                        icon_top,
                        icon,
                        icon_side,
                        icon_side,
                        0,
                        None,
                        windows::Win32::UI::WindowsAndMessaging::DI_NORMAL,
                    );
                }
            } else {
                let dot = RECT {
                    left: icon_left + icon_side / 4,
                    top: icon_top + icon_side / 4,
                    right: icon_left + icon_side * 3 / 4,
                    bottom: icon_top + icon_side * 3 / 4,
                };
                fill_round_rect(hdc, &dot, ctx.px(4), ctx.theme.accent_color());
            }

            // A minimized window reads as recessed through colour, rather than
            // through a "- " prefix bolted onto its title.
            let color = if pill.minimized {
                ctx.theme.text_dim_color()
            } else {
                ctx.theme.text_color()
            };
            let text = RECT {
                left: icon_left + icon_side + ctx.px(6),
                right: pill.rect.right - ctx.px(token::ITEM_GAP),
                ..pill.rect
            };
            draw_label(hdc, &text, &pill.label, ctx.body_font(), color);
        }
        if pills.len() < total {
            let last = pills.last().map(|pill| pill.rect).unwrap_or(rect);
            let marker = if ctx.vertical {
                RECT {
                    top: last.bottom + ctx.px(token::ITEM_GAP),
                    ..content_rect(rect, ctx)
                }
            } else {
                RECT {
                    left: last.right + ctx.px(token::ITEM_GAP),
                    ..content_rect(rect, ctx)
                }
            };
            draw_label(
                hdc,
                &marker,
                &format!("+{}", total - pills.len()),
                ctx.small_font(),
                ctx.theme.text_dim_color(),
            );
        }
    }
    fn on_click(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        let hdc = unsafe { windows::Win32::Graphics::Gdi::GetDC(Some(ctx.hwnd)) };
        let (pills, _) = self.pills(hdc, rect, ctx);
        if !hdc.is_invalid() {
            unsafe {
                windows::Win32::Graphics::Gdi::ReleaseDC(Some(ctx.hwnd), hdc);
            }
        }
        for pill in pills {
            if point_in_rect(point.0, point.1, &pill.rect) {
                crate::focus::toggle_window_from_list(pill.hwnd);
                break;
            }
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
    fn kind(&self) -> &'static str {
        "launcher"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(104)
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::Whole
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let button = content_rect(rect, ctx);
        let hovered = ctx.pointer_in(&button);
        let background = if hovered {
            ctx.theme.accent_color()
        } else {
            ctx.theme.accent_active_color()
        };
        fill_round_rect(hdc, &button, ctx.radius(), background);
        let body = inset_rect(button, ctx.px(token::PAD), 0);
        let mark_width = draw_mark(hdc, &body, ctx, ctx.theme.text_color());
        let label = self
            .cfg
            .label
            .as_deref()
            .or(self.cfg.icon.as_deref())
            .unwrap_or("AltDWM");
        draw_label(
            hdc,
            &RECT {
                left: body.left + mark_width + ctx.px(6),
                ..body
            },
            label,
            ctx.strong_font(),
            ctx.theme.text_color(),
        );
    }
    fn on_click(&self, _point: (i32, i32), _rect: RECT, ctx: &PanelCtx) -> Option<String> {
        // A configured action remains an escape hatch for launch-only widgets;
        // the built-in launcher opens AltDWM's discoverable command surface.
        if let Some(action) = self.cfg.action.clone().or_else(|| self.cfg.command.clone()) {
            return Some(action);
        }
        crate::command_center::toggle(ctx.hwnd);
        None
    }
}

/// Custom Rhai-drawn widget — script returns text to draw.
///
/// Evaluation happens on the panel timer, never during `WM_PAINT`. Running a
/// script and reading its file from the paint handler stalled the shell for as
/// long as the script took, and did it while the panel collection was locked.
pub struct CustomWidget {
    pub cfg: WidgetConfig,
    state: Mutex<CustomState>,
}

#[derive(Default)]
struct CustomState {
    evaluated_at: Option<Instant>,
    text: String,
}

impl CustomWidget {
    fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.cfg.interval.unwrap_or(1000).max(50) as u64)
    }

    fn evaluate(&self) -> String {
        let Some(script) = &self.cfg.script else {
            return self.cfg.label.clone().unwrap_or_else(|| "custom".into());
        };
        let code = if let Some(inline) = script.strip_prefix("rhai:") {
            Ok(inline.trim().to_string())
        } else {
            read_widget_script(script)
        };
        code.and_then(|code| crate::scripting::eval_text(&code))
            .unwrap_or_else(|error| format!("rhai: {error}"))
    }
}

impl Widget for CustomWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "custom"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(120)
    }
    fn hover_paint(&self) -> HoverPaint {
        if self.cfg.action.is_some() {
            HoverPaint::Whole
        } else {
            HoverPaint::None
        }
    }
    fn tick(&self) {
        let due = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .evaluated_at
                .is_none_or(|last| last.elapsed() >= self.interval())
        };
        if !due {
            return;
        }
        // Evaluated without the state lock held: a script may take a while and
        // must not block a concurrent paint from reading the previous value.
        let text = self.evaluate();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.text = text;
        state.evaluated_at = Some(Instant::now());
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let text = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.text.clone()
        };
        let area = inset_rect(content_rect(rect, ctx), ctx.px(token::PAD), 0);
        draw_label(hdc, &area, &text, ctx.body_font(), ctx.theme.text_color());
    }
    fn on_click(&self, _point: (i32, i32), _rect: RECT, _ctx: &PanelCtx) -> Option<String> {
        self.cfg.action.clone()
    }
    fn interval_ms(&self) -> Option<u32> {
        self.cfg.interval
    }
}

/// Widget scripts are read once and re-read only when the file changes on disk.
static SCRIPT_CACHE: LazyLock<Mutex<HashMap<String, (std::time::SystemTime, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn widget_script_candidates(script: &str) -> Vec<std::path::PathBuf> {
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
}

fn read_widget_script(script: &str) -> Result<String, String> {
    for path in widget_script_candidates(script) {
        let Ok(modified) = std::fs::metadata(&path).and_then(|meta| meta.modified()) else {
            continue;
        };
        let key = path.to_string_lossy().to_string();
        if let Some((cached_at, cached)) = SCRIPT_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            if *cached_at == modified {
                return Ok(cached.clone());
            }
        }
        if let Ok(code) = std::fs::read_to_string(&path) {
            SCRIPT_CACHE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, (modified, code.clone()));
            return Ok(code);
        }
    }
    Err(format!("script not found: {script}"))
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
            state: Mutex::new(CustomState::default()),
        }),
        other => {
            eprintln!(
                "[widgets] unknown type '{}' for '{}' -> custom fallback",
                other, cfg.name
            );
            Box::new(CustomWidget {
                cfg: cfg.clone(),
                state: Mutex::new(CustomState::default()),
            })
        }
    }
}

/// The rectangle a panel should highlight when the pointer is over a widget
/// that asked for `HoverPaint::Whole`.
pub fn widget_content_rect(rect: RECT, ctx: &PanelCtx) -> RECT {
    content_rect(rect, ctx)
}

#[cfg(test)]
mod tests {
    use super::{format_time, truncate_chars};

    #[test]
    fn truncates_unicode_at_character_boundaries() {
        let mut title = "Browser 🌍 тест".to_string();
        truncate_chars(&mut title, 10);
        assert_eq!(title, "Browser 🌍 ");
    }

    #[test]
    fn clock_format_escapes_a_literal_percent() {
        assert_eq!(format_time("100%%"), "100%");
    }

    #[test]
    fn clock_format_leaves_unknown_specifiers_intact() {
        assert_eq!(format_time("%q"), "%q");
    }

    #[test]
    fn clock_format_expands_the_documented_specifiers() {
        let rendered = format_time("%H:%M|%a|%b");
        let mut parts = rendered.split('|');
        let time = parts.next().unwrap();
        assert_eq!(time.len(), 5, "HH:MM");
        assert!(time.as_bytes()[2] == b':');
        assert_eq!(parts.next().unwrap().len(), 3, "abbreviated weekday");
        assert_eq!(parts.next().unwrap().len(), 3, "abbreviated month");
    }
}

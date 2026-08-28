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
use crate::theme::{get_cached_font_variant, get_cached_symbol_font, Theme};
use crate::tray::TrayEntry;
use crate::ui::{
    draw_label, fill_rect, fill_round_rect, inset_rect, measure_label, point_in_rect, px,
    rect_height, rect_width, token,
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
        GetClassLongPtrW, SendMessageTimeoutW, GCLP_HICONSM, HICON, ICON_SMALL2, SMTO_ABORTIFHUNG,
        WM_GETICON,
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

/// Drop cached icons for windows that are no longer live.
pub fn retain_icons(live: &std::collections::HashSet<isize>) {
    ICON_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .retain(|key, _| live.contains(key));
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
    /// The panel's configured monitor target, e.g. `all` or `primary`.
    pub monitor: String,
    /// The `HMONITOR` this panel is actually on, as an integer key. Workspaces
    /// are per monitor, so the strip has to know which display it belongs to.
    pub monitor_key: isize,
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

    /// Windows' native icon face at one of Microsoft's recommended optical
    /// sizes. The value is specified in DIPs and scaled with the panel.
    pub fn symbol_font(&self) -> HFONT {
        get_cached_symbol_font(self.px(16))
    }

    /// Corner radius for pills on this panel, in physical pixels.
    pub fn radius(&self) -> i32 {
        self.px(self.theme.rounding)
    }

    fn pointer_in(&self, rect: &RECT) -> bool {
        self.pointer.is_some_and(|(x, y)| point_in_rect(x, y, rect))
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
    /// Handle a right click. Most widgets have no second meaning for one; the
    /// tray does, because that is where applications put their menus.
    fn on_right_click(&self, _point: (i32, i32), _rect: RECT, _ctx: &PanelCtx) -> Option<String> {
        None
    }
    /// Handle a double click. The default is a second ordinary click, which is
    /// how every widget behaved before the panel started asking for
    /// `CS_DBLCLKS` on the tray's behalf.
    fn on_double_click(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        self.on_click(point, rect, ctx)
    }
    /// Handle a mouse wheel notch over the widget. `delta` is +1 for a scroll
    /// away from the user and -1 towards. Returning true consumes the event.
    fn on_scroll(&self, _delta: i32, _point: (i32, i32), _rect: RECT, _ctx: &PanelCtx) -> bool {
        false
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::None
    }
    fn interval_ms(&self) -> Option<u32> {
        None
    }
    /// Refresh any state that costs real work, and report whether what the
    /// widget displays actually changed.
    ///
    /// Called from the panel's timer, never from `WM_PAINT`, so a slow script
    /// cannot stall painting. The return value is what stops a bar with a
    /// sub-second widget from repainting continuously: the panel only
    /// invalidates when a widget says its content moved. Widgets whose data is
    /// pushed to them — anything reading `crate::system`, the tray, or the
    /// window list — return `false` and rely on the invalidate that accompanies
    /// the change.
    fn tick(&self) -> bool {
        false
    }
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
    /// Rendered time and date, refreshed on the timer. A `%H:%M` clock changes
    /// once a minute; formatting it on every paint and repainting regardless was
    /// most of a bar's idle cost.
    state: Mutex<(String, String)>,
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
    fn tick(&self) -> bool {
        let next = (
            format_time(self.cfg.format.as_deref().unwrap_or("%H:%M")),
            format_time("%a %d %b"),
        );
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if *state == next {
            return false;
        }
        *state = next;
        true
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let body = content_rect(rect, ctx);
        let (time, date) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.clone()
        };
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
            &date,
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

/// Notification-area geometry, in device-independent pixels.
const TRAY_ICON: i32 = 16;
/// A square button around a 16px icon.
const TRAY_ITEM: i32 = 26;
/// Explorer-mirrored entries carry no bitmap, only a name, and need room for it.
const TRAY_LABEL_ITEM: i32 = 72;
const TRAY_CHEVRON: i32 = 26;
/// Enough for "No tray icons", so an empty tray reads as empty rather than as a
/// gap in the bar.
const TRAY_EMPTY: i32 = 104;

fn tray_item_span(entry: &TrayEntry) -> i32 {
    if entry.icon != 0 {
        TRAY_ITEM
    } else {
        TRAY_LABEL_ITEM
    }
}

/// Where every tray item ended up. Computed once and used by `draw` and by all
/// three click handlers, so a click can only ever land on something that was
/// actually drawn.
struct TrayLayout {
    /// Entries on the bar, each with the rectangle it was drawn in.
    items: Vec<(TrayEntry, RECT)>,
    /// The overflow button, present only when it has something behind it.
    chevron: Option<RECT>,
    /// Entries the bar is not showing: those that asked to be hidden, then any
    /// that did not fit.
    overflow: Vec<TrayEntry>,
}

impl TrayWidget {
    fn max_items(&self) -> usize {
        self.cfg
            .extra
            .get("max_items")
            .and_then(|value| value.as_integer())
            .map(|value| value.max(0) as usize)
            .unwrap_or(3)
    }

    fn layout(&self, rect: RECT, ctx: &PanelCtx) -> TrayLayout {
        self.layout_of(crate::tray::entries(), rect, ctx)
    }

    fn layout_of(&self, entries: Vec<TrayEntry>, rect: RECT, ctx: &PanelCtx) -> TrayLayout {
        let (bar, hidden): (Vec<TrayEntry>, Vec<TrayEntry>) =
            entries.into_iter().partition(|entry| !entry.hidden);
        let body = content_rect(rect, ctx);
        let gap = ctx.px(token::ITEM_GAP);
        let chevron_span = ctx.px(TRAY_CHEVRON);
        let limit = self.max_items();

        let place = |reserve: bool| -> Vec<(TrayEntry, RECT)> {
            let far = if ctx.vertical {
                body.bottom
            } else {
                body.right
            } - if reserve { chevron_span + gap } else { 0 };
            let mut offset = if ctx.vertical { body.top } else { body.left };
            let mut placed = Vec::new();
            for entry in bar.iter().take(limit) {
                let span = ctx.px(tray_item_span(entry));
                if offset + span > far {
                    break;
                }
                let item = if ctx.vertical {
                    RECT {
                        left: body.left,
                        top: offset,
                        right: body.right,
                        bottom: offset + span,
                    }
                } else {
                    RECT {
                        left: offset,
                        top: body.top,
                        right: offset + span,
                        bottom: body.bottom,
                    }
                };
                placed.push((entry.clone(), item));
                offset += span + gap;
            }
            placed
        };

        let mut items = place(!hidden.is_empty());
        if hidden.is_empty() && items.len() < bar.len() {
            // Something was clipped after all, so room has to be found for a
            // chevron — which may push one more item behind it.
            items = place(true);
        }
        let mut overflow: Vec<TrayEntry> = bar.into_iter().skip(items.len()).collect();
        overflow.extend(hidden);
        let chevron = (!overflow.is_empty()).then(|| {
            if ctx.vertical {
                RECT {
                    top: body.bottom - chevron_span,
                    ..body
                }
            } else {
                RECT {
                    left: body.right - chevron_span,
                    ..body
                }
            }
        });
        TrayLayout {
            items,
            chevron,
            overflow,
        }
    }

    /// Route a click to the item under it. The overflow button is checked first
    /// because it is drawn last and therefore sits on top.
    fn dispatch(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx, button: crate::tray::Button) {
        let layout = self.layout(rect, ctx);
        if let Some(chevron) = layout.chevron {
            if point_in_rect(point.0, point.1, &chevron) {
                crate::tray_overflow::toggle(
                    crate::ui::client_rect_to_screen(ctx.hwnd, chevron),
                    layout.overflow,
                );
                return;
            }
        }
        for (entry, item) in layout.items {
            if point_in_rect(point.0, point.1, &item) {
                crate::tray::invoke(entry.id, button);
                return;
            }
        }
    }
}

fn open_quick_settings(rect: RECT, ctx: &PanelCtx) {
    crate::quick_settings::toggle_from_panel(
        ctx.hwnd,
        crate::ui::client_rect_to_screen(ctx.hwnd, rect),
        &ctx.panel_name,
    );
}

fn widget_flag(cfg: &WidgetConfig, name: &str, default: bool) -> bool {
    cfg.extra
        .get(name)
        .and_then(|value| value.as_bool())
        .unwrap_or(default)
}

impl Widget for TrayWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "tray"
    }
    /// Sized to its contents. A fixed width either clipped a busy tray or left a
    /// hole on a quiet one; `width` in configuration still wins, for anyone who
    /// would rather the bar's geometry stopped moving.
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        if let Some(width) = self.cfg.width {
            return width;
        }
        let entries = crate::tray::entries();
        if entries.is_empty() {
            return TRAY_EMPTY;
        }
        let bar: Vec<&TrayEntry> = entries
            .iter()
            .filter(|entry| !entry.hidden)
            .take(self.max_items())
            .collect();
        let mut span: i32 = bar.iter().map(|entry| tray_item_span(entry)).sum();
        span += token::ITEM_GAP * (bar.len() as i32 - 1).max(0);
        if entries.len() > bar.len() {
            span += TRAY_CHEVRON + if bar.is_empty() { 0 } else { token::ITEM_GAP };
        }
        span + token::ITEM_GAP * 2
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::SelfDrawn
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let layout = self.layout(rect, ctx);
        if layout.items.is_empty() && layout.chevron.is_none() {
            let area = inset_rect(content_rect(rect, ctx), ctx.px(token::PAD), 0);
            draw_label(
                hdc,
                &area,
                "No tray icons",
                ctx.small_font(),
                ctx.theme.text_dim_color(),
            );
            return;
        }
        let radius = ctx.radius();
        let idle = ctx.theme.color(&ctx.theme.tray_bg);
        for (entry, item) in &layout.items {
            let hovered = ctx.pointer_in(item);
            let background = if hovered {
                ctx.theme.surface_hover_color()
            } else {
                idle
            };
            fill_round_rect(hdc, item, radius, background);
            if entry.icon != 0 {
                let side = ctx.px(TRAY_ICON);
                let left = item.left + (rect_width(item) - side) / 2;
                let top = item.top + (rect_height(item) - side) / 2;
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::DrawIconEx(
                        hdc,
                        left,
                        top,
                        windows::Win32::UI::WindowsAndMessaging::HICON(
                            entry.icon as *mut std::ffi::c_void,
                        ),
                        side,
                        side,
                        0,
                        None,
                        windows::Win32::UI::WindowsAndMessaging::DI_NORMAL,
                    );
                }
            } else {
                // The Explorer bridge can name a button but never draw it.
                let text = inset_rect(*item, ctx.px(6), 0);
                let color = if hovered {
                    ctx.theme.text_color()
                } else {
                    ctx.theme.text_dim_color()
                };
                draw_label(
                    hdc,
                    &text,
                    &crate::tray::compact_name(&entry.name),
                    ctx.body_font(),
                    color,
                );
            }
        }
        if let Some(chevron) = layout.chevron {
            let hovered = ctx.pointer_in(&chevron);
            fill_round_rect(
                hdc,
                &chevron,
                radius,
                if hovered {
                    ctx.theme.surface_hover_color()
                } else {
                    idle
                },
            );
            // A count rather than a glyph, for the same reason `draw_mark`
            // exists: chevron characters are not reliably present in the
            // configured face, and "+3" says more than an arrow would anyway.
            let label = format!("+{}", layout.overflow.len());
            let font = ctx.small_font();
            let text_width = measure_label(hdc, &label, font);
            let centred = RECT {
                left: chevron.left + (rect_width(&chevron) - text_width).max(0) / 2,
                ..chevron
            };
            draw_label(
                hdc,
                &centred,
                &label,
                font,
                if hovered {
                    ctx.theme.text_color()
                } else {
                    ctx.theme.text_dim_color()
                },
            );
        }
    }

    fn on_click(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        self.dispatch(point, rect, ctx, crate::tray::Button::Left);
        None
    }

    /// Right click is most of the point of a tray icon — it is where an
    /// application puts Quit.
    fn on_right_click(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        self.dispatch(point, rect, ctx, crate::tray::Button::Right);
        None
    }

    /// Older applications open their main window on a double click and ignore a
    /// single one entirely.
    fn on_double_click(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        self.dispatch(point, rect, ctx, crate::tray::Button::DoubleLeft);
        None
    }
}

/// Current layout and managed count. Named `layout` in configuration; this is
/// not the workspace strip, which is `WorkspacesWidget` below.
pub struct LayoutWidget {
    pub cfg: WidgetConfig,
}
impl Widget for LayoutWidget {
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
        // Cycling forward through the built-ins; a custom Rhai layout is
        // selected by name from configuration or the command center.

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

/// The workspace strip: one pill per configured workspace, click to switch.
///
/// This is what the `workspaces` widget name has always promised. Until now it
/// was an alias for the layout capsule, because AltDWM had no workspaces.
pub struct WorkspacesWidget {
    pub cfg: WidgetConfig,
}

impl WorkspacesWidget {
    /// Pill rectangles, shared by draw and hit-testing.
    fn pills(&self, rect: RECT, ctx: &PanelCtx) -> Vec<(usize, RECT)> {
        let body = content_rect(rect, ctx);
        let strip = crate::workspace::summary(ctx.monitor_key, &ctx.windows);
        if strip.is_empty() {
            return Vec::new();
        }
        let gap = ctx.px(token::ITEM_GAP);
        let extent = if ctx.vertical {
            rect_height(&body)
        } else {
            body.right - body.left
        };
        crate::ui::split_span(
            if ctx.vertical { body.top } else { body.left },
            extent,
            strip.len(),
            gap,
        )
        .into_iter()
        .enumerate()
        .map(|(index, (start, end))| {
            let pill = if ctx.vertical {
                RECT {
                    left: body.left,
                    top: start,
                    right: body.right,
                    bottom: end,
                }
            } else {
                RECT {
                    left: start,
                    top: body.top,
                    right: end,
                    bottom: body.bottom,
                }
            };
            (index, pill)
        })
        .collect()
    }
}

impl Widget for WorkspacesWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "workspaces"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        // Device-independent, like every other widget width: the panel scales it.
        // Sized from the workspace count rather than fixed, so the strip does not
        // leave dead space or clip when the count changes.
        self.cfg
            .width
            .unwrap_or_else(|| crate::workspace::count() as i32 * 30 + 8)
            .max(1)
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::SelfDrawn
    }
    fn interval_ms(&self) -> Option<u32> {
        None
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let strip = crate::workspace::summary(ctx.monitor_key, &ctx.windows);
        let radius = ctx.radius();
        for ((index, pill), info) in self.pills(rect, ctx).into_iter().zip(&strip) {
            let hovered = ctx.pointer_in(&pill);
            let background = if info.active {
                ctx.theme.accent_active_color()
            } else if hovered {
                ctx.theme.surface_hover_color()
            } else if info.occupied {
                ctx.theme.surface_color()
            } else {
                // An empty, inactive workspace is drawn as an outline only, so
                // the strip communicates where the windows are at a glance.
                ctx.theme.panel_bg(&ctx.panel_name)
            };
            fill_round_rect(hdc, &pill, radius, background);
            if !info.active && !info.occupied && !hovered {
                // Hairline so an empty workspace is still a visible target.
                let underline = RECT {
                    left: pill.left + radius,
                    top: pill.bottom - ctx.px(2),
                    right: pill.right - radius,
                    bottom: pill.bottom - ctx.px(1),
                };
                fill_rect(hdc, &underline, ctx.theme.border_color());
            }
            let color = if info.active || info.occupied || hovered {
                ctx.theme.text_color()
            } else {
                ctx.theme.text_dim_color()
            };
            let label = format!("{}", info.number);
            // Centred by measuring, so single and double digits both sit right.
            let font = if info.active {
                ctx.strong_font()
            } else {
                ctx.body_font()
            };
            let text_width = measure_label(hdc, &label, font);
            let left = pill.left + ((pill.right - pill.left) - text_width) / 2;
            draw_label(
                hdc,
                &RECT {
                    left: left.max(pill.left),
                    ..pill
                },
                &label,
                font,
                color,
            );
            let _ = index;
        }
    }
    fn on_click(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        for (index, pill) in self.pills(rect, ctx) {
            if point_in_rect(point.0, point.1, &pill) {
                return Some(format!("workspace({})", index + 1));
            }
        }
        None
    }
    fn on_scroll(&self, delta: i32, _point: (i32, i32), _rect: RECT, _ctx: &PanelCtx) -> bool {
        crate::workspace::cycle(if delta > 0 { -1 } else { 1 });
        true
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
    fn on_click(&self, _point: (i32, i32), _rect: RECT, _ctx: &PanelCtx) -> Option<String> {
        // A configured action remains an escape hatch for launch-only widgets;
        // the built-in launcher opens AltDWM's discoverable command surface.
        // Returned rather than opened here, so the panel dispatches it with no
        // lock held — showing a window pumps messages.
        Some(
            self.cfg
                .action
                .clone()
                .or_else(|| self.cfg.command.clone())
                .unwrap_or_else(|| "command_center".into()),
        )
    }
}

/// A label with a leading status mark, which is the shape every system-status
/// widget in the bar shares.
fn draw_status(
    hdc: HDC,
    rect: RECT,
    ctx: &PanelCtx,
    mark: COLORREF,
    primary: &str,
    secondary: Option<&str>,
) {
    let body = inset_rect(content_rect(rect, ctx), ctx.px(token::PAD), 0);
    let mark_width = draw_mark(hdc, &body, ctx, mark);
    let text = RECT {
        left: body.left + mark_width + ctx.px(8),
        ..body
    };
    match secondary {
        Some(secondary) if rect_height(&body) >= ctx.px(34) => {
            let split = text.top + rect_height(&text) * 55 / 100;
            draw_label(
                hdc,
                &RECT {
                    bottom: split,
                    ..text
                },
                primary,
                ctx.strong_font(),
                ctx.theme.text_color(),
            );
            draw_label(
                hdc,
                &RECT { top: split, ..text },
                secondary,
                ctx.small_font(),
                ctx.theme.text_dim_color(),
            );
        }
        _ => draw_label(hdc, &text, primary, ctx.body_font(), ctx.theme.text_color()),
    }
}

/// Speaker level and mute state. Scrolling over it changes the volume; clicking
/// opens quick settings, which is where the slider lives.
pub struct VolumeWidget {
    pub cfg: WidgetConfig,
}

impl Widget for VolumeWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "volume"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(86)
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::Whole
    }
    fn interval_ms(&self) -> Option<u32> {
        Some(self.cfg.interval.unwrap_or(1000))
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let volume = crate::system::status().volume;
        let (mark, primary, secondary) = match volume {
            Some(volume) if volume.muted => (
                ctx.theme.text_dim_color(),
                "Muted".to_string(),
                Some("Volume".to_string()),
            ),
            Some(volume) => (
                ctx.theme.accent_active_color(),
                format!("{}%", volume.percent()),
                Some("Volume".to_string()),
            ),
            None => (
                ctx.theme.text_dim_color(),
                "—".to_string(),
                Some("No output".to_string()),
            ),
        };
        draw_status(hdc, rect, ctx, mark, &primary, secondary.as_deref());
    }
    fn on_click(&self, _point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        open_quick_settings(rect, ctx);
        None
    }
    fn on_scroll(&self, delta: i32, _point: (i32, i32), _rect: RECT, _ctx: &PanelCtx) -> bool {
        crate::system::adjust_volume(delta as f32 * 0.05);
        true
    }
}

/// Charge level and whether the machine is running on mains.
pub struct BatteryWidget {
    pub cfg: WidgetConfig,
}

impl Widget for BatteryWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "battery"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(92)
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::Whole
    }
    fn interval_ms(&self) -> Option<u32> {
        Some(self.cfg.interval.unwrap_or(2000))
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let Some(battery) = crate::system::status().battery else {
            draw_status(hdc, rect, ctx, ctx.theme.text_dim_color(), "Power", None);
            return;
        };
        let percent = battery.percent.unwrap_or(0);
        // Low charge on battery is the one state worth colouring differently.
        let mark = if battery.charging || battery.on_ac {
            ctx.theme.accent_active_color()
        } else if percent <= 15 {
            ctx.theme.color("#e06c5a")
        } else {
            ctx.theme.text_color()
        };
        let primary = battery
            .percent
            .map(|percent| format!("{percent}%"))
            .unwrap_or_else(|| "—".into());
        let secondary = if battery.charging {
            "Charging".to_string()
        } else if battery.on_ac {
            "Plugged in".to_string()
        } else if let Some(minutes) = battery.minutes_remaining {
            format!("{}h {:02}m", minutes / 60, minutes % 60)
        } else {
            "On battery".to_string()
        };
        draw_status(hdc, rect, ctx, mark, &primary, Some(&secondary));
    }
    fn on_click(&self, _point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        open_quick_settings(rect, ctx);
        None
    }
}

/// Connection name and signal.
pub struct NetworkWidget {
    pub cfg: WidgetConfig,
}

impl Widget for NetworkWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "network"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(140)
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::Whole
    }
    fn interval_ms(&self) -> Option<u32> {
        Some(self.cfg.interval.unwrap_or(2000))
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        use crate::system::NetworkStatus;
        let status = crate::system::status().network;
        let (mark, primary, secondary) = match &status {
            NetworkStatus::WiFi { signal, .. } => (
                ctx.theme.accent_active_color(),
                status.label(),
                format!("Wi-Fi · {signal}%"),
            ),
            NetworkStatus::Wired => (
                ctx.theme.accent_active_color(),
                "Network".to_string(),
                "Connected".to_string(),
            ),
            NetworkStatus::Offline => (
                ctx.theme.color("#e06c5a"),
                "Offline".to_string(),
                "No connection".to_string(),
            ),
            NetworkStatus::Unknown => (
                ctx.theme.text_dim_color(),
                "Network".to_string(),
                "Unknown".to_string(),
            ),
        };
        draw_status(hdc, rect, ctx, mark, &primary, Some(&secondary));
    }
    fn on_click(&self, _point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        open_quick_settings(rect, ctx);
        None
    }
}

/// Active keyboard layout. Clicking opens the Windows-style layout chooser;
/// the wheel remains a quick way to cycle for existing configurations.
pub struct InputWidget {
    pub cfg: WidgetConfig,
    /// Last known layout tag. The active layout belongs to the foreground
    /// window's thread, so it has to be polled — but only a change is worth a
    /// repaint.
    tag: Mutex<String>,
}

impl Widget for InputWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "input"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or(62)
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::Whole
    }
    fn interval_ms(&self) -> Option<u32> {
        // The layout changes with the foreground window, so this is polled
        // rather than pushed.
        Some(self.cfg.interval.unwrap_or(500))
    }
    fn tick(&self) -> bool {
        let next = crate::input::current()
            .map(|layout| layout.tag)
            .unwrap_or_else(|| "--".into());
        let mut tag = self.tag.lock().unwrap_or_else(|e| e.into_inner());
        if *tag == next {
            return false;
        }
        *tag = next;
        true
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let tag = self.tag.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let body = inset_rect(content_rect(rect, ctx), ctx.px(token::PAD), 0);
        draw_label(hdc, &body, &tag, ctx.strong_font(), ctx.theme.text_color());
    }
    fn on_click(&self, _point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        open_quick_settings(rect, ctx);
        None
    }
    fn on_scroll(&self, _delta: i32, _point: (i32, i32), _rect: RECT, _ctx: &PanelCtx) -> bool {
        crate::input::cycle();
        true
    }
}

/// One coherent status cluster rather than four unrelated text cards. Every
/// part can be disabled in TOML (`show_volume`, `show_network`, `show_battery`,
/// `show_input`) and the cluster opens the same anchored control surface.
pub struct SystemStatusWidget {
    pub cfg: WidgetConfig,
}

struct StatusCell {
    rect: RECT,
    glyph: char,
    label: Option<String>,
    color: COLORREF,
}

fn volume_glyph(volume: Option<crate::system::VolumeStatus>) -> char {
    match volume {
        Some(volume) if volume.muted || volume.percent() == 0 => '\u{e992}', // Volume0
        Some(volume) if volume.percent() <= 33 => '\u{e993}',                // Volume1
        Some(volume) if volume.percent() <= 66 => '\u{e994}',                // Volume2
        Some(_) => '\u{e995}',                                               // Volume3
        None => '\u{e767}',                                                  // Volume
    }
}

fn network_glyph(network: &crate::system::NetworkStatus) -> char {
    use crate::system::NetworkStatus;
    match network {
        NetworkStatus::WiFi { signal, .. } if *signal <= 33 => '\u{e872}', // Wifi1
        NetworkStatus::WiFi { signal, .. } if *signal <= 66 => '\u{e873}', // Wifi2
        NetworkStatus::WiFi { .. } => '\u{e874}',                          // Wifi3
        NetworkStatus::Wired => '\u{e839}',                                // Ethernet
        NetworkStatus::Offline => '\u{e871}',                              // SignalNotConnected
        NetworkStatus::Unknown => '\u{e968}',                              // Network
    }
}

fn battery_glyph(battery: Option<crate::system::BatteryStatus>) -> char {
    let Some(battery) = battery else {
        return '\u{e7e8}'; // PowerButton: a desktop has AC power and no battery.
    };
    let Some(percent) = battery.percent else {
        return '\u{e996}'; // BatteryUnknown
    };
    if battery.charging || battery.on_ac {
        let level = u32::from(percent.min(100)) * 9 / 100;
        // Charging0..8 are contiguous; Charging9 is the preceding late-added
        // glyph in the same official family.
        return if level == 9 {
            '\u{e83e}'
        } else {
            char::from_u32(0xe85a + level).unwrap_or('\u{e996}')
        };
    }
    let level = u32::from(percent.min(100)) * 10 / 100;
    if level == 10 {
        '\u{e83f}' // Battery10
    } else {
        char::from_u32(0xe850 + level).unwrap_or('\u{e996}')
    }
}

impl SystemStatusWidget {
    fn natural_width(&self) -> i32 {
        let icon_parts = ["show_volume", "show_network", "show_battery"]
            .into_iter()
            .filter(|name| widget_flag(&self.cfg, name, true))
            .count() as i32;
        let input = i32::from(widget_flag(&self.cfg, "show_input", true));
        (icon_parts * 32 + input * 54 + 10).max(46)
    }

    fn cells(&self, rect: RECT, ctx: &PanelCtx) -> Vec<StatusCell> {
        let status = crate::system::status();
        let mut values = Vec::new();
        if widget_flag(&self.cfg, "show_volume", true) {
            let color = match status.volume {
                Some(volume) if volume.muted => ctx.theme.text_dim_color(),
                Some(_) => ctx.theme.text_color(),
                None => ctx.theme.text_dim_color(),
            };
            values.push((volume_glyph(status.volume), None, color, 32));
        }
        if widget_flag(&self.cfg, "show_network", true) {
            use crate::system::NetworkStatus;
            let color = match status.network {
                NetworkStatus::WiFi { .. } | NetworkStatus::Wired => ctx.theme.text_color(),
                NetworkStatus::Offline => ctx.theme.color("#e06c5a"),
                NetworkStatus::Unknown => ctx.theme.text_dim_color(),
            };
            values.push((network_glyph(&status.network), None, color, 32));
        }
        if widget_flag(&self.cfg, "show_battery", true) {
            let color = match status.battery {
                Some(battery)
                    if !battery.on_ac && battery.percent.is_some_and(|value| value <= 15) =>
                {
                    ctx.theme.color("#e06c5a")
                }
                Some(_) => ctx.theme.text_color(),
                None => ctx.theme.text_dim_color(),
            };
            values.push((battery_glyph(status.battery), None, color, 32));
        }
        if widget_flag(&self.cfg, "show_input", true) {
            values.push((
                '\u{f2b7}', // LocaleLanguage — the globe Windows itself uses for
                // the input language, rather than the busy QWERTY keyboard glyph.
                Some(
                    crate::input::current()
                        .map(|layout| layout.tag)
                        .unwrap_or_else(|| "--".into()),
                ),
                ctx.theme.text_color(),
                54,
            ));
        }

        let body = content_rect(rect, ctx);
        let available = if ctx.vertical {
            rect_height(&body)
        } else {
            rect_width(&body)
        };
        let total_weight = values
            .iter()
            .map(|(_, _, _, weight)| *weight)
            .sum::<i32>()
            .max(1);
        let mut used_weight = 0;
        values
            .into_iter()
            .map(|(glyph, label, color, weight)| {
                let start = available * used_weight / total_weight;
                used_weight += weight;
                let end = available * used_weight / total_weight;
                let cell = if ctx.vertical {
                    RECT {
                        top: body.top + start,
                        bottom: body.top + end,
                        ..body
                    }
                } else {
                    RECT {
                        left: body.left + start,
                        right: body.left + end,
                        ..body
                    }
                };
                StatusCell {
                    rect: cell,
                    glyph,
                    label,
                    color,
                }
            })
            .collect()
    }
}

impl Widget for SystemStatusWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "system_status"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.cfg.width.unwrap_or_else(|| self.natural_width())
    }
    fn hover_paint(&self) -> HoverPaint {
        HoverPaint::Whole
    }
    fn interval_ms(&self) -> Option<u32> {
        Some(self.cfg.interval.unwrap_or(500))
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let surface = content_rect(rect, ctx);
        fill_round_rect(hdc, &surface, ctx.radius(), ctx.theme.surface_color());
        for cell in self.cells(rect, ctx) {
            let body = inset_rect(cell.rect, ctx.px(4), 0);
            let glyph = cell.glyph.to_string();
            let icon_font = ctx.symbol_font();
            let icon_width = measure_label(hdc, &glyph, icon_font);
            let gap = if cell.label.is_some() { ctx.px(4) } else { 0 };
            let label_width = cell
                .label
                .as_deref()
                .map(|label| measure_label(hdc, label, ctx.strong_font()))
                .unwrap_or(0);
            let total_width = icon_width + gap + label_width;
            let left = body.left + (rect_width(&body) - total_width).max(0) / 2;
            let stack = cell.label.is_some() && total_width > rect_width(&body);
            let icon_rect = if stack {
                let middle = body.top + rect_height(&body) * 55 / 100;
                RECT {
                    left: body.left + (rect_width(&body) - icon_width).max(0) / 2,
                    bottom: middle,
                    ..body
                }
            } else {
                RECT { left, ..body }
            };
            draw_label(hdc, &icon_rect, &glyph, icon_font, cell.color);
            if let Some(label) = cell.label {
                let text_rect = if stack {
                    RECT {
                        left: body.left + (rect_width(&body) - label_width).max(0) / 2,
                        top: icon_rect.bottom,
                        ..body
                    }
                } else {
                    RECT {
                        left: left + icon_width + gap,
                        ..body
                    }
                };
                draw_label(hdc, &text_rect, &label, ctx.strong_font(), cell.color);
            }
        }
    }
    fn on_click(&self, _point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        open_quick_settings(rect, ctx);
        None
    }
    fn on_scroll(&self, delta: i32, _point: (i32, i32), _rect: RECT, _ctx: &PanelCtx) -> bool {
        if widget_flag(&self.cfg, "show_volume", true) {
            crate::system::adjust_volume(delta as f32 * 0.05);
            true
        } else {
            false
        }
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
    fn tick(&self) -> bool {
        let due = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .evaluated_at
                .is_none_or(|last| last.elapsed() >= self.interval())
        };
        if !due {
            return false;
        }
        // Evaluated without the state lock held: a script may take a while and
        // must not block a concurrent paint from reading the previous value.
        let text = self.evaluate();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let changed = state.text != text;
        state.text = text;
        state.evaluated_at = Some(Instant::now());
        changed
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
        "clock" => Box::new(ClockWidget {
            cfg: cfg.clone(),
            state: Mutex::new((String::new(), String::new())),
        }),
        "spacer" => Box::new(SpacerWidget { cfg: cfg.clone() }),
        "window_title" | "title" => Box::new(WindowTitleWidget { cfg: cfg.clone() }),
        "window_list" | "tasklist" => Box::new(WindowListWidget { cfg: cfg.clone() }),
        "tray" | "systray" => Box::new(TrayWidget { cfg: cfg.clone() }),
        "workspaces" | "workspaces_pills" => Box::new(WorkspacesWidget { cfg: cfg.clone() }),
        "layout" | "layout_status" => Box::new(LayoutWidget { cfg: cfg.clone() }),
        "launcher" | "start" => Box::new(LauncherWidget { cfg: cfg.clone() }),
        "volume" | "audio" => Box::new(VolumeWidget { cfg: cfg.clone() }),
        "battery" | "power" => Box::new(BatteryWidget { cfg: cfg.clone() }),
        "network" | "wifi" => Box::new(NetworkWidget { cfg: cfg.clone() }),
        "input" | "keyboard" | "language" => Box::new(InputWidget {
            cfg: cfg.clone(),
            tag: Mutex::new(String::new()),
        }),
        "system_status" | "status" | "system" => Box::new(SystemStatusWidget { cfg: cfg.clone() }),
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
    use super::{
        battery_glyph, format_time, network_glyph, truncate_chars, volume_glyph, HoverPaint,
        PanelCtx, SystemStatusWidget, TrayWidget, Widget,
    };
    use crate::config::WidgetConfig;
    use crate::system::{BatteryStatus, NetworkStatus, VolumeStatus};
    use crate::theme::Theme;
    use crate::tray::{TrayEntry, TrayId};
    use windows::Win32::Foundation::{HWND, RECT};

    fn tray_entry(uid: u32, hidden: bool) -> TrayEntry {
        TrayEntry {
            id: TrayId::Native { owner: 1, uid },
            name: format!("App {uid}"),
            // Non-zero: an icon-bearing entry is the narrow, square kind.
            icon: 0x100 + uid as isize,
            hidden,
            process: "test.exe".into(),
        }
    }

    fn ctx(width: i32, scale: f32) -> PanelCtx {
        PanelCtx {
            panel_name: "test".into(),
            monitor: "all".into(),
            monitor_key: 0,
            width,
            height: 40,
            hwnd: HWND(std::ptr::null_mut()),
            windows: Vec::new(),
            scale,
            vertical: false,
            pointer: None,
            theme: Theme::default(),
        }
    }

    fn tray_widget() -> TrayWidget {
        let mut extra = std::collections::HashMap::new();
        extra.insert("max_items".into(), toml::Value::Integer(99));
        TrayWidget {
            cfg: WidgetConfig {
                widget_type: "tray".into(),
                name: "tray".into(),
                format: None,
                interval: None,
                script: None,
                action: None,
                command: None,
                label: None,
                icon: None,
                width: None,
                extra,
            },
        }
    }

    fn status_widget() -> SystemStatusWidget {
        SystemStatusWidget {
            cfg: WidgetConfig {
                widget_type: "system_status".into(),
                name: "system".into(),
                format: None,
                interval: None,
                script: None,
                action: None,
                command: None,
                label: None,
                icon: None,
                width: None,
                extra: std::collections::HashMap::new(),
            },
        }
    }

    #[test]
    fn system_status_defaults_to_a_compact_icon_cluster() {
        assert_eq!(status_widget().natural_width(), 160);
    }

    #[test]
    fn status_glyphs_follow_live_levels_and_connection_kind() {
        assert_eq!(
            volume_glyph(Some(VolumeStatus {
                level: 0.0,
                muted: false
            })),
            '\u{e992}'
        );
        assert_eq!(
            volume_glyph(Some(VolumeStatus {
                level: 0.5,
                muted: false
            })),
            '\u{e994}'
        );
        assert_eq!(
            volume_glyph(Some(VolumeStatus {
                level: 1.0,
                muted: false
            })),
            '\u{e995}'
        );
        assert_eq!(
            network_glyph(&NetworkStatus::WiFi {
                ssid: "test".into(),
                signal: 20,
            }),
            '\u{e872}'
        );
        assert_eq!(network_glyph(&NetworkStatus::Wired), '\u{e839}');
        assert_eq!(network_glyph(&NetworkStatus::Offline), '\u{e871}');
    }

    #[test]
    fn battery_icon_carries_level_and_ac_state_by_itself() {
        let unplugged = BatteryStatus {
            percent: Some(50),
            charging: false,
            on_ac: false,
            minutes_remaining: Some(60),
        };
        let charging = BatteryStatus {
            charging: true,
            on_ac: true,
            ..unplugged
        };
        assert_eq!(battery_glyph(Some(unplugged)), '\u{e855}');
        assert_eq!(battery_glyph(Some(charging)), '\u{e85e}');
        assert_eq!(battery_glyph(None), '\u{e7e8}');
    }

    #[test]
    fn tray_defaults_to_three_visible_items() {
        let mut widget = tray_widget();
        widget.cfg.extra.clear();
        assert_eq!(widget.max_items(), 3);
        let entries: Vec<TrayEntry> = (0..6).map(|uid| tray_entry(uid, false)).collect();
        let layout = widget.layout_of(entries, bar(400), &ctx(400, 1.0));
        assert_eq!(layout.items.len(), 3);
        assert_eq!(layout.overflow.len(), 3);
        assert!(layout.chevron.is_some());
    }

    fn bar(width: i32) -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: 40,
        }
    }

    /// The width the widget asks the panel for has to be the width its own
    /// layout then needs. When these disagree the last icon is drawn clipped or
    /// the bar carries a hole.
    #[test]
    fn the_tray_asks_for_exactly_the_room_its_icons_need() {
        let widget = tray_widget();
        for count in 1..=6u32 {
            let entries: Vec<TrayEntry> = (0..count).map(|uid| tray_entry(uid, false)).collect();
            let requested = super::TRAY_ITEM * count as i32
                + super::token::ITEM_GAP * (count as i32 - 1)
                + super::token::ITEM_GAP * 2;
            let ctx = ctx(requested, 1.0);
            let layout = widget.layout_of(entries, bar(requested), &ctx);
            assert_eq!(
                layout.items.len(),
                count as usize,
                "{count} icons should all fit in {requested}px"
            );
            assert!(layout.chevron.is_none(), "nothing should overflow");
            assert!(layout.items.last().unwrap().1.right <= requested);
        }
    }

    /// Items must never overlap, or one would swallow its neighbour's clicks.
    #[test]
    fn tray_items_are_laid_out_in_order_without_overlapping() {
        let widget = tray_widget();
        let entries: Vec<TrayEntry> = (0..5).map(|uid| tray_entry(uid, false)).collect();
        let layout = widget.layout_of(entries, bar(400), &ctx(400, 1.5));
        assert_eq!(layout.items.len(), 5);
        for pair in layout.items.windows(2) {
            assert!(pair[0].1.right <= pair[1].1.left, "items overlap");
        }
        // Order is the order the applications registered in, not whatever the
        // layout pass found convenient.
        let ids: Vec<TrayId> = layout.items.iter().map(|(entry, _)| entry.id).collect();
        let expected: Vec<TrayId> = (0..5).map(|uid| TrayId::Native { owner: 1, uid }).collect();
        assert_eq!(ids, expected);
    }

    /// A bar too narrow for every icon grows a chevron, and the chevron has to
    /// fit inside the widget rather than on top of the last icon it hides.
    #[test]
    fn icons_that_do_not_fit_move_behind_a_chevron() {
        let widget = tray_widget();
        let entries: Vec<TrayEntry> = (0..8).map(|uid| tray_entry(uid, false)).collect();
        let narrow = 120;
        let layout = widget.layout_of(entries, bar(narrow), &ctx(narrow, 1.0));
        let chevron = layout.chevron.expect("a clipped tray needs a chevron");
        assert!(!layout.overflow.is_empty());
        assert_eq!(layout.items.len() + layout.overflow.len(), 8);
        assert!(chevron.right <= narrow);
        for (_, item) in &layout.items {
            assert!(
                item.right <= chevron.left,
                "an icon was drawn under the chevron"
            );
        }
    }

    /// `NIS_HIDDEN` entries belong in the overflow however much room there is,
    /// and they must not consume any of the bar's width.
    #[test]
    fn icons_that_asked_to_be_hidden_never_reach_the_bar() {
        let widget = tray_widget();
        let entries = vec![
            tray_entry(0, false),
            tray_entry(1, true),
            tray_entry(2, true),
            tray_entry(3, false),
        ];
        let layout = widget.layout_of(entries, bar(600), &ctx(600, 1.0));
        assert_eq!(layout.items.len(), 2);
        assert_eq!(layout.overflow.len(), 2);
        assert!(layout.overflow.iter().all(|entry| entry.hidden));
        assert!(layout.chevron.is_some());
    }

    #[test]
    fn an_empty_tray_reserves_a_readable_slot_rather_than_flexing() {
        let widget = tray_widget();
        // 0 would mean "flex" to the panel, which would hand the tray most of
        // the bar to draw nothing in.
        assert!(widget.width(&ctx(400, 1.0)) > 0);
        assert!(widget.hover_paint() == HoverPaint::SelfDrawn);
        let layout = widget.layout_of(Vec::new(), bar(400), &ctx(400, 1.0));
        assert!(layout.items.is_empty() && layout.chevron.is_none());
    }

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

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, DeferWindowPos, EndDeferWindowPos, EnumWindows, GetCursorPos, GetWindow,
    GetWindowLongPtrW, GetWindowRect, IsWindow, IsZoomed, SendMessageTimeoutW, SetWindowPos,
    ShowWindow, GWL_EXSTYLE, GWL_STYLE, GW_OWNER, HWND_TOP, MINMAXINFO, SMTO_ABORTIFHUNG,
    SMTO_BLOCK, SWP_NOACTIVATE, SWP_NOZORDER, SW_RESTORE, WM_GETMINMAXINFO, WS_EX_DLGMODALFRAME,
    WS_THICKFRAME,
};

use crate::layout::{compute_layout, Layout};
use crate::util::is_manageable_or_minimized;

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

// EnumWindows enumerates in Z-order. Focusing a window changes that order, so
// feeding it directly to a layout makes every focus change reshuffle the tiles.
// Keep the discovery order of each live HWND and only append newly managed
// windows. Temporarily hidden/minimized windows retain their former slot.
static WINDOW_ORDER: LazyLock<Mutex<Vec<isize>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static EXPECTED_RECTS: LazyLock<Mutex<HashMap<isize, RECT>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static AUTO_FLOATING: LazyLock<Mutex<HashSet<isize>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WindowConstraints {
    min_width: i32,
    min_height: i32,
    max_width: i32,
    max_height: i32,
}

pub fn is_auto_floating(hwnd: HWND) -> bool {
    AUTO_FLOATING
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(&(hwnd.0 as isize))
}

fn query_window_constraints(hwnd: HWND) -> WindowConstraints {
    let mut info = MINMAXINFO::default();
    unsafe {
        let _ = SendMessageTimeoutW(
            hwnd,
            WM_GETMINMAXINFO,
            WPARAM(0),
            LPARAM(&mut info as *mut MINMAXINFO as isize),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            40,
            None,
        );
    }
    WindowConstraints {
        min_width: info.ptMinTrackSize.x.max(0),
        min_height: info.ptMinTrackSize.y.max(0),
        max_width: info.ptMaxTrackSize.x.max(0),
        max_height: info.ptMaxTrackSize.y.max(0),
    }
}

fn rect_violates_constraints(rect: RECT, constraints: WindowConstraints) -> bool {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    (constraints.min_width > 0 && width < constraints.min_width)
        || (constraints.min_height > 0 && height < constraints.min_height)
}

fn is_automatic_utility_window(hwnd: HWND, constraints: WindowConstraints) -> bool {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.0.is_null());
        let modal_frame = (ex_style & WS_EX_DLGMODALFRAME.0) != 0;
        let resizable = (style & WS_THICKFRAME.0) != 0;
        let fixed_by_limits = constraints.min_width > 0
            && constraints.min_height > 0
            && constraints.max_width > 0
            && constraints.max_height > 0
            && constraints.min_width >= constraints.max_width.saturating_sub(2)
            && constraints.min_height >= constraints.max_height.saturating_sub(2);
        let mut rect = RECT::default();
        let compact_non_resizable = GetWindowRect(hwnd, &mut rect).is_ok()
            && !resizable
            && rect.right - rect.left <= 900
            && rect.bottom - rect.top <= 800;
        owned || modal_frame || fixed_by_limits || compact_non_resizable
    }
}

fn contained_floating_rect(current: RECT, area: RECT, constraints: WindowConstraints) -> RECT {
    let area_width = (area.right - area.left).max(1);
    let area_height = (area.bottom - area.top).max(1);
    let width = (current.right - current.left)
        .max(constraints.min_width)
        .clamp(1, area_width);
    let height = (current.bottom - current.top)
        .max(constraints.min_height)
        .clamp(1, area_height);
    let left = current.left.clamp(area.left, area.right - width);
    let top = current.top.clamp(area.top, area.bottom - height);
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

#[derive(Clone)]
struct MoveState {
    hwnd: isize,
    start_rect: RECT,
    slots: Vec<(isize, RECT)>,
}

static MOVE_STATE: LazyLock<Mutex<Option<MoveState>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Clone)]
struct LayoutOverride {
    members: Vec<isize>,
    base_bounds: RECT,
    rects: HashMap<isize, RECT>,
}

static LAYOUT_OVERRIDES: LazyLock<Mutex<HashMap<isize, LayoutOverride>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn clear_layout_overrides() {
    LAYOUT_OVERRIDES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

pub fn is_tracked_window(hwnd: HWND) -> bool {
    WINDOW_ORDER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(&(hwnd.0 as isize))
}

fn same_rect(left: &RECT, right: &RECT) -> bool {
    left.left == right.left
        && left.top == right.top
        && left.right == right.right
        && left.bottom == right.bottom
}

/// True when a location event merely reports the rectangle AltDWM most
/// recently assigned. This prevents our own DeferWindowPos pass from scheduling
/// another pass forever while still allowing user moves and maximize requests
/// to be detected.
pub fn is_expected_location(hwnd: HWND) -> bool {
    let expected = EXPECTED_RECTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&(hwnd.0 as isize))
        .copied();
    let Some(expected) = expected else {
        return false;
    };
    let mut actual = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut actual).is_ok() && same_rect(&actual, &expected) }
}

pub fn is_move_active(hwnd: HWND) -> bool {
    MOVE_STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|state| state.hwnd == hwnd.0 as isize)
}

/// Capture the current tiled rectangles at the beginning of an interactive
/// move. The pre-drag rectangles are the drop slots; the dragged window can
/// cover another window without hiding the intended target from us.
pub fn begin_interactive_move(hwnd: HWND) {
    *MOVE_STATE.lock().unwrap_or_else(|error| error.into_inner()) = None;
    if !is_tracked_window(hwnd) {
        return;
    }
    let mut start_rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut start_rect) }.is_err() {
        return;
    }
    let order = WINDOW_ORDER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let expected = EXPECTED_RECTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let slots = order
        .into_iter()
        .filter_map(|key| expected.get(&key).copied().map(|rect| (key, rect)))
        .collect();
    *MOVE_STATE.lock().unwrap_or_else(|error| error.into_inner()) = Some(MoveState {
        hwnd: hwnd.0 as isize,
        start_rect,
        slots,
    });
}

fn point_in_rect(point: POINT, rect: &RECT) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

fn squared_distance_to_center(point: POINT, rect: &RECT) -> i64 {
    let dx = i64::from(point.x) - i64::from(rect.left + (rect.right - rect.left) / 2);
    let dy = i64::from(point.y) - i64::from(rect.top + (rect.bottom - rect.top) / 2);
    dx * dx + dy * dy
}

fn rect_width(rect: &RECT) -> i32 {
    rect.right - rect.left
}

fn rect_height(rect: &RECT) -> i32 {
    rect.bottom - rect.top
}

fn rect_union(rects: impl Iterator<Item = RECT>) -> Option<RECT> {
    rects.reduce(|bounds, rect| RECT {
        left: bounds.left.min(rect.left),
        top: bounds.top.min(rect.top),
        right: bounds.right.max(rect.right),
        bottom: bounds.bottom.max(rect.bottom),
    })
}

fn ranges_overlap(start_a: i32, end_a: i32, start_b: i32, end_b: i32) -> bool {
    start_a < end_b && start_b < end_a
}

/// Apply the changed edges of one tile to the neighbors that shared those
/// boundaries. Outer layout edges stay anchored so a resize cannot intrude into
/// panels or outside the monitor's managed area.
fn adjust_rects_for_resize(
    slots: &[(isize, RECT)],
    dragged: isize,
    start: RECT,
    mut final_rect: RECT,
) -> Option<(RECT, HashMap<isize, RECT>)> {
    const MIN_TILE: i32 = 80;
    const MAX_SHARED_GAP: i32 = 64;
    let bounds = rect_union(slots.iter().map(|(_, rect)| *rect))?;

    if start.left == bounds.left {
        final_rect.left = bounds.left;
    }
    if start.top == bounds.top {
        final_rect.top = bounds.top;
    }
    if start.right == bounds.right {
        final_rect.right = bounds.right;
    }
    if start.bottom == bounds.bottom {
        final_rect.bottom = bounds.bottom;
    }
    final_rect.left = final_rect
        .left
        .clamp(bounds.left, final_rect.right - MIN_TILE);
    final_rect.top = final_rect
        .top
        .clamp(bounds.top, final_rect.bottom - MIN_TILE);
    final_rect.right = final_rect
        .right
        .clamp(final_rect.left + MIN_TILE, bounds.right);
    final_rect.bottom = final_rect
        .bottom
        .clamp(final_rect.top + MIN_TILE, bounds.bottom);

    let mut adjusted: HashMap<isize, RECT> = slots.iter().copied().collect();
    adjusted.insert(dragged, final_rect);

    for (key, original) in slots.iter().copied().filter(|(key, _)| *key != dragged) {
        let mut rect = original;
        let vertical_overlap = ranges_overlap(start.top, start.bottom, rect.top, rect.bottom);
        let horizontal_overlap = ranges_overlap(start.left, start.right, rect.left, rect.right);

        let right_gap = rect.left - start.right;
        if final_rect.right != start.right
            && vertical_overlap
            && (0..=MAX_SHARED_GAP).contains(&right_gap)
        {
            rect.left = (final_rect.right + right_gap).min(rect.right - MIN_TILE);
        }
        let left_gap = start.left - rect.right;
        if final_rect.left != start.left
            && vertical_overlap
            && (0..=MAX_SHARED_GAP).contains(&left_gap)
        {
            rect.right = (final_rect.left - left_gap).max(rect.left + MIN_TILE);
        }
        let bottom_gap = rect.top - start.bottom;
        if final_rect.bottom != start.bottom
            && horizontal_overlap
            && (0..=MAX_SHARED_GAP).contains(&bottom_gap)
        {
            rect.top = (final_rect.bottom + bottom_gap).min(rect.bottom - MIN_TILE);
        }
        let top_gap = start.top - rect.bottom;
        if final_rect.top != start.top
            && horizontal_overlap
            && (0..=MAX_SHARED_GAP).contains(&top_gap)
        {
            rect.bottom = (final_rect.top - top_gap).max(rect.top + MIN_TILE);
        }
        adjusted.insert(key, rect);
    }

    Some((bounds, adjusted))
}

fn remember_interactive_resize(state: &MoveState, final_rect: RECT) -> bool {
    let start_width = rect_width(&state.start_rect);
    let start_height = rect_height(&state.start_rect);
    let resized = (rect_width(&final_rect) - start_width).abs() >= 8
        || (rect_height(&final_rect) - start_height).abs() >= 8;
    if !resized {
        return false;
    }

    let start_center = POINT {
        x: state.start_rect.left + start_width / 2,
        y: state.start_rect.top + start_height / 2,
    };
    let monitor = unsafe { MonitorFromPoint(start_center, MONITOR_DEFAULTTONEAREST) };
    let monitor_key = monitor.0 as isize;
    let monitor_slots: Vec<(isize, RECT)> = state
        .slots
        .iter()
        .copied()
        .filter(|(_, rect)| unsafe {
            let center = POINT {
                x: rect.left + rect_width(rect) / 2,
                y: rect.top + rect_height(rect) / 2,
            };
            MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST) == monitor
        })
        .collect();
    let Some((base_bounds, rects)) =
        adjust_rects_for_resize(&monitor_slots, state.hwnd, state.start_rect, final_rect)
    else {
        return false;
    };
    let members = monitor_slots.iter().map(|(key, _)| *key).collect();
    LAYOUT_OVERRIDES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(
            monitor_key,
            LayoutOverride {
                members,
                base_bounds,
                rects,
            },
        );
    println!("[manager] retained interactive resize for {:?}", state.hwnd);
    true
}

fn apply_layout_override(monitor: isize, windows: &[HWND], computed: Vec<RECT>) -> Vec<RECT> {
    let members: Vec<isize> = windows.iter().map(|hwnd| hwnd.0 as isize).collect();
    let computed_bounds = rect_union(computed.iter().copied());
    let mut overrides = LAYOUT_OVERRIDES
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let valid = overrides.get(&monitor).is_some_and(|saved| {
        saved.members == members
            && computed_bounds.is_some_and(|bounds| same_rect(&bounds, &saved.base_bounds))
    });
    if !valid {
        overrides.remove(&monitor);
        return computed;
    }
    let saved = &overrides[&monitor];
    members
        .iter()
        .zip(computed)
        .map(|(key, fallback)| saved.rects.get(key).copied().unwrap_or(fallback))
        .collect()
}

fn swap_window_order(order: &mut [isize], dragged: isize, target: isize) -> bool {
    let Some(from) = order.iter().position(|key| *key == dragged) else {
        return false;
    };
    let Some(to) = order.iter().position(|key| *key == target) else {
        return false;
    };
    if from == to {
        return false;
    }
    order.swap(from, to);
    true
}

/// Finish an interactive move and swap the dragged window with the tiled slot
/// under the pointer. Returns true when the stable layout order changed.
pub fn finish_interactive_move(hwnd: HWND) -> bool {
    let state = MOVE_STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    let Some(state) = state.filter(|state| state.hwnd == hwnd.0 as isize) else {
        return false;
    };

    let mut final_rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut final_rect) }.is_err() {
        return false;
    }
    if remember_interactive_resize(&state, final_rect) {
        return true;
    }
    let moved = (final_rect.left - state.start_rect.left).abs()
        + (final_rect.top - state.start_rect.top).abs();
    if moved < 16 {
        return false;
    }

    let mut pointer = POINT::default();
    if unsafe { GetCursorPos(&mut pointer) }.is_err() {
        pointer.x = final_rect.left + (final_rect.right - final_rect.left) / 2;
        pointer.y = final_rect.top + (final_rect.bottom - final_rect.top) / 2;
    }

    let candidates: Vec<(isize, RECT)> = state
        .slots
        .into_iter()
        .filter(|(key, _)| *key != state.hwnd)
        .filter(|(key, _)| unsafe { IsWindow(Some(HWND(*key as *mut std::ffi::c_void))).as_bool() })
        .filter(|(_, rect)| unsafe {
            let center = POINT {
                x: rect.left + (rect.right - rect.left) / 2,
                y: rect.top + (rect.bottom - rect.top) / 2,
            };
            MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST)
                == MonitorFromPoint(pointer, MONITOR_DEFAULTTONEAREST)
        })
        .collect();
    let target = candidates
        .iter()
        .find(|(_, rect)| point_in_rect(pointer, rect))
        .or_else(|| {
            if point_in_rect(pointer, &state.start_rect) {
                None
            } else {
                candidates
                    .iter()
                    .min_by_key(|(_, rect)| squared_distance_to_center(pointer, rect))
            }
        })
        .map(|(key, _)| *key);
    let Some(target) = target else {
        return false;
    };

    let changed = swap_window_order(
        &mut WINDOW_ORDER
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        state.hwnd,
        target,
    );
    if changed {
        clear_layout_overrides();
        println!(
            "[manager] reordered dragged window {:?} with {:?}",
            hwnd.0, target
        );
    }
    changed
}

fn reconcile_window_order<F>(order: &mut Vec<isize>, eligible: &[isize], mut is_alive: F)
where
    F: FnMut(isize) -> bool,
{
    order.retain(|key| is_alive(*key));
    let mut known: HashSet<isize> = order.iter().copied().collect();
    for key in eligible {
        if known.insert(*key) {
            order.push(*key);
        }
    }
}

/// Collect all manageable windows, grouped by monitor
pub fn collect_windows(taskbar_hwnd: Option<HWND>) -> Vec<HWND> {
    collect_windows_including_minimized(taskbar_hwnd)
        .into_iter()
        .filter(|hwnd| unsafe {
            !windows::Win32::UI::WindowsAndMessaging::IsIconic(*hwnd).as_bool()
        })
        .collect()
}

/// Collect all manageable application windows in stable tiling order, including
/// iconic windows so panels can act as a complete task list. Minimized windows
/// are excluded only when the actual layout is computed.
pub fn collect_windows_including_minimized(taskbar_hwnd: Option<HWND>) -> Vec<HWND> {
    let mut windows: Vec<HWND> = Vec::new();
    unsafe {
        let ptr = &mut windows as *mut Vec<HWND> as isize;
        let _ = EnumWindows(Some(enum_cb), LPARAM(ptr));
    }
    let eligible: Vec<HWND> = windows
        .into_iter()
        .filter(|hwnd| is_manageable_or_minimized(*hwnd, taskbar_hwnd))
        .collect();
    let eligible_keys: Vec<isize> = eligible.iter().map(|hwnd| hwnd.0 as isize).collect();
    let eligible_set: HashSet<isize> = eligible_keys.iter().copied().collect();
    let mut order = WINDOW_ORDER.lock().unwrap_or_else(|e| e.into_inner());
    reconcile_window_order(&mut order, &eligible_keys, |key| unsafe {
        let hwnd = HWND(key as *mut std::ffi::c_void);
        IsWindow(Some(hwnd)).as_bool()
    });
    EXPECTED_RECTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .retain(|key, _| order.contains(key));
    order
        .iter()
        .filter(|key| eligible_set.contains(key))
        .map(|key| HWND(*key as *mut std::ffi::c_void))
        .collect()
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let vec = &mut *(lparam.0 as *mut Vec<HWND>);
    vec.push(hwnd);
    BOOL(1)
}

fn is_primary_monitor(mi: &MONITORINFO) -> bool {
    // MONITORINFOF_PRIMARY = 1
    (mi.dwFlags & 1) != 0
}

pub fn get_all_monitors() -> Vec<HMONITOR> {
    let mut mons: Vec<HMONITOR> = Vec::new();
    unsafe extern "system" fn enum_cb(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let v = &mut *(lparam.0 as *mut Vec<HMONITOR>);
        v.push(hmon);
        BOOL(1)
    }
    unsafe {
        let ptr = &mut mons as *mut Vec<HMONITOR> as isize;
        let _ = EnumDisplayMonitors(None, None, Some(enum_cb), LPARAM(ptr));
    }
    mons
}

pub fn hmonitor_for_target(target: &str) -> Option<HMONITOR> {
    let mons = get_all_monitors();
    if mons.is_empty() {
        return None;
    }
    let lower = target.to_lowercase();
    if lower == "primary" || lower == "1" {
        // primary is at (0,0), find it, else first
        for &h in &mons {
            unsafe {
                let mut mi = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(h, &mut mi as *mut _ as *mut _).as_bool()
                    && is_primary_monitor(&mi)
                {
                    return Some(h);
                }
            }
        }
        return Some(mons[0]);
    }
    if let Ok(idx) = lower.parse::<usize>() {
        if idx >= 1 && idx <= mons.len() {
            return Some(mons[idx - 1]);
        }
    }
    if lower == "all" {
        return None;
    } // don't override
      // try substring match on device name via MONITORINFOEXW
    for &h in &mons {
        unsafe {
            let mut ex = MONITORINFOEXW {
                monitorInfo: MONITORINFO {
                    cbSize: size_of::<MONITORINFOEXW>() as u32,
                    ..Default::default()
                },
                szDevice: [0; 32],
            };
            if GetMonitorInfoW(h, &mut ex as *mut _ as *mut _ as *mut MONITORINFO).as_bool() {
                let dev = String::from_utf16_lossy(&ex.szDevice)
                    .trim_matches(char::from(0))
                    .to_string();
                if dev.to_lowercase().contains(&lower) {
                    return Some(h);
                }
            }
        }
    }
    None
}

fn panel_reserves_for_monitor(hmon: HMONITOR, cfg: &crate::config::Config) -> (i32, i32, i32, i32) {
    let mut left = 0;
    let mut top = 0;
    let mut right = 0;
    let mut bottom = 0;
    for panel in &cfg.panels {
        let applies = panel.monitor.eq_ignore_ascii_case("all")
            || hmonitor_for_target(&panel.monitor).is_some_and(|target| target == hmon);
        if !applies {
            continue;
        }
        match panel.position.as_str() {
            "top" => top += panel.edge_consumption(),
            "right" => right += panel.edge_consumption(),
            "bottom" => bottom += panel.edge_consumption(),
            "left" => left += panel.edge_consumption(),
            _ => {}
        }
    }
    (left, top, right, bottom)
}

fn apply_window_chrome(hwnd: HWND, cfg: &crate::config::Config, focused: bool) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE,
    };
    const DWMWCP_ROUND: u32 = 2;
    unsafe {
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as _,
            size_of_val(&corner) as u32,
        );
        let border = if focused {
            cfg.theme.active_window_border_color()
        } else {
            cfg.theme.inactive_window_border_color()
        };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border.0 as *const _ as _,
            size_of_val(&border) as u32,
        );
    }
}

pub fn refresh_window_borders() {
    let cfg = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let foreground = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    for hwnd in collect_windows_including_minimized(crate::taskbar::get_taskbar_hwnd()) {
        apply_window_chrome(hwnd, &cfg, hwnd == foreground);
    }
}

fn get_work_area_for_hmonitor(
    hmon: HMONITOR,
    top_reserve: i32,
    bottom_reserve: i32,
    taskbar_hwnd: Option<HWND>,
    cfg: &crate::config::Config,
) -> RECT {
    unsafe {
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut mi as *mut _ as *mut _).as_bool() {
            return RECT {
                left: 0,
                top: top_reserve,
                right: 1920,
                bottom: 1080 - bottom_reserve,
            };
        }
        // Explorer's AppBar reservation remains in rcWork after its taskbar is
        // hidden. When AltDWM owns shell chrome, start from the full monitor and
        // reserve only our configured panels/taskbar below.
        let mut work = if cfg.general.hide_native_taskbar {
            mi.rcMonitor
        } else {
            mi.rcWork
        };
        let is_primary = is_primary_monitor(&mi);
        let (panel_left, panel_top, panel_right, panel_bottom) = if cfg.panels.is_empty() {
            (0, top_reserve, 0, bottom_reserve)
        } else {
            panel_reserves_for_monitor(hmon, cfg)
        };
        if panel_left > 0 || panel_top > 0 || panel_right > 0 || panel_bottom > 0 {
            let apply = if taskbar_hwnd.is_some() {
                if let Some(tb) = taskbar_hwnd {
                    MonitorFromWindow(tb, MONITOR_DEFAULTTONEAREST) == hmon
                } else {
                    false
                }
            } else {
                !cfg.panels.is_empty() || is_primary
            };
            if apply {
                work.left += panel_left;
                work.top += panel_top;
                work.right -= panel_right;
                work.bottom -= panel_bottom;
            }
        }
        let outer_gap = cfg.general.outer_gap.unwrap_or(0);
        work.left += outer_gap;
        work.top += outer_gap;
        work.right -= outer_gap;
        work.bottom -= outer_gap;
        if work.bottom <= work.top {
            work.bottom = work.top + 100;
        }
        if work.right <= work.left {
            work.right = work.left + 100;
        }
        work
    }
}

fn contain_floating_windows(
    windows: &[HWND],
    top_reserve: i32,
    bottom_reserve: i32,
    taskbar_hwnd: Option<HWND>,
    cfg: &crate::config::Config,
    constraints: &HashMap<isize, WindowConstraints>,
) {
    let foreground = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    for hwnd in windows {
        let monitor = if let Some(target) = crate::rules::rule_monitor(*hwnd) {
            hmonitor_for_target(&target)
                .unwrap_or_else(|| unsafe { MonitorFromWindow(*hwnd, MONITOR_DEFAULTTONEAREST) })
        } else {
            unsafe { MonitorFromWindow(*hwnd, MONITOR_DEFAULTTONEAREST) }
        };
        let area =
            get_work_area_for_hmonitor(monitor, top_reserve, bottom_reserve, taskbar_hwnd, cfg);
        let mut current = RECT::default();
        if unsafe { GetWindowRect(*hwnd, &mut current) }.is_err() {
            continue;
        }
        let target = contained_floating_rect(
            current,
            area,
            constraints
                .get(&(hwnd.0 as isize))
                .copied()
                .unwrap_or_default(),
        );
        EXPECTED_RECTS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(hwnd.0 as isize));
        apply_window_chrome(*hwnd, cfg, *hwnd == foreground);
        if target != current {
            unsafe {
                let _ = SetWindowPos(
                    *hwnd,
                    Some(HWND_TOP),
                    target.left,
                    target.top,
                    target.right - target.left,
                    target.bottom - target.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }
}

/// Tile with explicit top/bottom reserves (for panels DSL)
pub fn tile_windows_reserved(
    taskbar_hwnd: Option<HWND>,
    top_reserve: i32,
    bottom_reserve: i32,
    layout: Layout,
    gap: i32,
) {
    // snapshot config once per tick to avoid repeated locking
    let cfg_snapshot = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let verbose = std::env::var_os("ALT_DWM_VERBOSE").is_some();
    let all_windows = collect_windows(taskbar_hwnd);
    if all_windows.is_empty() {
        return;
    }

    // virtual desktop filter (if enabled)
    let before_vd = all_windows.len();
    let all_windows: Vec<HWND> = if cfg_snapshot.general.filter_virtual_desktop {
        all_windows
            .into_iter()
            .filter(|hwnd| crate::virtual_desktop::is_on_current_desktop(*hwnd))
            .collect()
    } else {
        all_windows
    };
    if verbose && all_windows.len() != before_vd {
        println!(
            "[manager] filtered {} windows not on current virtual desktop",
            before_vd - all_windows.len()
        );
    }
    if all_windows.is_empty() {
        return;
    }

    // Explicit rules decide first. Manual runtime floating is authoritative;
    // automatic utility classification is only used when no rule opted the
    // window in or out.
    let constraints: HashMap<isize, WindowConstraints> = all_windows
        .iter()
        .map(|hwnd| (hwnd.0 as isize, query_window_constraints(*hwnd)))
        .collect();
    let mut windows = Vec::new();
    let mut floating = Vec::new();
    let mut auto_floating_keys = HashSet::new();
    for hwnd in all_windows {
        let key = hwnd.0 as isize;
        let manual = crate::focus::is_runtime_floating(hwnd);
        let decision = crate::rules::floating_decision(hwnd);
        let automatic = decision.is_none()
            && cfg_snapshot.general.auto_float_utility_windows
            && is_automatic_utility_window(
                hwnd,
                constraints.get(&key).copied().unwrap_or_default(),
            );
        if manual || decision == Some(true) || automatic {
            if automatic {
                auto_floating_keys.insert(key);
            }
            floating.push(hwnd);
        } else {
            windows.push(hwnd);
        }
    }
    if verbose && !floating.is_empty() {
        println!("[manager] floating {} window(s) by policy:", floating.len());
        for hwnd in &floating {
            let cls = crate::util::get_class_name(*hwnd);
            let title = crate::util::get_window_title(*hwnd);
            println!(
                "  ~ {:?} class={} title=\"{}\" (floating)",
                hwnd.0, cls, title
            );
        }
    }
    // Apply opacity to every managed window before any layout-specific early return.
    for hwnd in windows.iter().chain(floating.iter()) {
        if let Some(opacity) = crate::rules::rule_opacity(*hwnd) {
            if verbose {
                println!("[manager] opacity {} for {:?}", opacity, hwnd.0);
            }
            crate::rules::apply_opacity(*hwnd, opacity);
        }
    }
    if windows.is_empty() {
        *AUTO_FLOATING
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = auto_floating_keys;
        contain_floating_windows(
            &floating,
            top_reserve,
            bottom_reserve,
            taskbar_hwnd,
            &cfg_snapshot,
            &constraints,
        );
        if verbose {
            println!("[manager] no tilable windows (all floating)");
        }
        return;
    }

    let layout_name = cfg_snapshot.general.layout.clone();
    if verbose {
        println!(
            "[manager] tiling {} windows with layout {} gap={} ({} floating skipped)",
            windows.len(),
            layout_name,
            gap,
            floating.len()
        );
    }
    if verbose {
        for hwnd in &windows {
            let cls = crate::util::get_class_name(*hwnd);
            let title = crate::util::get_window_title(*hwnd);
            println!("  - {:?} class={} title=\"{}\"", hwnd.0, cls, title);
        }
    }

    let mut per_monitor: HashMap<isize, Vec<HWND>> = HashMap::new();
    let mut monitor_rects: HashMap<isize, RECT> = HashMap::new();

    for hwnd in windows {
        // check rule for monitor override
        let target_hmon = if let Some(mon_str) = crate::rules::rule_monitor(hwnd) {
            if let Some(h) = hmonitor_for_target(&mon_str) {
                if verbose {
                    println!(
                        "[manager] {:?} rule monitor='{}' -> hmon 0x{:x}",
                        hwnd.0, mon_str, h.0 as usize
                    );
                }
                h
            } else {
                eprintln!(
                    "[manager] rule monitor '{}' not found for {:?}",
                    mon_str, hwnd.0
                );
                unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }
            }
        } else {
            unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }
        };
        let key = target_hmon.0 as isize;
        per_monitor.entry(key).or_default().push(hwnd);
        monitor_rects.entry(key).or_insert_with(|| {
            get_work_area_for_hmonitor(
                target_hmon,
                top_reserve,
                bottom_reserve,
                taskbar_hwnd,
                &cfg_snapshot,
            )
        });
    }

    let total: usize = per_monitor.values().map(Vec::len).sum();
    let hdwp = unsafe {
        match BeginDeferWindowPos(total as i32) {
            Ok(h) => h,
            Err(e) => {
                println!("[manager] BeginDeferWindowPos failed: {:?}", e);
                return;
            }
        }
    };
    let mut hdwp = hdwp;

    for (mon, wins) in per_monitor {
        let area = monitor_rects.get(&mon).copied().unwrap_or(RECT {
            left: 0,
            top: top_reserve,
            right: 1920,
            bottom: 1080 - bottom_reserve,
        });
        if verbose {
            println!(
                "[manager] monitor 0x{:x} area {:?}",
                mon,
                crate::util::rect_to_string(&area)
            );
        }
        let mut monitor_cfg = cfg_snapshot.clone();
        let mut monitor_layout = layout;
        if let Some(rule_layout) = crate::rules::layout_for_windows(&wins) {
            monitor_cfg.general.layout = rule_layout;
            monitor_layout = monitor_cfg.layout_enum();
        }
        let mut tiled_wins = wins;
        let rects = loop {
            // try custom layout first (if general.layout names a key in layouts with script)
            let computed_rects = if let Some(custom) =
                crate::layout::try_compute_custom(tiled_wins.len(), area, gap, &monitor_cfg)
            {
                if verbose {
                    println!("[manager] custom layout '{}'", monitor_cfg.general.layout);
                }
                custom
            } else {
                compute_layout(tiled_wins.len(), area, gap, monitor_layout)
            };
            let rects = apply_layout_override(mon, &tiled_wins, computed_rects);
            if !cfg_snapshot.general.respect_window_size_constraints {
                break rects;
            }
            let violations = tiled_wins
                .iter()
                .zip(rects.iter())
                .enumerate()
                .filter_map(|(index, (hwnd, rect))| {
                    let limits = constraints
                        .get(&(hwnd.0 as isize))
                        .copied()
                        .unwrap_or_default();
                    rect_violates_constraints(*rect, limits).then_some(index)
                })
                .collect::<Vec<_>>();
            if violations.is_empty() {
                break rects;
            }
            for index in violations.into_iter().rev() {
                let hwnd = tiled_wins.remove(index);
                auto_floating_keys.insert(hwnd.0 as isize);
                floating.push(hwnd);
                if verbose {
                    let limits = constraints
                        .get(&(hwnd.0 as isize))
                        .copied()
                        .unwrap_or_default();
                    println!(
                        "[manager] auto-float {:?}: minimum {}x{} does not fit assigned tile",
                        hwnd.0, limits.min_width, limits.min_height
                    );
                }
            }
            if tiled_wins.is_empty() {
                break Vec::new();
            }
        };
        for (i, hwnd) in tiled_wins.iter().enumerate() {
            let r = if i < rects.len() {
                rects[i]
            } else {
                continue;
            };
            let w = r.right - r.left;
            let h = r.bottom - r.top;
            if w <= 0 || h <= 0 {
                continue;
            }
            let foreground =
                unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            apply_window_chrome(*hwnd, &cfg_snapshot, *hwnd == foreground);
            EXPECTED_RECTS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(hwnd.0 as isize, r);
            // SetWindowPos changes only the restore bounds of a maximized window;
            // the visible maximized frame remains over our panels. Restore first
            // so the tiled rectangle becomes the actual frame and app-owned title
            // bars/toolbars stay below the reserved panel area.
            unsafe {
                if IsZoomed(*hwnd).as_bool() {
                    let _ = ShowWindow(*hwnd, SW_RESTORE);
                }
            }
            if verbose {
                println!(
                    "[manager] -> {:?} => {}x{} @ {},{}",
                    hwnd.0, w, h, r.left, r.top
                );
            }
            unsafe {
                match DeferWindowPos(
                    hdwp,
                    *hwnd,
                    Some(HWND_TOP),
                    r.left,
                    r.top,
                    w,
                    h,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                ) {
                    Ok(h) => hdwp = h,
                    Err(e) => println!("[manager] DeferWindowPos failed for {:?}: {:?}", hwnd.0, e),
                }
            }
        }
    }

    unsafe {
        match EndDeferWindowPos(hdwp) {
            Ok(_) => {
                if verbose {
                    println!("[manager] tiling committed");
                }
            }
            Err(e) => println!("[manager] EndDeferWindowPos failed: {:?}", e),
        }
    }
    *AUTO_FLOATING
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = auto_floating_keys;
    contain_floating_windows(
        &floating,
        top_reserve,
        bottom_reserve,
        taskbar_hwnd,
        &cfg_snapshot,
        &constraints,
    );
}

#[cfg(test)]
mod tests {
    use super::{
        adjust_rects_for_resize, contained_floating_rect, panel_reserves_for_monitor,
        reconcile_window_order, rect_violates_constraints, swap_window_order, WindowConstraints,
    };
    use crate::config::{Config, PanelConfig};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::HMONITOR;

    #[test]
    fn minimum_size_violation_is_detected_before_placement() {
        let tile = RECT {
            left: 0,
            top: 0,
            right: 500,
            bottom: 400,
        };
        assert!(rect_violates_constraints(
            tile,
            WindowConstraints {
                min_width: 640,
                min_height: 360,
                ..Default::default()
            }
        ));
        assert!(!rect_violates_constraints(
            tile,
            WindowConstraints {
                min_width: 480,
                min_height: 360,
                ..Default::default()
            }
        ));
    }

    #[test]
    fn floating_rect_is_kept_inside_work_area() {
        let result = contained_floating_rect(
            RECT {
                left: 900,
                top: 700,
                right: 1500,
                bottom: 1200,
            },
            RECT {
                left: 0,
                top: 0,
                right: 1000,
                bottom: 800,
            },
            WindowConstraints::default(),
        );
        assert_eq!(result.left, 400);
        assert_eq!(result.top, 300);
        assert_eq!(result.right, 1000);
        assert_eq!(result.bottom, 800);
    }

    #[test]
    fn reserves_all_four_panel_edges_with_margins() {
        let config = Config {
            panels: [
                ("left", 10, [0, 2, 0, 3]),
                ("top", 20, [4, 0, 5, 0]),
                ("right", 30, [0, 7, 0, 6]),
                ("bottom", 40, [8, 0, 9, 0]),
            ]
            .into_iter()
            .map(|(position, height, margin)| PanelConfig {
                position: position.into(),
                height,
                margin: Some(margin),
                ..PanelConfig::default()
            })
            .collect(),
            ..Config::default()
        };
        let reserves = panel_reserves_for_monitor(HMONITOR::default(), &config);
        assert_eq!(reserves, (15, 29, 43, 57));
    }

    #[test]
    fn window_order_does_not_follow_focus_z_order() {
        let mut order = Vec::new();
        reconcile_window_order(&mut order, &[10, 20, 30], |_| true);
        reconcile_window_order(&mut order, &[30, 10, 20], |_| true);
        assert_eq!(order, vec![10, 20, 30]);
    }

    #[test]
    fn temporarily_ineligible_window_keeps_its_slot() {
        let mut order = vec![10, 20, 30];
        reconcile_window_order(&mut order, &[10, 30], |_| true);
        reconcile_window_order(&mut order, &[20, 30, 10], |_| true);
        assert_eq!(order, vec![10, 20, 30]);
    }

    #[test]
    fn destroyed_windows_are_pruned_before_new_windows_append() {
        let mut order = vec![10, 20, 30];
        reconcile_window_order(&mut order, &[40, 30, 10], |key| key != 20);
        assert_eq!(order, vec![10, 30, 40]);
    }

    #[test]
    fn dragging_onto_another_slot_swaps_stable_order() {
        let mut order = vec![10, 20, 30, 40];
        assert!(swap_window_order(&mut order, 10, 30));
        assert_eq!(order, vec![30, 20, 10, 40]);
    }

    #[test]
    fn resizing_a_tile_moves_the_shared_neighbor_boundary() {
        let slots = vec![
            (
                10,
                windows::Win32::Foundation::RECT {
                    left: 10,
                    top: 10,
                    right: 600,
                    bottom: 990,
                },
            ),
            (
                20,
                windows::Win32::Foundation::RECT {
                    left: 610,
                    top: 10,
                    right: 990,
                    bottom: 990,
                },
            ),
        ];
        let resized = windows::Win32::Foundation::RECT {
            left: 10,
            top: 10,
            right: 700,
            bottom: 990,
        };
        let (_, adjusted) = adjust_rects_for_resize(&slots, 10, slots[0].1, resized).unwrap();
        assert_eq!(adjusted[&10].right, 700);
        assert_eq!(adjusted[&20].left, 710);
        assert_eq!(adjusted[&20].right, 990);
    }

    #[test]
    fn resizing_cannot_move_the_outer_managed_edges() {
        let slots = vec![
            (
                10,
                windows::Win32::Foundation::RECT {
                    left: 10,
                    top: 10,
                    right: 600,
                    bottom: 990,
                },
            ),
            (
                20,
                windows::Win32::Foundation::RECT {
                    left: 610,
                    top: 10,
                    right: 990,
                    bottom: 990,
                },
            ),
        ];
        let resized = windows::Win32::Foundation::RECT {
            left: -200,
            top: -200,
            right: 600,
            bottom: 1200,
        };
        let (_, adjusted) = adjust_rects_for_resize(&slots, 10, slots[0].1, resized).unwrap();
        assert_eq!(adjusted[&10].left, 10);
        assert_eq!(adjusted[&10].top, 10);
        assert_eq!(adjusted[&10].bottom, 990);
    }
}

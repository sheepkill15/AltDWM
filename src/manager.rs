use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, DeferWindowPos, EndDeferWindowPos, EnumWindows, GetCursorPos, GetWindow,
    GetWindowLongPtrW, GetWindowRect, IsWindow, IsZoomed, SendMessageTimeoutW, SetWindowPos,
    ShowWindow, GWL_EXSTYLE, GW_OWNER, HWND_TOP, MINMAXINFO, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    SWP_NOACTIVATE, SWP_NOZORDER, SW_RESTORE, WM_GETMINMAXINFO, WS_EX_DLGMODALFRAME,
};

use crate::layout::{compute_layout, Layout};
use crate::util::is_manageable_or_minimized;

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

// EnumWindows enumerates in Z-order. Focusing a window changes that order, so
// feeding it directly to a layout makes every focus change reshuffle the tiles.
// Keep the discovery order of each live HWND and only append newly managed
// windows. Temporarily hidden/minimized windows retain their former slot.
static WINDOW_ORDER: LazyLock<Mutex<Vec<isize>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static EXPECTED_RECTS: LazyLock<Mutex<HashMap<isize, RECT>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static AUTO_FLOATING: LazyLock<Mutex<HashSet<isize>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
/// Size constraints per live window, with a flag for whether the answer is
/// trusted yet. The first reading is taken while the window may still be
/// initialising, so it is used but not kept — see `query_window_constraints`.
static CONSTRAINT_CACHE: LazyLock<Mutex<HashMap<isize, (WindowConstraints, bool)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Utility-window verdict per live window. `None` means "seen once, verdict
/// deliberately deferred to the next pass" — see `classify_utility_window`.
static UTILITY_CLASS: LazyLock<Mutex<HashMap<isize, Option<bool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WindowConstraints {
    min_width: i32,
    min_height: i32,
    max_width: i32,
    max_height: i32,
}

/// A shared, short-lived view of the managed windows.
///
/// Panels used to enumerate every top-level window — and ask COM for
/// virtual-desktop membership per window — on each paint and each mouse move
/// over the bar. Dragging a window re-derived the whole list dozens of times a
/// second. The snapshot is invalidated by the events that can actually change
/// it, with a short maximum age as a backstop.
type WindowSnapshot = Option<(Instant, Vec<isize>)>;
static WINDOW_SNAPSHOT: LazyLock<Mutex<WindowSnapshot>> = LazyLock::new(|| Mutex::new(None));
const SNAPSHOT_MAX_AGE: Duration = Duration::from_millis(400);

pub fn invalidate_window_snapshot() {
    *WINDOW_SNAPSHOT
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
}

pub fn window_snapshot() -> Vec<HWND> {
    let cached = WINDOW_SNAPSHOT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .filter(|(taken, _)| taken.elapsed() < SNAPSHOT_MAX_AGE)
        .map(|(_, keys)| keys.clone());
    if let Some(keys) = cached {
        return keys
            .into_iter()
            .map(|key| HWND(key as *mut std::ffi::c_void))
            .collect();
    }
    let mut windows = collect_windows_including_minimized();
    windows.retain(|hwnd| crate::virtual_desktop::is_on_current_desktop(*hwnd));
    windows.retain(|hwnd| crate::workspace::is_visible(*hwnd));
    *WINDOW_SNAPSHOT
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some((
        Instant::now(),
        windows.iter().map(|hwnd| hwnd.0 as isize).collect(),
    ));
    windows
}

fn clear_auto_floating() {
    AUTO_FLOATING
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

pub fn is_auto_floating(hwnd: HWND) -> bool {
    AUTO_FLOATING
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(&(hwnd.0 as isize))
}

/// WM_GETMINMAXINFO is a system-to-window notification: Windows fills the
/// structure with defaults and the application only *adjusts* the fields it
/// cares about. Sending a zeroed struct makes every app that clamps rather than
/// assigns report a minimum of zero, so the defaults have to be filled in here
/// exactly as DefWindowProc would.
fn prefilled_minmaxinfo(hwnd: HWND) -> MINMAXINFO {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXMAXTRACK, SM_CXMINTRACK, SM_CYMAXTRACK, SM_CYMINTRACK,
    };
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MINMAXINFO::default();
    unsafe {
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut mi as *mut _ as *mut _).as_bool() {
            info.ptMaxSize.x = mi.rcWork.right - mi.rcWork.left;
            info.ptMaxSize.y = mi.rcWork.bottom - mi.rcWork.top;
            info.ptMaxPosition.x = mi.rcWork.left;
            info.ptMaxPosition.y = mi.rcWork.top;
        }
        info.ptMinTrackSize.x = GetSystemMetrics(SM_CXMINTRACK);
        info.ptMinTrackSize.y = GetSystemMetrics(SM_CYMINTRACK);
        info.ptMaxTrackSize.x = GetSystemMetrics(SM_CXMAXTRACK);
        info.ptMaxTrackSize.y = GetSystemMetrics(SM_CYMAXTRACK);
    }
    info
}

fn query_window_constraints_uncached(hwnd: HWND) -> WindowConstraints {
    let mut info = prefilled_minmaxinfo(hwnd);
    let defaults = info;
    unsafe {
        let _ = SendMessageTimeoutW(
            hwnd,
            WM_GETMINMAXINFO,
            WPARAM(0),
            LPARAM(&mut info as *mut MINMAXINFO as isize),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            30,
            None,
        );
    }
    // A window that leaves the system default untouched has expressed no
    // opinion. Reporting the default as a real minimum would let the generic
    // system floor decide whether a window is tilable.
    let min_width = if info.ptMinTrackSize.x > defaults.ptMinTrackSize.x {
        info.ptMinTrackSize.x
    } else {
        0
    };
    let min_height = if info.ptMinTrackSize.y > defaults.ptMinTrackSize.y {
        info.ptMinTrackSize.y
    } else {
        0
    };
    WindowConstraints {
        min_width: min_width.max(0),
        min_height: min_height.max(0),
        max_width: info.ptMaxTrackSize.x.max(0),
        max_height: info.ptMaxTrackSize.y.max(0),
    }
}

/// Size constraints are a property of the window rather than of the moment, and
/// querying them costs a cross-process SendMessage with a timeout — so the
/// answer is cached for as long as the window lives.
///
/// The catch is *when* the first answer arrives. At `EVENT_OBJECT_CREATE` many
/// applications have not yet installed their WM_GETMINMAXINFO handler, so the
/// first reading can report no minimum at all, or a placeholder. It is used for
/// that pass but deliberately not kept: the second reading, one pass later, is
/// the one that gets cached. Without this the deferral in
/// `classify_utility_window` would be defeated by a stale first answer.
fn query_window_constraints(hwnd: HWND) -> WindowConstraints {
    let key = hwnd.0 as isize;
    if let Some((constraints, settled)) = CONSTRAINT_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
        .copied()
    {
        if settled {
            return constraints;
        }
    }
    let constraints = query_window_constraints_uncached(hwnd);
    let mut cache = CONSTRAINT_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let seen_before = cache.contains_key(&key);
    cache.insert(key, (constraints, seen_before));
    constraints
}

fn rect_violates_constraints(rect: RECT, constraints: WindowConstraints) -> bool {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    (constraints.min_width > 0 && width < constraints.min_width)
        || (constraints.min_height > 0 && height < constraints.min_height)
}

/// True for the traits that mark a transient dialog or palette the instant it
/// is created. Owner and modal frame are set before the window is ever shown,
/// so these are safe to act on during the first layout pass.
fn is_utility_window_at_birth(hwnd: HWND) -> bool {
    unsafe {
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.0.is_null());
        let modal_frame = (ex_style & WS_EX_DLGMODALFRAME.0) != 0;
        owned || modal_frame
    }
}

/// A window whose minimum and maximum track sizes agree cannot be tiled at all.
/// Unlike the window's current rectangle this is intrinsic — AltDWM cannot
/// change it — so classifying on it can never become self-fulfilling.
fn has_fixed_size(constraints: WindowConstraints) -> bool {
    constraints.min_width > 0
        && constraints.min_height > 0
        && constraints.max_width > 0
        && constraints.max_height > 0
        && constraints.min_width >= constraints.max_width.saturating_sub(2)
        && constraints.min_height >= constraints.max_height.saturating_sub(2)
}

/// Decide once per window whether it is an automatic utility window.
///
/// Two rules keep this stable. Nothing derived from the window's geometry is
/// consulted, because AltDWM sets that geometry and would otherwise reinforce
/// its own verdict forever. And the size-limit test is deferred by one pass:
/// at EVENT_OBJECT_CREATE many applications have not finished applying styles
/// or answering WM_GETMINMAXINFO, and a normal application window read too
/// early looks exactly like a fixed-size dialog.
fn classify_utility_window(hwnd: HWND, constraints: WindowConstraints) -> bool {
    let key = hwnd.0 as isize;
    if is_utility_window_at_birth(hwnd) {
        UTILITY_CLASS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key, Some(true));
        return true;
    }
    let mut cache = UTILITY_CLASS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match cache.get(&key).copied() {
        Some(Some(verdict)) => verdict,
        Some(None) => {
            let verdict = has_fixed_size(constraints);
            cache.insert(key, Some(verdict));
            verdict
        }
        None => {
            // First sighting: tile it, and settle the verdict next pass.
            cache.insert(key, None);
            false
        }
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

/// Applications are not obliged to accept the exact rectangle they are given.
/// Console hosts snap to whole character cells and others honour size
/// increments, so an exact comparison reports our own placement as a user move
/// and schedules another pass forever. A few pixels of slack breaks the loop.
const RECT_TOLERANCE: i32 = 4;

fn rects_are_close(left: &RECT, right: &RECT) -> bool {
    (left.left - right.left).abs() <= RECT_TOLERANCE
        && (left.top - right.top).abs() <= RECT_TOLERANCE
        && (left.right - right.right).abs() <= RECT_TOLERANCE
        && (left.bottom - right.bottom).abs() <= RECT_TOLERANCE
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
    unsafe { GetWindowRect(hwnd, &mut actual).is_ok() && rects_are_close(&actual, &expected) }
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
    // Only a tiled window has a slot to trade. Floating windows are in the
    // stable order too, and treating a floating drag as a reorder swapped two
    // unrelated tiles while the dragged window appeared not to respond.
    let is_tiled = EXPECTED_RECTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains_key(&(hwnd.0 as isize));
    if !is_tracked_window(hwnd) || !is_tiled {
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

/// Returns the rectangles to use and whether a retained interactive resize
/// supplied them. An override is the user's own explicit geometry, so callers
/// must not reshuffle it afterwards.
fn apply_layout_override(
    monitor: isize,
    windows: &[HWND],
    computed: Vec<RECT>,
) -> (Vec<RECT>, bool) {
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
        return (computed, false);
    }
    let saved = &overrides[&monitor];
    let rects = members
        .iter()
        .zip(computed)
        .map(|(key, fallback)| saved.rects.get(key).copied().unwrap_or(fallback))
        .collect();
    (rects, true)
}

/// The outcome of matching windows to the rectangles a layout produced.
struct SlotAssignment {
    /// Slot index for each window, in window order.
    slot_for_window: Vec<usize>,
    /// Windows that no remaining slot can satisfy, ascending.
    unplaceable: Vec<usize>,
}

fn slot_area(rect: &RECT) -> i64 {
    i64::from(rect_width(rect)) * i64::from(rect_height(rect))
}

/// Match windows to slots so that as many size constraints as possible are
/// satisfied.
///
/// Placement used to be positional: window *i* got slot *i*, and a window whose
/// slot was too small was dropped from the layout outright. That made
/// tilability a property of a window's position in the stable order rather than
/// of the window, so reordering by drag flipped the outcome — the master slot
/// fit, a stack slot did not.
///
/// The identity mapping is kept wherever it already works, so the first window
/// in the stable order still owns the master slot. Only windows that do not fit
/// are moved, by trading slots with a window that can take theirs.
fn assign_slots(limits: &[WindowConstraints], slots: &[RECT]) -> SlotAssignment {
    let n = limits.len().min(slots.len());
    let mut slot_for_window: Vec<usize> = (0..n).collect();
    let mut unplaceable: Vec<usize> = Vec::new();
    let fits = |window: usize, slot: usize| {
        !rect_violates_constraints(slots[slot], limits[window])
    };

    // Each clean trade settles one window permanently and each failure marks one
    // unplaceable, so `2n + 2` rounds is generous; the bound exists so
    // termination is provable locally rather than argued from the trade rules.
    for _ in 0..=(n * 2 + 2) {
        let Some(broken) = (0..n).find(|window| {
            !unplaceable.contains(window) && !fits(*window, slot_for_window[*window])
        }) else {
            break;
        };
        let available = |other: &usize| -> bool {
            *other != broken && !unplaceable.contains(other)
        };
        // Prefer a trade that leaves the partner satisfied too, taking the
        // tightest such slot so the roomiest tiles stay free for whoever needs
        // them.
        let clean = (0..n)
            .filter(available)
            .filter(|other| {
                fits(broken, slot_for_window[*other]) && fits(*other, slot_for_window[broken])
            })
            .min_by_key(|other| slot_area(&slots[slot_for_window[*other]]));
        if let Some(other) = clean {
            slot_for_window.swap(broken, other);
            continue;
        }
        // Otherwise trade with a window that is already violating its own slot.
        // That exchange cannot make the result worse and may resolve one of the
        // two.
        let salvage = (0..n)
            .filter(available)
            .filter(|other| {
                fits(broken, slot_for_window[*other]) && !fits(*other, slot_for_window[*other])
            })
            .min_by_key(|other| slot_area(&slots[slot_for_window[*other]]));
        if let Some(other) = salvage {
            slot_for_window.swap(broken, other);
            continue;
        }
        unplaceable.push(broken);
    }

    // Belt and braces: if the bound above were ever reached with windows still
    // in violation, reporting them as placeable would put a window into a slot
    // it does not fit. Report them instead, so the caller floats them.
    for (window, slot) in slot_for_window.iter().enumerate() {
        if !unplaceable.contains(&window) && !fits(window, *slot) {
            unplaceable.push(window);
        }
    }

    unplaceable.sort_unstable();
    unplaceable.dedup();
    SlotAssignment {
        slot_for_window,
        unplaceable,
    }
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
    // A drop outside the tiled area is not a reorder request. Without this the
    // nearest-slot fallback matched any pointer position at all, so releasing
    // over the desktop or a panel silently reshuffled the layout.
    let layout_bounds = rect_union(candidates.iter().map(|(_, rect)| *rect));
    let target = candidates
        .iter()
        .find(|(_, rect)| point_in_rect(pointer, rect))
        .or_else(|| {
            let inside_layout = layout_bounds.is_some_and(|bounds| point_in_rect(pointer, &bounds));
            if point_in_rect(pointer, &state.start_rect) || !inside_layout {
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

/// Move `hwnd` to the front of the stable order, making it the master window.
///
/// The layout reads the order positionally, so promotion is an order edit rather
/// than a geometry edit — which is why it survives the next retile.
fn promote_in_order(order: &mut Vec<isize>, key: isize) -> bool {
    let Some(position) = order.iter().position(|candidate| *candidate == key) else {
        return false;
    };
    if position == 0 {
        return false;
    }
    let entry = order.remove(position);
    order.insert(0, entry);
    true
}

pub fn promote_to_master(hwnd: HWND) -> bool {
    let changed = promote_in_order(
        &mut WINDOW_ORDER
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        hwnd.0 as isize,
    );
    if changed {
        clear_layout_overrides();
    }
    changed
}

/// Swap two windows' positions in the stable order.
pub fn swap_in_order(first: HWND, second: HWND) -> bool {
    let changed = swap_window_order(
        &mut WINDOW_ORDER
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        first.0 as isize,
        second.0 as isize,
    );
    if changed {
        clear_layout_overrides();
    }
    changed
}

/// Step `hwnd` one place forward or backward in the stable order, among the
/// windows currently sharing its monitor.
fn shift_within_order(order: &mut [isize], key: isize, delta: isize) -> bool {
    let Some(position) = order.iter().position(|candidate| *candidate == key) else {
        return false;
    };
    let target = position as isize + delta;
    if target < 0 || target as usize >= order.len() {
        return false;
    }
    order.swap(position, target as usize);
    true
}

pub fn shift_in_order(hwnd: HWND, delta: isize) -> bool {
    let changed = shift_within_order(
        &mut WINDOW_ORDER
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        hwnd.0 as isize,
        delta,
    );
    if changed {
        clear_layout_overrides();
    }
    changed
}

/// Managed windows on `monitor`, in stable order, excluding minimized ones.
///
/// Used to give focus somewhere sensible after a workspace switch.
pub fn windows_on_monitor(monitor: isize) -> Vec<HWND> {
    window_snapshot()
        .into_iter()
        .filter(|hwnd| unsafe {
            !windows::Win32::UI::WindowsAndMessaging::IsIconic(*hwnd).as_bool()
        })
        .filter(|hwnd| {
            let hmon = unsafe { MonitorFromWindow(*hwnd, MONITOR_DEFAULTTONEAREST) };
            hmon.0 as isize == monitor
        })
        .collect()
}

/// The rectangle AltDWM most recently assigned to a window, if it is tiled.
pub fn assigned_rect(hwnd: HWND) -> Option<RECT> {
    EXPECTED_RECTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&(hwnd.0 as isize))
        .copied()
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
pub fn collect_windows() -> Vec<HWND> {
    collect_windows_including_minimized()
        .into_iter()
        .filter(|hwnd| unsafe {
            !windows::Win32::UI::WindowsAndMessaging::IsIconic(*hwnd).as_bool()
        })
        .collect()
}

/// Collect all manageable application windows in stable tiling order, including
/// iconic windows so panels can act as a complete task list. Minimized windows
/// are excluded only when the actual layout is computed.
pub fn collect_windows_including_minimized() -> Vec<HWND> {
    let mut windows: Vec<HWND> = Vec::new();
    unsafe {
        let ptr = &mut windows as *mut Vec<HWND> as isize;
        let _ = EnumWindows(Some(enum_cb), LPARAM(ptr));
    }
    let eligible: Vec<HWND> = windows
        .into_iter()
        .filter(|hwnd| is_manageable_or_minimized(*hwnd))
        .collect();
    let eligible_keys: Vec<isize> = eligible.iter().map(|hwnd| hwnd.0 as isize).collect();
    let eligible_set: HashSet<isize> = eligible_keys.iter().copied().collect();
    let mut order = WINDOW_ORDER.lock().unwrap_or_else(|e| e.into_inner());
    reconcile_window_order(&mut order, &eligible_keys, |key| unsafe {
        let hwnd = HWND(key as *mut std::ffi::c_void);
        IsWindow(Some(hwnd)).as_bool()
    });
    let live: HashSet<isize> = order.iter().copied().collect();
    EXPECTED_RECTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .retain(|key, _| live.contains(key));
    CONSTRAINT_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .retain(|key, _| live.contains(key));
    UTILITY_CLASS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .retain(|key, _| live.contains(key));
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

/// Reserved edges for one monitor, in that monitor's physical pixels.
///
/// Panel geometry is declared in device-independent pixels, so a 40px bar
/// occupies 60 physical pixels at 150%. Reserving the unscaled value let tiled
/// windows run underneath the bar on any scaled display.
fn panel_reserves_for_monitor(hmon: HMONITOR, cfg: &crate::config::Config) -> (i32, i32, i32, i32) {
    let scale = crate::ui::scale_for_monitor(hmon);
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
        let consumption = crate::ui::px(panel.edge_consumption(), scale);
        match panel.position.as_str() {
            "top" => top += consumption,
            "right" => right += consumption,
            "bottom" => bottom += consumption,
            "left" => left += consumption,
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
    for hwnd in collect_windows_including_minimized() {
        apply_window_chrome(hwnd, &cfg, hwnd == foreground);
    }
}

fn get_work_area_for_hmonitor(
    hmon: HMONITOR,
    top_reserve: i32,
    bottom_reserve: i32,
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
        // Panels declare which monitors they occupy, so the reservation is
        // resolved per monitor rather than guessed from a single bar handle.
        let (panel_left, panel_top, panel_right, panel_bottom) = if cfg.panels.is_empty() {
            (0, top_reserve, 0, bottom_reserve)
        } else {
            panel_reserves_for_monitor(hmon, cfg)
        };
        work.left += panel_left;
        work.top += panel_top;
        work.right -= panel_right;
        work.bottom -= panel_bottom;
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
        let area = get_work_area_for_hmonitor(monitor, top_reserve, bottom_reserve, cfg);
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
        // Without the tolerance a window that cannot honour the containment
        // rectangle exactly is repositioned on every pass, forever.
        if !rects_are_close(&target, &current) {
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
    let all_windows = collect_windows();
    if all_windows.is_empty() {
        clear_auto_floating();
        return;
    }

    // Workspaces are applied before anything else looks at the window list:
    // hiding is what makes a window absent from the layout, and it has to happen
    // against the full set, including windows the previous workspace hid.
    crate::workspace::apply_visibility(&collect_windows_including_minimized());
    let all_windows: Vec<HWND> = all_windows
        .into_iter()
        .filter(|hwnd| crate::workspace::is_visible(*hwnd))
        .collect();
    if all_windows.is_empty() {
        clear_auto_floating();
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
        clear_auto_floating();
        return;
    }

    // Explicit rules decide first. Manual runtime floating is authoritative;
    // automatic utility classification is only used when no rule opted the
    // window in or out.
    let constraints: HashMap<isize, WindowConstraints> = all_windows
        .iter()
        .map(|hwnd| (hwnd.0 as isize, query_window_constraints(*hwnd)))
        .collect();
    // One rule walk per window, reused for the float decision, the monitor
    // override, and the opacity. Each of those used to re-walk the rule list and
    // re-read the window's class, title, and executable name.
    let resolved: HashMap<isize, crate::rules::ResolvedRules> = all_windows
        .iter()
        .map(|hwnd| (hwnd.0 as isize, crate::rules::resolve(*hwnd)))
        .collect();
    let mut windows = Vec::new();
    let mut floating = Vec::new();
    let mut auto_floating_keys = HashSet::new();
    for hwnd in all_windows {
        let key = hwnd.0 as isize;
        let manual = crate::focus::is_runtime_floating(hwnd);
        let decision = resolved.get(&key).and_then(|rules| rules.floating);
        let automatic = decision.is_none()
            && cfg_snapshot.general.auto_float_utility_windows
            && classify_utility_window(hwnd, constraints.get(&key).copied().unwrap_or_default());
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
    // Opacity is synchronised rather than only applied, so a window loses its
    // WS_EX_LAYERED style again once its rule stops matching.
    let opacity_targets: Vec<(HWND, Option<f32>)> = windows
        .iter()
        .chain(floating.iter())
        .map(|hwnd| {
            (
                *hwnd,
                resolved
                    .get(&(hwnd.0 as isize))
                    .and_then(|rules| rules.opacity),
            )
        })
        .collect();
    if verbose {
        for (hwnd, opacity) in opacity_targets.iter() {
            if let Some(opacity) = opacity {
                println!("[manager] opacity {} for {:?}", opacity, hwnd.0);
            }
        }
    }
    crate::rules::sync_opacity(&opacity_targets);
    if windows.is_empty() {
        *AUTO_FLOATING
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = auto_floating_keys;
        contain_floating_windows(
            &floating,
            top_reserve,
            bottom_reserve,
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
        let monitor_rule = resolved
            .get(&(hwnd.0 as isize))
            .and_then(|rules| rules.monitor.clone());
        let target_hmon = if let Some(mon_str) = monitor_rule {
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
            get_work_area_for_hmonitor(target_hmon, top_reserve, bottom_reserve, &cfg_snapshot)
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
        // A layout that positions nothing is a request to leave these windows
        // alone, not to run the whole placement pass and discard it.
        if matches!(monitor_layout, Layout::Floating)
            && !crate::layout::has_custom_layout(&monitor_cfg)
        {
            if verbose {
                println!(
                    "[manager] monitor 0x{:x} floating layout — {} window(s) left in place",
                    mon,
                    tiled_wins.len()
                );
            }
            for hwnd in &tiled_wins {
                EXPECTED_RECTS
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&(hwnd.0 as isize));
            }
            floating.append(&mut tiled_wins);
            continue;
        }
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
            let (rects, overridden) = apply_layout_override(mon, &tiled_wins, computed_rects);
            // A retained interactive resize is the user's own geometry. Honour
            // it verbatim rather than reassigning slots underneath them.
            if !cfg_snapshot.general.respect_window_size_constraints
                || overridden
                || rects.len() < tiled_wins.len()
            {
                break rects;
            }
            let limits: Vec<WindowConstraints> = tiled_wins
                .iter()
                .map(|hwnd| {
                    constraints
                        .get(&(hwnd.0 as isize))
                        .copied()
                        .unwrap_or_default()
                })
                .collect();
            let assignment = assign_slots(&limits, &rects);
            if assignment.unplaceable.is_empty() {
                if verbose {
                    for (index, slot) in assignment.slot_for_window.iter().enumerate() {
                        if *slot != index {
                            println!(
                                "[manager] {:?} moved to slot {} to satisfy its {}x{} minimum",
                                tiled_wins[index].0,
                                slot,
                                limits[index].min_width,
                                limits[index].min_height
                            );
                        }
                    }
                }
                break assignment
                    .slot_for_window
                    .iter()
                    .map(|slot| rects[*slot])
                    .collect();
            }
            // Floating is the last resort, reached only when no slot in the
            // layout can hold the window at all. Dropping it frees space, so the
            // remaining windows are re-laid out with larger tiles.
            for index in assignment.unplaceable.into_iter().rev() {
                let hwnd = tiled_wins.remove(index);
                auto_floating_keys.insert(hwnd.0 as isize);
                floating.push(hwnd);
                if verbose {
                    println!(
                        "[manager] auto-float {:?}: minimum {}x{} does not fit any tile in this layout",
                        hwnd.0, limits[index].min_width, limits[index].min_height
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
        &cfg_snapshot,
        &constraints,
    );
}

#[cfg(test)]
mod tests {
    use super::{
        adjust_rects_for_resize, assign_slots, contained_floating_rect, panel_reserves_for_monitor,
        promote_in_order, reconcile_window_order, rect_violates_constraints, rects_are_close,
        shift_within_order, swap_window_order, WindowConstraints,
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

    fn tile(width: i32, height: i32) -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }
    }

    fn min_size(width: i32, height: i32) -> WindowConstraints {
        WindowConstraints {
            min_width: width,
            min_height: height,
            ..Default::default()
        }
    }

    #[test]
    fn unconstrained_windows_keep_their_positional_slots() {
        let slots = vec![tile(600, 1000), tile(380, 500), tile(380, 500)];
        let limits = vec![WindowConstraints::default(); 3];
        let assignment = assign_slots(&limits, &slots);
        assert_eq!(assignment.slot_for_window, vec![0, 1, 2]);
        assert!(assignment.unplaceable.is_empty());
    }

    /// The reported bug: three windows tiled as two. A window that only fits the
    /// master slot used to be dropped from the layout because it happened to be
    /// third in the stable order.
    #[test]
    fn a_window_that_only_fits_the_master_slot_is_moved_there_not_floated() {
        let slots = vec![tile(600, 1000), tile(380, 500), tile(380, 500)];
        let limits = vec![
            WindowConstraints::default(),
            WindowConstraints::default(),
            min_size(500, 600),
        ];
        let assignment = assign_slots(&limits, &slots);
        assert!(
            assignment.unplaceable.is_empty(),
            "a window that fits somewhere must never be floated"
        );
        assert_eq!(assignment.slot_for_window[2], 0, "demanding window takes master");
        assert_eq!(
            assignment.slot_for_window.iter().copied().collect::<std::collections::HashSet<_>>(),
            [0, 1, 2].into_iter().collect(),
            "assignment must remain a permutation"
        );
    }

    /// Dragging rewrites the stable order. The set of tiled windows must not
    /// depend on that order, or a drag flips the outcome at random.
    #[test]
    fn tilability_does_not_depend_on_stable_order() {
        let slots = vec![tile(600, 1000), tile(380, 500), tile(380, 500)];
        let demanding = min_size(500, 600);
        let relaxed = WindowConstraints::default();
        for limits in [
            vec![demanding, relaxed, relaxed],
            vec![relaxed, demanding, relaxed],
            vec![relaxed, relaxed, demanding],
        ] {
            let assignment = assign_slots(&limits, &slots);
            assert!(
                assignment.unplaceable.is_empty(),
                "order {limits:?} changed whether the window could be tiled"
            );
        }
    }

    #[test]
    fn only_a_window_that_fits_nowhere_is_reported_unplaceable() {
        let slots = vec![tile(600, 1000), tile(380, 500)];
        let limits = vec![WindowConstraints::default(), min_size(1600, 1200)];
        let assignment = assign_slots(&limits, &slots);
        assert_eq!(assignment.unplaceable, vec![1]);
    }

    #[test]
    fn two_demanding_windows_share_the_slots_that_fit_them() {
        let slots = vec![tile(300, 300), tile(900, 900), tile(900, 900)];
        let limits = vec![min_size(800, 800), min_size(800, 800), WindowConstraints::default()];
        let assignment = assign_slots(&limits, &slots);
        assert!(assignment.unplaceable.is_empty());
        assert_eq!(assignment.slot_for_window[2], 0, "relaxed window takes the small slot");
    }

    #[test]
    fn placement_tolerates_windows_that_quantize_their_size() {
        let asked = tile(800, 600);
        let granted = RECT {
            left: 2,
            top: 0,
            right: 799,
            bottom: 597,
        };
        assert!(
            rects_are_close(&asked, &granted),
            "a few pixels of drift must not read as a user move"
        );
        assert!(!rects_are_close(&asked, &tile(700, 600)));
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
        // HMONITOR::default() has no DPI, so scale_for_monitor falls back to
        // 1.0 and the reserves stay in their declared units.
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
    fn promoting_moves_a_window_to_the_master_slot_without_reordering_the_rest() {
        let mut order = vec![10, 20, 30, 40];
        assert!(promote_in_order(&mut order, 30));
        assert_eq!(order, vec![30, 10, 20, 40]);
        // Already master: nothing to do, and the caller must not retile.
        assert!(!promote_in_order(&mut order, 30));
        assert!(!promote_in_order(&mut order, 99), "unknown window");
    }

    #[test]
    fn shifting_steps_one_place_and_stops_at_the_ends() {
        let mut order = vec![10, 20, 30];
        assert!(shift_within_order(&mut order, 20, 1));
        assert_eq!(order, vec![10, 30, 20]);
        assert!(shift_within_order(&mut order, 20, -1));
        assert_eq!(order, vec![10, 20, 30]);
        // At the ends the move is refused rather than wrapping, so a repeated
        // key does not cycle a window around the layout unexpectedly.
        assert!(!shift_within_order(&mut order, 10, -1));
        assert!(!shift_within_order(&mut order, 30, 1));
        assert_eq!(order, vec![10, 20, 30]);
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

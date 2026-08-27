//! Focus navigation — cycle through tilable windows
//! Exposed to keybinds via `focus_next()` / `focus_prev()` etc. and Rhai `focus_next()`
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::AttachThreadInput;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsIconic, SetForegroundWindow, SetWindowPos,
    ShowWindow, HWND_TOP, SWP_NOSIZE, SWP_NOZORDER, SW_MINIMIZE, SW_RESTORE,
};

use crate::manager::collect_windows;

static RUNTIME_FLOATING: LazyLock<Mutex<HashSet<isize>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn is_runtime_floating(hwnd: HWND) -> bool {
    RUNTIME_FLOATING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&(hwnd.0 as isize))
}

fn prune_stale_floating() {
    // Remove destroyed windows only — keep IsWindow==true entries (floating windows may be cloaked)
    let mut to_remove = Vec::new();
    {
        let set = RUNTIME_FLOATING.lock().unwrap_or_else(|e| e.into_inner());
        for k in set.iter() {
            let hwnd = HWND(*k as *mut std::ffi::c_void);
            unsafe {
                if !windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(hwnd)).as_bool() {
                    to_remove.push(*k);
                }
            }
        }
    }
    if !to_remove.is_empty() {
        let mut set = RUNTIME_FLOATING.lock().unwrap_or_else(|e| e.into_inner());
        for k in to_remove {
            set.remove(&k);
        }
    }
}
pub fn toggle_floating_focused() {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return;
    }
    let key = hwnd.0 as isize;
    let mut set = RUNTIME_FLOATING.lock().unwrap_or_else(|e| e.into_inner());
    if set.contains(&key) {
        set.remove(&key);
        println!("[focus] untiled (floating off) {:?}", hwnd.0);
    } else {
        set.insert(key);
        println!("[focus] floated {:?}", hwnd.0);
    }
    crate::request_retile();
}
pub fn move_focused_to_monitor(dir: &str) {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return;
    }
    // get all monitors
    let mons = crate::manager::get_all_monitors();
    if mons.len() <= 1 {
        return;
    }
    let cur = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let idx = mons.iter().position(|&h| h.0 == cur.0).unwrap_or(0);
    let target_idx = match dir.to_lowercase().as_str() {
        "next" | "right" | "down" | "l" | "j" => (idx + 1) % mons.len(),
        "prev" | "left" | "up" | "h" | "k" => (idx + mons.len() - 1) % mons.len(),
        _ => (idx + 1) % mons.len(),
    };
    let target = mons[target_idx];
    // center on target monitor work area
    unsafe {
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(target, &mut mi as *mut _ as *mut _).as_bool() {
            let work = mi.rcWork;
            let mut rect = RECT::default();
            if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect).is_ok() {
                let w = rect.right - rect.left;
                let h = rect.bottom - rect.top;
                let x = work.left + (work.right - work.left - w) / 2;
                let y = work.top + (work.bottom - work.top - h) / 2;
                let _ = SetWindowPos(hwnd, Some(HWND_TOP), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
                // also set foreground
                set_foreground(hwnd);
                crate::request_retile();
                println!(
                    "[focus] move {:?} to monitor {} (0x{:x})",
                    hwnd.0,
                    target_idx + 1,
                    target.0 as usize
                );
            }
        }
    }
}

/// Get tilable windows in tiling order (same as manager)
fn tilable_windows() -> Vec<HWND> {
    prune_stale_floating();
    let mut wins = collect_windows();
    wins.retain(|hwnd| {
        !crate::rules::is_floating(*hwnd)
            && !is_runtime_floating(*hwnd)
            && !crate::manager::is_auto_floating(*hwnd)
    });
    wins.retain(|hwnd| crate::virtual_desktop::is_on_current_desktop(*hwnd));
    wins
}

fn set_foreground(hwnd: HWND) {
    unsafe {
        // AttachThreadInput dance to allow SetForegroundWindow from background
        let fg = GetForegroundWindow();
        let mut fg_pid = 0;
        let mut cur_pid = 0;
        let fg_tid = GetWindowThreadProcessId(fg, Some(&mut fg_pid));
        let cur_tid = GetWindowThreadProcessId(hwnd, Some(&mut cur_pid));
        let cur_thread = windows::Win32::System::Threading::GetCurrentThreadId();
        // try attach
        let attached = if fg_tid != cur_tid && fg_tid != 0 {
            AttachThreadInput(fg_tid, cur_thread, true).as_bool()
        } else {
            false
        };
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        if attached {
            let _ = AttachThreadInput(fg_tid, cur_thread, false);
        }
        println!(
            "[focus] -> {:?} {}",
            hwnd.0,
            crate::util::get_window_title(hwnd)
        );
    }
}

/// Index of the focused window within the tilable list.
///
/// `focus_next` and `focus_prev` used to disagree about the not-found case: one
/// fell back to `usize::MAX` and jumped to the first window, the other to `0`
/// and jumped to the last. Sharing the lookup keeps the two keys symmetric.
fn focused_index(wins: &[HWND]) -> Option<usize> {
    let foreground = unsafe { GetForegroundWindow() };
    wins.iter().position(|hwnd| hwnd.0 == foreground.0)
}

/// Step `delta` places through the tilable windows, wrapping. With no focused
/// tilable window, stepping forward starts at the first and backward at the
/// last.
fn focus_step(delta: isize) {
    let wins = tilable_windows();
    if wins.is_empty() {
        return;
    }
    let len = wins.len() as isize;
    let target = match focused_index(&wins) {
        Some(current) => (current as isize + delta).rem_euclid(len),
        None if delta >= 0 => 0,
        None => len - 1,
    };
    set_foreground(wins[target as usize]);
}

pub fn focus_next() {
    focus_step(1);
}

pub fn focus_prev() {
    focus_step(-1);
}

/// Task-list behavior: restore minimized windows, minimize the active window,
/// and focus an inactive visible window.
pub fn toggle_window_from_list(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            focus_hwnd(hwnd);
            crate::request_retile();
        } else if GetForegroundWindow() == hwnd {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        } else {
            focus_hwnd(hwnd);
        }
    }
    crate::panel::invalidate_all();
}

/// A compass direction on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "left" | "h" | "west" => Some(Direction::Left),
            "right" | "l" | "east" => Some(Direction::Right),
            "up" | "k" | "north" => Some(Direction::Up),
            "down" | "j" | "south" => Some(Direction::Down),
            _ => None,
        }
    }

    /// Which way this direction moves a window through the stable order when
    /// there is no neighbour to swap with.
    fn order_step(&self) -> isize {
        match self {
            Direction::Right | Direction::Down => 1,
            Direction::Left | Direction::Up => -1,
        }
    }
}

fn center(rect: &RECT) -> (i32, i32) {
    (
        rect.left + (rect.right - rect.left) / 2,
        rect.top + (rect.bottom - rect.top) / 2,
    )
}

/// Half-open span overlap, the same convention as `RECT`.
fn spans_overlap(a_start: i32, a_end: i32, b_start: i32, b_end: i32) -> bool {
    a_start < b_end && b_start < a_end
}

/// Two tiles that merely share a boundary are not displaced from each other, and
/// integer rounding can leave a pixel of slack either way.
const EDGE_TOLERANCE: i32 = 8;

/// The tiled window nearest to `origin` in `direction`.
///
/// A candidate qualifies only if it is genuinely on that side *and* its extent
/// across the axis of travel overlaps the origin's. The overlap requirement is
/// what makes the direction keys behave: with centre distance alone, pressing Up
/// from a full-height master window jumped sideways into the stack, because a
/// stack tile's centre happens to sit above master's. Among qualifying
/// candidates, distance along the axis dominates and offset across it breaks
/// ties, so a window straight ahead beats a nearer one off to the side.
///
/// When nothing qualifies the caller falls back to list order, so a direction key
/// is never inert.
fn neighbour(origin: RECT, direction: Direction, candidates: &[(HWND, RECT)]) -> Option<HWND> {
    let (ox, oy) = center(&origin);
    candidates
        .iter()
        .filter_map(|(hwnd, rect)| {
            let (cx, cy) = center(rect);
            let (along, across) = match direction {
                Direction::Left | Direction::Right => {
                    if !spans_overlap(origin.top, origin.bottom, rect.top, rect.bottom) {
                        return None;
                    }
                    let along = if direction == Direction::Left {
                        ox - cx
                    } else {
                        cx - ox
                    };
                    (along, (cy - oy).abs())
                }
                Direction::Up | Direction::Down => {
                    if !spans_overlap(origin.left, origin.right, rect.left, rect.right) {
                        return None;
                    }
                    let along = if direction == Direction::Up {
                        oy - cy
                    } else {
                        cy - oy
                    };
                    (along, (cx - ox).abs())
                }
            };
            if along <= EDGE_TOLERANCE {
                return None;
            }
            Some((i64::from(along) + i64::from(across) * 3, *hwnd))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, hwnd)| hwnd)
}

/// Tiled windows with the rectangles AltDWM assigned them, which are stable even
/// while an application is mid-animation.
fn positioned_windows() -> Vec<(HWND, RECT)> {
    tilable_windows()
        .into_iter()
        .filter_map(|hwnd| {
            crate::manager::assigned_rect(hwnd)
                .or_else(|| {
                    let mut rect = RECT::default();
                    unsafe {
                        windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect)
                            .ok()
                            .map(|_| rect)
                    }
                })
                .map(|rect| (hwnd, rect))
        })
        .collect()
}

fn focused_with_rect() -> Option<(HWND, RECT)> {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.0.is_null() {
        return None;
    }
    positioned_windows()
        .into_iter()
        .find(|(hwnd, _)| hwnd.0 == foreground.0)
}

/// Move focus geometrically. Falls back to list order when there is no window in
/// that direction, so the key is never inert.
pub fn focus_direction(dir: &str) {
    let Some(direction) = Direction::parse(dir) else {
        focus_next();
        return;
    };
    let Some((focused, rect)) = focused_with_rect() else {
        focus_next();
        return;
    };
    let candidates: Vec<(HWND, RECT)> = positioned_windows()
        .into_iter()
        .filter(|(hwnd, _)| hwnd.0 != focused.0)
        .collect();
    match neighbour(rect, direction, &candidates) {
        Some(target) => set_foreground(target),
        None => {
            if matches!(direction, Direction::Right | Direction::Down) {
                focus_next()
            } else {
                focus_prev()
            }
        }
    }
}

/// Swap the focused window with its neighbour in `dir`, moving it through the
/// layout without the mouse.
pub fn move_window_direction(dir: &str) {
    let Some(direction) = Direction::parse(dir) else {
        return;
    };
    let Some((focused, rect)) = focused_with_rect() else {
        return;
    };
    let candidates: Vec<(HWND, RECT)> = positioned_windows()
        .into_iter()
        .filter(|(hwnd, _)| hwnd.0 != focused.0)
        .collect();
    let moved = match neighbour(rect, direction, &candidates) {
        Some(target) => crate::manager::swap_in_order(focused, target),
        // At the edge of the layout, fall back to stepping through the order so
        // the window can still be moved out of the master slot.
        None => crate::manager::shift_in_order(focused, direction.order_step()),
    };
    if moved {
        crate::request_retile();
    }
}

/// Make the focused window the master window.
pub fn promote_focused() {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return;
    }
    if crate::manager::promote_to_master(hwnd) {
        println!("[focus] promoted {:?} to master", hwnd.0);
        crate::request_retile();
    }
}

/// Called from scripting: `focus_next()`
pub fn focus_window_by_title_substr(substr: &str) {
    let wins = tilable_windows();
    for hwnd in wins {
        let title = crate::util::get_window_title(hwnd);
        if title.to_lowercase().contains(&substr.to_lowercase()) {
            set_foreground(hwnd);
            break;
        }
    }
}

pub fn focus_hwnd(hwnd: HWND) {
    set_foreground(hwnd);
}

#[cfg(test)]
mod tests {
    use super::{neighbour, Direction};
    use windows::Win32::Foundation::{HWND, RECT};

    fn hwnd(value: isize) -> HWND {
        HWND(value as *mut std::ffi::c_void)
    }

    fn tile(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    /// A master-stack arrangement: master on the left, two stacked on the right.
    fn master_stack() -> (RECT, Vec<(HWND, RECT)>) {
        let master = tile(0, 0, 600, 1000);
        let top = tile(610, 0, 1000, 495);
        let bottom = tile(610, 505, 1000, 1000);
        (
            master,
            vec![(hwnd(1), master), (hwnd(2), top), (hwnd(3), bottom)],
        )
    }

    #[test]
    fn directions_parse_from_names_and_vim_keys() {
        assert_eq!(Direction::parse("left"), Some(Direction::Left));
        assert_eq!(Direction::parse("H"), Some(Direction::Left));
        assert_eq!(Direction::parse(" Down "), Some(Direction::Down));
        assert_eq!(Direction::parse("j"), Some(Direction::Down));
        assert_eq!(Direction::parse("sideways"), None);
    }

    #[test]
    fn moving_right_from_master_finds_the_stack() {
        let (master, all) = master_stack();
        let others: Vec<_> = all.iter().copied().filter(|(h, _)| h.0 as isize != 1).collect();
        // Master spans the full height, so both stack tiles overlap it and sit
        // almost exactly equidistant from its centre. Either is a correct answer;
        // what matters is that focus leaves master and lands in the stack.
        let found = neighbour(master, Direction::Right, &others).map(|h| h.0 as isize);
        assert!(matches!(found, Some(2) | Some(3)), "got {found:?}");
    }

    #[test]
    fn moving_right_prefers_the_vertically_aligned_tile() {
        // An asymmetric stack, where one tile clearly lines up with the origin.
        let origin = tile(0, 400, 600, 600);
        let aligned = tile(610, 380, 1000, 620);
        let distant = tile(610, 0, 1000, 200);
        let others = vec![(hwnd(2), distant), (hwnd(3), aligned)];
        assert_eq!(
            neighbour(origin, Direction::Right, &others).map(|h| h.0 as isize),
            Some(3)
        );
    }

    #[test]
    fn moving_left_from_the_stack_finds_master() {
        let (_, all) = master_stack();
        let stack_top = all[1].1;
        let others: Vec<_> = all.iter().copied().filter(|(h, _)| h.0 as isize != 2).collect();
        assert_eq!(
            neighbour(stack_top, Direction::Left, &others).map(|h| h.0 as isize),
            Some(1)
        );
    }

    #[test]
    fn moving_down_within_the_stack_skips_master() {
        let (_, all) = master_stack();
        let stack_top = all[1].1;
        let others: Vec<_> = all.iter().copied().filter(|(h, _)| h.0 as isize != 2).collect();
        // Master's centre is below the top stack tile's centre, but it is far
        // off-axis; the tile directly underneath wins.
        assert_eq!(
            neighbour(stack_top, Direction::Down, &others).map(|h| h.0 as isize),
            Some(3)
        );
    }

    #[test]
    fn there_is_no_neighbour_past_the_edge_of_the_layout() {
        let (master, all) = master_stack();
        let others: Vec<_> = all.iter().copied().filter(|(h, _)| h.0 as isize != 1).collect();
        assert_eq!(neighbour(master, Direction::Left, &others), None);
    }

    /// The regression this exists for: with centre distance alone, Up from a
    /// full-height master matched the upper stack tile, because that tile's
    /// centre sits above master's. Pressing Up must not move focus sideways.
    #[test]
    fn vertical_moves_ignore_windows_in_another_column() {
        let (master, all) = master_stack();
        let others: Vec<_> = all.iter().copied().filter(|(h, _)| h.0 as isize != 1).collect();
        assert_eq!(neighbour(master, Direction::Up, &others), None);
        assert_eq!(neighbour(master, Direction::Down, &others), None);
    }

    /// And the mirror case: a horizontal move must not pick a window that shares
    /// no rows with the origin.
    #[test]
    fn horizontal_moves_ignore_windows_in_another_row() {
        let origin = tile(0, 0, 400, 300);
        let elsewhere = tile(500, 700, 900, 1000);
        let others = vec![(hwnd(2), elsewhere)];
        assert_eq!(neighbour(origin, Direction::Right, &others), None);
    }

    /// Two tiles sharing an edge must not each count as the other's neighbour in
    /// the perpendicular direction, or a single key press would jitter focus.
    #[test]
    fn windows_sharing_an_edge_are_not_neighbours_across_it() {
        let left = tile(0, 0, 500, 1000);
        let right = tile(500, 0, 1000, 1000);
        let others = vec![(hwnd(2), right)];
        assert_eq!(
            neighbour(left, Direction::Right, &others).map(|h| h.0 as isize),
            Some(2)
        );
        assert_eq!(neighbour(left, Direction::Up, &others), None);
        assert_eq!(neighbour(left, Direction::Down, &others), None);
    }

    #[test]
    fn a_window_straight_ahead_beats_a_nearer_one_off_to_the_side() {
        let origin = tile(0, 400, 200, 600);
        let ahead = tile(400, 400, 600, 600);
        let offset = tile(300, 0, 500, 100);
        let others = vec![(hwnd(2), offset), (hwnd(3), ahead)];
        assert_eq!(
            neighbour(origin, Direction::Right, &others).map(|h| h.0 as isize),
            Some(3)
        );
    }

    #[test]
    fn order_step_moves_forward_for_right_and_down() {
        assert_eq!(Direction::Right.order_step(), 1);
        assert_eq!(Direction::Down.order_step(), 1);
        assert_eq!(Direction::Left.order_step(), -1);
        assert_eq!(Direction::Up.order_step(), -1);
    }
}

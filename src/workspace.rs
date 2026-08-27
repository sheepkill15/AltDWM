//! Workspaces — AltDWM's own, per monitor.
//!
//! Windows' virtual desktops are only usefully switchable through
//! `IVirtualDesktopManagerInternal`, which is undocumented and changes shape
//! between builds. `crate::virtual_desktop` keeps the documented read-only half
//! of that API for filtering; this module implements workspaces natively
//! instead, the way every other third-party Windows tiling manager does: a
//! window not on the visible workspace is hidden, and shown again when its
//! workspace returns.
//!
//! Each monitor has its own active workspace, so switching on one display leaves
//! the other alone.
//!
//! Hiding other people's windows is the most destructive thing AltDWM does, so
//! three rules hold throughout.
//!
//! Only windows AltDWM hid are ever shown again — it never un-hides something an
//! application hid for its own reasons.
//!
//! Every in-process exit path restores them: normal shutdown, a Rust panic, an
//! unhandled exception, and Ctrl+C.
//!
//! And because none of those can catch `TerminateProcess` — Task Manager,
//! `Stop-Process`, a hard kill — the hidden set is also written to disk the
//! moment it changes. The next AltDWM start restores anything left behind, and
//! `alt-dwm --restore-windows` does it without starting the shell at all. A
//! window the user cannot find is the worst failure this program has, so it has
//! to be recoverable from outside the process that caused it.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, IsWindow, IsWindowVisible, ShowWindow, SW_HIDE, SW_SHOWNOACTIVATE,
};

/// Upper bound on configured workspaces. Nine keeps every workspace reachable
/// from a single digit key.
pub const MAX_WORKSPACES: usize = 9;

/// Workspace index per window, zero-based. A window absent from the map has
/// never been assigned and belongs to whichever workspace was active when it
/// first appeared.
static ASSIGNMENT: LazyLock<Mutex<HashMap<isize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Active workspace per monitor.
static ACTIVE: LazyLock<Mutex<HashMap<isize, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Windows AltDWM hid, and only those.
static HIDDEN: LazyLock<Mutex<HashSet<isize>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// How long after a switch AltDWM's own show/hide events keep arriving.
///
/// A bool set around the `ShowWindow` calls did not work: WinEvent hooks are
/// `WINEVENT_OUTOFCONTEXT`, so the SHOW and HIDE events are queued and delivered
/// when the message loop next runs — by which time a flag cleared synchronously
/// is already false, and every switch was mistaken for the user rearranging
/// windows. A deadline covers the delivery window instead.
static SUPPRESS_UNTIL: Mutex<Option<std::time::Instant>> = Mutex::new(None);
const SUPPRESS_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);
/// The display the user last worked on.
///
/// Switching a workspace hides the window that had focus, which leaves the
/// monitor with no foreground window at all — so resolving "the current monitor"
/// from the foreground window alone made the *next* switch land on whichever
/// display happened to inherit focus. Remembering it keeps repeated switches on
/// the display the user is looking at.
static LAST_MONITOR: Mutex<Option<isize>> = Mutex::new(None);

/// How many workspaces are configured. One means the feature is inert.
pub fn count() -> usize {
    crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .general
        .workspaces
        .clamp(1, MAX_WORKSPACES)
}

pub fn is_enabled() -> bool {
    count() > 1
}

/// True while the show/hide traffic AltDWM just generated is still arriving.
pub fn is_switching() -> bool {
    SUPPRESS_UNTIL
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .is_some_and(|deadline| std::time::Instant::now() < deadline)
}

fn begin_suppression() {
    *SUPPRESS_UNTIL
        .lock()
        .unwrap_or_else(|error| error.into_inner()) =
        Some(std::time::Instant::now() + SUPPRESS_WINDOW);
}

fn monitor_of(hwnd: HWND) -> isize {
    let monitor: HMONITOR =
        unsafe { windows::Win32::Graphics::Gdi::MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    monitor.0 as isize
}

pub fn active_for_monitor(monitor: isize) -> usize {
    ACTIVE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&monitor)
        .copied()
        .unwrap_or(0)
        .min(count() - 1)
}

/// Workspace a window belongs to. An unseen window adopts the active workspace
/// of the monitor it appeared on, so a newly launched application lands where
/// the user is looking.
pub fn workspace_of(hwnd: HWND) -> usize {
    let key = hwnd.0 as isize;
    if let Some(index) = ASSIGNMENT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
        .copied()
    {
        return index.min(count() - 1);
    }
    let index = active_for_monitor(monitor_of(hwnd));
    ASSIGNMENT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(key, index);
    index
}

/// True when the window's workspace is the visible one on its monitor.
pub fn is_visible(hwnd: HWND) -> bool {
    if !is_enabled() {
        return true;
    }
    workspace_of(hwnd) == active_for_monitor(monitor_of(hwnd))
}

/// What `apply_visibility` knows about one window before deciding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub key: isize,
    pub visible_now: bool,
    pub should_be_visible: bool,
    /// True when AltDWM is the one that hid it.
    pub hidden_by_us: bool,
}

/// Windows to hide and to show, in that order.
///
/// Pure so the rules can be tested: a window is only ever shown again if AltDWM
/// hid it, because un-hiding a window an application hid for its own reasons
/// would be AltDWM overriding a decision that was not its to make.
pub fn visibility_plan(candidates: &[Candidate]) -> (Vec<isize>, Vec<isize>) {
    let mut to_hide = Vec::new();
    let mut to_show = Vec::new();
    for candidate in candidates {
        if candidate.should_be_visible {
            if !candidate.visible_now && candidate.hidden_by_us {
                to_show.push(candidate.key);
            }
        } else if candidate.visible_now {
            to_hide.push(candidate.key);
        }
    }
    (to_hide, to_show)
}

/// Hide or show each window to match its workspace.
///
/// `windows` is the currently *visible* managed set, which the caller has
/// already enumerated. A window AltDWM has hidden fails `IsWindowVisible`, so it
/// is not manageable and cannot appear in that list — the windows this function
/// has to bring back are precisely the ones the caller cannot see. They are
/// added here from `HIDDEN` rather than expected from the caller.
pub fn apply_visibility(windows: &[HWND]) {
    if !is_enabled() {
        return;
    }
    let hidden_keys: HashSet<isize> = HIDDEN
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let mut candidates: Vec<Candidate> = Vec::with_capacity(windows.len() + hidden_keys.len());
    let mut seen: HashSet<isize> = HashSet::new();
    for hwnd in windows {
        let key = hwnd.0 as isize;
        if !seen.insert(key) {
            continue;
        }
        candidates.push(Candidate {
            key,
            visible_now: unsafe { IsWindowVisible(*hwnd).as_bool() },
            should_be_visible: is_visible(*hwnd),
            hidden_by_us: hidden_keys.contains(&key),
        });
    }
    for key in &hidden_keys {
        if !seen.insert(*key) {
            continue;
        }
        let hwnd = HWND(*key as *mut std::ffi::c_void);
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            continue;
        }
        candidates.push(Candidate {
            key: *key,
            visible_now: unsafe { IsWindowVisible(hwnd).as_bool() },
            should_be_visible: is_visible(hwnd),
            hidden_by_us: true,
        });
    }

    let (to_hide, to_show) = visibility_plan(&candidates);
    if to_hide.is_empty() && to_show.is_empty() {
        return;
    }
    begin_suppression();
    {
        let mut hidden = HIDDEN.lock().unwrap_or_else(|error| error.into_inner());
        for key in &to_hide {
            unsafe {
                let _ = ShowWindow(HWND(*key as *mut std::ffi::c_void), SW_HIDE);
            }
            hidden.insert(*key);
        }
        for key in &to_show {
            unsafe {
                // NOACTIVATE: revealing a workspace must not steal focus from
                // whichever window is about to be given it deliberately.
                let _ = ShowWindow(HWND(*key as *mut std::ffi::c_void), SW_SHOWNOACTIVATE);
            }
            hidden.remove(key);
        }
        persist(&hidden);
    }
    // Deliberately not cleared here: the events these calls produced have not
    // been delivered yet.
}

/// Where the hidden set is journalled.
///
/// Beside the rest of AltDWM's local state rather than next to the config, since
/// it is runtime recovery data and not something anyone edits.
fn journal_path() -> Option<std::path::PathBuf> {
    let base = dirs::data_local_dir()?.join("AltDWM");
    std::fs::create_dir_all(&base).ok()?;
    Some(base.join("hidden-windows"))
}

/// Owning process of a window, used to detect a recycled handle.
fn process_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}

/// One journal line: handle, owning process, and a human-readable title.
///
/// The process id is what makes replaying the journal safe. A raw handle is only
/// meaningful while its window lives, and Windows reuses handle values — so a
/// stale entry could otherwise name a completely unrelated window that is hidden
/// for good reason.
fn journal_line(hwnd: HWND) -> String {
    format!(
        "{:x} {} {}",
        hwnd.0 as isize,
        process_of(hwnd),
        crate::util::get_window_title(hwnd).replace(['\r', '\n'], " ")
    )
}

fn parse_journal_line(line: &str) -> Option<(isize, u32)> {
    let mut parts = line.split_whitespace();
    let handle = isize::from_str_radix(parts.next()?, 16).ok()?;
    let pid = parts.next()?.parse::<u32>().ok()?;
    Some((handle, pid))
}

/// Write the current hidden set out. Called after every change to it.
fn persist(hidden: &HashSet<isize>) {
    let Some(path) = journal_path() else {
        return;
    };
    if hidden.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let body: String = hidden
        .iter()
        .map(|key| journal_line(HWND(*key as *mut std::ffi::c_void)))
        .collect::<Vec<_>>()
        .join("\n");
    if let Err(error) = std::fs::write(&path, body) {
        eprintln!("[workspace] cannot journal hidden windows: {error}");
    }
}

/// Show anything a previous AltDWM left hidden, then clear the journal.
///
/// Runs at startup, and on demand through `--restore-windows`. Returns how many
/// windows were brought back.
pub fn restore_from_journal() -> usize {
    let Some(path) = journal_path() else {
        return 0;
    };
    let Ok(body) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let mut restored = 0;
    for line in body.lines() {
        let Some((handle, pid)) = parse_journal_line(line.trim()) else {
            continue;
        };
        let hwnd = HWND(handle as *mut std::ffi::c_void);
        unsafe {
            if !IsWindow(Some(hwnd)).as_bool() {
                continue;
            }
        }
        // Refuse a recycled handle: same value, different process.
        if process_of(hwnd) != pid {
            continue;
        }
        if unsafe { IsWindowVisible(hwnd).as_bool() } {
            continue;
        }
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        println!("[workspace] recovered hidden window {line}");
        restored += 1;
    }
    let _ = std::fs::remove_file(&path);
    if restored > 0 {
        println!("[workspace] recovered {restored} window(s) left hidden by a previous run");
    }
    restored
}

/// Windows AltDWM has hidden, whether or not they are still alive.
fn hidden_windows() -> Vec<HWND> {
    HIDDEN
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .map(|key| HWND(*key as *mut std::ffi::c_void))
        .collect()
}

/// Show every window AltDWM hid.
///
/// Runs on every exit path, including the panic hook: leaving a user's windows
/// hidden with no shell running would look exactly like having lost them.
pub fn restore_all() {
    let windows = hidden_windows();
    if windows.is_empty() {
        return;
    }
    println!("[workspace] restoring {} hidden window(s)", windows.len());
    begin_suppression();
    for hwnd in windows {
        unsafe {
            if IsWindow(Some(hwnd)).as_bool() {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }
    {
        let mut hidden = HIDDEN.lock().unwrap_or_else(|error| error.into_inner());
        hidden.clear();
        persist(&hidden);
    }
}

/// Drop assignments for windows that are no longer live.
///
/// `HIDDEN` is intentionally left alone: an entry there is a window AltDWM owes
/// a `ShowWindow` to, and a hidden window is not in the manageable set, so it
/// would never appear in `live`. Pruning it would be exactly how a window gets
/// lost.
pub fn retain_windows(live: &HashSet<isize>) {
    ASSIGNMENT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .retain(|key, _| live.contains(key));
}

/// Forget a destroyed window.
pub fn forget_window(hwnd: HWND) {
    let key = hwnd.0 as isize;
    ASSIGNMENT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&key);
    let mut hidden = HIDDEN.lock().unwrap_or_else(|error| error.into_inner());
    if hidden.remove(&key) {
        persist(&hidden);
    }
}

/// Record the display a newly focused window is on.
pub fn note_focus(hwnd: HWND) {
    if hwnd.0.is_null() || !crate::manager::is_tracked_window(hwnd) {
        return;
    }
    *LAST_MONITOR
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(monitor_of(hwnd));
}

/// The monitor the user is currently working on: the foreground window's display
/// when there is a managed one, otherwise the last display that had focus, and
/// only then the pointer or the primary.
fn focused_monitor() -> isize {
    let foreground = unsafe { GetForegroundWindow() };
    if !foreground.0.is_null() && crate::manager::is_tracked_window(foreground) {
        return monitor_of(foreground);
    }
    if let Some(remembered) = *LAST_MONITOR
        .lock()
        .unwrap_or_else(|error| error.into_inner())
    {
        return remembered;
    }
    if foreground.0.is_null() {
        // Fall back to the monitor under the pointer, then the primary.
        let mut point = windows::Win32::Foundation::POINT::default();
        unsafe {
            if windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point).is_ok() {
                return windows::Win32::Graphics::Gdi::MonitorFromPoint(
                    point,
                    MONITOR_DEFAULTTONEAREST,
                )
                .0 as isize;
            }
            windows::Win32::Graphics::Gdi::MonitorFromPoint(
                windows::Win32::Foundation::POINT { x: 0, y: 0 },
                windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTOPRIMARY,
            )
            .0 as isize
        }
    } else {
        monitor_of(foreground)
    }
}

/// Show workspace `index` on the monitor the user is working on.
pub fn switch_to(index: usize) {
    if !is_enabled() {
        eprintln!("[workspace] switching needs general.workspaces > 1");
        return;
    }
    let index = index.min(count() - 1);
    let monitor = focused_monitor();
    let previous = active_for_monitor(monitor);
    if previous == index {
        return;
    }
    ACTIVE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(monitor, index);
    *LAST_MONITOR
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(monitor);
    println!(
        "[workspace] monitor 0x{monitor:x}: {} -> {}",
        previous + 1,
        index + 1
    );
    crate::manager::invalidate_window_snapshot();
    // Synchronous, so visibility is applied before anything is focused — and so
    // the switch does not visibly lag behind the key press.
    crate::retile_now();
    crate::manager::invalidate_window_snapshot();
    focus_something_on(monitor);
    crate::panel::invalidate_all();
}

/// Give focus to a window on `monitor` after a switch.
///
/// Without this the workspace appears but the keyboard still belongs to whatever
/// inherited focus when the previous workspace was hidden, which is usually a
/// window on another display.
fn focus_something_on(monitor: isize) {
    let foreground = unsafe { GetForegroundWindow() };
    let already_here = !foreground.0.is_null()
        && crate::manager::is_tracked_window(foreground)
        && monitor_of(foreground) == monitor
        && unsafe { IsWindowVisible(foreground).as_bool() };
    if already_here {
        return;
    }
    if let Some(target) = crate::manager::windows_on_monitor(monitor).first().copied() {
        crate::focus::focus_hwnd(target);
    }
}

/// Step to the next or previous workspace on the focused monitor, wrapping.
pub fn cycle(delta: isize) {
    if !is_enabled() {
        return;
    }
    let total = count() as isize;
    let monitor = focused_monitor();
    let current = active_for_monitor(monitor) as isize;
    switch_to(((current + delta).rem_euclid(total)) as usize);
}

/// Send the focused window to workspace `index` and follow it or not.
pub fn move_focused_to(index: usize, follow: bool) {
    if !is_enabled() {
        eprintln!("[workspace] moving needs general.workspaces > 1");
        return;
    }
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() || !crate::manager::is_tracked_window(hwnd) {
        return;
    }
    let index = index.min(count() - 1);
    ASSIGNMENT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(hwnd.0 as isize, index);
    println!("[workspace] moved {:?} to workspace {}", hwnd.0, index + 1);
    if follow {
        switch_to(index);
    } else {
        crate::manager::invalidate_window_snapshot();
        crate::request_retile();
        crate::panel::invalidate_all();
    }
}

/// One-based workspace number active on the monitor the user is working on.
pub fn current_number() -> usize {
    active_for_monitor(focused_monitor()) + 1
}

/// What the workspace widget needs to draw one monitor's strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceInfo {
    /// One-based, as shown to the user.
    pub number: usize,
    pub active: bool,
    /// True when at least one managed window lives here.
    pub occupied: bool,
}

/// Workspace strip for the monitor a panel is on.
///
/// `visible` is the caller's managed set, which by construction contains only
/// windows on the *active* workspace — a hidden window fails `IsWindowVisible`
/// and is therefore not manageable. The occupied markers exist precisely to show
/// where the windows the user cannot see are, so the hidden set has to be folded
/// back in here or every inactive workspace would always read as empty.
pub fn summary(monitor: isize, visible: &[HWND]) -> Vec<WorkspaceInfo> {
    let total = count();
    let active = active_for_monitor(monitor);
    let mut occupied = vec![false; total];
    let hidden = hidden_windows();
    let candidates = visible.iter().copied().chain(hidden.into_iter().filter(|hwnd| unsafe {
        IsWindow(Some(*hwnd)).as_bool()
    }));
    for hwnd in candidates {
        if monitor_of(hwnd) != monitor {
            continue;
        }
        let index = workspace_of(hwnd);
        if let Some(slot) = occupied.get_mut(index) {
            *slot = true;
        }
    }
    (0..total)
        .map(|index| WorkspaceInfo {
            number: index + 1,
            active: index == active,
            occupied: occupied[index],
        })
        .collect()
}

/// Reset every assignment, used when the configuration changes the workspace
/// count. Windows that were on a workspace that no longer exists would
/// otherwise be permanently invisible.
pub fn clamp_to_count() {
    let total = count();
    let mut assignment = ASSIGNMENT
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for index in assignment.values_mut() {
        if *index >= total {
            *index = total - 1;
        }
    }
    drop(assignment);
    let mut active = ACTIVE.lock().unwrap_or_else(|error| error.into_inner());
    for index in active.values_mut() {
        if *index >= total {
            *index = total - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_journal_line, visibility_plan, Candidate, WorkspaceInfo, MAX_WORKSPACES};

    #[test]
    fn journal_lines_round_trip_handle_and_process() {
        assert_eq!(parse_journal_line("204ac 1234 Claude"), Some((0x204ac, 1234)));
        // A title with spaces must not confuse the two leading fields.
        assert_eq!(
            parse_journal_line("1038c 99 Anime Relay - Episode 3"),
            Some((0x1038c, 99))
        );
    }

    #[test]
    fn malformed_journal_lines_are_skipped_not_guessed() {
        assert_eq!(parse_journal_line(""), None);
        assert_eq!(parse_journal_line("notahandle 1"), None);
        assert_eq!(parse_journal_line("204ac"), None, "missing process id");
        assert_eq!(parse_journal_line("204ac notapid"), None);
    }

    fn candidate(key: isize, visible_now: bool, should: bool, ours: bool) -> Candidate {
        Candidate {
            key,
            visible_now,
            should_be_visible: should,
            hidden_by_us: ours,
        }
    }

    #[test]
    fn a_window_leaving_the_visible_workspace_is_hidden() {
        let (hide, show) = visibility_plan(&[candidate(1, true, false, false)]);
        assert_eq!(hide, vec![1]);
        assert!(show.is_empty());
    }

    /// The bug this test exists for: a hidden window fails IsWindowVisible, so it
    /// is not in the manageable set. If the plan only considered visible windows
    /// nothing on an inactive workspace could ever come back.
    #[test]
    fn a_window_returning_to_the_visible_workspace_is_shown() {
        let (hide, show) = visibility_plan(&[candidate(1, false, true, true)]);
        assert!(hide.is_empty());
        assert_eq!(show, vec![1]);
    }

    #[test]
    fn a_window_hidden_by_its_own_application_is_left_alone() {
        // Should be visible by workspace, is not visible, but AltDWM did not
        // hide it — a minimized-to-tray application, for instance.
        let (hide, show) = visibility_plan(&[candidate(1, false, true, false)]);
        assert!(hide.is_empty());
        assert!(show.is_empty(), "AltDWM must not un-hide what it did not hide");
    }

    #[test]
    fn windows_already_in_the_right_state_are_not_touched() {
        let (hide, show) = visibility_plan(&[
            candidate(1, true, true, false),
            candidate(2, false, false, true),
        ]);
        assert!(hide.is_empty());
        assert!(show.is_empty());
    }

    #[test]
    fn a_switch_hides_and_shows_in_one_plan() {
        let (hide, show) = visibility_plan(&[
            candidate(10, true, false, false),
            candidate(11, true, false, false),
            candidate(20, false, true, true),
        ]);
        assert_eq!(hide, vec![10, 11]);
        assert_eq!(show, vec![20]);
    }

    #[test]
    fn workspace_count_is_bounded() {
        assert_eq!(0usize.clamp(1, MAX_WORKSPACES), 1);
        assert_eq!(4usize.clamp(1, MAX_WORKSPACES), 4);
        assert_eq!(99usize.clamp(1, MAX_WORKSPACES), MAX_WORKSPACES);
    }

    #[test]
    fn workspace_info_is_one_based_for_display() {
        let info = WorkspaceInfo {
            number: 1,
            active: true,
            occupied: false,
        };
        assert_eq!(info.number, 1, "the first workspace reads as 1, not 0");
    }
}

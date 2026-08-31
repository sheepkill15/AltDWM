mod apps;
mod command_center;
mod config;
mod desktop;
mod elevation;
mod focus;
mod input;
mod layout;
mod manager;
mod panel;
mod quick_settings;
mod rules;
mod scripted_widget;
mod scripting;
mod shell;
mod startup;
mod system;
mod theme;
mod tray;
mod tray_overflow;
mod ui;
mod util;
mod virtual_desktop;
mod watcher;
mod widgets;
mod workspace;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
    TranslateMessage, CW_USEDEFAULT, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND,
    EVENT_SYSTEM_MOVESIZESTART, HMENU, HWND_MESSAGE, MSG, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_CREATE, WM_DESTROY, WM_DISPLAYCHANGE, WM_HOTKEY, WM_TIMER,
};

use layout::Layout;
use windows::core::w;

// ------------------------------------------------------------------
// Global state — pub for scripting/panel/util access
// ------------------------------------------------------------------
pub static RETILE_PENDING: AtomicBool = AtomicBool::new(false);
pub static CONFIG_RELOAD_PENDING: AtomicBool = AtomicBool::new(false);
pub static TILING_ENABLED: AtomicBool = AtomicBool::new(true);

pub static CURRENT_LAYOUT: LazyLock<Mutex<Layout>> =
    LazyLock::new(|| Mutex::new(Layout::MasterStack));
pub static CURRENT_GAP: LazyLock<Mutex<i32>> = LazyLock::new(|| Mutex::new(8));
pub static CONFIG_PATH: LazyLock<Mutex<Option<PathBuf>>> = LazyLock::new(|| Mutex::new(None));
pub static CURRENT_CONFIG: LazyLock<Mutex<config::Config>> =
    LazyLock::new(|| Mutex::new(config::Config::default()));
pub static HOTKEY_ACTIONS: LazyLock<Mutex<HashMap<i32, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub static ACTIVE_KEYBINDS: LazyLock<Mutex<Vec<config::KeybindConfig>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static TRANSITIONS_TO_RESTORE: LazyLock<Mutex<HashMap<isize, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub static MAIN_TID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

// helpers for scripting / manager
pub fn request_retile() {
    RETILE_PENDING.store(true, Ordering::SeqCst);
}

/// Re-tile immediately rather than on the next timer tick.
///
/// Used where the caller has to observe the result — switching a workspace has
/// to apply visibility before it can focus a window on the workspace it just
/// revealed.
pub fn retile_now() {
    RETILE_PENDING.store(false, Ordering::SeqCst);
    if TILING_ENABLED.load(Ordering::SeqCst) {
        tile_current_layout_now();
    }
}

/// Re-tile only one physical display. Workspace switching must not reposition,
/// hide, show, or restyle windows belonging to any other monitor.
pub fn retile_monitor_now(monitor: isize) {
    if !TILING_ENABLED.load(Ordering::SeqCst) {
        return;
    }
    let cfg = CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let (top_reserve, bottom_reserve) = bar_reserves(&cfg);
    manager::tile_windows_reserved_for_monitor(
        top_reserve,
        bottom_reserve,
        cfg.layout_enum(),
        cfg.general.gap,
        monitor,
    );
}
pub fn request_quit() {
    let v = HOST_HWND.load(Ordering::SeqCst);
    if v != 0 {
        let hwnd = HWND(v as *mut std::ffi::c_void);
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(hwnd),
                windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    } else if let Some(tid) = MAIN_TID.get().copied() {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                tid,
                windows::Win32::UI::WindowsAndMessaging::WM_QUIT,
                WPARAM(0),
                LPARAM(0),
            );
        }
    } else {
        unsafe {
            PostQuitMessage(0);
        }
    }
}
pub fn toggle_tiling() {
    let enabled = !TILING_ENABLED.load(Ordering::SeqCst);
    TILING_ENABLED.store(enabled, Ordering::SeqCst);
    println!(
        "[main] Tiling {}",
        if enabled { "ENABLED" } else { "DISABLED" }
    );
    if enabled {
        request_retile();
    }
}
pub fn set_layout_by_name(name: &str) {
    let normalized = name.trim();
    let layout = match normalized.to_lowercase().as_str() {
        "grid" => Layout::Grid,
        "monocle" => Layout::Monocle,
        "floating" => Layout::Floating,
        _ => Layout::MasterStack,
    };
    manager::clear_layout_overrides();
    *CURRENT_LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = layout;
    {
        let mut cfg = CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(custom_name) = cfg
            .layouts
            .keys()
            .find(|key| key.eq_ignore_ascii_case(normalized))
            .cloned()
        {
            // Custom Rhai layouts use MasterStack only as their native fallback.
            cfg.general.layout = custom_name;
        } else {
            cfg.set_layout(layout);
        }
    }
    println!("[main] Layout -> {}", normalized);
    request_retile();
}
/// Nudge the master column's share of the width and re-tile.
///
/// The ratio was a hardcoded 60% with no way to change it, which is the one
/// tiling adjustment people reach for constantly.
pub fn adjust_master_ratio(delta: f32) {
    let ratio = {
        let mut cfg = CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner());
        let next = (cfg.general.master_ratio + delta).clamp(0.1, 0.9);
        cfg.general.master_ratio = next;
        next
    };
    // A retained interactive resize would otherwise override the new ratio.
    manager::clear_layout_overrides();
    println!("[main] master ratio -> {:.0}%", ratio * 100.0);
    request_retile();
}

pub fn reload_config_async() {
    CONFIG_RELOAD_PENDING.store(true, Ordering::SeqCst);
    request_retile();
}
pub fn is_ignored_class(class: &str) -> bool {
    let cfg = CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    cfg.ignore.classes.iter().any(|c| c == class)
}
pub fn is_ignored_title(title: &str) -> bool {
    let cfg = CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    cfg.ignore.titles.iter().any(|t| title.contains(t))
}

/// Top and bottom reserves used only as a fallback when a monitor cannot be
/// resolved. Real placement asks `panel_reserves_for_monitor` per display.
fn bar_reserves(cfg: &config::Config) -> (i32, i32) {
    let reserved = |position: &str| {
        cfg.panels
            .iter()
            .filter(|panel| panel.position == position)
            .map(config::PanelConfig::edge_consumption)
            .sum()
    };
    (reserved("top"), reserved("bottom"))
}

fn tile_current_layout_now() {
    let cfg = CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let (top_reserve, bottom_reserve) = bar_reserves(&cfg);
    manager::tile_windows_reserved(
        top_reserve,
        bottom_reserve,
        cfg.layout_enum(),
        cfg.general.gap,
    );
}

fn place_new_window_immediately(hwnd: HWND) {
    let mut windows = manager::collect_windows();
    if !windows.contains(&hwnd) {
        windows.push(hwnd);
    }
    let restore_at = Instant::now() + Duration::from_millis(300);
    let mut pending_restore = TRANSITIONS_TO_RESTORE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for window in windows {
        util::set_transitions_forced_disabled(window, true);
        pending_restore.insert(window.0 as isize, restore_at);
    }
    drop(pending_restore);

    // This bypasses the normal 200 ms coalescing timer. CREATE may arrive
    // before WS_VISIBLE, while SHOW/FOREGROUND catches the first usable frame.
    RETILE_PENDING.store(false, Ordering::SeqCst);
    tile_current_layout_now();
    panel::invalidate_all();
    if std::env::var_os("ALT_DWM_VERBOSE").is_some() {
        println!("[manager] instant first layout for {:?}", hwnd.0);
    }
}

fn restore_initial_transition_settings() {
    let now = Instant::now();
    let ready: Vec<isize> = {
        let mut pending = TRANSITIONS_TO_RESTORE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let ready = pending
            .iter()
            .filter_map(|(hwnd, deadline)| (*deadline <= now).then_some(*hwnd))
            .collect::<Vec<_>>();
        for hwnd in &ready {
            pending.remove(hwnd);
        }
        ready
    };
    for raw in ready {
        util::set_transitions_forced_disabled(HWND(raw as *mut std::ffi::c_void), false);
    }
}

fn vk_from_name(name: &str) -> Option<u32> {
    let s = name.trim().to_lowercase();
    if s.len() == 1 {
        let c = s.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Some((c as u8).to_ascii_uppercase() as u32);
        }
        if c.is_ascii_digit() {
            return Some(c as u32);
        }
        // single symbols: , . ; / [ ] \ ` - = '
        return match c {
            ',' => Some(0xBC),
            '.' => Some(0xBE),
            ';' => Some(0xBA),
            '/' => Some(0xBF),
            '[' => Some(0xDB),
            ']' => Some(0xDD),
            '\\' => Some(0xDC),
            '`' | '~' => Some(0xC0),
            '-' | '_' => Some(0xBD),
            '=' | '+' => Some(0xBB),
            '\'' | '"' => Some(0xDE),
            _ => None,
        };
    }
    match s.as_str() {
        "space" => Some(0x20),
        "enter" | "return" => Some(0x0D),
        "tab" => Some(0x09),
        "esc" | "escape" => Some(0x1B),
        "backspace" => Some(0x08),
        "delete" | "del" => Some(0x2E),
        "insert" | "ins" => Some(0x2D),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pageup" | "pgup" => Some(0x21),
        "pagedown" | "pgdn" => Some(0x22),
        "left" => Some(0x25),
        "up" => Some(0x26),
        "right" => Some(0x27),
        "down" => Some(0x28),
        "pause" => Some(0x13),
        "print" | "printscreen" => Some(0x2C),
        _ if s.len() > 1 && s.starts_with('f') => {
            if let Ok(n) = s[1..].parse::<u32>() {
                if (1..=24).contains(&n) {
                    return Some(0x70 + n - 1);
                }
            }
            None
        }
        _ => None,
    }
}

fn parse_hotkey(keys: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut vk: Option<u32> = None;
    for part in keys.split('+') {
        let p = part.trim();
        let lp = p.to_lowercase();
        match lp.as_str() {
            "win" | "windows" | "super" | "mod4" | "os" => mods |= MOD_WIN,
            "shift" => mods |= MOD_SHIFT,
            "ctrl" | "control" | "ctl" => mods |= MOD_CONTROL,
            "alt" | "mod1" => mods |= MOD_ALT,
            _ => {
                if vk.is_none() {
                    vk = vk_from_name(p);
                    if vk.is_none() {
                        eprintln!("[hotkey] unknown key '{}' in '{}'", p, keys);
                        return None;
                    }
                } else {
                    eprintln!("[hotkey] extra key '{}' in '{}'", p, keys);
                    return None;
                }
            }
        }
    }
    let vk = vk?;
    mods |= MOD_NOREPEAT;
    Some((mods, vk))
}

fn register_keybinds(cfg: &config::Config) {
    // clear old
    let mut map = HOTKEY_ACTIONS.lock().unwrap_or_else(|e| e.into_inner());
    // unregister previous ids
    for id in map.keys().copied().collect::<Vec<_>>() {
        unsafe {
            let _ = UnregisterHotKey(None, id);
        }
    }
    map.clear();
    drop(map);
    ACTIVE_KEYBINDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();

    let mut next_id = 1;
    for kb in &cfg.keybinds {
        if let Some((mods, vk)) = parse_hotkey(&kb.keys) {
            let id = next_id;
            next_id += 1;
            unsafe {
                match RegisterHotKey(None, id, mods, vk) {
                    Ok(_) => {
                        println!("[hotkey] {} -> '{}' id={}", kb.keys, kb.action, id);
                        HOTKEY_ACTIONS
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(id, kb.action.clone());
                        ACTIVE_KEYBINDS
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(kb.clone());
                    }
                    Err(e) => eprintln!("[hotkey] failed {} -> '{}': {:?}", kb.keys, kb.action, e),
                }
            }
        } else {
            eprintln!("[hotkey] skip invalid '{}'", kb.keys);
        }
    }
    if HOTKEY_ACTIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
    {
        eprintln!("[hotkey] no keybinds registered! check config.toml");
    }
}

fn unregister_all_hotkeys() {
    let map = HOTKEY_ACTIONS.lock().unwrap_or_else(|e| e.into_inner());
    for id in map.keys().copied().collect::<Vec<_>>() {
        unsafe {
            let _ = UnregisterHotKey(None, id);
        }
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    if id_object != 0 {
        return;
    }
    if hwnd.0.is_null() {
        return;
    }
    if shell::is_native_taskbar(hwnd) {
        if CURRENT_CONFIG
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .general
            .hide_native_taskbar
        {
            shell::hide_native_taskbar(hwnd);
        }
        return;
    }
    // Show/hide traffic generated by a workspace switch is AltDWM's own doing.
    if workspace::is_switching()
        && matches!(
            event,
            EVENT_OBJECT_SHOW | EVENT_OBJECT_HIDE | EVENT_OBJECT_LOCATIONCHANGE
        )
    {
        return;
    }
    if event == EVENT_OBJECT_DESTROY {
        rules::forget_window(hwnd);
        widgets::forget_icon(hwnd);
        workspace::forget_window(hwnd);
        TRANSITIONS_TO_RESTORE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(hwnd.0 as isize));
    }
    // on_create rules — run even if tiling disabled, but only for CREATE/SHOW
    if event == EVENT_OBJECT_CREATE || event == EVENT_OBJECT_SHOW {
        rules::maybe_run_on_create(hwnd);
    }
    if event == EVENT_SYSTEM_MOVESIZESTART {
        manager::begin_interactive_move(hwnd);
        return;
    }
    if event == EVENT_SYSTEM_MOVESIZEEND {
        manager::finish_interactive_move(hwnd);
    }
    // Only events that change what a panel displays are worth a repaint.
    // Repainting on every LOCATIONCHANGE meant dragging one window redrew every
    // bar at event rate, and each redraw re-enumerated every top-level window.
    let changes_panel_contents = matches!(
        event,
        EVENT_SYSTEM_FOREGROUND
            | EVENT_OBJECT_CREATE
            | EVENT_OBJECT_DESTROY
            | EVENT_OBJECT_SHOW
            | EVENT_OBJECT_HIDE
            | EVENT_SYSTEM_MINIMIZESTART
            | EVENT_SYSTEM_MINIMIZEEND
    );
    if changes_panel_contents {
        manager::invalidate_window_snapshot();
        panel::invalidate_all();
    }
    if matches!(
        event,
        EVENT_SYSTEM_FOREGROUND | EVENT_OBJECT_SHOW | EVENT_OBJECT_LOCATIONCHANGE
    ) {
        panel::sync_fullscreen(windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow());
    }
    if event == EVENT_SYSTEM_FOREGROUND {
        // Remembered so workspace switching still targets the display the user
        // was working on after its focused window has been hidden.
        workspace::note_focus(hwnd);
        manager::refresh_window_borders();
    }
    let was_tracked = manager::is_tracked_window(hwnd);
    let is_first_layout_signal = matches!(
        event,
        EVENT_OBJECT_CREATE | EVENT_OBJECT_SHOW | EVENT_SYSTEM_FOREGROUND
    );
    if event == EVENT_SYSTEM_FOREGROUND && was_tracked {
        return;
    }
    // Location changes cover maximize/restore and keyboard-driven moves. During
    // a mouse drag, wait for MOVESIZEEND so AltDWM does not fight the pointer.
    // Ignore the exact rectangle assigned by our own most recent layout pass.
    if event == EVENT_OBJECT_LOCATIONCHANGE
        && (manager::is_move_active(hwnd) || manager::is_expected_location(hwnd))
    {
        return;
    }
    // Accessibility and shell UI can emit window-object events too. Only a
    // newly manageable HWND or one already known to the manager can affect the
    // layout; ignoring the rest prevents tray/UIA activity from causing retiles.
    let affects_layout = was_tracked || util::is_manageable(hwnd);
    if !affects_layout {
        return;
    }
    if !TILING_ENABLED.load(Ordering::SeqCst) {
        return;
    }
    let (auto, instant_first_layout) = {
        let general = CURRENT_CONFIG
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .general
            .clone();
        (general.auto_tile, general.instant_first_layout)
    };
    if !auto {
        return;
    }
    if instant_first_layout && is_first_layout_signal && !was_tracked {
        place_new_window_immediately(hwnd);
        return;
    }
    RETILE_PENDING.store(true, Ordering::SeqCst);
}

static HOST_HWND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "system" fn host_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(hwnd), 100, 200, None);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == 100 {
                restore_initial_transition_settings();
                // handle reload request if pending (checked via scripting flag? for now just retile)
                if RETILE_PENDING.load(Ordering::SeqCst) && TILING_ENABLED.load(Ordering::SeqCst) {
                    RETILE_PENDING.store(false, Ordering::SeqCst);
                    tile_current_layout_now();
                }
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            // Resolution changes, docking, and monitor hotplug all arrive here.
            // Panels were previously left at their stale positions until the
            // configuration happened to be reloaded.
            println!("[host] display configuration changed — re-placing panels");
            manager::clear_layout_overrides();
            manager::invalidate_window_snapshot();
            panel::reposition_all();
            desktop::reposition();
            request_retile();
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(hwnd), 100);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn create_host_window() -> Result<HWND, String> {
    unsafe {
        let hinstance = HINSTANCE(std::ptr::null_mut());
        let class_name = w!("AltDWM_Host");
        util::register_window_class(class_name, host_wndproc, "Host")?;
        let hwnd = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            class_name,
            w!("AltDWM Host"),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            Some(HWND_MESSAGE),
            Some(HMENU(std::ptr::null_mut())),
            Some(hinstance),
            None,
        )
        .map_err(|e| format!("Host CreateWindowExW failed: {:?}", e))?;
        HOST_HWND.store(hwnd.0 as usize, Ordering::SeqCst);
        println!("[host] message-only window hwnd={:?}", hwnd.0);
        Ok(hwnd)
    }
}

fn print_banner() {
    println!(
        r#"
  ___   _ _   ___  _ _ _ _  
 / _ \ | | | |   \| | | | | 
| |_| || | | | |) | | | | |  AltDWM 0.3.0 - Native Windows Shell
 \___/ |_|_| |___/|_|_|_|_|  Rust + Win32 + DWM (declarative panels + Rhai)
"#
    );
    // defaults are Alt+Shift to avoid Win+Shift system collisions (Win+Shift+S = Snipping Tool)
    println!("  Hotkeys (Alt+Shift+): R=retile T=toggle Q=quit G=grid M=monocle F=float S=master C=reload J/K/H/L=focus  (configurable)");
    println!("  ---");
}

fn print_help() {
    println!(
        r#"Usage: alt-dwm [OPTIONS]

Options:
  --config <path>     Use explicit config.toml path
  --generate-config   Write example config to default path and exit
  --check-config      Validate config and exit
  --list-apps [q]     List indexed applications (optionally matching a query)
  --status            Print live system state (audio, power, network, input)
  --list-tray         Host the notification area briefly and print what arrives
  --list-startup      List startup items and their enabled/disabled state
  --restore-windows   Un-hide any window a previous run left on a workspace
  --no-taskbar        Disable taskbar/panels (only tiling)
  --gap <px>          Override gap (default from config)
  --layout <name>     Override layout: masterstack, grid, monocle, floating
  --help              Show this help
  --replace-shell     Print registry command to replace explorer.exe

Examples:
  alt-dwm
  alt-dwm --config ./config.toml
  alt-dwm --gap 12 --layout grid
  alt-dwm --generate-config
  alt-dwm --check-config
  alt-dwm --list-tray
  alt-dwm --list-startup

Config search: exe_dir/config.toml -> %APPDATA%/AltDWM/config.toml -> ./config.toml
DSL: see docs/EXTENSIBILITY.md + examples/config.example.toml
"#
    );
}

fn do_generate_config(explicit: Option<&std::path::Path>) {
    let path = config::find_config_path(explicit).unwrap_or_else(config::default_config_path);
    let cfg = config::example_config_with_panels();
    match config::save_to_path(&cfg, &path) {
        Ok(_) => {
            println!("Generated example config at {}", path.display());
            match scripted_widget::export_builtin_scripts(&path) {
                Ok(count) => {
                    println!("Installed {count} editable widget script(s) beside the config")
                }
                Err(error) => {
                    eprintln!("Failed to install widget scripts: {error}");
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to generate config: {}", e);
            std::process::exit(1);
        }
    }
    std::process::exit(0);
}

/// Host the notification area for a moment and print every icon that arrives.
///
/// This genuinely takes the tray over while it runs — that is the only way to
/// see any of it — and hands it back before returning. Without this there is no
/// way to tell "no application published an icon" apart from "the widget is
/// broken", which is the question this whole subsystem tends to raise.
fn do_list_tray() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    println!("Hosting the notification area for a moment (Explorer gets it back on exit)...");
    tray::start(tray::Source::Native, true);
    tray::announce();
    // Applications answer `TaskbarCreated` on their own message loops, so this
    // is a wait for other processes to get around to it, not for work of ours.
    for _ in 0..120 {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let entries = tray::entries();
    if entries.is_empty() {
        println!("No application published a notification icon.");
    } else {
        println!("{} icon(s):", entries.len());
        for entry in &entries {
            println!(
                "  {:<28} icon={} owner={:<18} {}",
                tray::title_line(&entry.name),
                if entry.icon != 0 { "yes" } else { "no" },
                if entry.process.is_empty() {
                    "unknown"
                } else {
                    &entry.process
                },
                if entry.hidden { "(hidden)" } else { "" }
            );
        }
    }
    tray::shutdown();
    std::process::exit(0);
}

/// Print what the system readers actually see.
///
/// Each of volume, brightness, network, and battery is optional, and a machine
/// that cannot report one is indistinguishable from a bug in the reader unless
/// there is a way to look.
fn do_status() {
    // The worker polls on an interval; give it one cycle to publish.
    system::refresh();
    // Waiting for the status to differ from the default would mean waiting the
    // full timeout on a machine that genuinely reports nothing.
    for _ in 0..60 {
        if system::has_polled() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = system::status();
    println!(
        "audio      {}",
        match status.volume {
            Some(volume) if volume.muted => format!("{}% (muted)", volume.percent()),
            Some(volume) => format!("{}%", volume.percent()),
            None => "unavailable (no default render endpoint)".into(),
        }
    );
    println!(
        "brightness {}",
        match status.brightness {
            Some(brightness) => format!("{}%", brightness.percent),
            None => "unavailable (no monitor answered DDC/CI)".into(),
        }
    );
    println!("network    {:?}", status.network);
    println!(
        "wifi radio {}",
        match status.wifi_radio_on {
            Some(true) => "on".into(),
            Some(false) => "off".into(),
            None => "no wlan interface".to_string(),
        }
    );
    println!(
        "battery    {}",
        match status.battery {
            Some(battery) => format!(
                "{}{}{}",
                battery
                    .percent
                    .map(|percent| format!("{percent}%"))
                    .unwrap_or_else(|| "unknown".into()),
                if battery.charging {
                    " charging"
                } else if battery.on_ac {
                    " on ac"
                } else {
                    " discharging"
                },
                battery
                    .minutes_remaining
                    .map(|minutes| format!(" {}h{:02}m left", minutes / 60, minutes % 60))
                    .unwrap_or_default()
            ),
            None => "no battery".into(),
        }
    );
    let layouts = input::installed();
    println!(
        "keyboard   {} ({} installed: {})",
        input::current()
            .map(|layout| layout.name)
            .unwrap_or_else(|| "unknown".into()),
        layouts.len(),
        layouts
            .iter()
            .map(|layout| layout.tag.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    std::process::exit(0);
}

/// Print the application index, so a launcher problem can be told apart from a
/// search-ranking problem without guessing.
fn do_list_apps(query: &str) {
    apps::begin_indexing();
    for _ in 0..200 {
        if apps::is_ready() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if !apps::is_ready() {
        eprintln!("application index did not finish in time");
        std::process::exit(1);
    }
    let matches = apps::search(query, 200);
    if query.is_empty() {
        println!("{} applications indexed", apps::count());
    } else {
        println!(
            "{} of {} applications match '{}', best first",
            matches.len(),
            apps::count(),
            query
        );
    }
    for entry in matches {
        println!("  {:<44}  {}", entry.name, entry.id);
    }
    std::process::exit(0);
}

fn do_check_config(explicit: Option<&std::path::Path>) {
    let (mut cfg, path) = match config::load_existing(explicit) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("Config validation failed: {error}");
            std::process::exit(1);
        }
    };
    *CONFIG_PATH
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(path.clone());
    // Report the configuration the runtime will actually build, including the
    // bar synthesised for `taskbar = true` with no [[panels]] declared.
    config::ensure_default_bar(&mut cfg);
    println!("Config: {}", path.display());
    println!(
        "general: gap={} layout={} taskbar={}",
        cfg.general.gap, cfg.general.layout, cfg.general.taskbar
    );
    let warns = runtime_validation_errors(&cfg);
    if warns.is_empty() {
        println!("validate: ok");
    } else {
        for warning in warns {
            eprintln!("invalid: {warning}");
        }
        std::process::exit(1);
    }
    println!(
        "panels: {}  widgets: {}  rules: {}  keybinds: {}",
        cfg.panels.len(),
        cfg.widgets.len(),
        cfg.rules.len(),
        cfg.keybinds.len()
    );
    std::process::exit(0);
}

fn runtime_validation_errors(cfg: &config::Config) -> Vec<String> {
    let mut errors = cfg.validate();
    errors.extend(scripted_widget::validate_config_scripts(cfg));
    for keybind in &cfg.keybinds {
        if parse_hotkey(&keybind.keys).is_none() {
            errors.push(format!("invalid keybind '{}'", keybind.keys));
        }
    }
    errors
}

fn apply_cli_overrides(cfg: &mut config::Config, overrides: &[(String, String)]) {
    for (key, value) in overrides {
        match key.as_str() {
            "no-taskbar" => {
                cfg.general.taskbar = false;
                cfg.panels.clear();
            }
            "gap" => {
                if let Ok(gap) = value.parse::<i32>() {
                    cfg.general.gap = gap.max(0);
                }
            }
            "layout" => cfg.general.layout = value.clone(),
            _ => {}
        }
    }
}

fn reload_existing_config(
    overrides: &[(String, String)],
) -> Result<(config::Config, PathBuf), String> {
    let explicit = CONFIG_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let (mut cfg, path) = config::load_existing(explicit.as_deref())?;
    apply_cli_overrides(&mut cfg, overrides);
    let warnings = runtime_validation_errors(&cfg);
    if !warnings.is_empty() {
        return Err(warnings.join("; "));
    }
    Ok((cfg, path))
}

fn apply_config_reload(
    mut new_cfg: config::Config,
    new_path: Option<PathBuf>,
    panel_handles: &mut Vec<HWND>,
) {
    command_center::close();
    config::ensure_default_bar(&mut new_cfg);
    manager::clear_layout_overrides();
    for w in new_cfg.validate() {
        eprintln!("[config] warn: {}", w);
    }
    *CURRENT_GAP.lock().unwrap_or_else(|e| e.into_inner()) = new_cfg.general.gap;
    *CURRENT_LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = new_cfg.layout_enum();
    *CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = new_cfg.clone();
    desktop::configure(new_cfg.general.desktop);
    // A reload can shrink the workspace count; windows left beyond it would be
    // hidden with no way to reach them.
    workspace::clamp_to_count();
    *CONFIG_PATH.lock().unwrap_or_else(|e| e.into_inner()) = new_path.clone();
    tray::start(
        tray::Source::parse(&new_cfg.general.tray),
        new_cfg.general.hide_native_taskbar,
    );
    shell::set_native_taskbars_hidden(new_cfg.general.hide_native_taskbar);
    tray::announce();
    register_keybinds(&new_cfg);
    panel::destroy_panels();
    panel_handles.clear();
    if !new_cfg.panels.is_empty() {
        match panel::create_panels(&new_cfg) {
            Ok(hs) => *panel_handles = hs,
            Err(e) => {
                eprintln!("[panel] recreate failed: {}", e);
                panel::destroy_panels();
                new_cfg.panels.clear();
                *CURRENT_CONFIG
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = new_cfg.clone();
            }
        }
    }
    if let Some(p) = new_path {
        watcher::spawn_watcher(p);
    }
}

fn reload_and_apply_config(overrides: &[(String, String)], panel_handles: &mut Vec<HWND>) {
    match reload_existing_config(overrides) {
        Ok((new_cfg, new_path)) => {
            apply_config_reload(new_cfg, Some(new_path), panel_handles);
            request_retile();
        }
        Err(error) => {
            eprintln!("[config] reload rejected; keeping current config: {error}");
        }
    }
}

/// Put back everything AltDWM borrowed from the system: the native taskbar it
/// hid and any window it made translucent.
///
/// Safe to call more than once, and safe to call from a panic hook: the guard
/// stops a panic raised *inside* this work from re-entering it and deadlocking
/// on a lock the unwinding thread already holds.
fn release_borrowed_system_state() {
    static RELEASING: AtomicBool = AtomicBool::new(false);
    if RELEASING.swap(true, Ordering::SeqCst) {
        return;
    }
    // Hidden windows come back first: a user left with no shell and no windows
    // cannot tell the difference between hidden and lost.
    workspace::restore_all();
    rules::restore_all_opacity();
    // Before the taskbar comes back, so the "re-register your icons" broadcast
    // does not race Explorer's own window into the topmost band.
    tray::shutdown();
    shell::restore_native_taskbars();
    RELEASING.store(false, Ordering::SeqCst);
}

/// The native taskbar is hidden by forcing a zero-alpha layered style on
/// Explorer's own window, and released on the normal shutdown path. With
/// `panic = "abort"` in the release profile there is no unwinding, so without
/// these handlers a crash or a Ctrl+C left the user with no taskbar and no
/// obvious way to get it back.
fn install_crash_safety() {
    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT,
    };
    use windows::Win32::System::Diagnostics::Debug::{
        SetUnhandledExceptionFilter, EXCEPTION_POINTERS,
    };

    unsafe extern "system" fn on_console_ctrl(event: u32) -> windows::core::BOOL {
        if matches!(
            event,
            CTRL_C_EVENT
                | CTRL_BREAK_EVENT
                | CTRL_CLOSE_EVENT
                | CTRL_LOGOFF_EVENT
                | CTRL_SHUTDOWN_EVENT
        ) {
            eprintln!("[main] console signal {event} — restoring shell chrome");
            release_borrowed_system_state();
        }
        // Report as unhandled so the default terminate behaviour still runs.
        windows::core::BOOL(0)
    }

    unsafe extern "system" fn on_unhandled_exception(_info: *const EXCEPTION_POINTERS) -> i32 {
        eprintln!("[main] unhandled exception — restoring shell chrome before exit");
        release_borrowed_system_state();
        // EXCEPTION_CONTINUE_SEARCH: let the default handler report the crash.
        0
    }

    unsafe {
        let _ = SetConsoleCtrlHandler(Some(on_console_ctrl), true);
        SetUnhandledExceptionFilter(Some(on_unhandled_exception));
    }
    // A Rust panic in debug builds unwinds instead of aborting, so cover that
    // path too rather than relying on the exception filter.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        release_borrowed_system_state();
        previous(info);
    }));
}

fn main() {
    match elevation::relaunch_installed_if_needed() {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("[elevation] {error}");
            return;
        }
    }
    let _ = MAIN_TID.set(unsafe { windows::Win32::System::Threading::GetCurrentThreadId() });
    install_crash_safety();
    print_banner();
    virtual_desktop::init();

    // --- early arg scan for --config / --generate-config / --check-config / --help
    let args: Vec<String> = std::env::args().collect();
    let mut explicit_cfg: Option<PathBuf> = None;
    let mut do_generate = false;
    let mut do_check = false;
    let mut cli_overrides: Vec<(String, String)> = Vec::new();
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--config" => {
                if let Some(v) = iter.next() {
                    explicit_cfg = Some(PathBuf::from(v));
                }
            }
            "--generate-config" => do_generate = true,
            "--check-config" => do_check = true,
            "--list-apps" => {
                let query = iter.next().cloned().unwrap_or_default();
                do_list_apps(&query);
            }
            "--status" => do_status(),
            "--list-tray" => do_list_tray(),
            "--list-startup" => startup::print_entries_and_exit(),
            "--restore-windows" => {
                // Recovery for the one case no in-process handler can catch: a
                // hard kill while windows were hidden on another workspace.
                let restored = workspace::restore_from_journal();
                if restored == 0 {
                    println!("No windows were left hidden.");
                } else {
                    println!("Restored {restored} window(s).");
                }
                std::process::exit(0);
            }
            "--replace-shell" => {
                let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("alt-dwm.exe"));
                println!("\nTo REPLACE explorer.exe (shell) with AltDWM:");
                println!("  1. Copy {} to C:\\AltDWM\\alt-dwm.exe", exe.display());
                println!("  2. Run as ADMIN: reg add \"HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon\" /v Shell /t REG_SZ /d \"C:\\AltDWM\\alt-dwm.exe\" /f");
                println!("  3. Logoff/reboot. Test: taskkill /f /im explorer.exe -> alt-dwm will tile -> explorer.exe to restore");
                println!("  Restore: reg add \"HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon\" /v Shell /t REG_SZ /d \"explorer.exe\" /f");
                std::process::exit(0);
            }
            "--no-taskbar" => cli_overrides.push(("no-taskbar".into(), "".into())),
            "--gap" => {
                if let Some(v) = iter.next() {
                    cli_overrides.push(("gap".into(), v.clone()));
                }
            }
            "--layout" => {
                if let Some(v) = iter.next() {
                    cli_overrides.push(("layout".into(), v.clone()));
                }
            }
            other => eprintln!("Unknown arg '{}' -- use --help", other),
        }
    }

    if do_generate {
        do_generate_config(explicit_cfg.as_deref());
    }
    if do_check {
        do_check_config(explicit_cfg.as_deref());
    }

    // --- load config
    let (mut cfg, cfg_path) = config::load_or_default(explicit_cfg.as_deref());
    *CONFIG_PATH.lock().unwrap_or_else(|e| e.into_inner()) = cfg_path.clone();
    // apply CLI overrides (they win over file)
    apply_cli_overrides(&mut cfg, &cli_overrides);
    // validate
    for w in cfg.validate() {
        eprintln!("[config] warn: {}", w);
    }

    // `general.taskbar = true` with no panels declared means "give me a bar";
    // synthesise one rather than running a second, less capable bar implementation.
    config::ensure_default_bar(&mut cfg);

    // push to globals
    *CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = cfg.clone();
    *CURRENT_GAP.lock().unwrap_or_else(|e| e.into_inner()) = cfg.general.gap;
    *CURRENT_LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = cfg.layout_enum();
    TILING_ENABLED.store(true, Ordering::SeqCst);

    // spawn file watcher for auto-reload (config.toml -> hot-reload without restart)
    if let Some(p) = cfg_path.clone() {
        watcher::spawn_watcher(p);
    } else {
        watcher::spawn_watcher(config::default_config_path());
    }

    let gap = cfg.general.gap;
    let layout = cfg.layout_enum();
    println!("[main] config {:?} — gap={} layout={} taskbar={} panels={} widgets={} rules={} keybinds={}",
        cfg_path, gap, layout.name(), cfg.general.taskbar, cfg.panels.len(), cfg.widgets.len(), cfg.rules.len(), cfg.keybinds.len());
    println!(
        "[main] pid={} exe={:?}",
        std::process::id(),
        std::env::current_exe().unwrap_or_default()
    );

    let host_hwnd = match create_host_window() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[main] host failed: {}", e);
            return;
        }
    };

    if let Err(error) = desktop::start(cfg.general.desktop) {
        eprintln!("[desktop] {error} — continuing without a desktop surface");
    }

    // Order matters. The host window has to exist before Explorer's taskbar is
    // pushed out of the way, and the broadcast that makes applications
    // re-publish their icons has to come after — otherwise they resolve
    // `FindWindow("Shell_TrayWnd")` to Explorer's window and hand their icons to
    // a taskbar nobody can see.
    tray::start(
        tray::Source::parse(&cfg.general.tray),
        cfg.general.hide_native_taskbar,
    );
    shell::set_native_taskbars_hidden(cfg.general.hide_native_taskbar);
    tray::announce();
    startup::launch_for_shell_once(cfg.general.launch_startup_apps);

    let mut panel_handles: Vec<HWND> = Vec::new();
    if !cfg.panels.is_empty() {
        match panel::create_panels(&cfg) {
            Ok(hs) => panel_handles = hs,
            Err(e) => {
                eprintln!("[panel] failed: {} — continuing without a bar", e);
                panel::destroy_panels();
                cfg.panels.clear();
                *CURRENT_CONFIG
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = cfg.clone();
            }
        }
    }

    // Before anything else touches window state: if a previous run was killed
    // outright while windows were hidden on another workspace, bring them back.
    // Nothing in-process can catch TerminateProcess, so recovery has to happen
    // on the way in rather than on the way out.
    workspace::restore_from_journal();

    // The application index backs the command center's search. Building it
    // takes a moment, so it starts now rather than on the first keystroke.
    apps::begin_indexing();

    // hotkeys — dynamic from config.toml [[keybinds]]
    register_keybinds(&cfg);

    let mut hooks: Vec<HWINEVENTHOOK> = Vec::new();
    unsafe {
        let mut try_hook = |event_min: u32, event_max: u32, label: &str| {
            let h = SetWinEventHook(
                event_min,
                event_max,
                None,
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            if h.0.is_null() {
                eprintln!(
                    "[hook] {} failed: {:?}",
                    label,
                    windows::Win32::Foundation::GetLastError()
                );
            } else {
                println!(
                    "[hook] {} 0x{:x}-0x{:x} => {:?}",
                    label, event_min, event_max, h.0
                );
                hooks.push(h);
            }
        };
        try_hook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            "FOREGROUND",
        );
        try_hook(
            EVENT_SYSTEM_MINIMIZESTART,
            EVENT_SYSTEM_MINIMIZEEND,
            "MINIMIZE",
        );
        try_hook(
            EVENT_SYSTEM_MOVESIZESTART,
            EVENT_SYSTEM_MOVESIZESTART,
            "MOVESIZESTART",
        );
        try_hook(
            EVENT_SYSTEM_MOVESIZEEND,
            EVENT_SYSTEM_MOVESIZEEND,
            "MOVESIZEEND",
        );
        try_hook(EVENT_OBJECT_CREATE, EVENT_OBJECT_CREATE, "CREATE");
        try_hook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_DESTROY, "DESTROY");
        try_hook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, "SHOW");
        try_hook(EVENT_OBJECT_HIDE, EVENT_OBJECT_HIDE, "HIDE");
        try_hook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            "LOCATIONCHANGE",
        );
    }

    if TILING_ENABLED.load(Ordering::SeqCst) {
        let (top, bottom) = bar_reserves(&cfg);
        manager::tile_windows_reserved(top, bottom, layout, gap);
    }

    println!("[main] message loop — Alt+Shift+Q quit, Alt+Shift+C reload");
    let mut msg = MSG::default();
    unsafe {
        loop {
            // Checked before dispatch so a pending reload cannot be stranded by
            // the WM_HOTKEY path, which skips the tail of the loop body.
            let watcher_reload = watcher::should_reload();
            if watcher_reload || CONFIG_RELOAD_PENDING.swap(false, Ordering::SeqCst) {
                println!(
                    "[config] {} -> reloading",
                    if watcher_reload {
                        "file changed"
                    } else {
                        "action requested"
                    }
                );
                reload_and_apply_config(&cli_overrides, &mut panel_handles);
            }
            let ret = GetMessageW(&mut msg, None, 0, 0);
            if ret.0 == 0 {
                println!("[main] WM_QUIT");
                break;
            }
            if ret.0 == -1 {
                eprintln!(
                    "[main] GetMessageW {:?}",
                    windows::Win32::Foundation::GetLastError()
                );
                break;
            }
            if msg.message == WM_HOTKEY {
                let id = msg.wParam.0 as i32;
                let action = {
                    HOTKEY_ACTIONS
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&id)
                        .cloned()
                };
                if let Some(act) = action {
                    println!("[hotkey] id={} -> '{}'", id, act);
                    if act == "quit" {
                        break;
                    } else if act == "reload_config" {
                        println!("[hotkey] reload config");
                        reload_and_apply_config(&cli_overrides, &mut panel_handles);
                    } else if act == "retile" {
                        let l = *CURRENT_LAYOUT.lock().unwrap_or_else(|e| e.into_inner());
                        let g = *CURRENT_GAP.lock().unwrap_or_else(|e| e.into_inner());
                        let cfg = CURRENT_CONFIG
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        let (top, bottom) = bar_reserves(&cfg);
                        manager::tile_windows_reserved(top, bottom, l, g);
                    } else {
                        // delegate to scripting engine (handles toggle_tiling, set_layout, launch, rhai: ...)
                        scripting::dispatch_action(&act);
                    }
                } else {
                    eprintln!("[hotkey] unknown id {}", id);
                }
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        unregister_all_hotkeys();
        for h in hooks {
            let _ = UnhookWinEvent(h);
        }
        panel::destroy_panels();
        command_center::close();
        quick_settings::close();
        desktop::shutdown();
        release_borrowed_system_state();
        let _ = host_hwnd;
        let _ = panel_handles;
    }
    println!("[main] bye");
}

#[cfg(test)]
mod tests {
    use super::{apply_cli_overrides, parse_hotkey, runtime_validation_errors};
    use crate::config::Config;

    #[test]
    fn cli_overrides_are_reusable_for_reload() {
        let mut config = Config::default();
        config.panels.push(Default::default());
        apply_cli_overrides(
            &mut config,
            &[
                ("no-taskbar".into(), String::new()),
                ("gap".into(), "12".into()),
                ("layout".into(), "Grid".into()),
            ],
        );
        assert!(!config.general.taskbar);
        assert!(config.panels.is_empty());
        assert_eq!(config.general.gap, 12);
        assert_eq!(config.general.layout, "Grid");
    }

    #[test]
    fn hotkey_parser_rejects_multiple_primary_keys() {
        assert!(parse_hotkey("Alt+Shift+R+T").is_none());
        assert!(parse_hotkey("Alt+Shift+R").is_some());
    }

    #[test]
    fn runtime_validation_rejects_invalid_hotkeys() {
        let mut config = Config::default();
        config.keybinds[0].keys = "Alt+Shift+R+T".into();
        assert!(runtime_validation_errors(&config)
            .iter()
            .any(|error| error.contains("invalid keybind")));
    }
}

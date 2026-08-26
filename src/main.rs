mod config;
mod focus;
mod layout;
mod manager;
mod panel;
mod rules;
mod scripting;
mod taskbar;
mod theme;
mod util;
mod virtual_desktop;
mod watcher;
mod widgets;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, LazyLock};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage, RegisterClassExW,
    TranslateMessage, HMENU, HWND_MESSAGE, MSG, WM_HOTKEY, WM_CREATE, WM_DESTROY, WM_TIMER, WNDCLASSEXW, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_SHOW,
    EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

use layout::Layout;
use windows::core::w;

// ------------------------------------------------------------------
// Global state — pub for scripting/panel/util access
// ------------------------------------------------------------------
pub static RETILE_PENDING: AtomicBool = AtomicBool::new(false);
pub static CONFIG_RELOAD_PENDING: AtomicBool = AtomicBool::new(false);
pub static TILING_ENABLED: AtomicBool = AtomicBool::new(true);
pub static TASKBAR_ENABLED: AtomicBool = AtomicBool::new(true);

pub static CURRENT_LAYOUT: LazyLock<Mutex<Layout>> = LazyLock::new(|| Mutex::new(Layout::MasterStack));
pub static CURRENT_GAP: LazyLock<Mutex<i32>> = LazyLock::new(|| Mutex::new(8));
pub static CONFIG_PATH: LazyLock<Mutex<Option<PathBuf>>> = LazyLock::new(|| Mutex::new(None));
pub static CURRENT_CONFIG: LazyLock<Mutex<config::Config>> = LazyLock::new(|| Mutex::new(config::Config::default()));
pub static HOTKEY_ACTIONS: LazyLock<Mutex<HashMap<i32, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
pub static MAIN_TID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

// helpers for scripting / manager
pub fn request_retile() { RETILE_PENDING.store(true, Ordering::SeqCst); }
pub fn request_quit() {
    let v = HOST_HWND.load(Ordering::SeqCst);
    if v != 0 {
        let hwnd = HWND(v as *mut std::ffi::c_void);
        unsafe { let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(hwnd), windows::Win32::UI::WindowsAndMessaging::WM_CLOSE, WPARAM(0), LPARAM(0)); }
    } else if let Some(tid) = MAIN_TID.get().copied() {
        unsafe { let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(tid, windows::Win32::UI::WindowsAndMessaging::WM_QUIT, WPARAM(0), LPARAM(0)); }
    } else {
        unsafe { windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0); }
    }
}
pub fn toggle_tiling() {
    let enabled = !TILING_ENABLED.load(Ordering::SeqCst);
    TILING_ENABLED.store(enabled, Ordering::SeqCst);
    println!("[main] Tiling {}", if enabled { "ENABLED" } else { "DISABLED" });
    if enabled { request_retile(); }
}
pub fn set_layout_by_name(name: &str) {
    let normalized = name.trim();
    let layout = match normalized.to_lowercase().as_str() {
        "grid" => Layout::Grid,
        "monocle" => Layout::Monocle,
        "floating" => Layout::Floating,
        _ => Layout::MasterStack,
    };
    *CURRENT_LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = layout;
    {
        let mut cfg = CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(custom_name) = cfg.layouts.keys().find(|key| key.eq_ignore_ascii_case(normalized)).cloned() {
            // Custom Rhai layouts use MasterStack only as their native fallback.
            cfg.general.layout = custom_name;
        } else {
            cfg.set_layout(layout);
        }
    }
    println!("[main] Layout -> {}", normalized);
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

fn vk_from_name(name: &str) -> Option<u32> {
    let s = name.trim().to_lowercase();
    if s.len() == 1 {
        let c = s.chars().next().unwrap();
        if c.is_ascii_alphabetic() { return Some((c as u8).to_ascii_uppercase() as u32); }
        if c.is_ascii_digit() { return Some(c as u32); }
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
                if (1..=24).contains(&n) { return Some(0x70 + n - 1); }
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
                    if vk.is_none() { eprintln!("[hotkey] unknown key '{}' in '{}'", p, keys); return None; }
                } else {
                    eprintln!("[hotkey] extra key '{}' in '{}'", p, keys); return None;
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
        unsafe { let _ = UnregisterHotKey(None, id); }
    }
    map.clear();
    drop(map);

    let mut next_id = 1;
    for kb in &cfg.keybinds {
        if let Some((mods, vk)) = parse_hotkey(&kb.keys) {
            let id = next_id; next_id += 1;
            unsafe {
                match RegisterHotKey(None, id, mods, vk) {
                    Ok(_) => {
                        println!("[hotkey] {} -> '{}' id={}", kb.keys, kb.action, id);
                        HOTKEY_ACTIONS.lock().unwrap_or_else(|e| e.into_inner()).insert(id, kb.action.clone());
                    }
                    Err(e) => eprintln!("[hotkey] failed {} -> '{}': {:?}", kb.keys, kb.action, e),
                }
            }
        } else {
            eprintln!("[hotkey] skip invalid '{}'", kb.keys);
        }
    }
    if HOTKEY_ACTIONS.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
        eprintln!("[hotkey] no keybinds registered! check config.toml");
    }
}

fn unregister_all_hotkeys() {
    let map = HOTKEY_ACTIONS.lock().unwrap_or_else(|e| e.into_inner());
    for id in map.keys().copied().collect::<Vec<_>>() {
        unsafe { let _ = UnregisterHotKey(None, id); }
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
    if id_object != 0 { return; }
    if hwnd.0.is_null() { return; }
    // on_create rules — run even if tiling disabled, but only for CREATE/SHOW
    if event == EVENT_OBJECT_CREATE || event == EVENT_OBJECT_SHOW {
        crate::rules::maybe_run_on_create(hwnd);
    }
    if !TILING_ENABLED.load(Ordering::SeqCst) { return; }
    let auto = { CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()).general.auto_tile };
    if !auto { return; }
    RETILE_PENDING.store(true, Ordering::SeqCst);
}

static HOST_HWND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "system" fn host_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(hwnd), 100, 200, None);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == 100 {
                // handle reload request if pending (checked via scripting flag? for now just retile)
                if RETILE_PENDING.load(Ordering::SeqCst) && TILING_ENABLED.load(Ordering::SeqCst) {
                    RETILE_PENDING.store(false, Ordering::SeqCst);
                    // compute panel/taskbar reservation — now top+bottom aware
                    let cfg = CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let gap = cfg.general.gap;
                    let layout = cfg.layout_enum();
                    let (top_reserve, bottom_reserve) = if !cfg.panels.is_empty() {
                        let top: i32 = cfg.panels.iter().filter(|p| p.position=="top").map(|p| p.height).sum();
                        let bottom: i32 = cfg.panels.iter().filter(|p| p.position=="bottom").map(|p| p.height).sum();
                        (top, bottom)
                    } else if cfg.general.taskbar { (0, cfg.general.taskbar_height) } else { (0,0) };
                    let taskbar_hwnd = taskbar::get_taskbar_hwnd();
                    let reserve = if !cfg.panels.is_empty() { None } else { taskbar_hwnd };
                    manager::tile_windows_reserved(reserve, top_reserve, bottom_reserve, layout, gap);
                }
            }
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
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(host_wndproc),
            cbClsExtra: 0, cbWndExtra: 0,
            hInstance: hinstance.into(),
            hIcon: Default::default(), hCursor: Default::default(), hbrBackground: Default::default(),
            lpszMenuName: windows::core::PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: Default::default(),
        };
        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            let err = windows::Win32::Foundation::GetLastError();
            if err.0 != 1410 { return Err(format!("Host RegisterClassExW failed: {:?}", err)); }
        }
        let hwnd = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            class_name, w!("AltDWM Host"),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0),
            CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT,
            Some(HWND_MESSAGE), Some(HMENU(std::ptr::null_mut())), Some(hinstance), None,
        ).map_err(|e| format!("Host CreateWindowExW failed: {:?}", e))?;
        HOST_HWND.store(hwnd.0 as usize, Ordering::SeqCst);
        println!("[host] message-only window hwnd={:?}", hwnd.0);
        Ok(hwnd)
    }
}

fn print_banner() {
    println!(r#"
  ___   _ _   ___  _ _ _ _  
 / _ \ | | | |   \| | | | | 
| |_| || | | | |) | | | | |  AltDWM 0.2.0 - Experimental Windows Shell
 \___/ |_|_| |___/|_|_|_|_|  Rust + Win32 + DWM (declarative panels + Rhai)
"#);
    // defaults are Alt+Shift to avoid Win+Shift system collisions (Win+Shift+S = Snipping Tool)
    println!("  Hotkeys (Alt+Shift+): R=retile T=toggle Q=quit G=grid M=monocle F=float S=master C=reload J/K/H/L=focus  (configurable)");
    println!("  ---");
}

fn print_help() {
    println!(r#"Usage: alt-dwm [OPTIONS]

Options:
  --config <path>     Use explicit config.toml path
  --generate-config   Write example config to default path and exit
  --check-config      Validate config and exit
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

Config search: ./config.toml -> exe_dir/config.toml -> %APPDATA%/AltDWM/config.toml -> ./config.toml
DSL: see docs/EXTENSIBILITY.md + examples/config.example.toml
"#);
}

fn do_generate_config(explicit: Option<&std::path::Path>) {
    let path = config::find_config_path(explicit).unwrap_or_else(config::default_config_path);
    let cfg = config::example_config_with_panels();
    match config::save_to_path(&cfg, &path) {
        Ok(_) => println!("Generated example config at {}", path.display()),
        Err(e) => { eprintln!("Failed to generate config: {}", e); std::process::exit(1); }
    }
    std::process::exit(0);
}

fn do_check_config(explicit: Option<&std::path::Path>) {
    let (cfg, path) = config::load_or_default(explicit);
    println!("Config: {:?}", path);
    println!("general: gap={} layout={} taskbar={}", cfg.general.gap, cfg.general.layout, cfg.general.taskbar);
    let warns = cfg.validate();
    if warns.is_empty() { println!("validate: ok"); } else { for w in warns { println!("warn: {}", w); } }
    println!("panels: {}  widgets: {}  rules: {}  keybinds: {}", cfg.panels.len(), cfg.widgets.len(), cfg.rules.len(), cfg.keybinds.len());
    std::process::exit(0);
}

fn apply_config_reload(new_cfg: config::Config, new_path: Option<PathBuf>, panel_handles: &mut Vec<HWND>, taskbar_hwnd: &mut Option<HWND>) {
    for w in new_cfg.validate() { eprintln!("[config] warn: {}", w); }
    *CURRENT_GAP.lock().unwrap_or_else(|e| e.into_inner()) = new_cfg.general.gap;
    *CURRENT_LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = new_cfg.layout_enum();
    *CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = new_cfg.clone();
    *CONFIG_PATH.lock().unwrap_or_else(|e| e.into_inner()) = new_path.clone();
    register_keybinds(&new_cfg);
    panel::destroy_panels();
    panel_handles.clear();
    // destroy legacy taskbar if panels now exist or taskbar disabled
    if !new_cfg.panels.is_empty() || !new_cfg.general.taskbar {
        if let Some(h) = taskbar_hwnd.take() {
            unsafe { let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(h); }
            TASKBAR_ENABLED.store(false, Ordering::SeqCst);
        }
    }
    if !new_cfg.panels.is_empty() {
        match panel::create_panels(&new_cfg) {
            Ok(hs) => *panel_handles = hs,
            Err(e) => eprintln!("[panel] recreate failed: {}", e),
        }
        TASKBAR_ENABLED.store(false, Ordering::SeqCst);
    } else if new_cfg.general.taskbar {
        if taskbar_hwnd.is_none() {
            match taskbar::create_taskbar() {
                Ok(h) => { *taskbar_hwnd = Some(h); TASKBAR_ENABLED.store(true, Ordering::SeqCst); },
                Err(e) => { eprintln!("[taskbar] recreate failed: {}", e); TASKBAR_ENABLED.store(false, Ordering::SeqCst); },
            }
        }
    }
    if let Some(p) = new_path { watcher::spawn_watcher(p); }
}

fn main() {
    let _ = MAIN_TID.set(unsafe { windows::Win32::System::Threading::GetCurrentThreadId() });
    print_banner();
    virtual_desktop::init();

    // --- early arg scan for --config / --generate-config / --check-config / --help
    let args: Vec<String> = std::env::args().collect();
    let mut explicit_cfg: Option<PathBuf> = None;
    let mut do_generate = false;
    let mut do_check = false;
    let mut cli_overrides: Vec<(String,String)> = Vec::new();
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => { print_help(); std::process::exit(0); }
            "--config" => { if let Some(v)=iter.next(){ explicit_cfg = Some(PathBuf::from(v)); } }
            "--generate-config" => do_generate = true,
            "--check-config" => do_check = true,
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
            "--gap" => if let Some(v)=iter.next(){ cli_overrides.push(("gap".into(), v.clone())); },
            "--layout" => if let Some(v)=iter.next(){ cli_overrides.push(("layout".into(), v.clone())); },
            other => eprintln!("Unknown arg '{}' -- use --help", other),
        }
    }

    if do_generate { do_generate_config(explicit_cfg.as_deref()); }
    if do_check { do_check_config(explicit_cfg.as_deref()); }

    // --- load config
    let (mut cfg, cfg_path) = config::load_or_default(explicit_cfg.as_deref());
    *CONFIG_PATH.lock().unwrap_or_else(|e| e.into_inner()) = cfg_path.clone();
    // apply CLI overrides (they win over file)
    for (k,v) in &cli_overrides {
        match k.as_str() {
            "no-taskbar" => { cfg.general.taskbar = false; cfg.panels.clear(); }
            "gap" => if let Ok(g)=v.parse::<i32>(){ cfg.general.gap = g.max(0); }
            "layout" => cfg.general.layout = v.clone(),
            _ => {}
        }
    }
    // validate
    for w in cfg.validate() { eprintln!("[config] warn: {}", w); }

    // push to globals
    *CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = cfg.clone();
    *CURRENT_GAP.lock().unwrap_or_else(|e| e.into_inner()) = cfg.general.gap;
    *CURRENT_LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = cfg.layout_enum();
    TASKBAR_ENABLED.store(cfg.general.taskbar && cfg.panels.is_empty(), Ordering::SeqCst);
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
    println!("[main] pid={} exe={:?}", std::process::id(), std::env::current_exe().unwrap_or_default());

    let host_hwnd = match create_host_window() {
        Ok(h) => h,
        Err(e) => { eprintln!("[main] host failed: {}", e); return; }
    };

    // --- panels vs legacy taskbar
    let mut panel_handles: Vec<HWND> = Vec::new();
    let mut taskbar_hwnd: Option<HWND> = if !cfg.panels.is_empty() {
        match panel::create_panels(&cfg) {
            Ok(hs) => { panel_handles = hs; None },
            Err(e) => { eprintln!("[panel] failed: {} -> fallback to taskbar", e); None }
        }
    } else { None };

    if taskbar_hwnd.is_none() && cfg.general.taskbar && cfg.panels.is_empty() {
        match taskbar::create_taskbar() {
            Ok(h) => taskbar_hwnd = Some(h),
            Err(e) => { eprintln!("[taskbar] failed: {} - continuing without bar", e); TASKBAR_ENABLED.store(false, Ordering::SeqCst); }
        }
    }

    // hotkeys — dynamic from config.toml [[keybinds]]
    register_keybinds(&cfg);

    let mut hooks: Vec<HWINEVENTHOOK> = Vec::new();
    unsafe {
        let mut try_hook = |event_min: u32, event_max: u32, label: &str| {
            let h = SetWinEventHook(event_min, event_max, None, Some(win_event_proc), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
            if h.0.is_null() { eprintln!("[hook] {} failed: {:?}", label, windows::Win32::Foundation::GetLastError()); }
            else { println!("[hook] {} 0x{:x}-0x{:x} => {:?}", label, event_min, event_max, h.0); hooks.push(h); }
        };
        try_hook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, "FOREGROUND");
        try_hook(EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND, "MINIMIZE");
        try_hook(EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND, "MOVESIZEEND");
        try_hook(EVENT_OBJECT_CREATE, EVENT_OBJECT_CREATE, "CREATE");
        try_hook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_DESTROY, "DESTROY");
        try_hook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, "SHOW");
        try_hook(EVENT_OBJECT_HIDE, EVENT_OBJECT_HIDE, "HIDE");
    }

    if TILING_ENABLED.load(Ordering::SeqCst) {
        let (top, bottom) = if !cfg.panels.is_empty() {
            let t: i32 = cfg.panels.iter().filter(|p| p.position=="top").map(|p| p.height).sum();
            let b: i32 = cfg.panels.iter().filter(|p| p.position=="bottom").map(|p| p.height).sum();
            (t,b)
        } else if TASKBAR_ENABLED.load(Ordering::SeqCst) { (0, taskbar::TASKBAR_HEIGHT) } else { (0,0) };
        manager::tile_windows_reserved(taskbar_hwnd, top, bottom, layout, gap);
    }

    println!("[main] message loop — Win+Shift+Q quit, C reload");
    let mut msg = MSG::default();
    unsafe {
        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            if ret.0 == 0 { println!("[main] WM_QUIT"); break; }
            if ret.0 == -1 { eprintln!("[main] GetMessageW {:?}", windows::Win32::Foundation::GetLastError()); break; }
            if msg.message == WM_HOTKEY {
                let id = msg.wParam.0 as i32;
                let action = { HOTKEY_ACTIONS.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned() };
                if let Some(act) = action {
                    println!("[hotkey] id={} -> '{}'", id, act);
                    if act == "quit" {
                        break;
                    } else if act == "reload_config" {
                        println!("[hotkey] reload config");
                        let explicit = CONFIG_PATH.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        let (new_cfg, new_path) = config::load_or_default(explicit.as_deref());
                        apply_config_reload(new_cfg, new_path, &mut panel_handles, &mut taskbar_hwnd);
                        request_retile();
                    } else if act == "retile" {
                        let l=*CURRENT_LAYOUT.lock().unwrap_or_else(|e| e.into_inner()); let g=*CURRENT_GAP.lock().unwrap_or_else(|e| e.into_inner()); let cfg=CURRENT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        let (top,bottom)= if !cfg.panels.is_empty(){
                            let t: i32 = cfg.panels.iter().filter(|p| p.position=="top").map(|p| p.height).sum();
                            let b: i32 = cfg.panels.iter().filter(|p| p.position=="bottom").map(|p| p.height).sum();
                            (t,b)
                        } else if TASKBAR_ENABLED.load(Ordering::SeqCst){ (0, taskbar::TASKBAR_HEIGHT)} else { (0,0)};
                        manager::tile_windows_reserved(taskbar_hwnd, top, bottom, l, g);
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
            // Reload requests can originate from a file-watch event, a widget action, or Rhai.
            let watcher_reload = watcher::should_reload();
            if watcher_reload || CONFIG_RELOAD_PENDING.swap(false, Ordering::SeqCst) {
                println!("[config] {} -> reloading", if watcher_reload { "file changed" } else { "action requested" });
                let explicit = CONFIG_PATH.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let (new_cfg, new_path) = config::load_or_default(explicit.as_deref());
                apply_config_reload(new_cfg, new_path, &mut panel_handles, &mut taskbar_hwnd);
                request_retile();
            }
        }
        unregister_all_hotkeys();
        for h in hooks { let _=UnhookWinEvent(h); }
        panel::destroy_panels();
        let _=host_hwnd; let _=panel_handles;
    }
    println!("[main] bye");
}

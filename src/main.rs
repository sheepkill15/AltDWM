mod layout;
mod manager;
mod taskbar;
mod util;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassExW, TranslateMessage, HMENU, HWND_MESSAGE, MSG, WM_HOTKEY, WM_TIMER, WM_CREATE,
    WM_DESTROY, WNDCLASSEXW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_SHOW,
    EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART,
    EVENT_SYSTEM_MOVESIZEEND, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

use layout::Layout;
use windows::core::w;

// Global state
static RETILE_PENDING: AtomicBool = AtomicBool::new(false);
static TILING_ENABLED: AtomicBool = AtomicBool::new(true);
static TASKBAR_ENABLED: AtomicBool = AtomicBool::new(true);

static CURRENT_LAYOUT: Mutex<Layout> = Mutex::new(Layout::MasterStack);
static CURRENT_GAP: Mutex<i32> = Mutex::new(8);

// Hotkey IDs
const HK_RETILE: i32 = 1;
const HK_TOGGLE: i32 = 2;
const HK_QUIT: i32 = 3;
const HK_GRID: i32 = 4;
const HK_MONOCLE: i32 = 5;
const HK_FLOAT: i32 = 6;
const HK_MASTERSTACK: i32 = 7;

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
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
    RETILE_PENDING.store(true, Ordering::SeqCst);
}

static mut HOST_HWND: HWND = HWND(std::ptr::null_mut());

unsafe extern "system" fn host_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(hwnd), 100, 200, None);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == 100 {
                if RETILE_PENDING.load(Ordering::SeqCst) && TILING_ENABLED.load(Ordering::SeqCst) {
                    RETILE_PENDING.store(false, Ordering::SeqCst);
                    let taskbar_hwnd = taskbar::get_taskbar_hwnd();
                    let gap = *CURRENT_GAP.lock().unwrap();
                    let layout = *CURRENT_LAYOUT.lock().unwrap();
                    let tb_height = if TASKBAR_ENABLED.load(Ordering::SeqCst) {
                        taskbar::TASKBAR_HEIGHT
                    } else {
                        0
                    };
                    manager::tile_windows(taskbar_hwnd, tb_height, layout, gap);
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
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance.into(),
            hIcon: Default::default(),
            hCursor: Default::default(),
            hbrBackground: Default::default(),
            lpszMenuName: windows::core::PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: Default::default(),
        };
        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            let err = windows::Win32::Foundation::GetLastError();
            if err.0 != 1410 {
                return Err(format!("Host RegisterClassExW failed: {:?}", err));
            }
        }
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
        HOST_HWND = hwnd;
        println!("[host] message-only window hwnd={:?}", hwnd.0);
        Ok(hwnd)
    }
}

fn print_banner() {
    println!(r#"
  ___   _ _   ___  _ _ _ _  
 / _ \ | | | |   \| | | | | 
| |_| || | | | |) | | | | |  AltDWM 0.1.0 - Experimental Windows Shell
 \___/ |_|_| |___/|_|_|_|_|  Rust + Win32 + DWM (dwm.exe stays, explorer.exe replaced)
"#);
    println!("  Hotkeys (Win+Shift+): R=retile T=toggle tiling Q=quit G=grid M=monocle F=float S=masterStack");
    println!("  ---");
}

fn print_help() {
    println!(r#"Usage: alt-dwm [OPTIONS]

Options:
  --no-taskbar       Disable taskbar replacement (only tiling WM)
  --gap <px>         Gap between windows (default 8)
  --layout <name>    Initial layout: masterstack, grid, monocle, floating (default masterstack)
  --help             Show this help
  --replace-shell    Print registry command to replace explorer.exe (requires admin)

Examples:
  alt-dwm
  alt-dwm --gap 12 --layout grid
  alt-dwm --no-taskbar

Shell replacement (run as admin):
  reg add "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon" /v Shell /t REG_SZ /d "C:\path\to\alt-dwm.exe" /f
  # Or per-user (no admin, logoff required):
  reg add "HKCU\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon" /v Shell /t REG_SZ /d "C:\path\to\alt-dwm.exe" /f
  # Restore:
  reg add "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon" /v Shell /t REG_SZ /d "explorer.exe" /f

Note: dwm.exe is NOT replaceable on Windows 11. AltDWM tiles via SetWinEventHook + DeferWindowPos.
"#);
}

fn parse_args() {
    let args: Vec<String> = std::env::args().collect();
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--no-taskbar" => {
                TASKBAR_ENABLED.store(false, Ordering::SeqCst);
                println!("[args] taskbar disabled");
            }
            "--gap" => {
                if let Some(v) = iter.next() {
                    if let Ok(g) = v.parse::<i32>() {
                        *CURRENT_GAP.lock().unwrap() = g;
                        println!("[args] gap={}", g);
                    }
                }
            }
            "--layout" => {
                if let Some(v) = iter.next() {
                    let layout = match v.to_lowercase().as_str() {
                        "grid" => Layout::Grid,
                        "monocle" => Layout::Monocle,
                        "floating" => Layout::Floating,
                        "masterstack" | "master" | "bsp" => Layout::MasterStack,
                        _ => {
                            eprintln!("Unknown layout '{}', use masterstack|grid|monocle|floating", v);
                            continue;
                        }
                    };
                    *CURRENT_LAYOUT.lock().unwrap() = layout;
                    println!("[args] layout={}", layout.name());
                }
            }
            "--replace-shell" => {
                let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("alt-dwm.exe"));
                println!("\nTo REPLACE explorer.exe (shell) with AltDWM:");
                println!("  1. Copy {} to a safe location (e.g. C:\\AltDWM\\alt-dwm.exe)", exe.display());
                println!("  2. Run as ADMIN:");
                println!("     reg add \"HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon\" /v Shell /t REG_SZ /d \"C:\\AltDWM\\alt-dwm.exe\" /f");
                println!("  3. Log off/on or reboot. Kill explorer.exe to test: taskkill /f /im explorer.exe");
                println!("  Restore: reg add \"HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon\" /v Shell /t REG_SZ /d \"explorer.exe\" /f");
                println!("\nPer-user (no admin):");
                println!("  reg add \"HKCU\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon\" /v Shell /t REG_SZ /d \"{}\" /f", exe.display());
                std::process::exit(0);
            }
            other => {
                eprintln!("Unknown arg '{}' -- use --help", other);
            }
        }
    }
}

fn main() {
    print_banner();
    parse_args();

    let gap = *CURRENT_GAP.lock().unwrap();
    let layout = *CURRENT_LAYOUT.lock().unwrap();
    println!(
        "[main] starting - tiling={} layout={} gap={} taskbar={}",
        TILING_ENABLED.load(Ordering::SeqCst),
        layout.name(),
        gap,
        TASKBAR_ENABLED.load(Ordering::SeqCst)
    );
    println!(
        "[main] pid={} exe={:?}",
        std::process::id(),
        std::env::current_exe().unwrap_or_default()
    );

    let host_hwnd = match create_host_window() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[main] failed to create host window: {}", e);
            return;
        }
    };

    let taskbar_hwnd = if TASKBAR_ENABLED.load(Ordering::SeqCst) {
        match taskbar::create_taskbar() {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("[taskbar] failed: {} - continuing without taskbar", e);
                TASKBAR_ENABLED.store(false, Ordering::SeqCst);
                None
            }
        }
    } else {
        None
    };

    unsafe {
        let mods = MOD_WIN | MOD_SHIFT | MOD_NOREPEAT;
        let ok = |id, vk| RegisterHotKey(None, id, mods, vk as u32);
        let _ = ok(HK_RETILE, 0x52);
        let _ = ok(HK_TOGGLE, 0x54);
        let _ = ok(HK_QUIT, 0x51);
        let _ = ok(HK_GRID, 0x47);
        let _ = ok(HK_MONOCLE, 0x4D);
        let _ = ok(HK_FLOAT, 0x46);
        let _ = ok(HK_MASTERSTACK, 0x53);
        println!("[hotkey] registered Win+Shift+R/T/Q/G/M/F/S");
    }

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
                    "[hook] SetWinEventHook failed for {} (0x{:x}-0x{:x}): {:?}",
                    label,
                    event_min,
                    event_max,
                    windows::Win32::Foundation::GetLastError()
                );
            } else {
                println!("[hook] {} 0x{:x}-0x{:x} => {:?}", label, event_min, event_max, h.0);
                hooks.push(h);
            }
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
        let tb_height = if TASKBAR_ENABLED.load(Ordering::SeqCst) {
            taskbar::TASKBAR_HEIGHT
        } else {
            0
        };
        manager::tile_windows(taskbar_hwnd, tb_height, layout, gap);
    }

    println!("[main] entering message loop - press Win+Shift+Q to quit");
    println!("[main] hint: to test shell replacement, run: taskkill /f /im explorer.exe  (then AltDWM will tile) -> run explorer.exe to restore");

    let mut msg = MSG::default();
    unsafe {
        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            // GetMessageW returns BOOL with -1 error, 0 WM_QUIT, >0 message
            if ret.0 == 0 {
                println!("[main] WM_QUIT received, exiting");
                break;
            }
            if ret.0 == -1 {
                eprintln!("[main] GetMessageW error: {:?}", windows::Win32::Foundation::GetLastError());
                break;
            }

            if msg.message == WM_HOTKEY {
                let id = msg.wParam.0 as i32;
                match id {
                    HK_RETILE => {
                        println!("[hotkey] Retile");
                        let tb_h = if TASKBAR_ENABLED.load(Ordering::SeqCst) {
                            taskbar::TASKBAR_HEIGHT
                        } else {
                            0
                        };
                        let l = *CURRENT_LAYOUT.lock().unwrap();
                        let g = *CURRENT_GAP.lock().unwrap();
                        manager::tile_windows(taskbar_hwnd, tb_h, l, g);
                    }
                    HK_TOGGLE => {
                        let enabled = !TILING_ENABLED.load(Ordering::SeqCst);
                        TILING_ENABLED.store(enabled, Ordering::SeqCst);
                        println!("[hotkey] Tiling {}", if enabled { "ENABLED" } else { "DISABLED (-> floating)" });
                        if enabled {
                            RETILE_PENDING.store(true, Ordering::SeqCst);
                        }
                    }
                    HK_QUIT => {
                        println!("[hotkey] Quit");
                        break;
                    }
                    HK_GRID => {
                        *CURRENT_LAYOUT.lock().unwrap() = Layout::Grid;
                        println!("[hotkey] Layout -> Grid");
                        RETILE_PENDING.store(true, Ordering::SeqCst);
                    }
                    HK_MONOCLE => {
                        *CURRENT_LAYOUT.lock().unwrap() = Layout::Monocle;
                        println!("[hotkey] Layout -> Monocle");
                        RETILE_PENDING.store(true, Ordering::SeqCst);
                    }
                    HK_FLOAT => {
                        *CURRENT_LAYOUT.lock().unwrap() = Layout::Floating;
                        println!("[hotkey] Layout -> Floating (no tiling)");
                    }
                    HK_MASTERSTACK => {
                        *CURRENT_LAYOUT.lock().unwrap() = Layout::MasterStack;
                        println!("[hotkey] Layout -> MasterStack");
                        RETILE_PENDING.store(true, Ordering::SeqCst);
                    }
                    _ => {}
                }
                continue;
            }

            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        println!("[main] cleaning up...");
        for vk in [HK_RETILE, HK_TOGGLE, HK_QUIT, HK_GRID, HK_MONOCLE, HK_FLOAT, HK_MASTERSTACK] {
            let _ = UnregisterHotKey(None, vk);
        }
        for h in hooks {
            let _ = UnhookWinEvent(h);
        }
        let _ = host_hwnd;
    }

    println!("[main] bye");
}

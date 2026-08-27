//! Rhai scripting for extensibility — actions, custom widgets, layouts.
//! Sandboxed, sync engine. Exposed functions handle side effects.
use rhai::{Dynamic, Engine, Scope};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static ENGINE: OnceLock<Mutex<Engine>> = OnceLock::new();

// ---- real CPU usage via GetSystemTimes ---------------------------------
static PREV_IDLE: AtomicU64 = AtomicU64::new(0);
static PREV_KERNEL: AtomicU64 = AtomicU64::new(0);
static PREV_USER: AtomicU64 = AtomicU64::new(0);
static PREV_TICK: std::sync::LazyLock<Mutex<Option<Instant>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

fn filetime_to_u64(ft: windows::Win32::Foundation::FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

fn get_cpu_usage_real() -> i64 {
    use windows::Win32::System::Threading::GetSystemTimes;
    unsafe {
        let mut idle = windows::Win32::Foundation::FILETIME::default();
        let mut kernel = windows::Win32::Foundation::FILETIME::default();
        let mut user = windows::Win32::Foundation::FILETIME::default();
        if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_err() {
            return 0;
        }
        let idle_u = filetime_to_u64(idle);
        let kernel_u = filetime_to_u64(kernel);
        let user_u = filetime_to_u64(user);
        let prev_idle = PREV_IDLE.load(Ordering::Relaxed);
        let prev_kernel = PREV_KERNEL.load(Ordering::Relaxed);
        let prev_user = PREV_USER.load(Ordering::Relaxed);
        let prev_tick = *PREV_TICK.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        PREV_IDLE.store(idle_u, Ordering::Relaxed);
        PREV_KERNEL.store(kernel_u, Ordering::Relaxed);
        PREV_USER.store(user_u, Ordering::Relaxed);
        *PREV_TICK.lock().unwrap_or_else(|e| e.into_inner()) = Some(now);
        if prev_idle == 0 || prev_tick.is_none() {
            return 0; // first call, need delta
        }
        let idle_delta = idle_u.saturating_sub(prev_idle);
        let kernel_delta = kernel_u.saturating_sub(prev_kernel);
        let user_delta = user_u.saturating_sub(prev_user);
        let total = kernel_delta + user_delta;
        if total == 0 {
            return 0;
        }
        let busy = total.saturating_sub(idle_delta);
        ((busy * 100) / total) as i64
    }
}

fn get_mem_usage_real() -> i64 {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    unsafe {
        let mut stat = MEMORYSTATUSEX {
            dwLength: size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if GlobalMemoryStatusEx(&mut stat).is_ok() {
            return stat.dwMemoryLoad as i64;
        }
        0
    }
}

fn build_engine() -> Engine {
    let mut eng = Engine::new();
    eng.set_max_expr_depths(256, 256);
    // Scripts execute on the shell's UI thread. Bound both CPU work and
    // allocations so a broken widget/layout cannot hang the replacement shell.
    eng.set_max_operations(100_000);
    eng.set_max_call_levels(32);
    eng.set_max_string_size(64 * 1024);
    eng.set_max_array_size(4_096);
    eng.set_max_map_size(1_024);
    eng.register_fn("launch", |cmd: &str| {
        println!("[rhai] launch {}", cmd);
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", cmd])
            .spawn();
    });
    eng.register_fn("log", |msg: &str| {
        println!("[rhai] {}", msg);
    });
    eng.register_fn("get_cpu_usage", || -> i64 { get_cpu_usage_real() });
    eng.register_fn("focused_title", || -> String {
        unsafe {
            let hwnd = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
            crate::util::get_window_title(hwnd)
        }
    });
    eng.register_fn("get_mem_usage", || -> i64 { get_mem_usage_real() });
    eng.register_fn("get_mem", || -> rhai::Map {
        let mut m = rhai::Map::new();
        m.insert("load".into(), Dynamic::from_int(get_mem_usage_real()));
        m
    });
    eng.register_fn("retile", || {
        println!("[rhai] retile()");
        // signal main via atomic if available
        crate::request_retile();
    });
    eng.register_fn("set_layout", |name: &str| {
        println!("[rhai] set_layout {}", name);
        crate::set_layout_by_name(name);
    });
    eng.register_fn("focus_next", || {
        crate::focus::focus_next();
    });
    eng.register_fn("focus_prev", || {
        crate::focus::focus_prev();
    });
    eng.register_fn("focus_direction", |dir: &str| {
        crate::focus::focus_direction(dir);
    });
    eng.register_fn("focus_window", |substr: &str| {
        crate::focus::focus_window_by_title_substr(substr);
    });
    eng.register_fn("toggle_floating", || {
        crate::focus::toggle_floating_focused();
    });
    eng.register_fn("move_to_next_monitor", || {
        crate::focus::move_focused_to_monitor("next");
    });
    eng.register_fn("move_to_prev_monitor", || {
        crate::focus::move_focused_to_monitor("prev");
    });
    eng.register_fn("shell", |cmd: &str| {
        let _ = std::process::Command::new("cmd").args(["/C", cmd]).spawn();
    });
    eng.register_fn("window_count", || -> i64 {
        let tb = crate::taskbar::get_taskbar_hwnd();
        let wins = crate::manager::collect_windows(tb);
        wins.len() as i64
    });
    eng.register_fn("tilable_count", || -> i64 {
        let tb = crate::taskbar::get_taskbar_hwnd();
        let mut wins = crate::manager::collect_windows(tb);
        wins.retain(|w| !crate::rules::is_floating(*w));
        wins.retain(|w| crate::virtual_desktop::is_on_current_desktop(*w));
        wins.len() as i64
    });
    eng
}

pub fn engine() -> &'static Mutex<Engine> {
    ENGINE.get_or_init(|| Mutex::new(build_engine()))
}

/// Evaluate Rhai expression that returns text (for custom widget)
pub fn eval_text(code: &str) -> Result<String, String> {
    let eng = engine().lock().map_err(|e| format!("engine lock: {}", e))?;
    let scope = Scope::new();
    let res: Result<Dynamic, _> = eng.eval_with_scope(&mut scope.clone(), code);
    match res {
        Ok(v) => {
            if let Ok(s) = v.clone().into_string() {
                Ok(s)
            } else {
                Ok(v.to_string())
            }
        }
        Err(e) => Err(format!("{}", e)),
    }
}

/// Evaluate for side effects (action)
pub fn eval_action(code: &str) -> Result<(), String> {
    let eng = engine().lock().map_err(|e| format!("engine lock: {}", e))?;
    let mut scope = Scope::new();
    eng.run_with_scope(&mut scope, code)
        .map_err(|e| format!("{}", e))
}

/// Dispatch string action: "retile" | "quit" | "reload_config" | "launch('...')" | "rhai: ..."
pub fn dispatch_action(action: &str) {
    let act = action.trim();
    if act.starts_with("rhai:") {
        let code = act.trim_start_matches("rhai:").trim();
        if let Err(e) = eval_action(code) {
            eprintln!("[scripting] rhai error '{}': {}", code, e);
        }
        return;
    }
    if (act.contains("launch(")
        || act.contains("log(")
        || act.contains("set_layout")
        || act.contains("focus_")
        || act.contains("toggle_floating")
        || act.contains("move_to_")
        || act.contains("retile"))
        && eval_action(act).is_ok()
    {
        return;
    }
    match act {
        "retile" => crate::request_retile(),
        "toggle_tiling" => crate::toggle_tiling(),
        "command_center" | "launcher" => crate::command_center::toggle_from_keyboard(),
        "quit" => crate::request_quit(),
        "reload_config" => {
            println!("[scripting] reload_config");
            crate::reload_config_async();
        }
        "toggle_floating" => crate::focus::toggle_floating_focused(),
        "move_to_next_monitor" => crate::focus::move_focused_to_monitor("next"),
        "move_to_prev_monitor" => crate::focus::move_focused_to_monitor("prev"),
        _ if act.starts_with("set_layout") => {
            if let Some(start) = act.find('"').or(act.find('\'')) {
                let end = act.rfind('"').or(act.rfind('\'')).unwrap_or(act.len() - 1);
                if end > start {
                    let name = &act[start + 1..end];
                    crate::set_layout_by_name(name);
                }
            }
        }
        _ if act.starts_with("launch") => {
            if let Some(p1) = act.find('\'').or(act.find('"')) {
                let p2 = act.rfind('\'').or(act.rfind('"')).unwrap_or(act.len());
                if p2 > p1 {
                    let cmd = &act[p1 + 1..p2];
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", cmd])
                        .spawn();
                }
            } else {
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", act])
                    .spawn();
            }
        }
        other => {
            println!("[scripting] unknown action '{}' -> launch attempt", other);
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", other])
                .spawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::engine;

    #[test]
    fn scripts_have_finite_resource_limits() {
        let engine = engine().lock().expect("engine lock");
        assert_eq!(engine.max_operations(), 100_000);
        assert_eq!(engine.max_call_levels(), 32);
        assert_eq!(engine.max_string_size(), 64 * 1024);
        assert_eq!(engine.max_array_size(), 4_096);
        assert_eq!(engine.max_map_size(), 1_024);
    }
}

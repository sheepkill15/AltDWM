//! Rhai scripting for extensibility — actions, custom widgets, layouts.
//! Sandboxed, sync engine. Exposed functions handle side effects.
use rhai::{Dynamic, Engine, Scope};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

// The engine is stored bare rather than behind a `Mutex`. Rhai is built with
// the `sync` feature, so `Engine` is `Send + Sync` and every method used after
// construction takes `&self` — a lock would buy nothing and would deadlock the
// one path that needs it: a script action (evaluated under the lock) can call
// `switch_to`, which retiles synchronously, which evaluates a custom layout
// script — re-entering the very same engine on the same thread.
static ENGINE: OnceLock<Engine> = OnceLock::new();

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
    eng.register_fn("format_time", |format: &str| -> String {
        crate::widgets::format_time(format)
    });
    eng.register_fn("truncate_text", |text: &str, max_chars: i64| -> String {
        let max_chars = max_chars.max(0) as usize;
        if text.chars().count() <= max_chars {
            text.to_string()
        } else {
            format!("{}…", text.chars().take(max_chars).collect::<String>())
        }
    });
    // Turn any Unicode codepoint into text. Widget scripts use this for icon
    // fonts, keeping both the chosen glyph and its state thresholds editable.
    eng.register_fn("symbol", |codepoint: i64| -> String {
        u32::try_from(codepoint)
            .ok()
            .and_then(char::from_u32)
            .map(|value| value.to_string())
            .unwrap_or_default()
    });
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
    eng.register_fn("move_window", |dir: &str| {
        crate::focus::move_window_direction(dir);
    });
    eng.register_fn("promote", || {
        crate::focus::promote_focused();
    });
    eng.register_fn("workspace", |index: i64| {
        // Configuration and the UI are one-based; the module is zero-based.
        crate::workspace::switch_to((index.max(1) - 1) as usize);
    });
    eng.register_fn("move_to_workspace", |index: i64| {
        crate::workspace::move_focused_to((index.max(1) - 1) as usize, false);
    });
    eng.register_fn("send_to_workspace", |index: i64| {
        crate::workspace::move_focused_to((index.max(1) - 1) as usize, true);
    });
    eng.register_fn("next_workspace", || {
        crate::workspace::cycle(1);
    });
    eng.register_fn("prev_workspace", || {
        crate::workspace::cycle(-1);
    });
    eng.register_fn("current_workspace", || -> i64 {
        crate::workspace::current_number() as i64
    });
    eng.register_fn("adjust_master_ratio", |delta: i64| {
        crate::adjust_master_ratio(delta as f32 / 100.0);
    });
    eng.register_fn("set_master_ratio", |percent: i64| {
        let target = percent.clamp(10, 90) as f32 / 100.0;
        let current = crate::layout::current_master_ratio();
        crate::adjust_master_ratio(target - current);
    });
    eng.register_fn("quick_settings", || {
        crate::quick_settings::toggle();
    });
    eng.register_fn("cycle_input", || {
        crate::input::cycle();
    });
    eng.register_fn("input_layout", || -> String {
        crate::input::current()
            .map(|layout| layout.tag)
            .unwrap_or_default()
    });
    eng.register_fn("adjust_volume", |delta: i64| {
        crate::system::adjust_volume(delta as f32 / 100.0);
    });
    eng.register_fn("set_volume", |percent: i64| {
        crate::system::set_volume(percent.clamp(0, 100) as f32 / 100.0);
    });
    eng.register_fn("toggle_mute", || {
        crate::system::toggle_mute();
    });
    eng.register_fn("get_volume", || -> i64 {
        crate::system::status()
            .volume
            .map(|volume| i64::from(volume.percent()))
            .unwrap_or(-1)
    });
    eng.register_fn("is_muted", || -> bool {
        crate::system::status()
            .volume
            .map(|volume| volume.muted)
            .unwrap_or(false)
    });
    eng.register_fn("set_brightness", |percent: i64| {
        crate::system::set_brightness(percent.clamp(0, 100) as u8);
    });
    eng.register_fn("get_brightness", || -> i64 {
        crate::system::status()
            .brightness
            .map(|value| i64::from(value.percent))
            .unwrap_or(-1)
    });
    eng.register_fn("get_battery", || -> i64 {
        crate::system::status()
            .battery
            .and_then(|battery| battery.percent)
            .map(i64::from)
            .unwrap_or(-1)
    });
    eng.register_fn("network_name", || -> String {
        crate::system::status().network.label()
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
        crate::manager::collect_windows().len() as i64
    });
    eng.register_fn("tilable_count", || -> i64 {
        let mut wins = crate::manager::collect_windows();
        wins.retain(|w| !crate::rules::is_floating(*w));
        wins.retain(|w| crate::virtual_desktop::is_on_current_desktop(*w));
        wins.len() as i64
    });
    eng
}

pub fn engine() -> &'static Engine {
    ENGINE.get_or_init(build_engine)
}

/// Evaluate for side effects (action)
pub fn eval_action(code: &str) -> Result<(), String> {
    let eng = engine();
    let mut scope = Scope::new();
    eng.run_with_scope(&mut scope, code)
        .map_err(|e| format!("{}", e))
}

/// Dispatch string action: "retile" | "quit" | "reload_config" | "launch('...')" | "rhai: ..."
/// True for actions written as a call, e.g. `set_layout("Grid")` or `retile()`.
fn looks_like_call(action: &str) -> bool {
    action.ends_with(')') && action.contains('(')
}

fn monitor_workspace_action(action: &str) -> Option<(Option<usize>, isize)> {
    let action = action.trim();
    if matches!(action, "next_workspace" | "next_workspace()") {
        return Some((None, 1));
    }
    if matches!(action, "prev_workspace" | "prev_workspace()") {
        return Some((None, -1));
    }
    let index = action
        .strip_prefix("workspace(")?
        .strip_suffix(')')?
        .trim()
        .parse::<usize>()
        .ok()?;
    Some((Some(index.saturating_sub(1)), 0))
}

/// Dispatch an action originating from a panel on `monitor`. Workspace actions
/// are monitor-local; every other action keeps the ordinary global/focused
/// semantics.
pub fn dispatch_action_on_monitor(action: &str, monitor: isize) {
    if let Some((workspace, delta)) = monitor_workspace_action(action) {
        match workspace {
            Some(index) => crate::workspace::switch_to_monitor(monitor, index),
            None => crate::workspace::cycle_on_monitor(monitor, delta),
        }
        return;
    }
    dispatch_action(action);
}

pub fn dispatch_action(action: &str) {
    let act = action.trim();
    if act.starts_with("rhai:") {
        let code = act.trim_start_matches("rhai:").trim();
        if let Err(e) = eval_action(code) {
            eprintln!("[scripting] rhai error '{}': {}", code, e);
        }
        return;
    }
    // A bare word is a named shell action; anything shaped like `name(args)` is
    // a script expression. Deciding structurally rather than from a keyword
    // whitelist means a new Rhai binding works without being registered in two
    // places — and, more importantly, an unrecognised call is reported rather
    // than handed to cmd.exe as a program name by the fallback below.
    // `launch('program')` predates the Rhai bridge, and every shipped config
    // quotes its argument with '…' — which Rhai reads as a char literal, not a
    // string, so handing it to the engine is a guaranteed syntax error. Its
    // dedicated spawner below understands both quote styles, so keep it off the
    // script path.
    if looks_like_call(act) && !act.starts_with("launch(") {
        if let Err(error) = eval_action(act) {
            eprintln!("[scripting] '{act}' failed: {error}");
        }
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
        "quick_settings" => crate::quick_settings::toggle(),
        "promote" | "promote_to_master" => crate::focus::promote_focused(),
        "next_workspace" => crate::workspace::cycle(1),
        "prev_workspace" => crate::workspace::cycle(-1),
        "wider_master" => crate::adjust_master_ratio(0.05),
        "narrower_master" => crate::adjust_master_ratio(-0.05),
        "cycle_input" | "next_layout" => {
            crate::input::cycle();
        }
        "volume_up" => crate::system::adjust_volume(0.05),
        "volume_down" => crate::system::adjust_volume(-0.05),
        "toggle_mute" => crate::system::toggle_mute(),
        "brightness_up" => crate::system::adjust_brightness(5),
        "brightness_down" => crate::system::adjust_brightness(-5),
        "rescan_apps" => crate::apps::refresh(),
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
    fn panel_workspace_actions_keep_their_monitor_context() {
        use super::monitor_workspace_action;
        assert_eq!(monitor_workspace_action("workspace(3)"), Some((Some(2), 0)));
        assert_eq!(monitor_workspace_action("next_workspace"), Some((None, 1)));
        assert_eq!(
            monitor_workspace_action("prev_workspace()"),
            Some((None, -1))
        );
        assert_eq!(monitor_workspace_action("toggle_tiling"), None);
    }

    #[test]
    fn call_shaped_actions_go_to_the_script_engine() {
        use super::looks_like_call;
        assert!(looks_like_call("focus_next()"));
        assert!(looks_like_call("set_layout(\"Grid\")"));
        assert!(looks_like_call("launch('wt.exe')"));
        assert!(looks_like_call("adjust_volume(-5)"));
        // Named verbs and plain programs are not calls.
        assert!(!looks_like_call("retile"));
        assert!(!looks_like_call("toggle_tiling"));
        assert!(!looks_like_call("notepad.exe"));
        // A path that merely contains parentheses is still a program to launch.
        assert!(!looks_like_call(r"C:\Program Files (x86)\app\app.exe"));
    }

    #[test]
    fn the_engine_can_be_used_re_entrantly() {
        // Regression: a `workspace(N)` / `next_workspace()` action is evaluated
        // by the engine, and — because the switch retiles synchronously — a
        // custom Rhai layout is evaluated again during that same call, on the
        // same thread. Behind a non-reentrant `Mutex<Engine>` this deadlocked
        // the whole window manager. Holding one `&Engine` live and evaluating
        // through another, as that nesting does, must simply return.
        let outer = engine();
        let a: i64 = outer.eval("40 + 2").expect("outer eval");
        let b: i64 = engine().eval("1 + 1").expect("nested eval");
        assert_eq!((a, b), (42, 2));
    }

    #[test]
    fn scripts_have_finite_resource_limits() {
        let engine = engine();
        assert_eq!(engine.max_operations(), 100_000);
        assert_eq!(engine.max_call_levels(), 32);
        assert_eq!(engine.max_string_size(), 64 * 1024);
        assert_eq!(engine.max_array_size(), 4_096);
        assert_eq!(engine.max_map_size(), 1_024);
    }
}

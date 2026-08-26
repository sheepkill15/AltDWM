//! Rhai scripting for extensibility — actions, custom widgets, layouts.
//! Sandboxed, sync engine. Exposed functions handle side effects.
use rhai::{Engine, Scope, Dynamic};
use std::sync::{OnceLock, Mutex};

static ENGINE: OnceLock<Mutex<Engine>> = OnceLock::new();

fn build_engine() -> Engine {
    let mut eng = Engine::new();
    eng.set_max_expr_depths(256, 256);
    eng.register_fn("launch", |cmd: &str| {
        println!("[rhai] launch {}", cmd);
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", cmd]).spawn();
    });
    eng.register_fn("log", |msg: &str| {
        println!("[rhai] {}", msg);
    });
    eng.register_fn("get_cpu_usage", || -> i64 { 42 });
    eng.register_fn("focused_title", || -> String {
        unsafe {
            let hwnd = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
            crate::util::get_window_title(hwnd)
        }
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
    eng.register_fn("focus_next", || { crate::focus::focus_next(); });
    eng.register_fn("focus_prev", || { crate::focus::focus_prev(); });
    eng.register_fn("focus_direction", |dir: &str| { crate::focus::focus_direction(dir); });
    eng.register_fn("focus_window", |substr: &str| { crate::focus::focus_window_by_title_substr(substr); });
    eng.register_fn("toggle_floating", || { crate::focus::toggle_floating_focused(); });
    eng.register_fn("move_to_next_monitor", || { crate::focus::move_focused_to_monitor("next"); });
    eng.register_fn("move_to_prev_monitor", || { crate::focus::move_focused_to_monitor("prev"); });
    eng.register_fn("shell", |cmd: &str| {
        let _ = std::process::Command::new("cmd").args(["/C", cmd]).spawn();
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
            if let Some(s) = v.clone().into_string().ok() { Ok(s) }
            else { Ok(v.to_string()) }
        }
        Err(e) => Err(format!("{}", e)),
    }
}

/// Evaluate for side effects (action)
pub fn eval_action(code: &str) -> Result<(), String> {
    let eng = engine().lock().map_err(|e| format!("engine lock: {}", e))?;
    let mut scope = Scope::new();
    eng.run_with_scope(&mut scope, code).map_err(|e| format!("{}", e))
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
    if act.contains("launch(") || act.contains("log(") || act.contains("set_layout") || act.contains("focus_") || act.contains("toggle_floating") || act.contains("move_to_") || act.contains("retile") {
        if eval_action(act).is_ok() { return; }
        // fall through to direct handling on error
    }
    match act {
        "retile" => crate::request_retile(),
        "toggle_tiling" => crate::toggle_tiling(),
        "quit" => crate::request_quit(),
        "reload_config" => {
            println!("[scripting] reload_config");
            crate::reload_config_async();
        },
        "toggle_floating" => crate::focus::toggle_floating_focused(),
        "move_to_next_monitor" => crate::focus::move_focused_to_monitor("next"),
        "move_to_prev_monitor" => crate::focus::move_focused_to_monitor("prev"),
        _ if act.starts_with("set_layout") => {
            if let Some(start) = act.find('"').or(act.find('\'')) {
                let end = act.rfind('"').or(act.rfind('\'')).unwrap_or(act.len()-1);
                if end > start {
                    let name = &act[start+1..end];
                    crate::set_layout_by_name(name);
                }
            }
        }
        _ if act.starts_with("launch") => {
            if let Some(p1) = act.find('\'').or(act.find('"')) {
                let p2 = act.rfind('\'').or(act.rfind('"')).unwrap_or(act.len());
                if p2 > p1 {
                    let cmd = &act[p1+1..p2];
                    let _ = std::process::Command::new("cmd").args(["/C","start","",cmd]).spawn();
                }
            } else {
                let _ = std::process::Command::new("cmd").args(["/C","start","",act]).spawn();
            }
        },
        other => {
            println!("[scripting] unknown action '{}' -> launch attempt", other);
            let _ = std::process::Command::new("cmd").args(["/C","start","",other]).spawn();
        }
    }
}

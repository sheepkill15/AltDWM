//! Rule engine — matches windows against `[[rules]]` in config.toml
//! See docs/EXTENSIBILITY.md §3. Supports class/title/process with exact or regex.
use regex::Regex;
use windows::Win32::Foundation::HWND;

use crate::config::RuleConfig;
use crate::util::{get_class_name, get_window_title};

fn matches_pattern(
    text: &str,
    pattern: &str,
    compiled: Option<&Regex>,
    regex_pattern: Option<&String>,
) -> bool {
    if let Some(re) = compiled {
        return re.is_match(text);
    }
    if regex_pattern.is_some() {
        // regex present but not compiled -> invalid, already warned in compile_regexes
        return false;
    }
    if pattern.is_empty() {
        return false;
    }
    text.to_lowercase().contains(&pattern.to_lowercase())
}

fn matches_exact(text: &str, pattern: &str) -> bool {
    text == pattern
}

pub fn rule_matches(hwnd: HWND, rule: &RuleConfig) -> bool {
    // if any match_* is set, it must match; if none set, rule matches nothing
    let mut has_condition = false;
    let mut matched = true;

    let class = get_class_name(hwnd);
    let title = get_window_title(hwnd);

    if let Some(pat) = &rule.match_class {
        has_condition = true;
        let ok = matches_exact(&class, pat)
            || matches_pattern(
                &class,
                pat,
                rule.compiled_class_regex.as_ref(),
                rule.match_class_regex.as_ref(),
            );
        matched &= ok;
    } else if rule.match_class_regex.is_some() {
        has_condition = true;
        if let Some(re) = rule.compiled_class_regex.as_ref() {
            matched &= re.is_match(&class);
        } else {
            matched = false;
        }
    }

    if let Some(pat) = &rule.match_title {
        has_condition = true;
        let ok = matches_pattern(
            &title,
            pat,
            rule.compiled_title_regex.as_ref(),
            rule.match_title_regex.as_ref(),
        );
        matched &= ok;
    } else if rule.match_title_regex.is_some() {
        has_condition = true;
        if let Some(re) = rule.compiled_title_regex.as_ref() {
            matched &= re.is_match(&title);
        } else {
            matched = false;
        }
    }

    if let Some(pat) = &rule.match_process {
        has_condition = true;
        // process name: try GetWindowThreadProcessId + OpenProcess + GetModuleBaseName
        let proc_name = get_process_name(hwnd);
        let ok = matches_pattern(&proc_name, pat, None, None);
        matched &= ok;
    }

    has_condition && matched
}

pub fn get_process_name(hwnd: HWND) -> String {
    unsafe {
        let mut pid: u32 = 0;
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let handle = windows::Win32::System::Threading::OpenProcess(
            windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        );
        if let Ok(h) = handle {
            let mut buf = [0u16; 260];
            let mut len = buf.len() as u32;
            // QueryFullProcessImageNameW
            let ok = windows::Win32::System::Threading::QueryFullProcessImageNameW(
                h,
                windows::Win32::System::Threading::PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut len,
            );
            let _ = windows::Win32::Foundation::CloseHandle(h);
            if ok.is_ok() {
                let s = String::from_utf16_lossy(&buf[..len as usize]);
                // extract basename
                if let Some(pos) = s.rfind('\\') {
                    return s[pos + 1..].to_string();
                }
                if let Some(pos) = s.rfind('/') {
                    return s[pos + 1..].to_string();
                }
                return s;
            }
        }
        String::new()
    }
}

/// Returns true if window should float (not tiled) per rules
pub fn is_floating(hwnd: HWND) -> bool {
    let cfg = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for rule in &cfg.rules {
        if rule_matches(hwnd, rule) && rule.floating == Some(true) {
            return true;
        }
    }
    false
}

/// Get monitor target from rule, if any (e.g. rule monitor=2)
pub fn rule_monitor(_hwnd: HWND) -> Option<String> {
    let cfg = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for rule in &cfg.rules {
        if rule_matches(_hwnd, rule) {
            if let Some(m) = &rule.monitor {
                return Some(m.clone());
            }
        }
    }
    None
}

/// Get a per-monitor layout override. Config rule order is authoritative.
pub fn layout_for_windows(windows: &[HWND]) -> Option<String> {
    let cfg = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    cfg.rules
        .iter()
        .find(|rule| rule.layout.is_some() && windows.iter().any(|hwnd| rule_matches(*hwnd, rule)))
        .and_then(|rule| rule.layout.clone())
}

/// Get opacity from rule, if any (0.0-1.0)
pub fn rule_opacity(hwnd: HWND) -> Option<f32> {
    let cfg = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for rule in &cfg.rules {
        if rule_matches(hwnd, rule) {
            if let Some(o) = rule.opacity {
                return Some(o.clamp(0.0, 1.0));
            }
        }
    }
    None
}

static OPACITY_CACHE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<isize, u8>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

pub fn apply_opacity(hwnd: HWND, opacity: f32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE, LWA_ALPHA,
        WS_EX_LAYERED,
    };
    let alpha = (opacity.clamp(0.0, 1.0) * 255.0) as u8;
    {
        let mut cache = OPACITY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&prev) = cache.get(&(hwnd.0 as isize)) {
            if prev == alpha {
                return;
            }
        }
        cache.insert(hwnd.0 as isize, alpha);
        // prune destroyed windows lazily
        if cache.len() > 512 {
            let mut dead = Vec::new();
            for k in cache.keys() {
                let h = HWND(*k as *mut std::ffi::c_void);
                unsafe {
                    if !windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(h)).as_bool() {
                        dead.push(*k);
                    }
                }
            }
            for k in dead {
                cache.remove(&k);
            }
        }
    }
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if (ex & WS_EX_LAYERED.0) == 0 {
            let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex | WS_EX_LAYERED.0) as isize);
        }
        let _ = SetLayeredWindowAttributes(
            hwnd,
            windows::Win32::Foundation::COLORREF(0),
            alpha,
            LWA_ALPHA,
        );
    }
}

/// Execute on_create action if rule matches (rhai)
pub fn maybe_run_on_create(hwnd: HWND) {
    let actions: Vec<String> = {
        let cfg = crate::CURRENT_CONFIG
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cfg.rules
            .iter()
            .filter(|r| rule_matches(hwnd, r))
            .filter_map(|r| r.on_create.clone())
            .collect()
    };
    let actions: Vec<String> = {
        let mut seen = ON_CREATE_SEEN.lock().unwrap_or_else(|e| e.into_inner());
        actions
            .into_iter()
            .filter(|action| seen.insert((hwnd.0 as isize, action.clone())))
            .collect()
    };
    for act in actions {
        println!("[rules] on_create for {:?} -> '{}'", hwnd.0, act);
        crate::scripting::dispatch_action(&act);
    }
}

static ON_CREATE_SEEN: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<(isize, String)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

pub fn forget_window(hwnd: HWND) {
    ON_CREATE_SEEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(handle, _)| *handle != hwnd.0 as isize);
}

//! Rule engine — matches windows against `[[rules]]` in config.toml
//! See docs/EXTENSIBILITY.md §3. Supports class/title/process with exact or regex.
use regex::Regex;
use windows::Win32::Foundation::HWND;

use crate::config::RuleConfig;
use crate::util::{get_class_name, get_window_title};

fn matches_pattern(text: &str, pattern: &str, regex_pattern: Option<&String>) -> bool {
    if let Some(rx) = regex_pattern {
        if let Ok(re) = Regex::new(rx) {
            return re.is_match(text);
        } else {
            eprintln!("[rules] invalid regex '{}'", rx);
        }
    }
    // fallback: substring contains (case-insensitive for class? keep exact)
    if pattern.is_empty() { return false; }
    // for match_class / match_title, treat pattern as substring
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
        // allow exact or contains? use exact if equal, else contains
        let ok = matches_exact(&class, pat) || matches_pattern(&class, pat, rule.match_class_regex.as_ref());
        matched &= ok;
    } else if let Some(rx) = &rule.match_class_regex {
        has_condition = true;
        if let Ok(re) = Regex::new(rx) {
            matched &= re.is_match(&class);
        } else { matched = false; }
    }

    if let Some(pat) = &rule.match_title {
        has_condition = true;
        let ok = matches_pattern(&title, pat, rule.match_title_regex.as_ref());
        matched &= ok;
    } else if let Some(rx) = &rule.match_title_regex {
        has_condition = true;
        if let Ok(re) = Regex::new(rx) { matched &= re.is_match(&title); } else { matched = false; }
    }

    if let Some(pat) = &rule.match_process {
        has_condition = true;
        // process name: try GetWindowThreadProcessId + OpenProcess + GetModuleBaseName
        let proc_name = get_process_name(hwnd);
        let ok = matches_pattern(&proc_name, pat, None);
        matched &= ok;
    }

    has_condition && matched
}

fn get_process_name(hwnd: HWND) -> String {
    unsafe {
        let mut pid: u32 = 0;
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 { return String::new(); }
        let handle = windows::Win32::System::Threading::OpenProcess(
            windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        );
        if let Ok(h) = handle {
            let mut buf = [0u16; 260];
            let mut len = buf.len() as u32;
            // QueryFullProcessImageNameW
            let ok = windows::Win32::System::Threading::QueryFullProcessImageNameW(h, windows::Win32::System::Threading::PROCESS_NAME_FORMAT(0), windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
            let _ = windows::Win32::Foundation::CloseHandle(h);
            if ok.is_ok() {
                let s = String::from_utf16_lossy(&buf[..len as usize]);
                // extract basename
                if let Some(pos) = s.rfind('\\') { return s[pos+1..].to_string(); }
                if let Some(pos) = s.rfind('/') { return s[pos+1..].to_string(); }
                return s;
            }
        }
        String::new()
    }
}

/// Returns true if window should float (not tiled) per rules
pub fn is_floating(hwnd: HWND) -> bool {
    let cfg = crate::CURRENT_CONFIG.lock().unwrap();
    for rule in &cfg.rules {
        if rule_matches(hwnd, rule) && rule.floating == Some(true) {
            return true;
        }
    }
    false
}

/// Get monitor target from rule, if any (e.g. rule monitor=2)
pub fn rule_monitor(_hwnd: HWND) -> Option<String> {
    let cfg = crate::CURRENT_CONFIG.lock().unwrap();
    for rule in &cfg.rules {
        if rule_matches(_hwnd, rule) {
            if let Some(m) = &rule.monitor { return Some(m.clone()); }
        }
    }
    None
}

/// Get opacity from rule, if any (0.0-1.0)
pub fn rule_opacity(hwnd: HWND) -> Option<f32> {
    let cfg = crate::CURRENT_CONFIG.lock().unwrap();
    for rule in &cfg.rules {
        if rule_matches(hwnd, rule) {
            if let Some(o) = rule.opacity { return Some(o.clamp(0.0, 1.0)); }
        }
    }
    None
}

pub fn apply_opacity(hwnd: HWND, opacity: f32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SetWindowLongPtrW, SetLayeredWindowAttributes, GWL_EXSTYLE, WS_EX_LAYERED, LWA_ALPHA};
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if (ex & WS_EX_LAYERED.0) == 0 {
            let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex | WS_EX_LAYERED.0) as isize);
        }
        let alpha = (opacity.clamp(0.0, 1.0) * 255.0) as u8;
        let _ = SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), alpha, LWA_ALPHA);
    }
}

/// Execute on_create action if rule matches (rhai)
pub fn maybe_run_on_create(hwnd: HWND) {
    let actions: Vec<String> = {
        let cfg = crate::CURRENT_CONFIG.lock().unwrap();
        cfg.rules.iter()
            .filter(|r| rule_matches(hwnd, r))
            .filter_map(|r| r.on_create.clone())
            .collect()
    };
    for act in actions {
        println!("[rules] on_create for {:?} -> '{}'", hwnd.0, act);
        crate::scripting::dispatch_action(&act);
    }
}

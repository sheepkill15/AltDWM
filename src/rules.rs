//! Rule engine — matches windows against `[[rules]]` in config.toml
//! See docs/EXTENSIBILITY.md §3.
//!
//! Matching semantics, which used to be undocumented and surprising:
//!
//! * `match_class` and `match_process` are **exact**, case-insensitive. Wrap the
//!   pattern in `*` to match partially (`match_class = "*Chrome*"`). They used to
//!   fall back to a substring test, so a rule written for one application quietly
//!   captured every window whose class or executable merely contained the text —
//!   which presented as windows randomly refusing to tile.
//! * `match_title` is a case-insensitive **substring** test, because window
//!   titles are long and change at runtime. `*` wildcards work here too.
//! * `match_*_regex` variants are full regular expressions.
//!
//! Every condition present on a rule must match for the rule to apply.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use windows::Win32::Foundation::HWND;

use crate::config::RuleConfig;
use crate::util::{get_class_name, get_window_title};



/// Class name and executable name never change for a live window, so they are
/// resolved once. Process resolution in particular costs an `OpenProcess` round
/// trip that used to be paid for every window on every rule evaluation, several
/// times per tiling pass.
static CLASS_CACHE: LazyLock<Mutex<HashMap<isize, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PROCESS_CACHE: LazyLock<Mutex<HashMap<isize, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cached_class_name(hwnd: HWND) -> String {
    let key = hwnd.0 as isize;
    if let Some(cached) = CLASS_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
        .cloned()
    {
        return cached;
    }
    let class = get_class_name(hwnd);
    if !class.is_empty() {
        CLASS_CACHE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key, class.clone());
    }
    class
}

/// Case-insensitive glob supporting `*` (any run of characters) and `?` (one
/// character). Anchored at both ends.
fn glob_matches(text: &str, pattern: &str) -> bool {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    // Classic two-pointer glob with backtracking on the most recent `*`.
    let mut t = 0;
    let mut p = 0;
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            t += 1;
            p += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn has_wildcard(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

/// Exact by default, glob when the pattern says so.
fn matches_identifier(text: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if has_wildcard(pattern) {
        glob_matches(text, pattern)
    } else {
        text.eq_ignore_ascii_case(pattern)
    }
}

/// Substring by default, glob when the pattern says so. Titles are long and
/// mutable, so a substring test is the useful default here.
fn matches_title(text: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if has_wildcard(pattern) {
        glob_matches(text, pattern)
    } else {
        text.to_lowercase().contains(&pattern.to_lowercase())
    }
}

/// The window facts a rule can be tested against, gathered once per evaluation.
struct WindowFacts {
    class: String,
    title: String,
    process: Option<String>,
}

impl WindowFacts {
    fn gather(hwnd: HWND, needs_process: bool) -> Self {
        Self {
            class: cached_class_name(hwnd),
            title: get_window_title(hwnd),
            process: needs_process.then(|| get_process_name(hwnd)),
        }
    }
}

fn rule_needs_process(rule: &RuleConfig) -> bool {
    rule.match_process.is_some()
}

fn rule_matches_facts(facts: &WindowFacts, rule: &RuleConfig) -> bool {
    let mut has_condition = false;
    let mut matched = true;

    if let Some(pattern) = &rule.match_class {
        has_condition = true;
        matched &= matches_identifier(&facts.class, pattern);
    } else if rule.match_class_regex.is_some() {
        has_condition = true;
        matched &= rule
            .compiled_class_regex
            .as_ref()
            .is_some_and(|re| re.is_match(&facts.class));
    }

    if let Some(pattern) = &rule.match_title {
        has_condition = true;
        matched &= matches_title(&facts.title, pattern);
    } else if rule.match_title_regex.is_some() {
        has_condition = true;
        matched &= rule
            .compiled_title_regex
            .as_ref()
            .is_some_and(|re| re.is_match(&facts.title));
    }

    if let Some(pattern) = &rule.match_process {
        has_condition = true;
        let process = facts.process.as_deref().unwrap_or_default();
        matched &= matches_identifier(process, pattern);
    }

    // A rule with no conditions matches nothing. `validate` warns about these.
    has_condition && matched
}

/// Everything the configuration has to say about one window.
///
/// Resolving these together means the rule list is walked once per window per
/// tiling pass instead of once per property — each walk previously re-read the
/// class name, the title, and sometimes the executable name.
#[derive(Clone, Debug, Default)]
pub struct ResolvedRules {
    pub floating: Option<bool>,
    pub monitor: Option<String>,
    pub opacity: Option<f32>,
    pub layout: Option<String>,
    pub on_create: Vec<String>,
}

pub fn resolve(hwnd: HWND) -> ResolvedRules {
    let cfg = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if cfg.rules.is_empty() {
        return ResolvedRules::default();
    }
    let needs_process = cfg.rules.iter().any(rule_needs_process);
    let facts = WindowFacts::gather(hwnd, needs_process);
    let mut resolved = ResolvedRules::default();
    for rule in &cfg.rules {
        if !rule_matches_facts(&facts, rule) {
            continue;
        }
        // First matching rule wins per property, so config order is meaningful.
        if resolved.floating.is_none() {
            resolved.floating = rule.floating;
        }
        if resolved.monitor.is_none() {
            resolved.monitor = rule.monitor.clone();
        }
        if resolved.opacity.is_none() {
            resolved.opacity = rule.opacity.map(|value| value.clamp(0.0, 1.0));
        }
        if resolved.layout.is_none() {
            resolved.layout = rule.layout.clone();
        }
        if let Some(action) = &rule.on_create {
            resolved.on_create.push(action.clone());
        }
    }
    resolved
}

pub fn get_process_name(hwnd: HWND) -> String {
    let key = hwnd.0 as isize;
    if let Some(cached) = PROCESS_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
        .cloned()
    {
        return cached;
    }
    let name = query_process_name(hwnd);
    if !name.is_empty() {
        PROCESS_CACHE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key, name.clone());
    }
    name
}

fn query_process_name(hwnd: HWND) -> String {
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
        let Ok(h) = handle else {
            return String::new();
        };
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = windows::Win32::System::Threading::QueryFullProcessImageNameW(
            h,
            windows::Win32::System::Threading::PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(h);
        if ok.is_err() {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/'])
            .next()
            .unwrap_or(&path)
            .to_string()
    }
}

/// First matching explicit floating decision. This allows `floating = false`
/// to opt a window back into tiling even when automatic utility/constraint
/// heuristics would otherwise float it.
pub fn floating_decision(hwnd: HWND) -> Option<bool> {
    resolve(hwnd).floating
}

/// Returns true if a window is explicitly configured to float.
pub fn is_floating(hwnd: HWND) -> bool {
    floating_decision(hwnd) == Some(true)
}

/// Get monitor target from rule, if any (e.g. rule monitor=2)
pub fn rule_monitor(hwnd: HWND) -> Option<String> {
    resolve(hwnd).monitor
}

/// A per-monitor layout override, taken from the monitor's master window.
///
/// This used to apply if *any* window on the display matched, so one incidental
/// match re-laid out every other window there. Tying it to the master window
/// makes the intent expressible and the result predictable: "when this
/// application holds the master slot, use this layout".
pub fn layout_for_windows(windows: &[HWND]) -> Option<String> {
    windows.first().and_then(|hwnd| resolve(*hwnd).layout)
}

/// Windows AltDWM has made layered, with the ex-style to put back.
///
/// `WS_EX_LAYERED` changes the compositing path for the target application, so
/// leaving it set after a rule stops matching — or after AltDWM exits — is a
/// change to someone else's window that outlives the reason for it.
static LAYERED: LazyLock<Mutex<HashMap<isize, (u8, isize)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Bring every window's opacity in line with the rules, restoring any window
/// AltDWM previously made translucent that no longer has a rule.
pub fn sync_opacity(desired: &[(HWND, Option<f32>)]) {
    let wanted: HashMap<isize, u8> = desired
        .iter()
        .filter_map(|(hwnd, opacity)| {
            opacity.map(|value| (hwnd.0 as isize, (value.clamp(0.0, 1.0) * 255.0) as u8))
        })
        .collect();
    let stale: Vec<isize> = LAYERED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .keys()
        .copied()
        .filter(|key| !wanted.contains_key(key))
        .collect();
    for key in stale {
        restore_opacity(HWND(key as *mut std::ffi::c_void));
    }
    for (key, alpha) in wanted {
        apply_alpha(HWND(key as *mut std::ffi::c_void), alpha);
    }
}

fn apply_alpha(hwnd: HWND, alpha: u8) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE, LWA_ALPHA,
        WS_EX_LAYERED,
    };
    let key = hwnd.0 as isize;
    {
        let layered = LAYERED.lock().unwrap_or_else(|e| e.into_inner());
        if layered.get(&key).is_some_and(|(previous, _)| *previous == alpha) {
            return;
        }
    }
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let already_layered = (ex as u32 & WS_EX_LAYERED.0) != 0;
        if !already_layered {
            let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
        }
        let _ = SetLayeredWindowAttributes(
            hwnd,
            windows::Win32::Foundation::COLORREF(0),
            alpha,
            LWA_ALPHA,
        );
        LAYERED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, (alpha, ex));
    }
}

fn restore_opacity(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        IsWindow, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE, LWA_ALPHA,
    };
    let entry = LAYERED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(hwnd.0 as isize));
    let Some((_, original_ex_style)) = entry else {
        return;
    };
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return;
        }
        let _ = SetLayeredWindowAttributes(
            hwnd,
            windows::Win32::Foundation::COLORREF(0),
            255,
            LWA_ALPHA,
        );
        let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, original_ex_style);
    }
}

/// Put every window AltDWM made translucent back the way it was found.
pub fn restore_all_opacity() {
    let keys: Vec<isize> = LAYERED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .keys()
        .copied()
        .collect();
    for key in keys {
        restore_opacity(HWND(key as *mut std::ffi::c_void));
    }
}

/// Execute on_create action if rule matches (rhai)
pub fn maybe_run_on_create(hwnd: HWND) {
    let actions = resolve(hwnd).on_create;
    if actions.is_empty() {
        return;
    }
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

static ON_CREATE_SEEN: LazyLock<Mutex<std::collections::HashSet<(isize, String)>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// Drop cached state for windows that are no longer live.
pub fn retain_windows(live: &std::collections::HashSet<isize>) {
    CLASS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|key, _| live.contains(key));
    PROCESS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|key, _| live.contains(key));
    // LAYERED is deliberately not pruned here: an entry is the record of an
    // ex-style that still has to be put back, and `restore_all_opacity` needs it
    // even for a window that has left the managed set.
    //
    // ON_CREATE_SEEN is not pruned against `live` either, and for a sharper
    // reason: it is keyed by *any* window that ever matched an on_create rule,
    // including ones that are not manageable and so never appear in `live`.
    // Dropping those entries would let the action fire again on the window's
    // next SHOW event. It is bounded below instead.
    prune_dead_on_create_entries();
}

/// `ON_CREATE_SEEN` records that an action has already run for a window, so it
/// can only be pruned by whether the window still exists. That costs a syscall
/// per entry, so it is done only once the set has grown enough to be worth it.
fn prune_dead_on_create_entries() {
    const THRESHOLD: usize = 512;
    let mut seen = ON_CREATE_SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.len() < THRESHOLD {
        return;
    }
    seen.retain(|(handle, _)| unsafe {
        windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(HWND(
            *handle as *mut std::ffi::c_void,
        )))
        .as_bool()
    });
}

pub fn forget_window(hwnd: HWND) {
    let key = hwnd.0 as isize;
    ON_CREATE_SEEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(handle, _)| *handle != key);
    CLASS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    PROCESS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    LAYERED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
}

#[cfg(test)]
mod tests {
    use super::{glob_matches, matches_identifier, matches_title};

    #[test]
    fn identifiers_match_exactly_by_default() {
        assert!(matches_identifier("Chrome_WidgetWin_1", "chrome_widgetwin_1"));
        // The old substring fallback made this true, so a rule for "Chrome"
        // captured every class containing it.
        assert!(!matches_identifier("Chrome_WidgetWin_1", "Chrome"));
    }

    #[test]
    fn wildcards_opt_into_partial_matching() {
        assert!(matches_identifier("Chrome_WidgetWin_1", "*Chrome*"));
        assert!(matches_identifier("steamwebhelper.exe", "steam*"));
        assert!(!matches_identifier("notepad.exe", "steam*"));
        assert!(matches_identifier("code.exe", "cod?.exe"));
    }

    #[test]
    fn titles_still_match_on_substrings() {
        assert!(matches_title("Inbox — Mail", "inbox"));
        assert!(!matches_title("Inbox — Mail", "outbox"));
        assert!(matches_title("Inbox — Mail", "*mail"));
    }

    #[test]
    fn glob_handles_backtracking_and_anchoring() {
        assert!(glob_matches("aaa", "a*a"));
        assert!(glob_matches("abcabc", "*abc"));
        assert!(glob_matches("anything", "*"));
        assert!(!glob_matches("abc", "*d"));
        assert!(!glob_matches("abcd", "abc"));
        assert!(glob_matches("", "*"));
    }

    #[test]
    fn empty_patterns_never_match() {
        assert!(!matches_identifier("anything", ""));
        assert!(!matches_title("anything", ""));
    }
}

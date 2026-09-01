//! Installed-application index for the command center's search.
//!
//! The source is `shell:AppsFolder`, the virtual folder behind the Start menu's
//! "All apps" list. It is the right source for a launcher because it is the one
//! place that reports desktop programs and Store apps together, already
//! deduplicated and already carrying the display names the user recognises.
//! Walking the Start menu's `.lnk` files instead would miss every Store app and
//! pick up uninstallers and documentation shortcuts.
//!
//! Each entry keeps the AppsFolder-relative parsing name. Classic shell items
//! launch through `ShellExecuteW`; packaged AUMIDs use
//! `IApplicationActivationManager`, because treating them as file-like shell
//! paths intermittently returns `ERROR_ACCESS_DENIED`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppEntry {
    /// Display name, e.g. `Visual Studio Code`.
    pub name: String,
    /// AppUserModelID, the folder-relative parsing name.
    pub id: String,
    /// Small shell icon owned by the application index. Stored as a raw value
    /// so search results can be cloned without duplicating or freeing it.
    pub icon: isize,
    /// Lowercased name, kept so scoring does not re-allocate per keystroke.
    lowercase: String,
    /// First letter of each word, e.g. `vsc` for `Visual Studio Code`.
    initials: String,
}

static INDEX: LazyLock<Arc<Mutex<Vec<AppEntry>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));
static INDEXING: AtomicBool = AtomicBool::new(false);
static INDEXED: AtomicBool = AtomicBool::new(false);

/// Build the index on a worker thread.
///
/// Enumerating the apps folder takes long enough on a machine with many Store
/// apps that doing it on the UI thread would visibly stall the shell, and it
/// involves COM, so it gets its own apartment.
pub fn begin_indexing() {
    if INDEXING.swap(true, Ordering::SeqCst) {
        return;
    }
    let index = INDEX.clone();
    let spawned = std::thread::Builder::new()
        .name("AltDWM-apps".into())
        .spawn(move || {
            // Clears the in-progress flag however this thread ends, panic
            // included. Without it a single failed enumeration left the flag set
            // for the life of the process, and `refresh` — which returns early
            // when indexing is already under way — could never retry.
            let _guard = IndexingGuard;
            let mut entries = enumerate();
            entries.sort_by(|a, b| a.lowercase.cmp(&b.lowercase));
            let mut ids = HashSet::new();
            entries.retain(|entry| {
                if ids.insert(entry.id.clone()) {
                    true
                } else {
                    release_icon(entry.icon);
                    false
                }
            });
            let icon_count = entries.iter().filter(|entry| entry.icon != 0).count();
            println!(
                "[apps] indexed {} applications ({icon_count} icons)",
                entries.len()
            );
            let mut index = index.lock().unwrap_or_else(|error| error.into_inner());
            for previous in index.drain(..) {
                release_icon(previous.icon);
            }
            *index = entries;
            INDEXED.store(true, Ordering::SeqCst);
            crate::command_center::invalidate();
        });
    if spawned.is_err() {
        INDEXING.store(false, Ordering::SeqCst);
    }
}

/// Resets `INDEXING` when the worker thread ends, for any reason.
struct IndexingGuard;

impl Drop for IndexingGuard {
    fn drop(&mut self) {
        INDEXING.store(false, Ordering::SeqCst);
    }
}

/// Discard the index and rebuild it. Used after an install or uninstall.
pub fn refresh() {
    INDEXED.store(false, Ordering::SeqCst);
    begin_indexing();
}

#[cfg(test)]
mod guard_tests {
    use super::{IndexingGuard, INDEXING};
    use std::sync::atomic::Ordering;

    #[test]
    fn the_guard_clears_the_in_progress_flag_on_any_exit() {
        INDEXING.store(true, Ordering::SeqCst);
        {
            let _guard = IndexingGuard;
        }
        assert!(
            !INDEXING.load(Ordering::SeqCst),
            "a failed index must not block every later refresh"
        );
    }
}

pub fn is_ready() -> bool {
    INDEXED.load(Ordering::SeqCst)
}

pub fn count() -> usize {
    INDEX
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .len()
}

/// Extensions the apps folder exposes that are documents rather than programs.
///
/// The Start menu carries a manual, a changelog, and a quick-start guide for
/// half the software on a machine. They belong in a file search, not a launcher.
const DOCUMENT_EXTENSIONS: &[&str] = &[
    "chm", "doc", "docx", "htm", "html", "log", "md", "pdf", "ppt", "pptx", "rtf", "txt", "xls",
    "xlsx",
];

fn is_document(id: &str) -> bool {
    let tail = id.rsplit(['\\', '/']).next().unwrap_or(id);
    tail.rsplit_once('.').is_some_and(|(_, extension)| {
        DOCUMENT_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
    })
}

fn make_entry(name: String, id: String) -> Option<AppEntry> {
    let name = name.trim().to_string();
    if name.is_empty() || id.trim().is_empty() {
        return None;
    }
    if is_document(&id) {
        return None;
    }
    let lowercase = name.to_lowercase();
    let initials = lowercase
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .filter_map(|word| word.chars().next())
        .collect();
    Some(AppEntry {
        name,
        id: id.trim().to_string(),
        icon: 0,
        lowercase,
        initials,
    })
}

/// Resolution icons are cached at. The picker draws them around 24 DIP, which is
/// 48 physical pixels at 200% scaling, so a 32-pixel source had to be stretched
/// and looked pixelated. Caching at 64 lets every common display downscale a
/// crisp source instead of upscaling a coarse one.
pub const ICON_SIZE: i32 = 64;

fn shell_icon(item: &windows::Win32::UI::Shell::IShellItem) -> isize {
    use windows::core::Interface;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::UI::Shell::{IShellItemImageFactory, SIIGBF_ICONONLY, SIIGBF_RESIZETOFIT};

    let Ok(factory) = item.cast::<IShellItemImageFactory>() else {
        return 0;
    };
    unsafe {
        factory
            .GetImage(
                SIZE {
                    cx: ICON_SIZE,
                    cy: ICON_SIZE,
                },
                SIIGBF_ICONONLY | SIIGBF_RESIZETOFIT,
            )
            .map(|bitmap| bitmap.0 as isize)
            .unwrap_or(0)
    }
}

fn release_icon(icon: isize) {
    if icon != 0 {
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(
                windows::Win32::Graphics::Gdi::HBITMAP(icon as *mut std::ffi::c_void).into(),
            );
        }
    }
}

fn enumerate() -> Vec<AppEntry> {
    use windows::core::w;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{
        IEnumShellItems, IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY,
        SIGDN_PARENTRELATIVEPARSING,
    };

    // The shell's item enumerators expect an STA.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let mut entries = Vec::new();
    unsafe {
        let folder: IShellItem = match SHCreateItemFromParsingName(w!("shell:AppsFolder"), None) {
            Ok(folder) => folder,
            Err(error) => {
                eprintln!("[apps] cannot open shell:AppsFolder: {error:?}");
                return entries;
            }
        };
        let enumerator: IEnumShellItems =
            match folder.BindToHandler(None, &windows::Win32::UI::Shell::BHID_EnumItems) {
                Ok(enumerator) => enumerator,
                Err(error) => {
                    eprintln!("[apps] cannot enumerate shell:AppsFolder: {error:?}");
                    return entries;
                }
            };
        loop {
            let mut fetched: [Option<IShellItem>; 1] = [None];
            let mut count = 0u32;
            if enumerator.Next(&mut fetched, Some(&mut count)).is_err() || count == 0 {
                break;
            }
            let Some(item) = fetched[0].take() else {
                break;
            };
            let name = item
                .GetDisplayName(SIGDN_NORMALDISPLAY)
                .ok()
                .and_then(|value| {
                    let text = value.to_string().ok();
                    windows::Win32::System::Com::CoTaskMemFree(Some(value.0 as *const _));
                    text
                })
                .unwrap_or_default();
            let id = item
                .GetDisplayName(SIGDN_PARENTRELATIVEPARSING)
                .ok()
                .and_then(|value| {
                    let text = value.to_string().ok();
                    windows::Win32::System::Com::CoTaskMemFree(Some(value.0 as *const _));
                    text
                })
                .unwrap_or_default();
            if let Some(mut entry) = make_entry(name, id) {
                entry.icon = shell_icon(&item);
                entries.push(entry);
            }
        }
    }
    entries
}

/// How well `query` matches `entry`, higher is better; `None` means no match.
///
/// The tiers exist so that typing `code` puts *Visual Studio Code* above
/// *Encoder Settings*: an exact name beats a prefix, a prefix beats a word
/// start, a word start beats a bare substring, and initials
/// (`vsc` → *Visual Studio Code*) rank just under that. A subsequence match is
/// the last resort so that typos still find something.
fn score(entry: &AppEntry, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let name = entry.lowercase.as_str();
    // Shorter names win ties: "Code" should outrank "Code Runner Settings".
    let brevity = 100i32.saturating_sub(name.chars().count() as i32).max(0);

    if name == query {
        return Some(1000 + brevity);
    }
    if name.starts_with(query) {
        return Some(800 + brevity);
    }
    let word_start = name
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word.starts_with(query));
    if word_start {
        return Some(600 + brevity);
    }
    if entry.initials.starts_with(query) {
        return Some(500 + brevity);
    }
    if name.contains(query) {
        return Some(400 + brevity);
    }
    fuzzy_score(name, query).map(|score| score + brevity / 4)
}

/// Word-anchored fuzzy match: every query character appears in order, and the
/// first one must begin a word.
///
/// A plain subsequence test is far too permissive for a launcher. Typing `code`
/// matched *Microsoft Edge* — `mi(c)r(o)soft E(d)g(e)` — burying the app that
/// was actually wanted. Requiring the first character to start a word removes
/// almost all of that noise, and the gap penalty ranks tight matches above
/// scattered ones.
fn fuzzy_score(text: &str, query: &str) -> Option<i32> {
    let chars: Vec<char> = text.chars().collect();
    let query: Vec<char> = query.chars().collect();
    if query.is_empty() || chars.is_empty() {
        return None;
    }
    let starts_word = |index: usize| index == 0 || !chars[index - 1].is_alphanumeric();

    let mut matched = 0usize;
    let mut previous = 0usize;
    let mut boundary_hits = 0i32;
    let mut gaps = 0i32;
    for (index, character) in chars.iter().enumerate() {
        if matched == query.len() || *character != query[matched] {
            continue;
        }
        if matched == 0 {
            // Anchor: keep looking until the character begins a word.
            if !starts_word(index) {
                continue;
            }
        } else {
            // Weighted so a run of skipped characters costs more than a
            // word-boundary hit earns; otherwise any string with the right
            // initials scattered across it outranks a tight match.
            gaps += (index - previous - 1) as i32 * 4;
        }
        if starts_word(index) {
            boundary_hits += 1;
        }
        previous = index;
        matched += 1;
    }
    if matched != query.len() {
        return None;
    }
    Some(150 + boundary_hits * 12 - gaps.min(120))
}

/// Best matches for `query`, most relevant first.
pub fn search(query: &str, limit: usize) -> Vec<AppEntry> {
    let query = query.trim().to_lowercase();
    let index = INDEX.lock().unwrap_or_else(|error| error.into_inner());
    let mut scored: Vec<(i32, &AppEntry)> = index
        .iter()
        .filter_map(|entry| score(entry, &query).map(|score| (score, entry)))
        .collect();
    // Stable ordering for equal scores: the index is already sorted by name.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, entry)| entry.clone())
        .collect()
}

/// Launch an indexed application.
pub fn launch(entry: &AppEntry) {
    if crate::elevation::normal_launch_broker_required() {
        crate::startup::launch_app_at_normal_integrity(entry.id.clone(), entry.name.clone());
    } else if is_packaged_app_id(&entry.id) {
        println!("[apps] activate packaged {} ({})", entry.name, entry.id);
        if let Err(error) = activate_packaged_app(&entry.id) {
            eprintln!("[apps] failed to activate {}: {error}", entry.name);
        }
    } else {
        let _ = shell_launch(entry, None);
    }
}

/// Launch an indexed application elevated, prompting UAC.
///
/// The AppsFolder item exposes the same `runas` verb the Start menu's "Run as
/// administrator" uses, so classic desktop apps elevate through it. Store apps
/// have no such verb and simply cannot run elevated; the request fails
/// harmlessly there rather than doing anything unexpected.
pub fn launch_as_admin(entry: &AppEntry) {
    if crate::elevation::normal_launch_broker_required() {
        crate::startup::launch_app_as_admin_via_user_helper(entry.id.clone(), entry.name.clone());
        return;
    }
    if shell_launch(entry, Some(windows::core::w!("runas"))).is_err()
        && is_packaged_app_id(&entry.id)
    {
        // Not every package exposes an elevated verb. A normal activation is
        // still preferable to making the command-center action a dead button.
        eprintln!(
            "[apps] {} does not expose an elevated AppsFolder launch; activating normally",
            entry.name
        );
        if let Err(error) = activate_packaged_app(&entry.id) {
            eprintln!("[apps] failed to activate {}: {error}", entry.name);
        }
    }
}

fn shell_launch(entry: &AppEntry, verb: Option<windows::core::PCWSTR>) -> Result<(), isize> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let target = format!("shell:AppsFolder\\{}", entry.id);
    let wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let elevated = verb.is_some();
    println!(
        "[apps] launch{} {} ({})",
        if elevated { " as admin" } else { "" },
        entry.name,
        entry.id
    );
    unsafe {
        let result = ShellExecuteW(
            None,
            verb.unwrap_or_else(PCWSTR::null),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        // ShellExecuteW reports failure as a pseudo-handle of 32 or less. A user
        // declining the UAC prompt lands here too, which is not an error worth
        // shouting about, but the log line is still useful.
        let code = result.0 as isize;
        if code <= 32 {
            eprintln!(
                "[apps] failed to launch{} {}: {}",
                if elevated { " as admin" } else { "" },
                entry.name,
                code
            );
            return Err(code);
        }
    }
    Ok(())
}

pub(crate) fn is_packaged_app_id(id: &str) -> bool {
    id.split_once('!')
        .is_some_and(|(package, application)| !package.is_empty() && !application.is_empty())
}

/// Activate a package by its AUMID instead of treating `shell:AppsFolder` as a
/// file path. ShellExecute is reliable for classic shell items but returns
/// `ERROR_ACCESS_DENIED` for some packaged applications even at normal
/// integrity; this is the API Windows exposes for their launch contract.
pub(crate) fn activate_packaged_app(id: &str) -> Result<u32, String> {
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        ApplicationActivationManager, IApplicationActivationManager, AO_NONE,
    };

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let manager: IApplicationActivationManager =
            CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)
                .map_err(|error| format!("activation manager unavailable: {error:?}"))?;
        let id = id
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        manager
            .ActivateApplication(
                windows::core::PCWSTR(id.as_ptr()),
                windows::core::PCWSTR::null(),
                AO_NONE,
            )
            .map_err(|error| format!("ActivateApplication failed: {error:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{fuzzy_score, is_document, is_packaged_app_id, make_entry, score};

    fn entry(name: &str) -> super::AppEntry {
        make_entry(name.into(), format!("{name}.id")).expect("valid entry")
    }

    #[test]
    fn entries_need_a_name_and_an_id() {
        assert!(make_entry("  ".into(), "some.id".into()).is_none());
        assert!(make_entry("App".into(), "  ".into()).is_none());
        assert!(make_entry(" App ".into(), "id".into()).is_some());
    }

    #[test]
    fn packaged_ids_are_distinguished_from_classic_shell_items() {
        assert!(is_packaged_app_id(
            "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"
        ));
        assert!(is_packaged_app_id("OpenAI.Codex_2p2nqsd0c76g0!App"));
        assert!(!is_packaged_app_id(
            r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe"
        ));
        assert!(!is_packaged_app_id("broken!"));
    }

    #[test]
    fn initials_come_from_word_starts() {
        assert_eq!(entry("Visual Studio Code").initials, "vsc");
        assert_eq!(entry("7-Zip File Manager").initials, "7zfm");
    }

    #[test]
    fn an_exact_name_outranks_a_prefix_and_a_substring() {
        let query = "code";
        let exact = score(&entry("Code"), query).unwrap();
        let prefix = score(&entry("Code Runner"), query).unwrap();
        let word = score(&entry("Visual Studio Code"), query).unwrap();
        let substring = score(&entry("Encoder"), query).unwrap();
        assert!(exact > prefix, "{exact} > {prefix}");
        assert!(prefix > word, "{prefix} > {word}");
        assert!(word > substring, "{word} > {substring}");
    }

    #[test]
    fn initials_match_multi_word_names() {
        assert!(score(&entry("Visual Studio Code"), "vsc").is_some());
        assert!(score(&entry("Visual Studio Code"), "vsq").is_none());
    }

    #[test]
    fn shorter_names_win_ties() {
        let short = score(&entry("Terminal"), "term").unwrap();
        let long = score(&entry("Terminal Preview Insider"), "term").unwrap();
        assert!(short > long, "{short} > {long}");
    }

    #[test]
    fn unrelated_queries_do_not_match() {
        assert!(score(&entry("Calculator"), "photoshop").is_none());
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert!(score(&entry("Anything"), "").is_some());
    }

    #[test]
    fn fuzzy_matching_is_anchored_at_a_word_start() {
        // The regression this exists for: "code" used to match "Microsoft Edge"
        // as a bare subsequence and outrank nothing in particular.
        assert!(score(&entry("Microsoft Edge"), "code").is_none());
        assert!(fuzzy_score("microsoft edge", "code").is_none());
        // Genuine abbreviations still match.
        assert!(fuzzy_score("calculator", "clc").is_some());
        assert!(fuzzy_score("visual studio code", "vsc").is_some());
        // Order still matters, and unrelated queries still fail.
        assert!(fuzzy_score("abc", "cba").is_none());
        assert!(fuzzy_score("calculator", "zq").is_none());
    }

    #[test]
    fn word_starts_and_tight_runs_both_raise_the_score() {
        // Same number of word-boundary hits, different spread: the tighter run wins.
        let tight = fuzzy_score("cold", "cld").unwrap();
        let spread = fuzzy_score("caballero dorado", "cld").unwrap();
        assert!(tight > spread, "{tight} > {spread}");
        // Landing on word starts is worth more than landing mid-word.
        let boundaries = fuzzy_score("copy line down", "cld").unwrap();
        assert!(boundaries > spread, "{boundaries} > {spread}");
    }

    #[test]
    fn documentation_entries_are_not_applications() {
        assert!(is_document(r"C:\ProgramData\mok\Waverazor_Manual.pdf"));
        assert!(is_document(r"{GUID}\WinRAR\WhatsNew.txt"));
        assert!(is_document(r"{GUID}\WinRAR\WinRAR.chm"));
        assert!(!is_document(r"{GUID}\WinRAR\WinRAR.exe"));
        assert!(!is_document(r"{GUID}\WF.msc"));
        assert!(!is_document("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"));
        assert!(!is_document("steam://rungameid/302550"));
        assert!(make_entry("WinRAR help".into(), r"{GUID}\WinRAR\WinRAR.chm".into()).is_none());
    }
}

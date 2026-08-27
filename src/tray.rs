//! Explorer notification-area bridge for development/non-shell-replacement mode.
//!
//! Windows exposes tray buttons through UI Automation, but it does not expose a
//! documented global notification-icon enumeration API. This bridge mirrors the
//! live Explorer `SystemTray.*` buttons and invokes them on click. A future full
//! shell replacement still needs to receive Shell_NotifyIcon traffic itself.

use std::sync::{
    mpsc::{self, Receiver, Sender},
    Arc, LazyLock, Mutex,
};
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern,
    TreeScope_Descendants, UIA_ButtonControlTypeId, UIA_InvokePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

const REFRESH_INTERVAL: Duration = Duration::from_millis(750);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayEntry {
    pub name: String,
}

#[derive(Clone)]
struct CachedEntry {
    name: String,
    element: IUIAutomationElement,
}

enum Command {
    Invoke(usize),
}

struct TrayWorker {
    entries: Arc<Mutex<Vec<TrayEntry>>>,
    commands: Sender<Command>,
}

static WORKER: LazyLock<TrayWorker> = LazyLock::new(start_worker);

fn start_worker() -> TrayWorker {
    let entries = Arc::new(Mutex::new(Vec::new()));
    let worker_entries = entries.clone();
    let (commands, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("AltDWM-tray".into())
        .spawn(move || worker_loop(worker_entries, receiver))
        .expect("failed to start tray worker");
    TrayWorker { entries, commands }
}

fn create_automation() -> Option<IUIAutomation> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()
    }
}

fn discover(client: &IUIAutomation) -> Vec<CachedEntry> {
    unsafe {
        let Ok(taskbar) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) else {
            return Vec::new();
        };
        let Ok(root) = client.ElementFromHandle(taskbar) else {
            return Vec::new();
        };
        let Ok(condition) = client.CreateTrueCondition() else {
            return Vec::new();
        };
        let Ok(elements) = root.FindAll(TreeScope_Descendants, &condition) else {
            return Vec::new();
        };
        let count = elements.Length().unwrap_or(0).clamp(0, 256);
        let mut found = Vec::new();
        let verbose = std::env::var_os("ALT_DWM_VERBOSE").is_some();
        for index in 0..count {
            let Ok(element) = elements.GetElement(index) else {
                continue;
            };
            if element.CurrentControlType().ok() != Some(UIA_ButtonControlTypeId) {
                continue;
            }
            let class = element
                .CurrentClassName()
                .map(|value| value.to_string())
                .unwrap_or_default();
            let automation_id = element
                .CurrentAutomationId()
                .map(|value| value.to_string())
                .unwrap_or_default();
            let name = element
                .CurrentName()
                .map(|value| value.to_string())
                .unwrap_or_default();
            if verbose {
                println!(
                    "[tray:uia] class={:?} id={:?} name={:?}",
                    class, automation_id, name
                );
            }
            let class_lower = class.to_ascii_lowercase();
            let id_lower = automation_id.to_ascii_lowercase();
            let is_tray = class_lower.starts_with("systemtray.")
                || id_lower.contains("systemtray")
                || id_lower.contains("notificationarea");
            if !is_tray {
                continue;
            }
            // AltDWM deliberately hides Explorer's taskbar. Its notification
            // buttons then report off-screen but remain valid invocation targets.
            let name = name.trim().to_string();
            if name.is_empty() {
                continue;
            }
            found.push(CachedEntry { name, element });
        }
        found
    }
}

fn worker_loop(entries: Arc<Mutex<Vec<TrayEntry>>>, receiver: Receiver<Command>) {
    let Some(client) = create_automation() else {
        eprintln!("[tray] UI Automation unavailable; Explorer tray bridge disabled");
        return;
    };
    let mut cached: Vec<CachedEntry> = Vec::new();
    loop {
        let discovered = discover(&client);
        // Explorer may stop exposing descendants once its taskbar is hidden.
        // Retain the primed invocation elements until it becomes visible again.
        if !discovered.is_empty() || !crate::shell::native_taskbars_are_hidden() {
            cached = discovered;
        }
        let visible: Vec<TrayEntry> = cached
            .iter()
            .map(|entry| TrayEntry {
                name: entry.name.clone(),
            })
            .collect();
        let changed = entries
            .lock()
            .map(|current| *current != visible)
            .unwrap_or(true);
        if changed {
            println!(
                "[tray] Explorer items: {}",
                visible
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            *entries.lock().unwrap_or_else(|error| error.into_inner()) = visible;
            crate::panel::invalidate_all();
        }
        match receiver.recv_timeout(REFRESH_INTERVAL) {
            Ok(Command::Invoke(index)) => {
                if let Some(entry) = cached.get(index) {
                    unsafe {
                        if let Ok(pattern) = entry
                            .element
                            .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                        {
                            let _ = pattern.Invoke();
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

pub fn entries() -> Vec<TrayEntry> {
    WORKER
        .entries
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// Start discovery while Explorer's taskbar is still visible and briefly wait
/// for the first snapshot before shell chrome is hidden.
pub fn prime() {
    LazyLock::force(&WORKER);
    for _ in 0..40 {
        if !entries().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn invoke(index: usize) {
    let _ = WORKER.commands.send(Command::Invoke(index));
}

pub fn compact_name(name: &str) -> String {
    let normalized = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = normalized.to_lowercase();
    if lower.starts_with("show hidden") {
        return "More".into();
    }
    if lower.starts_with("tray input indicator") {
        return normalized
            .split_once('(')
            .and_then(|(_, language)| language.split_whitespace().next())
            .and_then(|language| language.chars().next())
            .map(|initial| initial.to_uppercase().to_string())
            .unwrap_or_else(|| "Input".into());
    }
    if lower.starts_with("network") {
        return "Network".into();
    }
    if lower.starts_with("volume") {
        return "Audio".into();
    }
    if lower.starts_with("clock") {
        return "Clock".into();
    }
    if lower.starts_with("show desktop") {
        return "Desk".into();
    }
    let mut text: String = normalized.chars().take(12).collect();
    if normalized.chars().count() > 12 {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::compact_name;

    #[test]
    fn tray_labels_are_unicode_safe_and_bounded() {
        assert_eq!(compact_name("Bluetooth"), "Bluetooth");
        assert_eq!(compact_name("Volume\r\nSpeakers"), "Audio");
        assert_eq!(compact_name("1234567890123"), "123456789012…");
        assert_eq!(
            compact_name("音量とネットワーク設定パネル"),
            "音量とネットワーク設定パ…"
        );
        assert_eq!(compact_name("Show Hidden Icons"), "More");
        assert_eq!(compact_name("Network Internet access"), "Network");
        assert_eq!(compact_name("Volume Speakers: 24%"), "Audio");
    }
}

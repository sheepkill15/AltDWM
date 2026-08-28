//! Notification area — AltDWM's own `Shell_NotifyIcon` host.
//!
//! Windows has no API for enumerating another application's tray icons. A shell
//! has exactly two ways to show them:
//!
//! * **Be** the tray. `Shell_NotifyIcon` resolves `FindWindow("Shell_TrayWnd")`
//!   and hands the icon to whatever answers, over `WM_COPYDATA`. Owning that
//!   class is the only route that yields real `HICON`s, real tooltips, and
//!   working click semantics, because the payload *is* the application's
//!   `NOTIFYICONDATA`. That is what this module does.
//! * Mirror Explorer's buttons through UI Automation. That is the fallback at
//!   the bottom of this file, for sessions that leave the native taskbar alone.
//!   It can name buttons and invoke them; it can never see their icons, and it
//!   goes blank the moment Explorer's taskbar is hidden — which is exactly the
//!   configuration AltDWM runs in by default.
//!
//! The wire format is not documented, but it is stable: a `DWORD` signature, a
//! `DWORD` `NIM_*` message, then the caller's `NOTIFYICONDATAW` verbatim. It is
//! verbatim in the *sender's* layout, so a 32-bit application on 64-bit Windows
//! sends 4-byte handles and different field offsets. `wire_layout` reconstructs
//! the offsets from the sender's pointer width and the struct version it
//! declares, rather than casting the buffer to one hard-coded `#[repr(C)]`.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{
    mpsc::{self, Receiver, Sender},
    Arc, LazyLock, Mutex,
};
use std::time::Duration;

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::{
    IsWow64Process, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern,
    TreeScope_Descendants, UIA_ButtonControlTypeId, UIA_InvokePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

// ------------------------------------------------------------------ model ---

/// Stable identity for a tray item across refreshes.
///
/// The widget hit-tests a rectangle and then has to invoke *that* item. Bare
/// indices were the old failure: an icon appearing between paint and click
/// shifted everything along and the click landed on a neighbour.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum TrayId {
    /// An icon this process received through `Shell_NotifyIcon`.
    Native { owner: isize, uid: u32 },
    /// A button mirrored out of Explorer's tray, addressed by position because
    /// UI Automation gives us nothing more durable.
    Explorer { index: usize },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TrayEntry {
    pub id: TrayId,
    /// The tooltip the owning application set — what it calls itself.
    pub name: String,
    /// `HICON` as an integer. 0 when the source cannot supply one.
    pub icon: isize,
    /// `NIS_HIDDEN`: the application asked to sit in the overflow rather than
    /// on the bar itself.
    pub hidden: bool,
}

/// Which mouse gesture to forward to the owning application.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Left,
    DoubleLeft,
    Right,
}

/// Where tray items come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// Host the notification area when AltDWM owns the taskbar, mirror
    /// Explorer's when it does not.
    Auto,
    /// Always host it, even alongside a visible Explorer taskbar. Icons then
    /// arrive here instead of there.
    Native,
    /// Always mirror Explorer's buttons over UI Automation.
    Explorer,
    Off,
}

impl Source {
    pub fn parse(value: &str) -> Source {
        match value.trim().to_ascii_lowercase().as_str() {
            "native" | "host" | "shell" => Source::Native,
            "explorer" | "uia" | "mirror" => Source::Explorer,
            "off" | "none" | "disabled" => Source::Off,
            _ => Source::Auto,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Source::Auto => "auto",
            Source::Native => "native",
            Source::Explorer => "explorer",
            Source::Off => "off",
        }
    }
}

/// The resolved source, so `entries()` does not re-read configuration on every
/// paint.
static ACTIVE: AtomicU8 = AtomicU8::new(ACTIVE_NONE);
const ACTIVE_NONE: u8 = 0;
const ACTIVE_NATIVE: u8 = 1;
const ACTIVE_EXPLORER: u8 = 2;

/// Start the notification area for `source`.
///
/// `owns_taskbar` decides what `auto` means: hosting the tray takes icons away
/// from Explorer's taskbar, which is right when AltDWM has hidden it and rude
/// when it has not.
pub fn start(source: Source, owns_taskbar: bool) {
    if source == Source::Off {
        ACTIVE.store(ACTIVE_NONE, Ordering::SeqCst);
        println!("[tray] disabled by configuration");
        return;
    }
    println!(
        "[tray] source={} taskbar={}",
        source.name(),
        if owns_taskbar { "altdwm" } else { "explorer" }
    );
    let host = match source {
        Source::Native => true,
        Source::Explorer => false,
        _ => owns_taskbar,
    };
    if host {
        match native::start() {
            Ok(created) => {
                if created {
                    PENDING_ANNOUNCE.store(true, Ordering::SeqCst);
                }
                ACTIVE.store(ACTIVE_NATIVE, Ordering::SeqCst);
                return;
            }
            Err(error) => {
                eprintln!("[tray] could not host the notification area: {error}");
                eprintln!("[tray] falling back to mirroring Explorer's tray");
            }
        }
    }
    // A reload can turn hosting off. Giving the tray back is not optional: the
    // window would go on answering `FindWindow` for icons nothing displays.
    if ACTIVE.load(Ordering::SeqCst) == ACTIVE_NATIVE {
        native::shutdown();
    }
    ACTIVE.store(ACTIVE_EXPLORER, Ordering::SeqCst);
    explorer::prime();
}

/// Tell every application to re-publish its icons.
///
/// Called once the native taskbar is out of the way, because `Shell_NotifyIcon`
/// resolves the tray window at call time: an application that registered before
/// AltDWM's `Shell_TrayWnd` existed handed its icon to Explorer and would never
/// mention it again unless asked.
pub fn announce() {
    if ACTIVE.load(Ordering::SeqCst) == ACTIVE_NATIVE
        && PENDING_ANNOUNCE.swap(false, Ordering::SeqCst)
    {
        native::announce();
    }
}

/// Set when a host window is created, cleared when the broadcast goes out. A
/// configuration reload runs the same start/hide/announce sequence as startup,
/// and re-broadcasting would make every application in the session tear its icon
/// down and rebuild it for nothing.
static PENDING_ANNOUNCE: AtomicBool = AtomicBool::new(false);

/// Hand the notification area back on the way out: the host window is destroyed
/// first, so the broadcast sends applications to Explorer rather than to a
/// window that is about to disappear.
pub fn shutdown() {
    if ACTIVE.swap(ACTIVE_NONE, Ordering::SeqCst) == ACTIVE_NATIVE {
        native::shutdown();
    }
}

pub fn entries() -> Vec<TrayEntry> {
    match ACTIVE.load(Ordering::SeqCst) {
        ACTIVE_NATIVE => native::entries(),
        ACTIVE_EXPLORER => explorer::entries(),
        _ => Vec::new(),
    }
}

/// Forward a click to whoever owns the item.
pub fn invoke(id: TrayId, button: Button) {
    match id {
        TrayId::Native { owner, uid } => native::invoke(owner, uid, button),
        TrayId::Explorer { index } => explorer::invoke(index),
    }
}

/// Shorten a tooltip for a label-only surface. Tray tooltips are frequently a
/// multi-line status dump; a bar has room for a few characters of it.
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

/// The first line of a tooltip, which is the part that names the application.
/// The rest is status detail that changes constantly.
pub fn title_line(name: &str) -> String {
    let line = name
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if line.is_empty() {
        "Notification icon".into()
    } else {
        line.to_string()
    }
}

// ------------------------------------------------------------ native host ---

/// AltDWM's own notification area: the `Shell_TrayWnd` window, the icon table it
/// accumulates, and the click routing back to each owner.
mod native {
    use super::*;

    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, CopyIcon, CreateWindowExW, DefWindowProcW, DestroyIcon,
        DestroyWindow, GetCursorPos, GetSystemMetrics, GetWindowThreadProcessId, IsWindow,
        KillTimer, PostMessageW, RegisterWindowMessageW, SendNotifyMessageW, SetForegroundWindow,
        SetTimer, SetWindowPos, HICON, HMENU, HWND_BROADCAST, HWND_TOPMOST, SM_CXSCREEN,
        SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WINDOW_EX_STYLE, WM_CONTEXTMENU,
        WM_COPYDATA, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN,
        WM_RBUTTONUP, WM_TIMER, WS_CHILD, WS_CLIPCHILDREN,
        WS_CLIPSIBLINGS, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    };

    /// `NIM_*`
    const NIM_ADD: u32 = 0;
    const NIM_MODIFY: u32 = 1;
    const NIM_DELETE: u32 = 2;
    const NIM_SETFOCUS: u32 = 3;
    const NIM_SETVERSION: u32 = 4;

    /// `NIF_*`
    const NIF_MESSAGE: u32 = 0x01;
    const NIF_ICON: u32 = 0x02;
    const NIF_TIP: u32 = 0x04;
    const NIF_STATE: u32 = 0x08;
    const NIF_GUID: u32 = 0x20;

    /// `NIS_HIDDEN`
    const NIS_HIDDEN: u32 = 0x01;

    /// `WM_COPYDATA` discriminators used by shell32.
    const COPY_APPBAR: usize = 0;
    const COPY_TRAY: usize = 1;
    const COPY_ICON_RECT: usize = 3;

    /// `NIN_SELECT` lives in the `WM_USER` range.
    const NIN_SELECT: u32 = 0x0400;

    const TIMER_SWEEP: usize = 1;
    const SWEEP_INTERVAL: u32 = 2000;

    #[derive(Clone)]
    struct Icon {
        owner: isize,
        uid: u32,
        /// Set when the application identifies itself by `guidItem` rather than
        /// by `(hWnd, uID)`. Both identities have to be honoured: an application
        /// that registered by GUID will modify and delete by GUID.
        guid: Option<[u8; 16]>,
        callback: u32,
        /// Our own copy of the icon — the sender may free theirs at any time.
        icon: isize,
        tip: String,
        /// The owning executable's name, resolved once when the icon is added.
        ///
        /// `NIF_TIP` is optional and plenty of applications skip it, which left
        /// unnamed rows in the overflow reading "Notification icon" with no way
        /// to tell them apart. A process name is not the label the application
        /// would have chosen, but it is one the user recognises.
        process: String,
        state: u32,
        /// `NOTIFYICON_VERSION*`, which changes the shape of the callback.
        version: u32,
    }

    impl Icon {
        fn entry(&self) -> TrayEntry {
            let name = if self.tip.trim().is_empty() {
                self.process.clone()
            } else {
                self.tip.clone()
            };
            TrayEntry {
                id: TrayId::Native {
                    owner: self.owner,
                    uid: self.uid,
                },
                name,
                icon: self.icon,
                hidden: self.state & NIS_HIDDEN != 0,
            }
        }
    }

    /// File stem of the executable behind `owner`, or an empty string when the
    /// process cannot be opened — which is normal for anything running at a
    /// higher integrity level than AltDWM.
    fn owner_process_name(owner: HWND) -> String {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(owner, Some(&mut pid));
            if pid == 0 {
                return String::new();
            }
            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return String::new();
            };
            let mut buffer = [0u16; 260];
            let mut length = buffer.len() as u32;
            let resolved = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
            .is_ok();
            let _ = CloseHandle(process);
            if !resolved {
                return String::new();
            }
            let path = String::from_utf16_lossy(&buffer[..length as usize]);
            std::path::Path::new(&path)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default()
        }
    }

    static ICONS: LazyLock<Mutex<Vec<Icon>>> = LazyLock::new(|| Mutex::new(Vec::new()));
    static HOST: AtomicUsize = AtomicUsize::new(0);
    static ANNOUNCED: AtomicBool = AtomicBool::new(false);
    static VERBOSE: LazyLock<bool> = LazyLock::new(|| std::env::var_os("ALT_DWM_VERBOSE").is_some());

    fn taskbar_created_message() -> u32 {
        static MESSAGE: AtomicUsize = AtomicUsize::new(0);
        let cached = MESSAGE.load(Ordering::SeqCst);
        if cached != 0 {
            return cached as u32;
        }
        let id = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        MESSAGE.store(id as usize, Ordering::SeqCst);
        id
    }

    pub fn entries() -> Vec<TrayEntry> {
        ICONS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(Icon::entry)
            .collect()
    }

    // ---------------------------------------------------------------- wire ---

    /// Offsets of the fields AltDWM reads out of a `NOTIFYICONDATAW` as the
    /// sender laid it out.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct Wire {
        pointer: usize,
        tip_chars: usize,
        /// V2 and later carry `dwState`, `szInfo`, and the version union.
        extended: bool,
        /// V3 and later carry `guidItem`.
        guid: bool,
        size: usize,
    }

    impl Wire {
        /// `szTip` follows `hIcon`, which is pointer-aligned: 40 bytes in on a
        /// 64-bit sender, 24 on a 32-bit one. Every later offset is derived from
        /// this one, so the two layouts share all the arithmetic below.
        fn tip(&self) -> usize {
            if self.pointer == 8 {
                40
            } else {
                24
            }
        }
        fn hwnd(&self) -> usize {
            self.pointer
        }
        fn uid(&self) -> usize {
            if self.pointer == 8 {
                16
            } else {
                8
            }
        }
        fn flags(&self) -> usize {
            self.uid() + 4
        }
        fn callback(&self) -> usize {
            self.uid() + 8
        }
        fn icon(&self) -> usize {
            self.tip() - self.pointer
        }
        fn state(&self) -> usize {
            self.tip() + self.tip_chars * 2
        }
        fn state_mask(&self) -> usize {
            self.state() + 4
        }
        /// After `dwState`, `dwStateMask`, and `szInfo[256]`.
        fn version(&self) -> usize {
            self.state() + 8 + 512
        }
        /// After `uVersion`, `szInfoTitle[64]`, and `dwInfoFlags`.
        fn guid_item(&self) -> usize {
            self.version() + 4 + 128 + 4
        }
    }

    /// Reconstruct the sender's struct layout from its pointer width and the
    /// `cbSize` it declared. `None` for a `cbSize` that matches no published
    /// version — the only validation available, and worth having: a mismatched
    /// layout would read a tooltip out of the middle of a handle.
    pub(super) fn wire_layout(pointer: usize, cb_size: usize) -> Option<Wire> {
        let tip = if pointer == 8 { 40 } else { 24 };
        let v1 = tip + 64 * 2;
        let v2 = tip + 128 * 2 + 8 + 512 + 4 + 128 + 4;
        let v3 = v2 + 16;
        let v4 = v3 + pointer;
        let build = |tip_chars, extended, guid| {
            Some(Wire {
                pointer,
                tip_chars,
                extended,
                guid,
                size: cb_size,
            })
        };
        match cb_size {
            size if size == v1 => build(64, false, false),
            size if size == v2 => build(128, true, false),
            size if size == v3 || size == v4 => build(128, true, true),
            _ => None,
        }
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        bytes
            .get(offset..offset + 4)
            .map(|slice| u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
            .unwrap_or(0)
    }

    /// Read a handle-sized field. Handles are 32-bit values even in a 64-bit
    /// process, so a 32-bit sender's four bytes widen cleanly.
    fn handle_at(bytes: &[u8], offset: usize, pointer: usize) -> isize {
        if pointer == 8 {
            bytes
                .get(offset..offset + 8)
                .map(|slice| {
                    let mut raw = [0u8; 8];
                    raw.copy_from_slice(slice);
                    i64::from_le_bytes(raw) as isize
                })
                .unwrap_or(0)
        } else {
            u32_at(bytes, offset) as i32 as isize
        }
    }

    /// A fixed-width, NUL-terminated UTF-16 field.
    fn wide_at(bytes: &[u8], offset: usize, chars: usize) -> String {
        let Some(slice) = bytes.get(offset..offset + chars * 2) else {
            return String::new();
        };
        let units: Vec<u16> = slice
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        String::from_utf16_lossy(&units)
    }

    /// One `Shell_NotifyIcon` call, decoded.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct Notification {
        pub message: u32,
        pub owner: isize,
        pub uid: u32,
        pub flags: u32,
        pub callback: u32,
        pub icon: isize,
        pub tip: String,
        pub state: u32,
        pub state_mask: u32,
        pub version: u32,
        pub guid: Option<[u8; 16]>,
    }

    /// Decode the `WM_COPYDATA` payload. `pointer` is the sender's pointer
    /// width; when it does not fit the declared `cbSize` the other width is
    /// tried, which covers a sender whose process could not be opened.
    pub(super) fn decode(payload: &[u8], pointer: usize) -> Option<Notification> {
        if payload.len() < 12 {
            return None;
        }
        // dwSignature, dwMessage, then the caller's struct.
        let message = u32_at(payload, 4);
        let body = &payload[8..];
        let cb_size = u32_at(body, 0) as usize;
        let other = if pointer == 8 { 4 } else { 8 };
        let wire = wire_layout(pointer, cb_size).or_else(|| wire_layout(other, cb_size))?;
        if body.len() < wire.size {
            return None;
        }
        let flags = u32_at(body, wire.flags());
        let guid = (wire.guid && flags & NIF_GUID != 0)
            .then(|| {
                body.get(wire.guid_item()..wire.guid_item() + 16)
                    .map(|slice| {
                        let mut bytes = [0u8; 16];
                        bytes.copy_from_slice(slice);
                        bytes
                    })
            })
            .flatten()
            // An all-zero GUID is "no GUID", however the flag was set.
            .filter(|bytes| bytes.iter().any(|byte| *byte != 0));
        Some(Notification {
            message,
            owner: handle_at(body, wire.hwnd(), wire.pointer),
            uid: u32_at(body, wire.uid()),
            flags,
            callback: u32_at(body, wire.callback()),
            icon: handle_at(body, wire.icon(), wire.pointer),
            tip: wide_at(body, wire.tip(), wire.tip_chars),
            state: if wire.extended {
                u32_at(body, wire.state())
            } else {
                0
            },
            state_mask: if wire.extended {
                u32_at(body, wire.state_mask())
            } else {
                0
            },
            version: if wire.extended {
                u32_at(body, wire.version())
            } else {
                0
            },
            guid,
        })
    }

    /// Pointer width of the process that owns `sender`. A 32-bit application on
    /// 64-bit Windows lays its `NOTIFYICONDATA` out differently, and two struct
    /// versions collide on `cbSize` across widths, so the payload alone cannot
    /// always say which it is.
    fn sender_pointer_width(sender: HWND) -> usize {
        let native = size_of::<usize>();
        if sender.0.is_null() {
            return native;
        }
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(sender, Some(&mut pid));
            if pid == 0 {
                return native;
            }
            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return native;
            };
            let mut wow64 = BOOL(0);
            let width = if IsWow64Process(process, &mut wow64).is_ok() && wow64.as_bool() {
                4
            } else {
                native
            };
            let _ = CloseHandle(process);
            width
        }
    }

    // ---------------------------------------------------------- icon table ---

    /// Locate an existing icon by whichever identity the application used.
    fn find(icons: &[Icon], notification: &Notification) -> Option<usize> {
        if let Some(guid) = notification.guid {
            if let Some(index) = icons.iter().position(|icon| icon.guid == Some(guid)) {
                return Some(index);
            }
        }
        icons
            .iter()
            .position(|icon| icon.owner == notification.owner && icon.uid == notification.uid)
    }

    /// The sender may free its icon the moment the call returns, and a shared
    /// icon dies with the window that owns it. Copy it, or the tray ends up
    /// painting freed handles.
    fn adopt_icon(raw: isize) -> isize {
        if raw == 0 {
            return 0;
        }
        unsafe {
            CopyIcon(HICON(raw as *mut std::ffi::c_void))
                .map(|copy| copy.0 as isize)
                .unwrap_or(0)
        }
    }

    fn release_icon(raw: isize) {
        if raw == 0 {
            return;
        }
        unsafe {
            let _ = DestroyIcon(HICON(raw as *mut std::ffi::c_void));
        }
    }

    fn apply(notification: Notification) -> LRESULT {
        let mut icons = ICONS.lock().unwrap_or_else(|error| error.into_inner());
        let existing = find(&icons, &notification);
        let handled = match notification.message {
            // An application that modifies an icon we never saw is not
            // misbehaving — it registered with Explorer before AltDWM took the
            // tray over. Treating a stray modify as an add is what rescues those
            // applications when they never act on `TaskbarCreated`.
            NIM_ADD | NIM_MODIFY => {
                match existing {
                    Some(index) => {
                        let icon = &mut icons[index];
                        if notification.flags & NIF_ICON != 0 {
                            let previous = icon.icon;
                            icon.icon = adopt_icon(notification.icon);
                            release_icon(previous);
                        }
                        if notification.flags & NIF_TIP != 0 {
                            icon.tip = notification.tip;
                        }
                        if notification.flags & NIF_MESSAGE != 0 {
                            icon.callback = notification.callback;
                        }
                        if notification.flags & NIF_STATE != 0 {
                            icon.state = (icon.state & !notification.state_mask)
                                | (notification.state & notification.state_mask);
                        }
                        if notification.guid.is_some() {
                            icon.guid = notification.guid;
                        }
                    }
                    None => icons.push(Icon {
                        owner: notification.owner,
                        process: owner_process_name(HWND(
                            notification.owner as *mut std::ffi::c_void,
                        )),
                        uid: notification.uid,
                        guid: notification.guid,
                        callback: if notification.flags & NIF_MESSAGE != 0 {
                            notification.callback
                        } else {
                            0
                        },
                        icon: adopt_icon(notification.icon),
                        tip: notification.tip,
                        state: notification.state & notification.state_mask,
                        version: 0,
                    }),
                }
                true
            }
            NIM_DELETE => match existing {
                Some(index) => {
                    release_icon(icons.remove(index).icon);
                    true
                }
                None => false,
            },
            NIM_SETVERSION => match existing {
                Some(index) => {
                    icons[index].version = notification.version;
                    true
                }
                None => false,
            },
            // Keyboard focus into the notification area. Nothing to do while
            // AltDWM offers no keyboard navigation through the tray, but the
            // caller is blocked waiting for the answer.
            NIM_SETFOCUS => true,
            _ => false,
        };
        drop(icons);
        if handled {
            crate::panel::invalidate_all();
        }
        LRESULT(isize::from(handled))
    }

    /// Drop icons whose owner has gone. An application that exits without
    /// calling `NIM_DELETE` — or is killed — would otherwise leave a dead icon
    /// on the bar for the rest of the session.
    fn sweep() {
        let mut icons = ICONS.lock().unwrap_or_else(|error| error.into_inner());
        let before = icons.len();
        icons.retain(|icon| {
            let owner = HWND(icon.owner as *mut std::ffi::c_void);
            let alive = unsafe { IsWindow(Some(owner)).as_bool() };
            if !alive {
                release_icon(icon.icon);
            }
            alive
        });
        let removed = before - icons.len();
        drop(icons);
        if removed > 0 {
            println!("[tray] dropped {removed} icon(s) whose owner exited");
            crate::panel::invalidate_all();
        }
    }

    // -------------------------------------------------------------- clicks ---

    /// `NOTIFYICON_VERSION_4` changes the callback's shape: the cursor position
    /// moves into `wParam` and the icon id joins the event in `lParam`.
    fn callback_params(icon: &Icon, event: u32) -> (WPARAM, LPARAM) {
        if icon.version >= 4 {
            let mut cursor = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut cursor);
            }
            let anchor = ((cursor.y as u16 as usize) << 16) | (cursor.x as u16 as usize);
            let payload = ((icon.uid as usize) << 16) | (event as u16 as usize);
            (WPARAM(anchor), LPARAM(payload as isize))
        } else {
            (WPARAM(icon.uid as usize), LPARAM(event as isize))
        }
    }

    fn post(icon: &Icon, owner: HWND, event: u32) {
        let (wparam, lparam) = callback_params(icon, event);
        unsafe {
            let _ = PostMessageW(Some(owner), icon.callback, wparam, lparam);
        }
    }

    pub fn invoke(owner_raw: isize, uid: u32, button: Button) {
        let icon = {
            let icons = ICONS.lock().unwrap_or_else(|error| error.into_inner());
            icons
                .iter()
                .find(|icon| icon.owner == owner_raw && icon.uid == uid)
                .cloned()
        };
        let Some(icon) = icon else {
            return;
        };
        if icon.callback == 0 {
            // The application never asked for callbacks — a purely decorative
            // status icon. There is nothing to send it.
            return;
        }
        let owner = HWND(icon.owner as *mut std::ffi::c_void);
        if !unsafe { IsWindow(Some(owner)).as_bool() } {
            sweep();
            return;
        }
        // A tray context menu only dismisses on click-away while its owner holds
        // the foreground. AltDWM's bar is `WS_EX_NOACTIVATE`, so the foreground
        // never moved here and the owner is free to take it — but it has to be
        // granted that right, or the menu sticks until the next click.
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(owner, Some(&mut pid));
            if pid != 0 {
                let _ = AllowSetForegroundWindow(pid);
            }
            let _ = SetForegroundWindow(owner);
        }
        // At `NOTIFYICON_VERSION_4` the documented events replace the button
        // messages rather than accompanying them: NIN_SELECT for a click,
        // WM_CONTEXTMENU for a right click. Sending both shapes would fire a
        // toggle twice in any application that handles either — which is most
        // of them, since the two arms sit in the same `switch`.
        match (button, icon.version >= 4) {
            (Button::Left, true) => post(&icon, owner, NIN_SELECT),
            (Button::Left, false) => {
                post(&icon, owner, WM_LBUTTONDOWN);
                post(&icon, owner, WM_LBUTTONUP);
            }
            // Version 4 has no double-click event. Two selects is what Explorer
            // produces for two clicks, and it is the honest translation.
            (Button::DoubleLeft, true) => post(&icon, owner, NIN_SELECT),
            (Button::DoubleLeft, false) => post(&icon, owner, WM_LBUTTONDBLCLK),
            (Button::Right, true) => post(&icon, owner, WM_CONTEXTMENU),
            (Button::Right, false) => {
                post(&icon, owner, WM_RBUTTONDOWN);
                post(&icon, owner, WM_RBUTTONUP);
            }
        }
        if *VERBOSE {
            println!(
                "[tray] {:?} -> {} (uid={} version={})",
                button,
                super::title_line(&icon.tip),
                icon.uid,
                icon.version
            );
        }
    }

    // -------------------------------------------------------------- window ---

    unsafe extern "system" fn tray_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_COPYDATA => {
                let data = lparam.0 as *const COPYDATASTRUCT;
                if data.is_null() {
                    return LRESULT(0);
                }
                let data = &*data;
                match data.dwData {
                    COPY_TRAY => {
                        if data.lpData.is_null() || data.cbData == 0 {
                            return LRESULT(0);
                        }
                        let payload =
                            std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize);
                        let sender = HWND(wparam.0 as *mut std::ffi::c_void);
                        match decode(payload, sender_pointer_width(sender)) {
                            Some(notification) => {
                                if *VERBOSE {
                                    println!(
                                        "[tray:wire] msg={} owner={:#x} uid={} flags={:#x} tip={:?}",
                                        notification.message,
                                        notification.owner,
                                        notification.uid,
                                        notification.flags,
                                        notification.tip
                                    );
                                }
                                apply(notification)
                            }
                            None => {
                                eprintln!(
                                    "[tray] ignored a notification with an unrecognised layout ({} bytes)",
                                    data.cbData
                                );
                                LRESULT(0)
                            }
                        }
                    }
                    // `SHAppBarMessage`. AltDWM reserves work area itself and
                    // does not let applications dock, so every request is
                    // declined — but it is declined promptly, because the caller
                    // is blocked on this reply.
                    COPY_APPBAR => LRESULT(0),
                    // `Shell_NotifyIconGetRect`. The reply packs a RECT into an
                    // LRESULT in a way that is neither documented nor stable, so
                    // failing is the honest answer: callers fall back to the
                    // cursor, which is where AltDWM's icons are anyway.
                    COPY_ICON_RECT => LRESULT(0),
                    _ => LRESULT(0),
                }
            }
            WM_TIMER => {
                if wparam.0 == TIMER_SWEEP {
                    sweep();
                    // Applications resolve the tray with `FindWindow`, which
                    // walks top-level windows in Z-order. Explorer re-asserts its
                    // own taskbar's position, so ours has to be re-asserted too
                    // or newly registered icons quietly go back to Explorer.
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = KillTimer(Some(hwnd), TIMER_SWEEP);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe extern "system" fn passive_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// Create the window chain applications expect to find.
    ///
    /// Only `Shell_TrayWnd` receives anything, but a fair number of applications
    /// probe for the `TrayNotifyWnd` / `SysPager` / `ToolbarWindow32`
    /// descendants before deciding a shell is present at all.
    /// `Ok(true)` when this call created the host window, `Ok(false)` when one
    /// was already up.
    pub fn start() -> Result<bool, String> {
        if HOST.load(Ordering::SeqCst) != 0 {
            return Ok(false);
        }
        crate::util::register_window_class(w!("Shell_TrayWnd"), tray_wndproc, "Tray")?;
        crate::util::register_window_class(w!("TrayNotifyWnd"), passive_wndproc, "TrayNotify")?;
        crate::util::register_window_class(w!("SysPager"), passive_wndproc, "SysPager")?;
        crate::util::register_window_class(
            w!("ToolbarWindow32"),
            passive_wndproc,
            "NotificationArea",
        )?;
        unsafe {
            let width = GetSystemMetrics(SM_CXSCREEN).max(1);
            let height = GetSystemMetrics(SM_CYSCREEN).max(1);
            let strip = 40;
            // Sized and placed like a taskbar, because applications read this
            // window's rectangle to position balloons — but never painted. It is
            // created without WS_VISIBLE and with WS_EX_TRANSPARENT, so it
            // contributes no pixels and takes no clicks.
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
                w!("Shell_TrayWnd"),
                PCWSTR::null(),
                WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                height - strip,
                width,
                strip,
                None,
                Some(HMENU(std::ptr::null_mut())),
                None,
                None,
            )
            .map_err(|error| format!("Shell_TrayWnd CreateWindowExW failed: {error:?}"))?;

            let child = |parent: HWND, class: PCWSTR, title: PCWSTR| {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    class,
                    title,
                    WS_CHILD,
                    0,
                    0,
                    width,
                    strip,
                    Some(parent),
                    Some(HMENU(std::ptr::null_mut())),
                    None,
                    None,
                )
                .ok()
            };
            if let Some(notify) = child(hwnd, w!("TrayNotifyWnd"), PCWSTR::null()) {
                if let Some(pager) = child(notify, w!("SysPager"), PCWSTR::null()) {
                    let _ = child(pager, w!("ToolbarWindow32"), w!("Notification Area"));
                }
            }
            let _ = SetTimer(Some(hwnd), TIMER_SWEEP, SWEEP_INTERVAL, None);
            HOST.store(hwnd.0 as usize, Ordering::SeqCst);
            println!(
                "[tray] hosting the notification area (Shell_TrayWnd hwnd={:?})",
                hwnd.0
            );
        }
        Ok(true)
    }

    pub fn announce() {
        let host = HOST.load(Ordering::SeqCst);
        if host == 0 {
            return;
        }
        unsafe {
            let hwnd = HWND(host as *mut std::ffi::c_void);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            let _ = SendNotifyMessageW(
                HWND_BROADCAST,
                taskbar_created_message(),
                WPARAM(0),
                LPARAM(0),
            );
        }
        ANNOUNCED.store(true, Ordering::SeqCst);
        println!("[tray] broadcast TaskbarCreated — applications will re-publish their icons");
    }

    pub fn shutdown() {
        let host = HOST.swap(0, Ordering::SeqCst);
        if host != 0 {
            unsafe {
                let _ = DestroyWindow(HWND(host as *mut std::ffi::c_void));
            }
        }
        let mut icons = ICONS.lock().unwrap_or_else(|error| error.into_inner());
        for icon in icons.drain(..) {
            release_icon(icon.icon);
        }
        drop(icons);
        // Only worth broadcasting if applications were sent here in the first
        // place. The host window is already gone, so this hands them back to
        // Explorer rather than to a handle that no longer resolves.
        if ANNOUNCED.swap(false, Ordering::SeqCst) {
            unsafe {
                let _ = SendNotifyMessageW(
                    HWND_BROADCAST,
                    taskbar_created_message(),
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            println!("[tray] released the notification area back to Explorer");
        }
    }
}

// -------------------------------------------------------- explorer bridge ---

/// Mirror of Explorer's own tray buttons over UI Automation.
///
/// Kept for sessions that leave the native taskbar in place. It cannot read
/// icons — UI Automation exposes a button's name, not its bitmap — so these
/// entries render as short labels.
mod explorer {
    use super::*;

    const REFRESH_INTERVAL: Duration = Duration::from_millis(750);

    #[derive(Clone)]
    struct CachedEntry {
        name: String,
        element: IUIAutomationElement,
    }

    enum Command {
        Invoke(usize),
    }

    struct Worker {
        entries: Arc<Mutex<Vec<String>>>,
        commands: Sender<Command>,
    }

    static WORKER: LazyLock<Worker> = LazyLock::new(start_worker);
    static STARTED: AtomicBool = AtomicBool::new(false);

    fn start_worker() -> Worker {
        let entries = Arc::new(Mutex::new(Vec::new()));
        let worker_entries = entries.clone();
        let (commands, receiver) = mpsc::channel();
        // As with the status poller: an empty tray beats aborting the shell.
        if let Err(error) = std::thread::Builder::new()
            .name("AltDWM-tray".into())
            .spawn(move || worker_loop(worker_entries, receiver))
        {
            eprintln!("[tray] Explorer bridge could not start: {error}");
        }
        Worker { entries, commands }
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
                // buttons then report off-screen but remain valid invocation
                // targets.
                let name = name.trim().to_string();
                if name.is_empty() {
                    continue;
                }
                found.push(CachedEntry { name, element });
            }
            found
        }
    }

    fn worker_loop(entries: Arc<Mutex<Vec<String>>>, receiver: Receiver<Command>) {
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
            let visible: Vec<String> = cached.iter().map(|entry| entry.name.clone()).collect();
            let changed = entries
                .lock()
                .map(|current| *current != visible)
                .unwrap_or(true);
            if changed {
                println!("[tray] Explorer items: {}", visible.join(", "));
                *entries.lock().unwrap_or_else(|error| error.into_inner()) = visible;
                crate::panel::invalidate_all();
            }
            match receiver.recv_timeout(REFRESH_INTERVAL) {
                Ok(Command::Invoke(index)) => {
                    if let Some(entry) = cached.get(index) {
                        unsafe {
                            if let Ok(pattern) = entry
                                .element
                                .GetCurrentPatternAs::<IUIAutomationInvokePattern>(
                                    UIA_InvokePatternId,
                                )
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
        if !STARTED.load(Ordering::SeqCst) {
            return Vec::new();
        }
        WORKER
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .enumerate()
            .map(|(index, name)| TrayEntry {
                id: TrayId::Explorer { index },
                name: name.clone(),
                icon: 0,
                hidden: false,
            })
            .collect()
    }

    /// Start discovery while Explorer's taskbar is still visible and briefly
    /// wait for the first snapshot before shell chrome is hidden.
    pub fn prime() {
        STARTED.store(true, Ordering::SeqCst);
        LazyLock::force(&WORKER);
        for _ in 0..40 {
            if !entries().is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn invoke(index: usize) {
        if !STARTED.load(Ordering::SeqCst) {
            return;
        }
        let _ = WORKER.commands.send(Command::Invoke(index));
    }
}

#[cfg(test)]
mod tests {
    use super::native::{decode, wire_layout};
    use super::{compact_name, title_line, Source};

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

    #[test]
    fn tooltips_are_named_by_their_first_line() {
        assert_eq!(title_line("Syncthing\n2 folders up to date"), "Syncthing");
        assert_eq!(title_line("\n\n  Steam  \n"), "Steam");
        assert_eq!(title_line("   "), "Notification icon");
    }

    #[test]
    fn source_parsing_falls_back_to_auto() {
        assert_eq!(Source::parse("native"), Source::Native);
        assert_eq!(Source::parse(" Explorer "), Source::Explorer);
        assert_eq!(Source::parse("off"), Source::Off);
        assert_eq!(Source::parse("nonsense"), Source::Auto);
    }

    /// The four published `NOTIFYICONDATAW` sizes, at both pointer widths. Two
    /// of them collide across widths — 952 is V2 from a 64-bit sender and V3
    /// from a 32-bit one — which is the whole reason the sender's bitness is
    /// probed separately rather than inferred from the payload.
    #[test]
    fn every_published_struct_version_is_recognised() {
        for (pointer, sizes) in [(8usize, [168, 952, 968, 976]), (4, [152, 936, 952, 956])] {
            for size in sizes {
                assert!(
                    wire_layout(pointer, size).is_some(),
                    "cbSize {size} unrecognised at pointer width {pointer}"
                );
            }
        }
        assert!(wire_layout(8, 900).is_none());
        assert!(wire_layout(4, 0).is_none());
    }

    /// Build a 64-bit V4 payload the way shell32 does, and read it back.
    fn payload_64(message: u32, flags: u32, uid: u32, callback: u32, tip: &str) -> Vec<u8> {
        let mut body = vec![0u8; 976];
        body[0..4].copy_from_slice(&976u32.to_le_bytes());
        body[8..16].copy_from_slice(&0x0002_1A34u64.to_le_bytes()); // hWnd
        body[16..20].copy_from_slice(&uid.to_le_bytes());
        body[20..24].copy_from_slice(&flags.to_le_bytes());
        body[24..28].copy_from_slice(&callback.to_le_bytes());
        body[32..40].copy_from_slice(&0x0000_BEEFu64.to_le_bytes()); // hIcon
        for (index, unit) in tip.encode_utf16().enumerate() {
            body[40 + index * 2..42 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        body[296..300].copy_from_slice(&1u32.to_le_bytes()); // dwState = NIS_HIDDEN
        body[300..304].copy_from_slice(&1u32.to_le_bytes()); // dwStateMask
        body[816..820].copy_from_slice(&4u32.to_le_bytes()); // uVersion
        let mut payload = Vec::with_capacity(984);
        payload.extend_from_slice(&0x3475_3423u32.to_le_bytes());
        payload.extend_from_slice(&message.to_le_bytes());
        payload.extend_from_slice(&body);
        payload
    }

    #[test]
    fn a_64_bit_notification_decodes_field_for_field() {
        let payload = payload_64(0, 0x01 | 0x02 | 0x04 | 0x08, 7, 0x0401, "Syncthing");
        let decoded = decode(&payload, 8).expect("payload should decode");
        assert_eq!(decoded.message, 0);
        assert_eq!(decoded.owner, 0x0002_1A34);
        assert_eq!(decoded.uid, 7);
        assert_eq!(decoded.callback, 0x0401);
        assert_eq!(decoded.icon, 0xBEEF);
        assert_eq!(decoded.tip, "Syncthing");
        assert_eq!(decoded.state & decoded.state_mask, 1);
        assert_eq!(decoded.version, 4);
        // NIF_GUID was not set, so no identity should be invented.
        assert_eq!(decoded.guid, None);
    }

    /// A sender whose process could not be opened falls back to our own pointer
    /// width; the declared `cbSize` has to rescue the decode.
    #[test]
    fn a_mismatched_pointer_width_recovers_from_cb_size() {
        let payload = payload_64(1, 0x04, 3, 0, "Backup running");
        let decoded = decode(&payload, 4).expect("payload should still decode");
        assert_eq!(decoded.tip, "Backup running");
        assert_eq!(decoded.uid, 3);
    }

    #[test]
    fn a_32_bit_sender_uses_its_own_offsets() {
        let mut body = vec![0u8; 956];
        body[0..4].copy_from_slice(&956u32.to_le_bytes());
        body[4..8].copy_from_slice(&0x0011_2233u32.to_le_bytes()); // hWnd
        body[8..12].copy_from_slice(&12u32.to_le_bytes()); // uID
        body[12..16].copy_from_slice(&(0x01u32 | 0x04).to_le_bytes()); // uFlags
        body[16..20].copy_from_slice(&0x0400u32.to_le_bytes()); // uCallbackMessage
        body[20..24].copy_from_slice(&0x00C0_FFEEu32.to_le_bytes()); // hIcon
        for (index, unit) in "Legacy app".encode_utf16().enumerate() {
            body[24 + index * 2..26 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let mut payload = vec![0u8; 8];
        payload.extend_from_slice(&body);
        let decoded = decode(&payload, 4).expect("payload should decode");
        assert_eq!(decoded.owner, 0x0011_2233);
        assert_eq!(decoded.uid, 12);
        assert_eq!(decoded.callback, 0x0400);
        assert_eq!(decoded.icon, 0x00C0_FFEE);
        assert_eq!(decoded.tip, "Legacy app");
    }

    #[test]
    fn a_truncated_or_nonsense_payload_is_rejected_rather_than_read() {
        assert!(decode(&[0u8; 4], 8).is_none());
        let mut payload = payload_64(0, 0x04, 1, 0, "x");
        payload.truncate(400);
        assert!(decode(&payload, 8).is_none());
    }
}

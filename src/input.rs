//! Keyboard layout reporting and switching — the shell's input indicator.
//!
//! Layouts are per-thread in Windows, so "the current layout" means the layout
//! of the foreground window's thread, and switching means asking that window to
//! change rather than changing our own. `ActivateKeyboardLayout` would only ever
//! affect AltDWM's own threads.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, GetKeyboardLayoutList, HKL};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, PostMessageW, WM_INPUTLANGCHANGEREQUEST,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Raw `HKL` value, kept as an integer so it can cross thread boundaries.
    pub handle: isize,
    /// Short tag for the bar, e.g. `EN`, `HU`.
    pub tag: String,
    /// Full name for menus and tooltips, e.g. `English (United States)`.
    pub name: String,
}

/// Locale names are stable per `HKL`, and resolving one costs two
/// `GetLocaleInfoEx` calls, so they are looked up once.
static NAME_CACHE: LazyLock<Mutex<HashMap<u16, (String, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The language identifier is the low word of an `HKL`; the high word is the
/// physical layout, which does not affect the language name.
fn language_id(hkl: isize) -> u16 {
    (hkl as usize & 0xFFFF) as u16
}

fn locale_strings(langid: u16) -> (String, String) {
    use windows::Win32::Globalization::{
        GetLocaleInfoEx, LCIDToLocaleName, LOCALE_ALLOW_NEUTRAL_NAMES, LOCALE_SISO639LANGNAME,
        LOCALE_SLOCALIZEDDISPLAYNAME,
    };
    if let Some(cached) = NAME_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&langid)
        .cloned()
    {
        return cached;
    }
    let mut locale = [0u16; 85];
    let resolved = unsafe {
        LCIDToLocaleName(
            u32::from(langid),
            Some(&mut locale),
            LOCALE_ALLOW_NEUTRAL_NAMES,
        )
    };
    let strings = if resolved == 0 {
        (format!("{langid:04X}"), format!("Layout {langid:04X}"))
    } else {
        let read = |what: u32| -> String {
            let mut buffer = [0u16; 128];
            let length = unsafe {
                GetLocaleInfoEx(
                    windows::core::PCWSTR(locale.as_ptr()),
                    what,
                    Some(&mut buffer),
                )
            };
            if length <= 1 {
                return String::new();
            }
            String::from_utf16_lossy(&buffer[..(length - 1) as usize])
        };
        let tag = read(LOCALE_SISO639LANGNAME);
        let name = read(LOCALE_SLOCALIZEDDISPLAYNAME);
        let tag = if tag.is_empty() {
            format!("{langid:04X}")
        } else {
            tag.to_uppercase()
        };
        let name = if name.is_empty() { tag.clone() } else { name };
        (tag, name)
    };
    NAME_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(langid, strings.clone());
    strings
}

fn describe(hkl: isize) -> Layout {
    let (tag, name) = locale_strings(language_id(hkl));
    Layout {
        handle: hkl,
        tag,
        name,
    }
}

/// Every layout installed for the current user, in the order Windows reports.
pub fn installed() -> Vec<Layout> {
    let count = unsafe { GetKeyboardLayoutList(None) };
    if count <= 0 {
        return Vec::new();
    }
    let mut handles = vec![HKL::default(); count as usize];
    let written = unsafe { GetKeyboardLayoutList(Some(&mut handles)) };
    handles.truncate(written.max(0) as usize);
    handles
        .into_iter()
        .map(|hkl| describe(hkl.0 as isize))
        .collect()
}

/// Layout of the foreground window's thread, which is what the user is actually
/// typing with.
pub fn current_for(hwnd: windows::Win32::Foundation::HWND) -> Option<Layout> {
    unsafe {
        let thread = if hwnd.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(hwnd, None)
        };
        let hkl = GetKeyboardLayout(thread);
        (!hkl.0.is_null()).then(|| describe(hkl.0 as isize))
    }
}

pub fn current() -> Option<Layout> {
    current_for(unsafe { GetForegroundWindow() })
}

/// Ask the foreground window to adopt `layout`.
///
/// `WM_INPUTLANGCHANGEREQUEST` is the documented way to do this from outside the
/// target process; it lets the application refuse, which is why the indicator
/// re-reads the layout rather than assuming the change took effect.
///
/// wParam is 0, not `INPUTLANGCHANGE_FORWARD`: the FORWARD/BACKWARD flags tell
/// the target to step to the next/previous locale and make it *ignore* the HKL
/// in lParam entirely — so passing the specific layout the user picked did
/// nothing useful. With no flag, lParam names the exact layout to switch to.
pub fn activate_for(hwnd: windows::Win32::Foundation::HWND, layout: &Layout) -> bool {
    unsafe {
        if hwnd.0.is_null() {
            return false;
        }
        PostMessageW(
            Some(hwnd),
            WM_INPUTLANGCHANGEREQUEST,
            WPARAM(0),
            LPARAM(layout.handle),
        )
        .is_ok()
    }
}

pub fn activate(layout: &Layout) -> bool {
    activate_for(unsafe { GetForegroundWindow() }, layout)
}

/// Move to the next installed layout, wrapping. Returns the layout requested.
pub fn cycle() -> Option<Layout> {
    let layouts = installed();
    if layouts.len() < 2 {
        return None;
    }
    let current = current();
    let index = current
        .as_ref()
        .and_then(|current| {
            layouts
                .iter()
                .position(|layout| layout.handle == current.handle)
        })
        .unwrap_or(0);
    let next = &layouts[(index + 1) % layouts.len()];
    activate(next).then(|| next.clone())
}

#[cfg(test)]
mod tests {
    use super::language_id;

    #[test]
    fn language_id_is_the_low_word_of_the_layout_handle() {
        // 0x0409 = en-US with the US physical layout in the high word.
        assert_eq!(language_id(0x0409_0409), 0x0409);
        // Alternative physical layouts keep the same language.
        assert_eq!(language_id(0xF0C0_040E), 0x040E);
        assert_eq!(language_id(0x0000_040E), 0x040E);
    }
}

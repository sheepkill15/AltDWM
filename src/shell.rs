//! Explorer shell-chrome ownership.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use windows::core::BOOL;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindow, SetLayeredWindowAttributes,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, HWND_BOTTOM, HWND_TOPMOST, LWA_ALPHA,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOW, WS_EX_LAYERED,
};

#[derive(Clone, Copy)]
struct NativeTaskbar {
    hwnd: isize,
    original_ex_style: isize,
}

static HIDDEN_TASKBARS: LazyLock<Mutex<Vec<NativeTaskbar>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static TASKBARS_HIDDEN: AtomicBool = AtomicBool::new(false);

pub fn native_taskbars_are_hidden() -> bool {
    TASKBARS_HIDDEN.load(Ordering::SeqCst)
}

pub fn is_native_taskbar(hwnd: HWND) -> bool {
    if !matches!(
        crate::util::get_class_name(hwnd).as_str(),
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    ) {
        return false;
    }
    // AltDWM's own notification-area host registers the `Shell_TrayWnd` class
    // too. Hiding it would be harmless — it is already invisible — but adding it
    // to the restore list means shutdown would try to give it back to a user who
    // never had it, and demoting it would hand the tray straight back to
    // Explorer. Class alone is not identity; ownership is.
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid != std::process::id()
}

pub fn hide_native_taskbar(hwnd: HWND) {
    if !is_native_taskbar(hwnd) {
        return;
    }
    let key = hwnd.0 as isize;
    unsafe {
        let original_ex_style = {
            let mut hidden = HIDDEN_TASKBARS
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(taskbar) = hidden.iter().find(|taskbar| taskbar.hwnd == key) {
                taskbar.original_ex_style
            } else {
                let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                hidden.push(NativeTaskbar {
                    hwnd: key,
                    original_ex_style: style,
                });
                style
            }
        };
        let _ = SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            original_ex_style | WS_EX_LAYERED.0 as isize,
        );
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
        // A previous AltDWM crash may have left the taskbar SW_HIDE'd. Showing a
        // zero-alpha layered window restores its live tray model without pixels.
        let _ = ShowWindow(hwnd, SW_SHOW);
        // `Shell_NotifyIcon` finds the tray with `FindWindow`, which walks
        // top-level windows in Z-order and does not care whether they are
        // visible. While Explorer's taskbar sits above AltDWM's own
        // `Shell_TrayWnd`, every icon an application registers goes to a window
        // the user cannot see. Dropping it out of the topmost band puts AltDWM
        // first; `restore_native_taskbars` puts it back.
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

unsafe extern "system" fn enum_taskbars(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    if is_native_taskbar(hwnd) {
        hide_native_taskbar(hwnd);
    }
    BOOL(1)
}

pub fn set_native_taskbars_hidden(hidden: bool) {
    if hidden {
        TASKBARS_HIDDEN.store(true, Ordering::SeqCst);
        unsafe {
            let _ = EnumWindows(Some(enum_taskbars), LPARAM(0));
        }
    } else {
        restore_native_taskbars();
    }
}

pub fn restore_native_taskbars() {
    TASKBARS_HIDDEN.store(false, Ordering::SeqCst);
    let handles = {
        let mut hidden = HIDDEN_TASKBARS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        std::mem::take(&mut *hidden)
    };
    for taskbar in handles {
        let hwnd = HWND(taskbar.hwnd as *mut std::ffi::c_void);
        unsafe {
            if IsWindow(Some(hwnd)).as_bool() {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
                let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, taskbar.original_ex_style);
                let _ = ShowWindow(hwnd, SW_SHOW);
                // Restoring WS_EX_TOPMOST in the style bits does not move the
                // window; only SetWindowPos does.
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
        }
    }
}

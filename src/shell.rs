//! Explorer shell-chrome ownership.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use windows::core::BOOL;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongPtrW, IsWindow, SetLayeredWindowAttributes, SetWindowLongPtrW,
    ShowWindow, GWL_EXSTYLE, LWA_ALPHA, SW_SHOW, WS_EX_LAYERED,
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
    matches!(
        crate::util::get_class_name(hwnd).as_str(),
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    )
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
            }
        }
    }
}

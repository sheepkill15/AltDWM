use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
    IsIconic, IsWindowVisible, GA_ROOT, GWL_EXSTYLE, GW_OWNER, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

const DWMWA_CLOAKED_U32: u32 = 14;

pub fn get_class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

pub fn get_window_title(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; (len + 1) as usize];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..copied as usize])
}

pub fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
    };
    if hr.is_err() {
        // Fallback to raw value 14 if enum variant not matched (avoid transmute)
        let hr2 = unsafe {
            windows::Win32::Graphics::Dwm::DwmGetWindowAttribute(
                hwnd,
                windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(DWMWA_CLOAKED_U32 as i32),
                &mut cloaked as *mut u32 as *mut _,
                std::mem::size_of::<u32>() as u32,
            )
        };
        if hr2.is_err() {
            return false;
        }
    }
    cloaked != 0
}

pub fn is_manageable(hwnd: HWND, taskbar_hwnd: Option<HWND>) -> bool {
    unsafe {
        if hwnd.0.is_null() {
            return false;
        }
        if let Some(tb) = taskbar_hwnd {
            if hwnd.0 == tb.0 {
                return false;
            }
        }
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        if IsIconic(hwnd).as_bool() {
            return false;
        }
        // Only top-level windows
        if GetAncestor(hwnd, GA_ROOT) != hwnd {
            return false;
        }
        // Owned windows are dialogs/popups, don't tile
        // GetWindow returns Result<HWND> in 0.61 - if Ok and not null, it's owned
        if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
            if !owner.0.is_null() {
                return false;
            }
        }

        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        // Skip tool windows (tooltips, etc.) unless they have APPWINDOW
        if (ex_style & WS_EX_TOOLWINDOW.0) != 0 && (ex_style & WS_EX_APPWINDOW.0) == 0 {
            return false;
        }

        if is_cloaked(hwnd) {
            return false;
        }

        let class = get_class_name(hwnd);
        // hard-coded shell classes
        match class.as_str() {
            "Progman" => return false,
            "WorkerW" => return false,
            "Shell_TrayWnd" => return false,
            "Shell_SecondaryTrayWnd" => return false,
            "AltDWM_Taskbar" => return false,
            "AltDWM_Panel" => return false,
            "AltDWM_Host" => return false,
            "Windows.UI.Core.CoreWindow" => {
                let title = get_window_title(hwnd);
                if title.is_empty() {
                    return false;
                }
            }
            _ => {}
        }
        // config-driven ignore (extensibility: user adds classes/titles in config.toml)
        if crate::is_ignored_class(&class) {
            return false;
        }
        let title = get_window_title(hwnd);
        if !title.is_empty() && crate::is_ignored_title(&title) {
            return false;
        }
        let process = crate::rules::get_process_name(hwnd);
        let ignored_process = {
            let cfg = crate::CURRENT_CONFIG
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cfg.ignore
                .processes
                .iter()
                .any(|ignored| process.eq_ignore_ascii_case(ignored))
        };
        if ignored_process {
            return false;
        }

        true
    }
}

pub fn rect_to_string(r: &RECT) -> String {
    format!(
        "({},{} {}x{})",
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top
    )
}

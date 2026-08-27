use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
    IsIconic, IsWindowVisible, GA_ROOT, GWL_EXSTYLE, GW_OWNER, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

pub fn register_window_class(
    class_name: PCWSTR,
    window_proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    label: &str,
) -> Result<(), String> {
    use windows::Win32::Graphics::Gdi::HBRUSH;
    use windows::Win32::UI::WindowsAndMessaging::{
        LoadCursorW, RegisterClassExW, CS_HREDRAW, CS_VREDRAW, IDC_ARROW, WNDCLASSEXW,
    };

    unsafe {
        let hinstance = HINSTANCE(std::ptr::null_mut());
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: Default::default(),
            hCursor: LoadCursorW(Some(hinstance), IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: Default::default(),
        };
        let atom = RegisterClassExW(&class);
        if atom == 0 {
            let error = windows::Win32::Foundation::GetLastError();
            // ERROR_CLASS_ALREADY_EXISTS is success for hot reload/recreation.
            if error.0 != 1410 {
                return Err(format!("{label} RegisterClassExW failed: {error:?}"));
            }
        }
        Ok(())
    }
}

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
            size_of::<u32>() as u32,
        )
    };
    if hr.is_err() {
        // Fallback to raw value 14 if enum variant not matched (avoid transmute)
        let hr2 = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(DWMWA_CLOAKED_U32 as i32),
                &mut cloaked as *mut u32 as *mut _,
                size_of::<u32>() as u32,
            )
        };
        if hr2.is_err() {
            return false;
        }
    }
    cloaked != 0
}

/// Suppress or restore DWM's transition animation for a window. This is used
/// only around the first synchronous layout so a newly launched application
/// appears in its tile instead of visibly gliding there from its default rect.
pub fn set_transitions_forced_disabled(hwnd: HWND, disabled: bool) {
    const DWMWA_TRANSITIONS_FORCEDISABLED_RAW: i32 = 3;
    let value: i32 = i32::from(disabled);
    unsafe {
        let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(DWMWA_TRANSITIONS_FORCEDISABLED_RAW),
            &value as *const _ as _,
            size_of_val(&value) as u32,
        );
    }
}

fn has_independent_app_presence(ex_style: u32, has_owner: bool) -> bool {
    let app_window = (ex_style & WS_EX_APPWINDOW.0) != 0;
    let tool_window = (ex_style & WS_EX_TOOLWINDOW.0) != 0;
    (!has_owner || app_window) && (!tool_window || app_window)
}

fn is_manageable_impl(hwnd: HWND, include_minimized: bool) -> bool {
    unsafe {
        if hwnd.0.is_null() {
            return false;
        }
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        if !include_minimized && IsIconic(hwnd).as_bool() {
            return false;
        }
        // Only top-level windows
        if GetAncestor(hwnd, GA_ROOT) != hwnd {
            return false;
        }
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        // Most owned windows are transient dialogs/popups. WS_EX_APPWINDOW is the
        // explicit exception: Windows uses it to give an owned top-level window
        // its own taskbar presence, so it must also be independently manageable.
        // Rejecting all owned windows caused legitimate application windows to
        // disappear from AltDWM entirely.
        let has_owner = GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.0.is_null());
        if !has_independent_app_presence(ex_style, has_owner) {
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
            "AltDWM_CommandCenter" => return false,
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
        // Resolving a process name costs an OpenProcess round trip, so it is
        // only worth paying when the configuration actually filters on it.
        let ignored_processes: Vec<String> = {
            let cfg = crate::CURRENT_CONFIG
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cfg.ignore.processes.clone()
        };
        if !ignored_processes.is_empty() {
            let process = crate::rules::get_process_name(hwnd);
            if ignored_processes
                .iter()
                .any(|ignored| process.eq_ignore_ascii_case(ignored))
            {
                return false;
            }
        }

        true
    }
}

pub fn is_manageable(hwnd: HWND) -> bool {
    is_manageable_impl(hwnd, false)
}

pub fn is_manageable_or_minimized(hwnd: HWND) -> bool {
    is_manageable_impl(hwnd, true)
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

#[cfg(test)]
mod tests {
    use super::has_independent_app_presence;
    use windows::Win32::UI::WindowsAndMessaging::{WS_EX_APPWINDOW, WS_EX_TOOLWINDOW};

    #[test]
    fn owned_appwindow_is_independently_manageable() {
        assert!(has_independent_app_presence(WS_EX_APPWINDOW.0, true));
    }

    #[test]
    fn transient_owned_and_tool_windows_are_not_independently_manageable() {
        assert!(!has_independent_app_presence(0, true));
        assert!(!has_independent_app_presence(WS_EX_TOOLWINDOW.0, false));
    }
}

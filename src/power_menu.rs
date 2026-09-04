//! Native session and power menu opened by the scripted `power_menu` widget.
//!
//! Windows owns menu navigation, accessibility, focus dismissal, and DPI.  We
//! only translate the selected command into the corresponding system API.

use std::mem::size_of;

use windows::core::{w, HRESULT};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_NOT_ALL_ASSIGNED, HANDLE, LPARAM, LUID, RECT, WIN32_ERROR,
    WPARAM,
};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::{
    ExitWindowsEx, LockWorkStation, EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT, EXIT_WINDOWS_FLAGS,
    SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_OTHER,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, PostMessageW, SetForegroundWindow, TrackPopupMenuEx,
    MF_SEPARATOR, MF_STRING, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTALIGN, TPM_TOPALIGN, WM_NULL,
};

use windows::Win32::Foundation::HWND;

const LOCK: usize = 1;
const SLEEP: usize = 2;
const SIGN_OUT: usize = 3;
const RESTART: usize = 4;
const SHUT_DOWN: usize = 5;

fn power_exit_flags(command: usize) -> Option<EXIT_WINDOWS_FLAGS> {
    match command {
        RESTART => Some(EWX_REBOOT),
        SHUT_DOWN => Some(EWX_POWEROFF),
        _ => None,
    }
}

fn validate_adjust_token_privileges(error: WIN32_ERROR) -> windows::core::Result<()> {
    if error == ERROR_NOT_ALL_ASSIGNED {
        Err(windows::core::Error::from_hresult(HRESULT::from_win32(
            error.0,
        )))
    } else {
        Ok(())
    }
}

/// Open a system-native popup against the clicked widget and execute the
/// selected command. `edge` is the panel position (`top`, `bottom`, etc.).
pub fn show(owner: HWND, widget: RECT, edge: &str) {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        eprintln!("[power-menu] CreatePopupMenu failed");
        return;
    };
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, LOCK, w!("Lock"));
        let _ = AppendMenuW(menu, MF_STRING, SLEEP, w!("Sleep"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, SIGN_OUT, w!("Sign out"));
        let _ = AppendMenuW(menu, MF_STRING, RESTART, w!("Restart"));
        let _ = AppendMenuW(menu, MF_STRING, SHUT_DOWN, w!("Shut down"));

        // Native popup menus dismiss reliably only when their owner is the
        // foreground window. WM_NULL completes the standard Win32 sequence.
        let _ = SetForegroundWindow(owner);
        let (x, y, align) = popup_anchor(widget, edge);
        let selected = TrackPopupMenuEx(
            menu,
            align | TPM_RETURNCMD.0 | TPM_NONOTIFY.0,
            x,
            y,
            owner,
            None,
        )
        .0 as usize;
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        execute(selected);
    }
}

fn popup_anchor(widget: RECT, edge: &str) -> (i32, i32, u32) {
    match edge {
        "bottom" => (widget.left, widget.top, TPM_LEFTALIGN.0 | TPM_BOTTOMALIGN.0),
        "left" => (widget.right, widget.top, TPM_LEFTALIGN.0 | TPM_TOPALIGN.0),
        "right" => (widget.left, widget.top, TPM_RIGHTALIGN.0 | TPM_TOPALIGN.0),
        _ => (widget.left, widget.bottom, TPM_LEFTALIGN.0 | TPM_TOPALIGN.0),
    }
}

unsafe fn execute(command: usize) {
    let result = match command {
        0 => return,
        LOCK => LockWorkStation(),
        SLEEP => enable_shutdown_privilege().and_then(|()| {
            if SetSuspendState(false, false, false) {
                Ok(())
            } else {
                Err(windows::core::Error::from_thread())
            }
        }),
        SIGN_OUT => ExitWindowsEx(
            EWX_LOGOFF,
            SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED,
        ),
        RESTART | SHUT_DOWN => enable_shutdown_privilege().and_then(|()| {
            let flags = power_exit_flags(command).expect("matched power command has exit flags");
            ExitWindowsEx(flags, SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED)
        }),
        _ => return,
    };
    if let Err(error) = result {
        eprintln!("[power-menu] command {command} failed: {error}");
    }
}

unsafe fn enable_shutdown_privilege() -> windows::core::Result<()> {
    let mut token = HANDLE::default();
    OpenProcessToken(
        GetCurrentProcess(),
        TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
        &mut token,
    )?;
    let result = (|| {
        let mut luid = LUID::default();
        LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut luid)?;
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        AdjustTokenPrivileges(
            token,
            false,
            Some(&privileges),
            size_of::<TOKEN_PRIVILEGES>() as u32,
            None,
            None,
        )?;
        // AdjustTokenPrivileges can return success while setting this error to
        // report that the token did not contain the requested privilege.
        validate_adjust_token_privileges(GetLastError())
    })();
    let _ = CloseHandle(token);
    result
}

#[cfg(test)]
mod tests {
    use super::{
        popup_anchor, power_exit_flags, validate_adjust_token_privileges, RESTART, SHUT_DOWN,
    };
    use windows::Win32::Foundation::{ERROR_NOT_ALL_ASSIGNED, ERROR_SUCCESS, RECT};
    use windows::Win32::System::Shutdown::{EWX_POWEROFF, EWX_REBOOT};
    use windows::Win32::UI::WindowsAndMessaging::{
        TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTALIGN, TPM_TOPALIGN,
    };

    #[test]
    fn menus_open_toward_the_desktop_from_every_panel_edge() {
        let widget = RECT {
            left: 10,
            top: 20,
            right: 50,
            bottom: 60,
        };
        assert_eq!(
            popup_anchor(widget, "bottom"),
            (10, 20, TPM_LEFTALIGN.0 | TPM_BOTTOMALIGN.0)
        );
        assert_eq!(
            popup_anchor(widget, "top"),
            (10, 60, TPM_LEFTALIGN.0 | TPM_TOPALIGN.0)
        );
        assert_eq!(
            popup_anchor(widget, "left"),
            (50, 20, TPM_LEFTALIGN.0 | TPM_TOPALIGN.0)
        );
        assert_eq!(
            popup_anchor(widget, "right"),
            (10, 20, TPM_RIGHTALIGN.0 | TPM_TOPALIGN.0)
        );
    }

    #[test]
    fn ordinary_power_commands_do_not_force_hung_applications_closed() {
        assert_eq!(power_exit_flags(RESTART), Some(EWX_REBOOT));
        assert_eq!(power_exit_flags(SHUT_DOWN), Some(EWX_POWEROFF));
        assert_eq!(power_exit_flags(usize::MAX), None);
    }

    #[test]
    fn missing_shutdown_privilege_is_reported_even_after_api_success() {
        assert!(validate_adjust_token_privileges(ERROR_NOT_ALL_ASSIGNED).is_err());
        assert!(validate_adjust_token_privileges(ERROR_SUCCESS).is_ok());
    }
}

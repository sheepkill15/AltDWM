//! Elevation policy for the protected installed copy.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

pub fn is_elevated() -> bool {
    let mut token = Default::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0;
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    unsafe {
        let _ = CloseHandle(token);
    }
    result.is_ok() && elevation.TokenIsElevated != 0
}

fn normalized(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn path_is_below(path: &Path, root: &Path) -> bool {
    let path = normalized(path).to_string_lossy().to_lowercase();
    let mut root = normalized(root).to_string_lossy().to_lowercase();
    root = root.trim_end_matches(['\\', '/']).to_string();
    path.starts_with(&format!("{root}\\")) || path.starts_with(&format!("{root}/"))
}

fn installed_executable() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let program_files = PathBuf::from(std::env::var_os("ProgramFiles")?);
    path_is_below(&exe, &program_files).then_some(exe)
}

/// Normal application launches from the protected installed shell must cross
/// back to the user's ordinary integrity level. `shell:AppsFolder` activation
/// from the elevated shell fails with `ERROR_ACCESS_DENIED` for many packaged
/// applications and would unnecessarily elevate classic applications.
pub fn normal_launch_broker_required() -> bool {
    is_elevated() && installed_executable().is_some()
}

fn quote_argument(argument: &OsStr) -> String {
    let argument = argument.to_string_lossy();
    if !argument.is_empty()
        && !argument
            .chars()
            .any(|character| matches!(character, ' ' | '\t' | '"'))
    {
        return argument.into_owned();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            backslashes += 1;
        } else if character == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
            quoted.push('"');
            backslashes = 0;
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
            quoted.push(character);
            backslashes = 0;
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

/// Relaunch the protected installed copy with UAC if it was started manually.
/// Returns `true` when this process should exit because the elevated copy was
/// launched successfully.
pub fn relaunch_installed_if_needed() -> Result<bool, String> {
    let Some(exe) = installed_executable() else {
        return Ok(false);
    };
    if is_elevated() {
        println!("[elevation] installed shell is running with administrator privileges");
        return Ok(false);
    }

    let parameters = std::env::args_os()
        .skip(1)
        .map(|argument| quote_argument(&argument))
        .collect::<Vec<_>>()
        .join(" ");
    let directory = exe.parent().unwrap_or_else(|| Path::new("."));
    let exe_wide = wide(exe.as_os_str());
    let parameters_wide = wide(OsString::from(parameters).as_os_str());
    let directory_wide = wide(directory.as_os_str());
    let result = unsafe {
        ShellExecuteW(
            None,
            windows::core::w!("runas"),
            PCWSTR(exe_wide.as_ptr()),
            PCWSTR(parameters_wide.as_ptr()),
            PCWSTR(directory_wide.as_ptr()),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err(format!(
            "the installed shell requires administrator privileges (ShellExecute code {})",
            result.0 as isize
        ))
    } else {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::{path_is_below, quote_argument};
    use std::ffi::OsStr;
    use std::path::Path;

    #[test]
    fn installed_path_check_does_not_accept_a_prefix_sibling() {
        assert!(path_is_below(
            Path::new(r"C:\Program Files\AltDWM\alt-dwm.exe"),
            Path::new(r"C:\Program Files")
        ));
        assert!(!path_is_below(
            Path::new(r"C:\Program Files Fake\AltDWM\alt-dwm.exe"),
            Path::new(r"C:\Program Files")
        ));
    }

    #[test]
    fn relaunch_arguments_follow_windows_quoting_rules() {
        assert_eq!(quote_argument(OsStr::new("--status")), "--status");
        assert_eq!(
            quote_argument(OsStr::new(r"C:\Program Files\AltDWM\config.toml")),
            r#""C:\Program Files\AltDWM\config.toml""#
        );
        assert_eq!(quote_argument(OsStr::new(r#"a"b"#)), r#""a\"b""#);
        assert_eq!(quote_argument(OsStr::new("")), r#""""#);
    }
}

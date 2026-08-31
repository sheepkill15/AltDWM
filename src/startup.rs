//! Startup processing normally supplied by Explorer when it is the shell.
//!
//! Direct Winlogon shell replacement does not process the ordinary `Run`
//! registrations or Startup folders.  AltDWM does that work only when its own
//! executable is the configured shell, and records completion in a volatile
//! HKCU key (discarded when the user's registry hive unloads at sign-out).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_ITEMS};
use windows::Win32::Security::{
    GetTokenInformation, TokenStatistics, TOKEN_QUERY, TOKEN_STATISTICS,
};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, OpenProcessToken, CREATE_NEW_PROCESS_GROUP,
    PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED_RUN: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const APPROVED_RUN32: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32";
const APPROVED_FOLDER: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder";
const WINLOGON: &str = r"Software\Microsoft\Windows NT\CurrentVersion\Winlogon";
const SESSION_MARKER: &str = r"Software\AltDWM\VolatileSession";

#[derive(Clone, Debug)]
struct Entry {
    source: String,
    name: String,
    target: String,
    enabled: bool,
    folder_item: bool,
}

pub fn launch_for_shell_once(enabled: bool) {
    if !enabled || !is_configured_shell() || session_was_processed() {
        return;
    }
    std::thread::spawn(|| {
        let entries = collect_entries();
        let mut launched = 0;
        for entry in entries.iter().filter(|entry| entry.enabled) {
            let result = if entry.folder_item {
                launch_folder_item(&entry.target)
            } else {
                launch_command(&entry.target)
            };
            if result {
                launched += 1;
                println!("[startup] launched {} ({})", entry.name, entry.source);
            } else {
                eprintln!("[startup] failed {} ({})", entry.name, entry.source);
            }
        }
        mark_session_processed();
        println!("[startup] processed {launched} enabled startup item(s)");
    });
}

pub fn print_entries_and_exit() -> ! {
    let entries = collect_entries();
    println!("Configured shell: {}", is_configured_shell());
    for entry in &entries {
        println!(
            "{:<8} {:<12} {:<28} {}",
            if entry.enabled { "enabled" } else { "disabled" },
            entry.source,
            entry.name,
            entry.target
        );
    }
    println!("{} startup item(s)", entries.len());
    std::process::exit(0);
}

fn collect_entries() -> Vec<Entry> {
    let mut entries = Vec::new();
    for (hive, hive_name) in [(HKEY_CURRENT_USER, "HKCU"), (HKEY_LOCAL_MACHINE, "HKLM")] {
        for (view, suffix, approved) in [
            (KEY_WOW64_64KEY, "Run", APPROVED_RUN),
            (KEY_WOW64_32KEY, "Run32", APPROVED_RUN32),
        ] {
            for (name, target) in enum_string_values(hive, RUN, view) {
                entries.push(Entry {
                    source: format!("{hive_name} {suffix}"),
                    enabled: approval_enabled(read_binary(hive, approved, &name, view).as_deref()),
                    name,
                    target: expand_environment(&target),
                    folder_item: false,
                });
            }
        }
    }

    if let Ok(appdata) = std::env::var("APPDATA") {
        collect_folder(
            &mut entries,
            "User Startup",
            Path::new(&appdata).join(r"Microsoft\Windows\Start Menu\Programs\Startup"),
            HKEY_CURRENT_USER,
        );
    }
    if let Ok(programdata) = std::env::var("PROGRAMDATA") {
        collect_folder(
            &mut entries,
            "Common Startup",
            Path::new(&programdata).join(r"Microsoft\Windows\Start Menu\Programs\StartUp"),
            HKEY_LOCAL_MACHINE,
        );
    }

    let mut seen = HashSet::new();
    entries.retain(|entry| seen.insert((entry.name.to_lowercase(), entry.target.to_lowercase())));
    entries
}

fn collect_folder(entries: &mut Vec<Entry>, source: &str, folder: PathBuf, hive: HKEY) {
    let Ok(items) = std::fs::read_dir(folder) else {
        return;
    };
    for item in items.flatten() {
        let path = item.path();
        if !path.is_file()
            || path
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("desktop.ini"))
        {
            continue;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        entries.push(Entry {
            source: source.into(),
            enabled: approval_enabled(
                read_binary(hive, APPROVED_FOLDER, &name, KEY_WOW64_64KEY).as_deref(),
            ),
            name,
            target: path.to_string_lossy().into_owned(),
            folder_item: true,
        });
    }
}

fn approval_enabled(value: Option<&[u8]>) -> bool {
    // StartupApproved state bytes used by Windows are even when enabled (02,
    // 06) and odd when disabled (03, 07). Missing approval data means enabled.
    value
        .and_then(|bytes| bytes.first())
        .is_none_or(|state| state & 1 == 0)
}

fn is_configured_shell() -> bool {
    let shell = read_string(HKEY_CURRENT_USER, WINLOGON, "Shell", KEY_WOW64_64KEY)
        .or_else(|| read_string(HKEY_LOCAL_MACHINE, WINLOGON, "Shell", KEY_WOW64_64KEY));
    let Some(shell) = shell else { return false };
    let Some(configured) = first_executable(&expand_environment(&shell)) else {
        return false;
    };
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    if Path::new(&configured).is_absolute() {
        paths_equal(Path::new(&configured), &current)
    } else {
        Path::new(&configured).file_name() == current.file_name()
    }
}

fn first_executable(command: &str) -> Option<String> {
    let value = command.trim();
    if let Some(rest) = value.strip_prefix('"') {
        return rest.split_once('"').map(|(exe, _)| exe.to_string());
    }
    value.split_whitespace().next().map(str::to_string)
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    a.canonicalize().unwrap_or_else(|_| a.to_path_buf())
        == b.canonicalize().unwrap_or_else(|_| b.to_path_buf())
}

fn session_was_processed() -> bool {
    let Some(session) = authentication_id() else {
        return false;
    };
    read_binary(
        HKEY_CURRENT_USER,
        SESSION_MARKER,
        "StartupProcessed",
        KEY_WOW64_64KEY,
    )
    .is_some_and(|stored| stored == session)
}

fn mark_session_processed() {
    let Some(session) = authentication_id() else {
        return;
    };
    let key_w = wide(SESSION_MARKER);
    let mut key = HKEY::default();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_w.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    };
    if status.is_ok() {
        let name = wide("StartupProcessed");
        unsafe {
            let _ = RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_BINARY, Some(&session));
            let _ = RegCloseKey(key);
        }
    }
}

fn authentication_id() -> Option<[u8; 8]> {
    let mut token = Default::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()? };
    let mut statistics = TOKEN_STATISTICS::default();
    let mut returned = 0;
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenStatistics,
            Some((&mut statistics as *mut TOKEN_STATISTICS).cast()),
            std::mem::size_of::<TOKEN_STATISTICS>() as u32,
            &mut returned,
        )
    };
    unsafe {
        let _ = CloseHandle(token);
    }
    result.ok()?;
    let mut id = [0; 8];
    id[..4].copy_from_slice(&statistics.AuthenticationId.LowPart.to_le_bytes());
    id[4..].copy_from_slice(&statistics.AuthenticationId.HighPart.to_le_bytes());
    Some(id)
}

fn enum_string_values(root: HKEY, path: &str, view: REG_SAM_FLAGS) -> Vec<(String, String)> {
    let Some(key) = open_key(root, path, KEY_READ | view) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let mut index = 0;
    loop {
        let mut name = vec![0u16; 16_384];
        let mut name_len = name.len() as u32;
        let mut data = vec![0u8; 65_536];
        let mut data_len = data.len() as u32;
        let mut kind = 0u32;
        let status = unsafe {
            RegEnumValueW(
                key,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status.is_ok() && (kind == REG_SZ.0 || kind == REG_EXPAND_SZ.0) {
            data.truncate(data_len as usize);
            result.push((
                String::from_utf16_lossy(&name[..name_len as usize]),
                bytes_to_utf16(&data),
            ));
        }
        index += 1;
    }
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

fn read_string(root: HKEY, path: &str, name: &str, view: REG_SAM_FLAGS) -> Option<String> {
    read_value(root, path, name, view).map(|(_, data)| bytes_to_utf16(&data))
}

fn read_binary(root: HKEY, path: &str, name: &str, view: REG_SAM_FLAGS) -> Option<Vec<u8>> {
    read_value(root, path, name, view).map(|(_, data)| data)
}

fn read_value(
    root: HKEY,
    path: &str,
    name: &str,
    view: REG_SAM_FLAGS,
) -> Option<(REG_VALUE_TYPE, Vec<u8>)> {
    let key = open_key(root, path, KEY_READ | view)?;
    let name_w = wide(name);
    let mut kind = REG_NONE;
    let mut size = 0u32;
    let first = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };
    if first.is_err() {
        unsafe {
            let _ = RegCloseKey(key);
        }
        return None;
    }
    let mut data = vec![0u8; size as usize];
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut kind),
            Some(data.as_mut_ptr()),
            Some(&mut size),
        )
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    status.is_ok().then_some((kind, data))
}

fn open_key(root: HKEY, path: &str, access: REG_SAM_FLAGS) -> Option<HKEY> {
    let path_w = wide(path);
    let mut key = HKEY::default();
    unsafe {
        RegOpenKeyExW(root, PCWSTR(path_w.as_ptr()), None, access, &mut key)
            .is_ok()
            .then_some(key)
    }
}

fn bytes_to_utf16(data: &[u8]) -> String {
    let words: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .take_while(|c| *c != 0)
        .collect();
    String::from_utf16_lossy(&words)
}

fn expand_environment(value: &str) -> String {
    let input = wide(value);
    let required = unsafe { ExpandEnvironmentStringsW(PCWSTR(input.as_ptr()), None) };
    if required == 0 {
        return value.into();
    }
    let mut output = vec![0u16; required as usize];
    let written = unsafe { ExpandEnvironmentStringsW(PCWSTR(input.as_ptr()), Some(&mut output)) };
    if written == 0 {
        value.into()
    } else {
        String::from_utf16_lossy(&output[..written.saturating_sub(1) as usize])
    }
}

fn launch_command(command: &str) -> bool {
    let mut command = wide(command);
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    let result = unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NEW_PROCESS_GROUP,
            None,
            PCWSTR::null(),
            &startup,
            &mut process,
        )
    };
    if result.is_ok() {
        unsafe {
            let _ = CloseHandle(process.hThread);
            let _ = CloseHandle(process.hProcess);
        }
        true
    } else {
        false
    }
}

fn launch_folder_item(path: &str) -> bool {
    let path = wide(path);
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR::null(),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    (result.0 as isize) > 32
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_approval_states_are_respected() {
        assert!(approval_enabled(None));
        assert!(approval_enabled(Some(&[2])));
        assert!(!approval_enabled(Some(&[3])));
        assert!(approval_enabled(Some(&[6])));
        assert!(!approval_enabled(Some(&[7])));
    }

    #[test]
    fn extracts_shell_executable() {
        assert_eq!(
            first_executable(r#""C:\AltDWM\alt-dwm.exe" --flag"#).as_deref(),
            Some(r"C:\AltDWM\alt-dwm.exe")
        );
        assert_eq!(
            first_executable("explorer.exe").as_deref(),
            Some("explorer.exe")
        );
    }
}

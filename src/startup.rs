//! Startup processing normally supplied by Explorer when it is the shell.
//!
//! Direct Winlogon shell replacement does not process the ordinary `Run`
//! registrations or Startup folders.  AltDWM does that work only when its own
//! executable is the configured shell, and records completion in a volatile
//! HKCU key (discarded when the user's registry hive unloads at sign-out).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
const STATE: &str = r"Software\AltDWM";

#[derive(Clone, Debug)]
struct Entry {
    source: String,
    name: String,
    target: String,
    enabled: bool,
    folder_item: bool,
}

pub fn launch_for_shell_once(enabled: bool, scheduled_shell_session: bool) {
    // The installed shell is deliberately bootstrapped through a highest-level
    // scheduled task. Ordinary startup applications must not inherit that
    // elevated token, so dispatch a second, Limited task to do Explorer's
    // startup work at the user's normal integrity level.
    if scheduled_shell_session {
        std::thread::spawn(move || {
            let command = (enabled && !session_was_processed()).then_some("startup");
            if let Err(error) = ensure_user_helper_and_send(command) {
                eprintln!("[startup] {error}");
            }
        });
        return;
    }
    if !enabled || !is_configured_shell() || session_was_processed() {
        return;
    }
    launch_entries_async();
}

fn launch_entries_async() {
    std::thread::spawn(|| {
        launch_entries();
    });
}

fn launch_entries() {
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
}

pub fn run_user_session_helper_and_exit() -> ! {
    if let Err(error) = run_user_helper() {
        eprintln!("[startup] normal-integrity helper stopped: {error}");
    }
    std::process::exit(0);
}

fn start_user_helper_task() -> bool {
    let task_path = read_string(HKEY_CURRENT_USER, STATE, "StartupTaskPath", KEY_WOW64_64KEY)
        .unwrap_or_else(|| "\\".into());
    let Some(task_name) = read_string(HKEY_CURRENT_USER, STATE, "StartupTaskName", KEY_WOW64_64KEY)
    else {
        return false;
    };
    let task = format!("{task_path}{task_name}");
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    launch_command(&format!(
        r#""{}\System32\schtasks.exe" /Run /TN "{}""#,
        system_root, task
    ))
}

fn pipe_name() -> String {
    let mut session = 0u32;
    unsafe {
        let _ = windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(
            std::process::id(),
            &mut session,
        );
    }
    format!(r"\\.\pipe\AltDWM.UserSession.{session}")
}

fn send_user_helper_command(command: &str) -> Result<(), String> {
    use windows::Win32::Foundation::GENERIC_WRITE;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, OPEN_EXISTING,
    };

    let name = wide(&pipe_name());
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|error| format!("normal-integrity helper is unavailable: {error:?}"))?;
    let bytes = command.as_bytes();
    let mut written = 0u32;
    let result = unsafe { WriteFile(handle, Some(bytes), Some(&mut written), None) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    result.map_err(|error| format!("could not send a launch request: {error:?}"))?;
    if written as usize != bytes.len() {
        return Err("normal-integrity helper accepted only part of a launch request".into());
    }
    Ok(())
}

fn ensure_user_helper_and_send(command: Option<&str>) -> Result<(), String> {
    if let Some(command) = command {
        if send_user_helper_command(command).is_ok() {
            return Ok(());
        }
    }
    if !start_user_helper_task() {
        return Err("failed to start the normal-integrity application helper".into());
    }
    let Some(command) = command else {
        return Ok(());
    };
    for _ in 0..40 {
        if send_user_helper_command(command).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("normal-integrity application helper did not become ready".into())
}

pub fn launch_app_at_normal_integrity(id: String, name: String) {
    std::thread::spawn(move || {
        println!("[apps] launch {name} ({id}) through normal-integrity helper");
        let command = format!("app\n{id}");
        if let Err(error) = ensure_user_helper_and_send(Some(&command)) {
            eprintln!("[apps] failed to launch {name}: {error}");
        }
    });
}

pub fn launch_app_as_admin_via_user_helper(id: String, name: String) {
    std::thread::spawn(move || {
        println!("[apps] launch as admin {name} ({id}) through normal-integrity helper");
        let command = format!("admin\n{id}");
        if let Err(error) = ensure_user_helper_and_send(Some(&command)) {
            eprintln!("[apps] failed to launch as admin {name}: {error}");
        }
    });
}

fn run_user_helper() -> Result<(), String> {
    use windows::Win32::Foundation::{GetLastError, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{ReadFile, PIPE_ACCESS_INBOUND};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
    };

    let name = wide(&pipe_name());
    let pipe = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            0,
            16 * 1024,
            0,
            None,
        )
    };
    if pipe == INVALID_HANDLE_VALUE {
        return Err(format!("CreateNamedPipeW failed: {:?}", unsafe {
            GetLastError()
        }));
    }
    println!("[startup] normal-integrity application helper ready");
    loop {
        let connected = unsafe { ConnectNamedPipe(pipe, None) }.is_ok()
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if !connected {
            let error = unsafe { GetLastError() };
            unsafe {
                let _ = CloseHandle(pipe);
            }
            return Err(format!("ConnectNamedPipe failed: {error:?}"));
        }
        let mut buffer = vec![0u8; 16 * 1024];
        let mut read = 0u32;
        let received = unsafe { ReadFile(pipe, Some(&mut buffer), Some(&mut read), None) }.is_ok();
        unsafe {
            let _ = DisconnectNamedPipe(pipe);
        }
        if !received {
            continue;
        }
        let command = String::from_utf8_lossy(&buffer[..read as usize]);
        if command == "shutdown" {
            break;
        }
        if command == "startup" {
            if !session_was_processed() {
                launch_entries();
            }
            continue;
        }
        if let Some(id) = command.strip_prefix("app\n") {
            launch_apps_folder_id(id.trim(), false);
            continue;
        }
        if let Some(id) = command.strip_prefix("admin\n") {
            launch_apps_folder_id(id.trim(), true);
        }
    }
    unsafe {
        let _ = CloseHandle(pipe);
    }
    Ok(())
}

fn launch_apps_folder_id(id: &str, elevated: bool) {
    if !elevated && crate::apps::is_packaged_app_id(id) {
        match crate::apps::activate_packaged_app(id) {
            Ok(pid) => println!("[apps] activated packaged {id} (pid={pid})"),
            Err(error) => eprintln!("[apps] packaged activation failed for {id}: {error}"),
        }
        return;
    }
    let target = wide(&format!("shell:AppsFolder\\{id}"));
    let result = unsafe {
        ShellExecuteW(
            None,
            if elevated {
                windows::core::w!("runas")
            } else {
                PCWSTR::null()
            },
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        eprintln!(
            "[apps] {} launch failed for {id}: {}",
            if elevated {
                "elevated"
            } else {
                "normal-integrity"
            },
            result.0 as isize
        );
        if elevated && crate::apps::is_packaged_app_id(id) {
            eprintln!("[apps] package {id} has no usable elevated verb; activating normally");
            match crate::apps::activate_packaged_app(id) {
                Ok(pid) => println!("[apps] activated packaged {id} (pid={pid})"),
                Err(error) => eprintln!("[apps] packaged activation failed for {id}: {error}"),
            }
        }
    }
}

pub fn shutdown_user_helper() {
    let _ = send_user_helper_command("shutdown");
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

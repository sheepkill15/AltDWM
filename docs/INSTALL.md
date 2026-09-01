# Install — Shell Replacement

AltDWM replaces `explorer.exe` as the Windows shell (`Winlogon\Shell`), not `dwm.exe` (compositor stays).

> **No registry is touched automatically.** You run `install.ps1` explicitly. Building alone (`cargo check` / `cargo build`) never writes registry — per your request, we test only via builds, not via running.

## Quick try (no reboot)

```powershell
$env:PATH += ";$HOME\.cargo\bin"
cargo build --release
.\target\release\alt-dwm.exe --help
.\target\release\alt-dwm.exe --generate-config   # %APPDATA%\AltDWM\config.toml
# edit %APPDATA%\AltDWM\config.toml — panels/widgets/rules/keybinds/layouts

# safe live test (kills explorer, AltDWM keeps tiling via dwm.exe):
taskkill /f /im explorer.exe
# ... move windows, Alt+Shift+R retile, Alt+Shift+J/K focus ...
explorer.exe   # restore
```

## Install as shell

```powershell
# install securely and register for the current account (requires admin, logoff):
.\install.ps1
# compatibility alias; shell registration is always scoped to the current account:
.\install.ps1 -PerUser
# secure custom dir / skip build:
.\install.ps1 -InstallDir "C:\Program Files\AltDWM-Test" -NoBuild

# check
.\target\release\alt-dwm.exe --config "C:\Program Files\AltDWM\config.toml" --check-config

# uninstall
.\uninstall.ps1
.\uninstall.ps1 -PerUser # compatibility alias
```

### What install.ps1 does

1. `cargo build --release` → `target\release\alt-dwm.exe`
2. Copies `alt-dwm.exe` + `scripts/` + `config.example.toml` to `C:\Program Files\AltDWM\`
3. Generates `$InstallDir\config.toml` via `--generate-config` if missing
4. Registers a current-user Task Scheduler action at `Highest` run level, saves the existing shell value under `HKCU\SOFTWARE\AltDWM`, then sets the current account's HKCU Winlogon shell to invoke that task

`uninstall.ps1` removes the scheduled task, restores the exact saved value, and only changes the registry when the current shell exactly matches its AltDWM installation command. Manual Explorer fallback:

```powershell
Set-ItemProperty -Path "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon" -Name Shell -Value "explorer.exe"
# per-user: Remove-ItemProperty -Path "HKCU:\...\Winlogon" -Name Shell
logoff
```

## Administrator access and elevated windows

The executable remains `asInvoker` because Winlogon cannot service a UAC prompt
while directly creating a replacement-shell process. Instead, `install.ps1`
registers an on-demand Task Scheduler entry for the current user with `Highest`
run level, then points Winlogon at `schtasks.exe /Run` for that entry. AltDWM
therefore starts with the user's elevated administrator token without risking a
login-time `ERROR_ELEVATION_REQUIRED` failure.

If the Program Files copy is launched manually, its runtime guard uses the UAC
`runas` path to restart elevated. It exits instead of silently continuing with
insufficient rights when elevation is declined. Builds outside Program Files do
not use that guard.

The installer rejects user-writable destinations and keeps the executable,
scripts, and Rhai configuration under Program Files. This is required because a
normal process that could replace any of those files could otherwise use the
scheduled task to gain administrator privileges. `-PerUser` is retained as a
compatibility alias because shell registration is now always per-account. A
machine-wide HKLM shell cannot safely target one user's elevated task and could
leave other accounts with a blank desktop.

Windows still blocks manipulation of protected processes and windows running as
`SYSTEM`. Applications launched directly by an elevated shell can inherit its
administrator token; this is the security and compatibility tradeoff of making
the shell itself elevated.

`uiAccess` remains a future alternative that would require code signing and a
trusted installation directory. It is not enabled by this installer.

## DPI

Manifest declares `PerMonitorV2` (`alt-dwm.manifest`). Panels and tiling derive display bounds from `GetMonitorInfoW`; test `monitor = "all"`, primary-only bars, and mixed-DPI displays.

## Troubleshooting

- **Hotkey already registered `1409`** — another non-Win shortcut owns it. Choose a different chord. Configured `Win+...` bindings are intercepted by AltDWM instead of being registered with `RegisterHotKey`, so they can take precedence over shell shortcuts; avoid assigning Windows security chords such as `Win+L`.
- **Config not reloading** — `notify` watcher debounces 500ms, also `Alt+Shift+C` manual reload. Check `[watcher] ... changed -> reload pending` in log.
- **Build only** — per your request, we validate via `cargo check` / `cargo build`, not by running.

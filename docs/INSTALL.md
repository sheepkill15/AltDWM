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
# per-machine (requires admin, logoff):
.\install.ps1
# per-user (no admin):
.\install.ps1 -PerUser
# custom dir / skip build:
.\install.ps1 -InstallDir C:\AltDWM -NoBuild

# check
.\target\release\alt-dwm.exe --config "C:\Program Files\AltDWM\config.toml" --check-config

# uninstall
.\uninstall.ps1
.\uninstall.ps1 -PerUser
```

### What install.ps1 does

1. `cargo build --release` → `target\release\alt-dwm.exe`
2. Copies `alt-dwm.exe` + `scripts/` + `config.example.toml` → `C:\Program Files\AltDWM\`
3. Generates `%APPDATA%\AltDWM\config.toml` (or `$InstallDir\config.toml` if per-machine) via `--generate-config` if missing
4. Sets `HKLM` or `HKCU\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\Shell = "C:\Program Files\AltDWM\alt-dwm.exe"`

Restore:

```powershell
Set-ItemProperty -Path "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon" -Name Shell -Value "explorer.exe"
# per-user: Remove-ItemProperty -Path "HKCU:\...\Winlogon" -Name Shell
logoff
```

## uiAccess (move elevated windows like Task Manager)

1. Edit `alt-dwm.manifest`: `requestedExecutionLevel uiAccess="true"`
2. Sign `alt-dwm.exe` with trusted cert (self-signed + import to Trusted Root, or EV)
3. Install to secure location: `C:\Program Files\AltDWM\` or `C:\Windows\System32\`
4. Rebuild: `cargo build --release` (manifest embedded via `build.rs` `/MANIFEST:EMBED`)
5. Run `install.ps1` again

Without `uiAccess`, `SetWindowPos` on elevated windows fails (`ERROR_ACCESS_DENIED`) — tiling will skip them.

## DPI

Manifest declares `PerMonitorV2` (`alt-dwm.manifest`). Panels and tiling derive display bounds from `GetMonitorInfoW`; test `monitor = "all"`, primary-only bars, and mixed-DPI displays.

## Troubleshooting

- **Hotkey already registered `1409`** — `Win+Shift+S` is Snipping Tool; defaults now `Alt+Shift` (`src/config.rs:225`). Change `[[keybinds]] keys = "Win+..."`
- **Config not reloading** — `notify` watcher debounces 500ms, also `Alt+Shift+C` manual reload. Check `[watcher] ... changed -> reload pending` in log.
- **Build only** — per your request, we validate via `cargo check` / `cargo build`, not by running.

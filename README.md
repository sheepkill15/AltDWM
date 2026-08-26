# AltDWM — Experimental Windows 11 Shell / Tiling WM

Rust prototype that **proves** a full Explorer replacement is possible on Windows 11 — without replacing `dwm.exe` (which is not replaceable).

> `explorer.exe` (taskbar + desktop + Start) **= replaceable** via `Winlogon\Shell`  
> `dwm.exe` (compositor) **= NOT replaceable** — you hook it via `Win32`/`DWM` APIs instead.  
> AltDWM does the latter: `SetWinEventHook` + `DeferWindowPos` + a custom taskbar.

Tested on Windows 11, 2 monitors, Rust 1.98 + `windows` 0.61.

## What this prototype does

- **Tiling WM** (`src/manager.rs`): enumerates top-level windows via `EnumWindows`, filters with `IsWindowVisible`, `IsIconic`, `DwmGetWindowAttribute(DWMWA_CLOAKED)`, `WS_EX_TOOLWINDOW`, `GetAncestor(GA_ROOT)`, etc. (`src/util.rs:15`). Groups per-monitor via `MonitorFromWindow`/`GetMonitorInfoW` and tiles with `BeginDeferWindowPos`/`DeferWindowPos`/`EndDeferWindowPos` atomic moves (`src/manager.rs:112`).
- **Layouts** (`src/layout.rs:7`): `MasterStack` (60/40, stack right), `Grid`, `Monocle`, `Floating`.
- **Event-driven** (`src/main.rs:43`): `SetWinEventHook` for `EVENT_SYSTEM_FOREGROUND`, `MINIMIZESTART/END`, `MOVESIZEEND`, `OBJECT_CREATE/DESTROY/SHOW/HIDE` (`WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS`). Callback just sets `RETILE_PENDING`; a 200ms `WM_TIMER` on a message-only `AltDWM_Host` window does the actual `tile_windows` to avoid re-entrancy.
- **Taskbar replacement** (`src/taskbar.rs:26`): `WS_EX_TOPMOST | WS_EX_TOOLWINDOW` popup at bottom (`1920x40`), `FillRect` + `TextOutW` clock, 1s timer. Reserves `40px` from work area so tiled windows don't overlap it. Excluded from tiling via class filter.
- **Hotkeys** (`src/main.rs:274`): Thread `RegisterHotKey` (`Win+Shift+`):
  - `R` retile, `T` toggle tiling, `Q` quit, `G` grid, `M` monocle, `F` floating, `S` masterStack

## Quick start

```powershell
# cargo is in .cargo\bin - add to PATH for this shell
$env:PATH += ";$HOME\.cargo\bin"

cargo run -- --help
cargo run -- --no-taskbar --gap 12 --layout grid   # tiling only
cargo run                                           # tiling + 40px taskbar

# hotkeys while running:
# Win+Shift+R retile, T toggle, Q quit, G/M/F/S switch layout
```

Exit with `Win+Shift+Q` or `Ctrl+C` (kills process - no shell hook yet).

## Verify shell-replacement capability

```powershell
# 1. Build release
cargo build --release  # -> target\release\alt-dwm.exe

# 2. Print registry commands
.\target\release\alt-dwm.exe --replace-shell

# 3. (Safe test without reboot) kill Explorer, AltDWM keeps tiling:
taskkill /f /im explorer.exe
# ... move windows, AltDWM retile still works (dwm.exe stays alive)
explorer.exe   # restore

# 4. Real shell replacement (requires admin, logoff required):
#    Copy exe to C:\AltDWM\alt-dwm.exe first!
reg add "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon" /v Shell /t REG_SZ /d "C:\AltDWM\alt-dwm.exe" /f
# restore:
reg add "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon" /v Shell /t REG_SZ /d "explorer.exe" /f
# per-user (no admin):
reg add "HKCU\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon" /v Shell /t REG_SZ /d "C:\AltDWM\alt-dwm.exe" /f
```

## Project layout

```
Cargo.toml           # windows = { version="0.61", features=[Win32_Foundation, Graphics_Gdi/Dwm, ...] } (src/main.rs imports)
src/main.rs          # Arg parse, host window, WinEventHook, message loop, hotkeys
src/manager.rs       # collect_windows, per-monitor DeferWindowPos tiling
src/layout.rs        # MasterStack/Grid/Monocle/Floating rect math
src/taskbar.rs       # AltDWM_Taskbar window
src/util.rs          # is_cloaked, is_manageable, class/title helpers
```

## Key APIs proven working (log from real run)

```
[host] message-only window hwnd=0x80636
[hotkey] registered Win+Shift+R/T/Q/G/M/F/S
[hook] FOREGROUND 0x3-0x3 => ...
[manager] tiling 3 windows with layout MasterStack gap=8
  - 0x503bc class=Chrome_WidgetWin_1 title="OpenCode"
[manager] monitor 0x40079 area "(0,0 1920x992)"   # primary, 40px reserved for AltDWM taskbar
[manager] monitor 0x2008b area "(-1920,0 1920x1032)"  # secondary
[manager] tiling committed
[taskbar] created hwnd=0x205f6 1920x40 @ 0,1040
```

## Why DWM is not replaceable

- `dwm.exe` + `dwmcore.dll`/`udwm.dll` + `win32k.sys` own the `DirectComposition` swapchains since Vista. No `RegisterCompositor` API exists. Replacing would need a kernel driver + reimplementing `WDDM` presentation — breaks PatchGuard/SecureBoot and every update.  
- All existing WMs (`GlazeWM`, `Komorebi`, `bug.n`, `FancyWM`, `Cairo`) keep `dwm.exe` and just call `DwmGet/SetWindowAttribute`, `DwmExtendFrame`, `SetWindowPos`. AltDWM follows that model.
- For deeper tweaks (Mica, rounded corners, `IVirtualDesktopManagerInternal`) you need `MinHook`/`Detours` DLL injection into `explorer.exe`/`dwm.exe` — fragile, per-build offsets (see `Windhawk`, `ExplorerPatcher`). Left as future work.

## Next steps

- `AltDHook.dll` (C++ `WH_CBT`/`WH_SHELL` hook) for windows that ignore `WinEvent` (elevated, `WS_EX_NOACTIVATE`).
- `IVirtualDesktopManagerInternal` (COM in `twinui.pcshell.dll`) for workspaces.
- System tray (`Shell_NotifyIcon` + `NOTIFYICONDATA`) and Start search (`ISearchManager`).
- `uiAccess=true` manifest + signing to tile elevated windows.
- Config file (`~/.config/altdwm/config.toml`) for gaps/layout per monitor.

## Build

```powershell
cargo check
cargo build
cargo build --release  # lto + opt-level="z"
```

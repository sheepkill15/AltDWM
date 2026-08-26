# AltDWM — Experimental Windows 11 Shell / Tiling WM

Rust prototype that **proves** a full Explorer replacement is possible on Windows 11 — without replacing `dwm.exe` (which is not replaceable).

> `explorer.exe` (taskbar + desktop + Start) **= replaceable** via `Winlogon\Shell`  
> `dwm.exe` (compositor) **= NOT replaceable** — you hook it via `Win32`/`DWM` APIs instead.  
> AltDWM does the latter: `SetWinEventHook` + `DeferWindowPos` + declarative panels.

Tested on Windows 11, 2 monitors, Rust 1.98 + `windows` 0.61.

## What this prototype does (0.2.0)

- **Tiling WM** (`src/manager.rs:84`): `EnumWindows` + `IsWindowVisible`/`IsIconic`/`DwmGetWindowAttribute(DWMWA_CLOAKED)`/`WS_EX_TOOLWINDOW`/`GA_ROOT` (`src/util.rs:57`). Per-monitor `MonitorFromWindow`/`GetMonitorInfoW`; atomic `BeginDeferWindowPos`/`DeferWindowPos` (`src/manager.rs:150`).
- **Layouts** (`src/layout.rs:7`): `MasterStack` (60/40), `Grid`, `Monocle`, `Floating` — pluggable via `[[layouts.my]] script="..."` (Rhai).
- **Event-driven** (`src/main.rs:89`): `SetWinEventHook` for `FOREGROUND/MINIMIZE/MOVESIZE/OBJECT_*` (`WINEVENT_OUTOFCONTEXT`). 200ms `WM_TIMER` on `AltDWM_Host` (message-only) does `tile_windows_reserved`.
- **Declarative panels DSL** (`src/config.rs:16`, `docs/EXTENSIBILITY.md`): TOML `[[panels]]`/`[[widgets]]`/`[[rules]]`/`[[keybinds]]`. Multiple bars: `position=top|bottom|left|right`, `monitor=all|primary`, `widgets=[...]`. Example `examples/config.example.toml` has `bottom` (40px, `workspaces/title/spacer/tray/clock`) + `top` (28px, `launcher/spacer/cpu`).
- **Widgets** (`src/widgets.rs:14` trait): `clock` (strftime `%H:%M:%S`), `workspaces`, `window_title` (foreground), `tray` (stub `Shell_NotifyIcon` sink), `spacer` (flex), `launcher`, `custom` (Rhai script returns text). `create_widget` factory + `extra` flattened map for forward-compat.
- **Panels** (`src/panel.rs:47`): Each `[[panels]]` is a `WS_POPUP|WS_EX_TOPMOST` window (`AltDWM_Panel` class), flex layout for widgets, 1s + 250ms timers, click → `scripting::dispatch_action`.
- **Scripting** (`src/scripting.rs:8`): Embedded `rhai` 1.26 engine. Exposes `launch(cmd)`, `log(msg)`, `get_cpu_usage()`, `focused_title()`, `retile()`, `set_layout(name)`. Any `action = "rhai: ..."` or `script = "scripts/cpu.rhai"` evaluated sandboxed.
- **Config** (`src/config.rs:109`): Search `exe_dir/config.toml` → `%APPDATA%/AltDWM/config.toml` → `./config.toml`. `general`+`ignore` + `panels/widgets/rules/keybinds/layouts` with `flatten` extras for easy extend. `--config`, `--generate-config`, `--check-config`, `Alt+Shift+C` hot-reload (configurable, `Win+Shift` collides with system e.g. `Win+Shift+S` = Snipping Tool). `validate()` warns on bad panels.
- **Hotkeys** (`src/main.rs:273`): Dynamic `RegisterHotKey` from `[[keybinds]]` (default `Alt+Shift+` `R` retile, `T` toggle, `Q` quit, `G` grid, `M` monocle, `F` floating, `S` masterStack, `C` reload, all dispatch via `scripting::dispatch_action`).

## Quick start

```powershell
$env:PATH += ";$HOME\.cargo\bin"
cargo run -- --help
cargo run                                           # default taskbar 40px
cargo run -- --no-taskbar --gap 12 --layout grid
cargo run -- --config ./examples/config.example.toml  # panels DSL demo
cargo run -- --generate-config   # writes %APPDATA%/AltDWM/config.toml
cargo run -- --check-config      # validate

# hotkeys: Alt+Shift+R/T/Q/G/M/F/S/C (reload) — change in config.toml [[keybinds]]
```

## Verify shell-replacement capability (no registry touched automatically)

See `docs/INSTALL.md` for `install.ps1` / `uninstall.ps1` (per-user or per-machine, `uiAccess`, DPI).

```powershell
cargo build --release  # -> target\release\alt-dwm.exe
.\target\release\alt-dwm.exe --replace-shell  # prints reg commands only

# safe test without reboot:
taskkill /f /im explorer.exe
# AltDWM keeps tiling (dwm.exe alive) -> explorer.exe to restore

# install as shell (admin):
.\install.ps1                 # HKLM, copies to C:\Program Files\AltDWM
.\install.ps1 -PerUser        # HKCU, no admin
.\uninstall.ps1; .\uninstall.ps1 -PerUser  # restore explorer.exe
```

## Extensibility DSL

See `docs/EXTENSIBILITY.md` + `examples/config.example.toml` + `scripts/*.rhai`.

```toml
[[panels]]
name = "bottom"
position = "bottom"
height = 40
monitor = "all"
widgets = ["workspaces", "window_title", "spacer", "tray", "clock"]

[[widgets]]
type = "clock"
name = "clock"
format = "%H:%M:%S"
interval = 1000
action = 'rhai: launch("explorer.exe")'

[[widgets]]
type = "custom"
name = "cpu"
script = "scripts/cpu.rhai"
interval = 2000

[[rules]]
match_class = "Spotify"
floating = true

[[keybinds]]
keys = "Alt+Shift+Return"
action = "launch('wt.exe')"
```

Panels reserve `top`+`bottom` heights before tiling — e.g. `top 28 + bottom 40` → primary work `1920x964` at `0,28` (see log). Widgets implement `trait Widget` (`src/widgets.rs:21`) — add a Rust widget by `create_widget` + `extra` map, or a Rhai script, or a future `plugins/*.dll` cdylib.

## Project layout

```
Cargo.toml  # windows 0.61 + serde/toml/dirs + rhai 1.26 + regex 1.13
src/main.rs      # host window, WinEventHook, message loop, config hot-reload, hotkeys (public statics for crate::)
src/config.rs    # Config{general,ignore,panels,widgets,rules,keybinds,layouts} + find/load/validate
src/manager.rs   # collect_windows, tile_windows_reserved(top,bottom), per-monitor DeferWindowPos
src/layout.rs    # MasterStack/Grid/Monocle/Floating + compute_layout
src/taskbar.rs   # legacy AltDWM_Taskbar (fallback when no panels)
src/panel.rs     # AltDWM_Panel — declarative panels from config
src/widgets.rs   # Widget trait + clock/spacer/title/tray/workspaces/launcher/custom
src/scripting.rs # rhai engine + dispatch_action
src/util.rs      # is_cloaked, is_manageable (+ config ignore)
docs/EXTENSIBILITY.md
examples/config.example.toml
scripts/cpu.rhai, spiral.rhai
```

## Key APIs proven (real log, 0.2.0 panels)

```
[config] loaded C:\...\alt_test_cfg.toml
[panel] 'bottom' @ 0,1040 1920x40 monitor=all widgets=workspaces,window_title,spacer,tray,clock
[panel] 'top' @ 0,0 1920x28 monitor=primary widgets=launcher,spacer,cpu
[manager] monitor 0x40079 area "(0,28 1920x964)"  # 28 top + 40 bottom reserved
[manager] -> 0x2046a => 1148x948 @ 8,36
```

Fallback (no panels) → `[taskbar] 1920x40 @ 0,1040` + `"(0,0 1920x992)"`.

## Why DWM is not replaceable

- `dwm.exe`+`dwmcore.dll`/`win32k.sys` own `DirectComposition` swapchains since Vista. No `RegisterCompositor` — would need kernel driver + `WDDM` reimpl, breaks PatchGuard. All WMs (`GlazeWM`, `Komorebi`, `Cairo`) keep `dwm.exe` and call `Dwm*`/`SetWindowPos`. For Mica/rounded corners/`IVirtualDesktopManagerInternal`, inject via `MinHook`/`Detours` (`Windhawk`) — per-build offsets.

## Next steps

- `AltDHook.dll` (`WH_CBT`) for elevated windows
- `IVirtualDesktopManager` + `IVirtualDesktopManagerInternal` workspaces
- Real `Shell_NotifyIcon` systray sink
- `uiAccess` manifest + signing
- Rhai custom layouts (`fn layout(n, area) -> rects`)

## Build

```powershell
cargo check
cargo build
cargo build --release  # lto + opt-level z
```

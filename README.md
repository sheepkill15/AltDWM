# AltDWM — Experimental Windows 11 Shell / Tiling WM

Rust prototype that **proves** a full Explorer replacement is possible on Windows 11 — without replacing `dwm.exe` (which is not replaceable).

> `explorer.exe` (taskbar + desktop + Start) **= replaceable** via `Winlogon\Shell`  
> `dwm.exe` (compositor) **= NOT replaceable** — you hook it via `Win32`/`DWM` APIs instead.  
> AltDWM does the latter: `SetWinEventHook` + `DeferWindowPos` + declarative panels.

Tested on Windows 11, 2 monitors, Rust 1.98 + `windows` 0.61.

## What AltDWM does (0.3.0)

- **Tiling WM** (`src/manager.rs:84`): `EnumWindows` + `IsWindowVisible`/`IsIconic`/`DwmGetWindowAttribute(DWMWA_CLOAKED)`/`WS_EX_TOOLWINDOW`/`GA_ROOT` (`src/util.rs:57`). Per-monitor `MonitorFromWindow`/`GetMonitorInfoW`; atomic `BeginDeferWindowPos`/`DeferWindowPos` (`src/manager.rs:150`).
- **Layouts** (`src/layout.rs:7`): `MasterStack` (60/40), `Grid`, `Monocle`, `Floating` — pluggable via `[[layouts.my]] script="..."` (Rhai).
- **Event-driven** (`src/main.rs`): `SetWinEventHook` for `FOREGROUND/MINIMIZE/MOVESIZE/OBJECT_*` (`WINEVENT_OUTOFCONTEXT`). A newly shown window bypasses the normal 200ms coalescing timer and is placed synchronously with its first DWM transition suppressed; ordinary event bursts still coalesce. Maximize and external location changes are folded back into the active layout, title-bar moves swap tile slots, and resizing moves shared boundaries.
- **Coherent native shell bar** (`src/panel.rs`, `src/widgets.rs`): a rounded per-display bar with hover feedback, a live/clickable layout capsule, real application icons, active/minimized window states, overflow counts, compact system status, and a two-line clock.
- **Constraint-aware window policy** (`src/manager.rs`, `src/rules.rs`): explicit floating rules win; owned/modal/fixed-size utilities float automatically; windows whose minimum tracking size cannot fit their proposed tile are floated and clamped inside the usable monitor area. `floating=false` forces a matching window back into tiling.
- **Focused window borders**: DWM border colors update on foreground changes without retiling. Configure `theme.border_active` and `theme.border_inactive` independently.
- **Searchable command center** (`src/command_center.rs`): open it from the AltDWM button or `Alt+Shift+Space`; type to filter, use arrows to select, view every successfully registered shortcut, and launch apps, edit/reload configuration, pause tiling, or change layouts without memorizing commands.
- **Declarative panels DSL** (`src/config.rs:16`, `docs/EXTENSIBILITY.md`): TOML `[[panels]]`/`[[widgets]]`/`[[rules]]`/`[[keybinds]]`. Multiple bars remain supported, while generated and example configs now start with one polished shell bar.
- **Widgets** (`src/widgets.rs` trait): `clock`, live `layout`/`workspaces` status, `window_list` (including minimized windows; click to focus/minimize/restore), `window_title`, Explorer-backed clickable `tray`, `spacer`, `launcher`, and Rhai `custom` status widgets.
- **Panels** (`src/panel.rs:47`): Each `[[panels]]` is a `WS_POPUP|WS_EX_TOPMOST` window (`AltDWM_Panel` class), flex layout for widgets, 1s + 250ms timers, click → `scripting::dispatch_action`.
- **Scripting** (`src/scripting.rs:8`): Embedded, resource-limited `rhai` 1.26 engine. Exposes `launch(cmd)`, `log(msg)`, live `get_cpu_usage()`/`get_mem_usage()`, `window_count()`, `focused_title()`, `retile()`, `set_layout(name)`, focus and monitor movement. Scripts are trusted local configuration—not a security boundary—because command execution is intentionally exposed.
- **Config** (`src/config.rs:109`): Search `exe_dir/config.toml` → `%APPDATA%/AltDWM/config.toml` → `./config.toml`. `general`+`ignore` + `panels/widgets/rules/keybinds/layouts` with `flatten` extras for easy extend. `--config`, `--generate-config`, `--check-config`, `Alt+Shift+C` hot-reload (configurable, `Win+Shift` collides with system e.g. `Win+Shift+S` = Snipping Tool). `validate()` warns on bad panels.
- **Native taskbar ownership**: `general.hide_native_taskbar=true` hides Explorer taskbars on every monitor, re-hides them if Explorer recreates them, removes their stale work-area reservation, and restores them when AltDWM exits.
- **Hotkeys** (`src/main.rs`): Dynamic `RegisterHotKey` from `[[keybinds]]` (default `Alt+Shift+Space` command center; `R` retile, `T` toggle, `Q` quit, `G/M/F/S` layouts, `C` reload, `J/K` focus, `Y` floating, `N/P` move monitor).

## Quick start

```powershell
$env:PATH += ";$HOME\.cargo\bin"
cargo run -- --help
cargo run                                           # default taskbar 40px
cargo run -- --no-taskbar --gap 12 --layout grid
cargo run -- --config ./examples/config.example.toml  # panels DSL demo
cargo run -- --generate-config   # writes %APPDATA%/AltDWM/config.toml
cargo run -- --check-config      # validate

# command center: Alt+Shift+Space; other hotkeys are configurable in [[keybinds]]
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
.\uninstall.ps1; .\uninstall.ps1 -PerUser  # restore the previously configured shell
```

## Extensibility DSL

See `docs/EXTENSIBILITY.md` + `examples/config.example.toml` + `scripts/*.rhai`.

```toml
[[panels]]
name = "shell"
position = "bottom"
height = 58
monitor = "all"
margin = [0, 8, 8, 8]
widgets = ["launcher", "layout", "window_list", "cpu", "tray", "clock"]

[[widgets]]
type = "launcher"
name = "launcher"
label = "AltDWM"
width = 120

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

Panels reserve their own monitor's `top`+`bottom` heights before tiling. `monitor="all"` creates one bar per display; `primary`, indices, and device-name substrings select one display. Widgets implement `trait Widget` (`src/widgets.rs:21`) — add a Rust widget through `create_widget` or a Rhai text widget.

## Project layout

```
Cargo.toml  # windows 0.62 + serde/toml/dirs + rhai + regex + notify
src/main.rs      # host window, WinEventHook, message loop, config hot-reload, hotkeys (public statics for crate::)
src/command_center.rs # searchable keyboard/mouse command surface
src/config.rs    # Config{general,ignore,panels,widgets,rules,keybinds,layouts} + find/load/validate
src/manager.rs   # collect_windows, tile_windows_reserved(top,bottom), per-monitor DeferWindowPos
src/layout.rs    # MasterStack/Grid/Monocle/Floating + compute_layout
src/taskbar.rs   # legacy AltDWM_Taskbar (fallback when no panels)
src/panel.rs     # AltDWM_Panel — declarative panels from config
src/widgets.rs   # Widget trait + clock/layout/tasklist/tray/launcher/custom
src/shell.rs     # reversible Explorer taskbar ownership
src/tray.rs      # Explorer notification-area discovery/invocation bridge
src/scripting.rs # rhai engine + dispatch_action
src/util.rs      # is_cloaked, is_manageable (+ config ignore)
docs/EXTENSIBILITY.md
examples/config.example.toml
scripts/cpu.rhai, spiral.rhai
```

## Key APIs proven (real log, 0.3.0 shell)

```
[config] loaded C:\...\config.full-demo.toml
[panel] 'shell' @ 8,1014 1904x58 monitor=all widgets=launcher,layout,window_list,cpu,tray,clock
[hotkey] Alt+Shift+Space -> 'command_center'
[main] message loop — Alt+Shift+Q quit, Alt+Shift+C reload
```

Fallback (no panels) → `[taskbar] 1920x40 @ 0,1040` + `"(0,0 1920x992)"`.

## Why DWM is not replaceable

- `dwm.exe`+`dwmcore.dll`/`win32k.sys` own `DirectComposition` swapchains since Vista. No `RegisterCompositor` — would need kernel driver + `WDDM` reimpl, breaks PatchGuard. All WMs (`GlazeWM`, `Komorebi`, `Cairo`) keep `dwm.exe` and call `Dwm*`/`SetWindowPos`. For Mica/rounded corners/`IVirtualDesktopManagerInternal`, inject via `MinHook`/`Detours` (`Windhawk`) — per-build offsets.

## Next steps

- `AltDHook.dll` (`WH_CBT`) for literal pre-show `HCBT_CREATEWND` placement and elevated windows
- `IVirtualDesktopManager` + `IVirtualDesktopManagerInternal` workspaces
- Explorer-free shell replacement and native notification-area hosting (design notes in `docs/EXTENSIBILITY.md`; the current tray bridges Explorer through UI Automation)
- `uiAccess` manifest + signing
- richer settings surfaces and a native notification-area host

## Build

```powershell
cargo check
cargo build
cargo build --release  # lto + opt-level z
```

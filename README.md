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
- **Real notification area** (`src/tray.rs`): AltDWM registers the `Shell_TrayWnd` class and receives `Shell_NotifyIcon` traffic itself, so the tray shows applications' own `HICON`s and tooltips rather than a mirror of Explorer's buttons. Add/modify/delete/setversion are honoured by `(hWnd, uID)` or `guidItem`; clicks are posted back as the callback the application registered, in the `NOTIFYICON_VERSION_4` shape where it asked for one; left, right, and double click all reach it. `TaskbarCreated` is broadcast once the native taskbar is out of the way and again on exit, so icons come to AltDWM at startup and return to Explorer on shutdown. `NIS_HIDDEN` icons and any that do not fit go behind an overflow button. `general.tray` selects `auto` (the default), `native`, `explorer`, or `off`; `--list-tray` hosts the tray for three seconds and prints what arrives.
- **Constraint-aware window policy** (`src/manager.rs`, `src/rules.rs`): explicit floating rules win; owned/modal/fixed-size utilities float automatically; windows whose minimum tracking size cannot fit their proposed tile are floated and clamped inside the usable monitor area. `floating=false` forces a matching window back into tiling.
- **Focused window borders**: DWM border colors update on foreground changes without retiling. Configure `theme.border_active` and `theme.border_inactive` independently.
- **Searchable command center** (`src/command_center.rs`): open it from the AltDWM button or `Alt+Shift+Space`; type to filter across both AltDWM's own commands and every installed application, use arrows to select, view every successfully registered shortcut, and launch apps, open quick settings, edit/reload configuration, pause tiling, or change layouts without memorizing commands.
- **Declarative panels DSL** (`src/config.rs:16`, `docs/EXTENSIBILITY.md`): TOML `[[panels]]`/`[[widgets]]`/`[[rules]]`/`[[keybinds]]`. Multiple bars remain supported, while generated and example configs now start with one polished shell bar.
- **Workspaces** (`src/workspace.rs`): `general.workspaces` gives each monitor its own set, switched with `workspace(n)` or the `workspaces` widget's pills, with `send_to_workspace(n)` to move a window. Opt-in (`1` by default) because switching hides windows. Restored on every in-process exit path, and journalled to disk so a hard kill is recoverable with `--restore-windows`.
- **Layout verbs** (`src/focus.rs`): geometric `focus_direction`, `move_window` to swap the focused window with its neighbour, `promote` to claim the master slot, and an adjustable `general.master_ratio`.
- **Widgets** (`src/widgets.rs` trait): `clock`, live `layout` status, real `workspaces` pills, `window_list` (including minimized windows; click to focus/minimize/restore), `window_title`, a real `tray` hosting `Shell_NotifyIcon` (application icons, left/right/double click, overflow flyout), `volume` (scroll to change), `battery`, `network`, `input` (click to cycle keyboard layout), `spacer`, `launcher`, and Rhai `custom` status widgets.
- **System status and quick settings** (`src/system.rs`, `src/quick_settings.rs`): volume through Core Audio, battery through `GetSystemPowerStatus`, connection and Wi-Fi radio through the WLAN API and `INetworkListManager`, brightness through DDC/CI, keyboard layout through `GetKeyboardLayoutList`. All polled on a worker thread and published as a snapshot, so no status call ever runs in a paint handler. The flyout offers volume and brightness sliders, mute and Wi-Fi toggles, and a layout switcher; what AltDWM cannot own end to end opens the matching `ms-settings:` page. `--status` prints exactly what the readers see.
- **Application search** (`src/apps.rs`): `shell:AppsFolder` is indexed on a worker thread at startup, covering desktop and Store apps together. Search is tiered — exact, prefix, word start, initials (`vsc` → *Visual Studio Code*), substring, then a word-anchored fuzzy pass — and launching goes through `shell:AppsFolder\<AppUserModelID>`. `--list-apps [query]` shows the index and its ranking.
- **Panels** (`src/panel.rs:47`): Each `[[panels]]` is a `WS_POPUP|WS_EX_TOPMOST` window (`AltDWM_Panel` class), flex layout for widgets, 1s + 250ms timers, click → `scripting::dispatch_action`.
- **Scripting** (`src/scripting.rs:8`): Embedded, resource-limited `rhai` 1.26 engine. Exposes `launch(cmd)`, `log(msg)`, live `get_cpu_usage()`/`get_mem_usage()`, `window_count()`, `focused_title()`, `retile()`, `set_layout(name)`, focus and monitor movement, plus `quick_settings()`, `cycle_input()`, `get_volume()`/`set_volume()`/`adjust_volume()`/`toggle_mute()`, `get_brightness()`/`set_brightness()`, `get_battery()`, and `network_name()`. An action shaped like a call is evaluated as a script; a bare word is a named verb; anything else is launched as a program. Scripts are trusted local configuration—not a security boundary—because command execution is intentionally exposed.
- **Config** (`src/config.rs:109`): Search `exe_dir/config.toml` → `%APPDATA%/AltDWM/config.toml` → `./config.toml`. `general`+`ignore` + `panels/widgets/rules/keybinds/layouts` with `flatten` extras for easy extend. `--config`, `--generate-config`, `--check-config`, `Alt+Shift+C` hot-reload (configurable, `Win+Shift` collides with system e.g. `Win+Shift+S` = Snipping Tool). `validate()` warns on bad panels.
- **Native taskbar ownership**: `general.hide_native_taskbar=true` hides Explorer taskbars on every monitor, re-hides them if Explorer recreates them, removes their stale work-area reservation, and restores them when AltDWM exits.
- **Hotkeys** (`src/main.rs`): Dynamic `RegisterHotKey` from `[[keybinds]]` (default `Alt+Shift+Space` command center; `R` retile, `T` toggle, `Q` quit, `G/M/F/S` layouts, `C` reload, `J/K` focus, `Y` floating, `N/P` move monitor).

## Quick start

```powershell
$env:PATH += ";$HOME\.cargo\bin"
cargo run -- --help
cargo run                                           # synthesises a 40px bottom bar
cargo run -- --no-taskbar --gap 12 --layout grid     # tiling only, no bar
cargo run -- --config ./examples/config.example.toml  # panels DSL demo
cargo run -- --generate-config   # writes %APPDATA%/AltDWM/config.toml
cargo run -- --check-config      # validate
cargo run -- --status            # live audio/power/network/input readings
cargo run -- --list-tray         # host the notification area briefly, print the icons
cargo run -- --list-apps code    # application index and search ranking
cargo run -- --restore-windows   # un-hide anything a killed run left on a workspace

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
match_class = "*Spotify*"   # exact by default; `*` opts into partial matching
floating = true

[[keybinds]]
keys = "Alt+Shift+Return"
action = "launch('wt.exe')"
```

Panels reserve their own monitor's `top`+`bottom` heights before tiling. `monitor="all"` creates one bar per display; `primary`, indices, and device-name substrings select one display. Widgets implement `trait Widget` (`src/widgets.rs`) — add a Rust widget through `create_widget` or a Rhai text widget. Draw through `src/ui.rs` so text is measured and every length scales with the display's DPI.

## Project layout

```
Cargo.toml  # windows 0.62 + serde/toml/dirs + rhai + regex + notify
src/main.rs      # host window, WinEventHook, message loop, config hot-reload, hotkeys (public statics for crate::)
src/command_center.rs # searchable keyboard/mouse command surface
src/config.rs    # Config{general,ignore,panels,widgets,rules,keybinds,layouts} + find/load/validate
src/manager.rs   # collect_windows, tile_windows_reserved(top,bottom), fit-aware slot assignment
src/layout.rs    # MasterStack/Grid/Monocle/Floating + compute_layout
src/panel.rs     # AltDWM_Panel — declarative panels from config, per-monitor DPI
src/ui.rs        # shared drawing: DPI scale, measured text, tokens
src/system.rs    # audio/power/network/brightness poller + commands
src/quick_settings.rs # volume/brightness/network/input control flyout
src/input.rs     # keyboard layout reporting and switching
src/apps.rs      # shell:AppsFolder index, ranked search, launch
src/workspace.rs # per-monitor workspaces, hide/show, journalled recovery
src/widgets.rs   # Widget trait + clock/layout/tasklist/tray/launcher/custom
src/shell.rs     # reversible Explorer taskbar ownership
src/tray.rs      # Notification-area host (Shell_NotifyIcon) + Explorer UIA fallback
src/tray_overflow.rs # Overflow flyout for hidden and clipped tray icons
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

`general.taskbar = true` with no `[[panels]]` synthesises a bottom bar and runs
it through the same panel pipeline, so it gets per-monitor placement, DPI
scaling, and the full widget set. `general.taskbar_height` sets its height.

All panel geometry — `height`, `margin`, `theme.font_size`, `theme.rounding` —
is in device-independent pixels at 96 DPI and scaled per display, so a 40px bar
occupies 60 physical pixels at 150% and reserves 60 pixels from the tiling area.
Panels re-place themselves on `WM_DPICHANGED` and `WM_DISPLAYCHANGE`.

## Why DWM is not replaceable

- `dwm.exe`+`dwmcore.dll`/`win32k.sys` own `DirectComposition` swapchains since Vista. No `RegisterCompositor` — would need kernel driver + `WDDM` reimpl, breaks PatchGuard. All WMs (`GlazeWM`, `Komorebi`, `Cairo`) keep `dwm.exe` and call `Dwm*`/`SetWindowPos`. For Mica/rounded corners/`IVirtualDesktopManagerInternal`, inject via `MinHook`/`Detours` (`Windhawk`) — per-build offsets.

## Next steps

- `AltDHook.dll` (`WH_CBT`) for literal pre-show `HCBT_CREATEWND` placement and elevated windows
- `IVirtualDesktopManager` + `IVirtualDesktopManagerInternal` workspaces
- Explorer-free shell replacement (design notes in `docs/EXTENSIBILITY.md`). The notification area is now hosted natively; what remains is running without `explorer.exe` at all.
- `uiAccess` manifest + signing
- richer settings surfaces and a native notification-area host

## Build

```powershell
cargo check
cargo build
cargo build --release  # lto + opt-level z
```

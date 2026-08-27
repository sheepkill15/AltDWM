# AltDWM Extensibility — Custom Language for Widgets/Panels

Goal: let users configure **and change anything** without forking Rust code — like `awesomeWM (Lua)`, `Qtile (Python)`, `Hyprland (hyprlang)` or `Eww (yuck)`.

## Design principles

1. **No recompilation for most changes** — edit `config.toml` + optional `*.rhai` scripts and hit `Alt+Shift+C` (default, `Win+Shift` collides with Snipping Tool) to hot-reload.
2. **Progressive disclosure** — simple TOML for 80% of users, full scripting for power users. Same file format powers both.
3. **Stable Rust core, pluggable edges** — `Panel`, `Widget`, `Layout`, `Rule`, `Keybind` form the extension boundary. New widget types are added in Rust through `create_widget`; Rhai covers text widgets, layouts, and actions.
4. **Fail-safe** — startup can use defaults when no valid config exists; a bad hot reload is rejected and the last-known-good configuration remains active.

`general.instant_first_layout = true` is the default. CREATE/SHOW/FOREGROUND
events for an untracked window trigger a synchronous layout and briefly suppress
DWM transitions, so applications appear in their tile instead of animating from
their own startup rectangle. Set it to `false` to restore timer-coalesced startup
placement. A genuinely pre-creation intercept remains a separate injected
`WH_CBT` hook feature because out-of-process WinEvent hooks run after HWND creation.

## Stack choice — Why TOML + Rhai (not Lua/Python)?

| Option           | Pros                                                                                | Cons                                                    |
|------------------|-------------------------------------------------------------------------------------|---------------------------------------------------------|
| **Lua (`mlua`)** | Familiar to awesomeWM users, large ecosystem                                        | Requires Lua DLL, `unsafe` FFI, non-Rust error handling |
| **Python**       | Familiar                                                                            | Heavy runtime, distribution hell on Windows             |
| **Rhai**         | Pure Rust, resource-limited, `serde` friendly, no DLL, `cargo` only, JS-like syntax | Less known, smaller ecosystem                           |

**Decision: `TOML` declarative layer + optional `Rhai` scripting.**  
- TOML is already used for `cargo` and Windows users expect it. Strict schema, good error messages.
- Rhai snippets live inside TOML strings (`on_click = "rhai: launch('notepad.exe')"`) or external `scripts/*.rhai`. No extra runtime — compiled into `alt-dwm.exe`.

Users who want full Rust can still use the plugin API (compile-time).

## What you can customize (v0.3 DSL)

### 0) Units

Every length in configuration — panel `height` and `margin`, `general.gap`,
`outer_gap`, `theme.font_size`, `theme.rounding`, widget `width` — is a
device-independent pixel at 96 DPI. AltDWM scales them for each display's DPI, so
one configuration looks the same on a 100% and a 200% monitor, and the tiling
area reserves the panel's *physical* size. Panels re-place themselves on
`WM_DPICHANGED` and `WM_DISPLAYCHANGE`.

### 1) Panels = taskbars / bars / docks

```toml
[[panels]]
name = "shell"
position = "bottom"      # top | bottom | left | right
height = 58              # or width for vertical bars
monitor = "all"          # all | primary | 1 | 2 | "Dell U2720Q"
margin = [0,8,8,8]       # top,right,bottom,left
widgets = ["launcher", "layout", "window_list", "tray", "clock"]
# each panel is a Win32 WS_POPUP | WS_EX_TOPMOST window, drawn via GDI/Direct2D
```

Multiple panels allowed — e.g. top bar + side dock.

### 2) Widgets — composable units inside panels

Built-ins (v0.3):

| widget         | description                                                                   | key config                   |
|----------------|-------------------------------------------------------------------------------|------------------------------|
| `clock`        | `Format: 09:41`                                                               | `format`, `interval`         |
| `layout` / `workspaces` | Current layout and managed count; click to cycle layouts              | `width`                      |
| `window_list`  | Clickable list of current desktop's tiled windows                             | `width`                      |
| `window_title` | Active window title                                                           | `max_len`                    |
| `tray`         | Clickable Explorer notification-area bridge; requires Explorer until a native shell receiver is implemented | `width`                      |
| `spacer`       | Flexible gap                                                                  | `width` (`0` means flexible) |
| `launcher`     | Opens the searchable AltDWM command center; an explicit action overrides it   | `label`, `icon`, `action`    |
| `volume` / `audio` | Output level and mute state; scroll to change, click for quick settings   | `width`, `interval`          |
| `battery` / `power` | Charge, charging state, and estimated time left                         | `width`, `interval`          |
| `network` / `wifi` | Connection name and signal, or `Offline`                                 | `width`, `interval`          |
| `input` / `keyboard` / `language` | Active keyboard layout; click or scroll to cycle           | `width`, `interval`          |
| `custom`       | Rhai-drawn widget                                                             | `script`, `interval`         |

System-status widgets read a snapshot published by a background poller
(`src/system.rs`), never by calling Core Audio, WLAN, or DDC/CI from the paint
handler. A capability the machine does not expose reports itself as unavailable
rather than showing a dead control: laptop panels that answer brightness only
through WMI, for example, are reported as such.

### 2a) Quick settings

`src/quick_settings.rs` is the shell's control flyout: volume and brightness
sliders, mute and Wi-Fi radio toggles, network and battery status, and a
keyboard-layout switcher. Open it with the `quick_settings` action, from any of
the status widgets, or from the command center.

Sliders respond to click, drag, and the scroll wheel. Rows that AltDWM cannot
own end to end — choosing a Wi-Fi network, pairing Bluetooth — open the matching
`ms-settings:` page instead of offering a partial implementation.

Widget registry is extensible:

```rust
// src/widgets.rs
pub trait Widget: Send + Sync {
  fn name(&self) -> &str;                          // instance name from config
  fn kind(&self) -> &'static str;                  // widget type
  fn width(&self, ctx: &PanelCtx) -> i32;          // 0 = flex, else 96-DPI pixels
  fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx);
  // `point` is in client coordinates and `rect` is the widget's own rectangle,
  // so hit-testing can reuse exactly the geometry `draw` produced.
  fn on_click(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String>;
  fn hover_paint(&self) -> HoverPaint;             // None | Whole | SelfDrawn
  fn interval_ms(&self) -> Option<u32>;
  fn tick(&self);                                  // refresh state, never in WM_PAINT
}
```

Widgets are stored behind `Arc`, and the panel clones the hit widget out and
releases its lock before calling `on_click` or `on_scroll`. Anything that shows a
window or sets the foreground window pumps messages, which can re-enter
`WM_PAINT` on the same thread — so a handler that ran under the lock could
deadlock the shell. Prefer returning an action string and letting
`dispatch_action` run it.

Adding a widget = implement the trait and add its branch in `create_widget`.
Dynamic DLL plugins are not implemented.

Draw through `crate::ui` rather than positioning text yourself: `ctx.px(n)`
scales a design constant for the panel's display, `ui::draw_label` centres one
line in a rectangle and ellipsises it to fit, and `ui::measure_label` gives a
real text width. Widgets that do expensive work (scripts, file reads, IPC) must
do it in `tick`, which runs on the panel timer — doing it in `draw` stalls the
shell's message loop for as long as the work takes.

### 3) Rules — auto-manage windows (like bspwm `bspc rule` / Hyprland `windowrule`)

```toml
[[rules]]
match_class = "Chrome_WidgetWin_1"   # exact, case-insensitive
match_title = "YouTube"              # substring, case-insensitive
monitor = 2
floating = true
opacity = 0.9
layout = "Grid"  # layout for this monitor while this window holds the master slot
on_create = "rhai: focus_next()"
```

**Matching semantics**

| Key | Default | Wildcards |
| --- | --- | --- |
| `match_class` | exact, case-insensitive | `*` and `?`, e.g. `"*Chrome*"` |
| `match_process` | exact, case-insensitive | `*` and `?`, e.g. `"steam*"` |
| `match_title` | substring, case-insensitive | `*` and `?` |
| `match_class_regex`, `match_title_regex` | full regular expression | — |

Every condition present on a rule must match. A rule with no conditions matches
nothing, and `--check-config` reports it.

`match_class` and `match_process` used to fall back to a substring test, so a
rule written for one application silently captured every window whose class or
executable merely contained the text — which looked like windows randomly
refusing to tile. Add `*` where you want the old behaviour.

A `layout` rule applies to a monitor when the rule matches that monitor's
**master** window, so "when this application is the master, use this layout" is
expressible. It previously applied if *any* window on the display matched, which
made layouts flip for reasons that were hard to trace.

Floating decisions use this order:

1. `Alt+Shift+Y` manual floating state.
2. The first matching rule with `floating = true` or `floating = false`.
3. Automatic utility detection for owned, modal, fixed-size, and compact non-resizable windows.
4. Minimum-size validation against the proposed tile; a window that cannot fit is floated.

Automatic floating windows retain their size where possible and are clamped into
the monitor work area. Set `general.auto_float_utility_windows = false` or
`general.respect_window_size_constraints = false` to disable either heuristic.
Use an explicit `floating = false` rule to force a particular application into
the layout. Steam Friends is included in the example configuration as an
explicit process/title rule because it is an independent resizable window and
cannot be reliably distinguished from Steam's main window by styles alone.

Window chrome colors are independent theme tokens:

```toml
[theme]
border_active = "#8b5cf6"
border_inactive = "#343842"
```

### 4) Layouts — pluggable tiling algorithms

Built-ins: `MasterStack`, `Grid`, `Monocle`, `Floating`. Custom:

```toml
[layouts.my_spiral]
script = "scripts/spiral.rhai"   # fn layout(n, left, top, right, bottom, gap) -> [rects]
gap = 8                           # optional layout-specific gap
```

Rhai `layout` receives window count, work-area bounds, and gap; it returns maps or arrays of `[left, top, right, bottom]`.

### 5) Keybinds & Actions — fully data-driven

```toml
[[keybinds]]
keys = "Alt+Shift+R"
action = "retile"

[[keybinds]]
keys = "Alt+Shift+1"
action = "focus_next()"
```

Actions: `retile`, `toggle_tiling`, `set_layout("grid")`, `launch("wt.exe")`, `focus_next()`, `toggle_floating()`, `move_to_next_monitor()`, `rhai: <code>`.

### 6) Scripting — Rhai callbacks

Any `on_*` field can be `rhai: <expr>`:

```toml
[[widgets]]
type = "custom"
name = "cpu"
interval = 1000
script = "scripts/cpu.rhai"      # or inline: on_update = "rhai: get_cpu()"
```
```rhai
# scripts/cpu.rhai
let cpu = get_cpu_usage();
`CPU ${cpu}%`;
```

Exposed API (via `rhai::Engine` in `src/scripting.rs`):

```
launch(cmd)           // CreateProcess
shell(cmd)            // cmd.exe /C; trusted local scripts only
get_cpu_usage() -> int
get_mem_usage() -> int / get_mem() -> map
focused_title() -> string
window_count() / tilable_count() -> int
retile() / set_layout(str)
focus_next() / focus_prev() / move_to_next_monitor()
log(msg)
```

Resource-limited, but **not a security sandbox**: scripts come from the trusted local configuration and the exposed `launch()`/`shell()` functions can execute commands with the user's permissions. Script operation, call, string, array, and map limits protect the shell UI from accidental runaway code.

## File layout

```
config.toml                 # main DSL (checked: exe_dir -> %APPDATA%/AltDWM/ -> ./ )
scripts/
  spiral.rhai               # custom layout
  cpu.rhai                  # custom widget
plugins/
  my_widget.dll             # optional Rust cdylib (libloading)
```

`alt-dwm --generate-config` writes commented `config.example.toml` with all options.

## Hot reload & safety

- `Alt+Shift+C` (default) or the file watcher on `config.toml` loads and validates the changed file before recreating panels.
- Parse/read errors are logged and the last-known-good runtime configuration remains active.
- `alt-dwm --check-config` validates without applying and exits nonzero for missing, malformed, or semantically invalid configuration.

## Example full config

See `examples/config.example.toml` (generated by `cargo run -- --generate-config`).

```toml
[general]
gap = 8
outer_gap = 0        # additional inset on every monitor edge
layout = "MasterStack"
taskbar = true  # with no [[panels]], synthesises a bottom bar
taskbar_height = 40  # height of that synthesised bar, in 96-DPI pixels

[[panels]]
name = "bottom"
position = "bottom"
height = 40
widgets = ["workspaces", "window_title", "tray", "clock"]

[[widgets]]
type = "clock"
name = "clock"
format = "%H:%M:%S"
interval = 1000

[[panels]]
name = "top"
position = "top"
height = 28
monitor = "primary"
widgets = ["launcher", "spacer", "cpu"]

[[widgets]]
type = "custom"
name = "cpu"
script = "scripts/cpu.rhai"
interval = 2000

[[rules]]
match_class = "*Spotify*"
floating = true

[[keybinds]]
keys = "Alt+Shift+Return"
action = "launch('wt.exe')"
```

## Actions

Keybinds, widget clicks, and `on_create` rules all resolve through
`scripting::dispatch_action`. An action written as a call — `set_layout("Grid")`,
`adjust_volume(-5)` — is evaluated by the Rhai engine; a bare word is a named
shell verb; anything else is launched as a program.

Named verbs: `retile`, `toggle_tiling`, `quit`, `reload_config`,
`command_center`, `quick_settings`, `toggle_floating`, `move_to_next_monitor`,
`move_to_prev_monitor`, `cycle_input`, `volume_up`, `volume_down`,
`toggle_mute`, `brightness_up`, `brightness_down`, `rescan_apps`.

Rhai bindings, in addition to the existing `launch`/`log`/`retile`/`set_layout`
and focus functions: `quick_settings()`, `cycle_input()`, `input_layout()`,
`get_volume()`, `set_volume(percent)`, `adjust_volume(delta)`, `toggle_mute()`,
`is_muted()`, `get_brightness()`, `set_brightness(percent)`, `get_battery()`,
`network_name()`.

## Workspaces

`general.workspaces` sets how many workspaces each monitor has. It defaults to
`1`, which leaves the feature inert — switching away from a workspace hides
windows, and a window the user cannot find is this program's worst failure mode,
so nobody reaches that state without asking for it.

Each monitor has its own active workspace, so switching on one display leaves the
other alone. A new window joins the active workspace of the display it appeared
on. Switching applies visibility synchronously and then focuses a window on the
revealed workspace, so the keyboard follows the switch.

Windows' own virtual desktops are only usefully *switchable* through
`IVirtualDesktopManagerInternal`, which is undocumented and changes shape between
builds; `general.filter_virtual_desktop` still uses the documented read-only half
of that API. Workspaces here are AltDWM's own, implemented by hiding windows.

**Recovery.** Hiding someone else's windows is the most destructive thing AltDWM
does, so three rules hold. Only windows AltDWM hid are ever shown again. Every
in-process exit path restores them — normal shutdown, a Rust panic, an unhandled
exception, Ctrl+C. And because none of those can catch `TerminateProcess` (Task
Manager, `Stop-Process`, a hard kill), the hidden set is journalled to
`%LOCALAPPDATA%\AltDWM\hidden-windows` the moment it changes. The next start
replays it, and:

```powershell
alt-dwm --restore-windows    # un-hide everything, without starting the shell
```

The journal records each window's owning process alongside its handle, so a
recycled handle can never cause an unrelated window to be shown.

## Layout verbs

| action | effect |
| --- | --- |
| `focus_direction("left"\|"right"\|"up"\|"down")` | Move focus geometrically |
| `move_window("left"\|…)` | Swap the focused window with its neighbour that way |
| `promote` | Make the focused window the master |
| `wider_master` / `narrower_master` | Adjust `general.master_ratio` by 5% |
| `set_master_ratio(percent)`, `adjust_master_ratio(delta)` | Same, from Rhai |
| `workspace(n)` | Show workspace *n* on the focused monitor |
| `move_to_workspace(n)` / `send_to_workspace(n)` | Send the focused window there, staying or following |
| `next_workspace` / `prev_workspace` | Step through workspaces |

Directional focus is geometric: a candidate qualifies only if it is on that side
*and* its extent across the axis of travel overlaps the origin's. The overlap
requirement is what stops Up from a full-height master window jumping sideways
into the stack. When nothing qualifies, the key falls back to list order, so it
is never inert.

`general.master_ratio` (default `0.6`) is the master column's share of the usable
width. It was previously hardcoded.

## Application search

`src/apps.rs` indexes `shell:AppsFolder` on a worker thread at startup. That is
the folder behind the Start menu's "All apps" list, and the only place that
reports desktop programs and Store apps together with the display names people
recognise. Documentation shortcuts — manuals, changelogs, quick-start guides —
are filtered out.

Ranking is tiered so the obvious answer wins: exact name, then prefix, then word
start, then initials (`vsc` finds *Visual Studio Code*), then substring, and
finally a word-anchored fuzzy match. Launching is a single `ShellExecuteW` on
`shell:AppsFolder\<AppUserModelID>`, which works identically for both kinds of
app.

Two diagnostics exist because "no results" and "wrong results" are different
bugs:

```powershell
alt-dwm --list-apps          # everything indexed
alt-dwm --list-apps code     # what matches, best first
alt-dwm --status             # what the system readers actually see
```

## Roadmap (commits)

1. **Done**: `config.toml` (`general`+`ignore`) + path discovery.
2. **Done**: `[[panels]]`/`[[widgets]]`/`[[rules]]`/`[[keybinds]]`, widget trait scaffold, panel manager, TOML widgets.
3. **Done**: Rhai engine + `scripts/*.rhai` for custom widgets/layouts/callbacks.
4. **Later**: real notification-area hosting, virtual-desktop workspace switching, elevated-window hook DLL, optional plugin ABI.

## Deferred: Explorer-free shell and notification area

The current implementation makes Explorer's taskbar fully transparent and uses
UI Automation to bridge its live notification-area items into AltDWM. This is a
compatibility stage: Explorer remains alive as the tray backend even though no
native taskbar pixels are visible and its work-area reservation is ignored.

A complete shell-replacement mode should instead:

1. Start AltDWM as the user's shell without starting `explorer.exe`.
2. Host a notification-area compatibility endpoint for applications calling
   `Shell_NotifyIcon`, including add/modify/delete/version operations, icon and
   tooltip ownership, mouse/keyboard callback messages, balloon notifications,
   DPI changes, overflow state, and the `TaskbarCreated` recovery broadcast.
3. Implement clock, audio, network, battery, input, quick settings, and
   notification access as AltDWM widgets backed by their Windows APIs rather
   than Explorer UI Automation.
4. Provide startup failure recovery and an explicit command that restores the
   normal Explorer shell before enabling this mode persistently.
5. Decide how folder browsing works: starting `explorer.exe` as a file manager
   can also recreate shell UI, so Explorer process lifetime must be controlled
   or a separate file manager must be used.

Windows documents the application-facing `Shell_NotifyIcon` contract but not a
supported third-party implementation contract for the receiving tray host.
Generic tray compatibility will therefore require a carefully isolated,
version-tested compatibility layer. This work is deliberately deferred; the
transparent Explorer bridge remains the default until that layer has recovery
and compatibility coverage.

This keeps easy tasks easy (edit TOML) and hard tasks possible (Rhai/Rust) — same model as Linux WMs but native to Windows.

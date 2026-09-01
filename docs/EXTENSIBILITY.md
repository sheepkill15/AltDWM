# AltDWM Extensibility — Custom Language for Widgets/Panels

Goal: let users configure **and change anything** without forking Rust code — like `awesomeWM (Lua)`, `Qtile (Python)`, `Hyprland (hyprlang)` or `Eww (yuck)`.

## Design principles

1. **No recompilation for most changes** — edit `config.toml` + optional `*.rhai` scripts and hit `Alt+Shift+C` (default, `Win+Shift` collides with Snipping Tool) to hot-reload.
2. **Progressive disclosure** — simple TOML for 80% of users, full scripting for power users. Same file format powers both.
3. **Stable Rust core, scripted policy** — Rust owns Win32 integration, snapshots, drawing primitives, and resource limits. Every shipped widget is a Rhai script, so appearance and behavior change without recompiling.
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
`outer_gap`, `theme.font_size`, `theme.font_weight`, `theme.strong_font_weight`, `theme.rounding`, widget `width` — is a
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
| `tray`         | Notification area. Hosts `Shell_NotifyIcon` itself (real icons, left/right/double click, overflow flyout); falls back to mirroring Explorer's buttons. Sizes itself unless `width` is set | `width`, `max_items`         |
| `spacer`       | Flexible gap                                                                  | `width` (`0` means flexible) |
| `launcher`     | Opens the searchable AltDWM command center; an explicit action overrides it   | `label`, `icon`, `action`    |
| `volume` / `audio` | Output level and mute state; scroll to change, click for quick settings   | `width`, `interval`          |
| `battery` / `power` | Charge, charging state, and estimated time left                         | `width`, `interval`          |
| `network` / `wifi` | Connection name and signal, or `Offline`                                 | `width`, `interval`          |
| `input` / `keyboard` / `language` | Active keyboard layout; click or scroll to cycle           | `width`, `interval`          |
| `custom`       | Any Rhai widget using the same full renderer as the shipped widgets           | `script`, `interval`         |

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

All built-ins resolve to `scripts/widgets/<type>.rhai`. An explicit `script`
replaces that path. AltDWM checks beside the config, the working directory, and
the executable, in that order, and falls back to an embedded copy only when a
shipped file is missing. `--generate-config` writes editable copies without
overwriting existing scripts. Changes are noticed on the next refresh; config
reload is not required.

The stable Rust host contract remains:

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

Normal widgets no longer need a Rust implementation. Add a `.rhai` file and a
`[[widgets]]` entry with `type = "custom"`. `native = true` opts a built-in back
into its legacy Rust renderer for compatibility and debugging. Dynamic DLL
plugins are not implemented.

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
3. Automatic utility detection for owned, modal, tool, and intrinsically fixed-size windows.
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
font_name = "Segoe UI"
font_size = 13
font_weight = 400
strong_font_weight = 500
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

### 6) Scripting — full Rhai widgets

Every widget script defines `render(ctx)`:

```toml
[[widgets]]
type = "custom"
name = "cpu"
interval = 1000
script = "scripts/cpu.rhai"
```

```rhai
fn render(ctx) {
    let cpu = get_cpu_usage();
    #{
        width: 120,
        interval: 1000,
        hover: "self",
        commands: [
            #{ type: "rect", x: 4, y: 5, w: ctx.width - 8,
               h: ctx.height - 10, radius: 8,
               background: "surface", hover_background: "surface_hover",
               action: "launch('taskmgr.exe')" },
            #{ type: "text", x: 12, y: 5, w: ctx.width - 24,
               h: ctx.height - 10, text: `CPU ${cpu}%`,
               font: "strong", color: "text",
               action: "launch('taskmgr.exe')" }
        ]
    }
}
```

The return value may be a command array or a map containing `width`, `interval`,
`hover`, and `commands`. `ctx` contains:

| field | value |
| --- | --- |
| `name`, `kind`, `panel`, `monitor` | Widget/panel identity |
| `width`, `height`, `vertical` | Widget rectangle in 96-DPI logical pixels |
| `config` | `format`, `label`, `icon`, `command`, `action`, plus every extra TOML key |
| `focused_title`, `layout`, `tiling` | Current shell state |
| `windows` | Maps with `id`, `title`, `icon`, `active`, `minimized`, `floating` |
| `workspaces` | Maps with `number`, `active`, `occupied` |
| `tray` | Maps with stable `id`, `name`, `icon`, `hidden`, `process` |
| `system` | `volume`, `muted`, `battery`, `charging`, `on_ac`, `network`, `network_kind`, `network_signal`, `connected`, `brightness`, `input` |

Drawing commands use logical `x`, `y`, `w`, `h`. Supported `type` values are
`rect`, `text`, and `icon`. Text supports `font = "body"|"strong"|"small"|"symbol"`,
`align = "left"|"center"|"right"`, optional `font_size`/`font_weight`, and theme color names `text`, `text_dim`,
`surface`, `surface_hover`, `accent`, `border`, and `panel`, or any literal color
accepted by the theme. Rectangles support `radius`, `background`, and
`hover_background`. Any command can declare `action`, `right_action`,
`double_action`, `scroll_up`, and `scroll_down`; hit testing uses the exact same
rectangle that was drawn.

Host actions beginning with `@` connect data snapshots back to Win32 safely:
`@window:<id>`, `@tray:<left|right|double>:<id>`, `@tray_overflow`, and
`@quick_settings`. Ordinary action strings use the same dispatcher as keybinds.

The callable API (via `rhai::Engine` in `src/scripting.rs`) includes:

```
launch(cmd)           // CreateProcess
shell(cmd)            // cmd.exe /C; trusted local scripts only
get_cpu_usage() -> int
format_time(format) -> string
truncate_text(text, max_chars) -> string
symbol(codepoint) -> string // for text commands using font = "symbol"
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
  widgets/*.rhai             # every shipped widget; open and editable
  spiral.rhai               # custom layout
  cpu.rhai                  # custom widget
plugins/
  my_widget.dll             # optional Rust cdylib (libloading)
```

`alt-dwm --generate-config` writes the example config and all editable built-in
widget scripts. Existing script files are never overwritten.

## Hot reload & safety

- `Alt+Shift+C` (default) or the file watcher on `config.toml` loads and validates the changed file before recreating panels.
- Widget script edits are polled at their own refresh interval and apply without a config reload.
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

Workspace numbers are local to each physical monitor. Panel clicks and wheel
actions target the monitor containing that panel; a switch applies visibility
and layout only there, leaving every window and panel on other displays
untouched. A window explicitly moved to another monitor joins that monitor's
currently active workspace.

Borderless or popup windows that cover an entire monitor are treated as
application-owned fullscreen surfaces. AltDWM does not resize, round, or contain
them, and hides panels only on the fullscreen application's monitor until it
leaves fullscreen or loses foreground focus.

Directional focus is geometric: a candidate qualifies only if it is on that side
*and* its extent across the axis of travel overlaps the origin's. The overlap
requirement is what stops Up from a full-height master window jumping sideways
into the stack. When nothing qualifies, the key falls back to list order, so it
is never inert.

`general.master_ratio` (default `0.6`) is the master column's share of the usable
width. It was previously hardcoded.

## Diagnostics

`--check-config` reports what the runtime will actually build, including the bar
synthesised for `taskbar = true`, and rejects out-of-range values: a negative
gap, a workspace count above 9, a `master_ratio` outside 0.1–0.9, a rule with no
conditions, and a `general.layout` that is neither a built-in nor a key in
`[layouts]` — that last one used to fall back to MasterStack in silence, which
reads as AltDWM ignoring the setting.

`--status` prints what the system readers see, `--list-apps [query]` prints the
application index and its ranking, and `--restore-windows` un-hides anything a
killed run left on a workspace.

Runtime logs warn once per distinct problem rather than on every retile. A
missing layout script or a typo'd layout name previously printed the same line
five to ten times a second for as long as the configuration stayed broken.

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
4. **Done**: notification-area hosting (`src/tray.rs`) — AltDWM answers to `Shell_TrayWnd` and decodes the `WM_COPYDATA` payload itself.
5. **Later**: running without `explorer.exe` at all, virtual-desktop workspace switching, elevated-window hook DLL, optional plugin ABI.

## Notification area

`Shell_NotifyIcon` does not go anywhere in particular. It resolves
`FindWindow("Shell_TrayWnd")` and posts the caller's `NOTIFYICONDATA` to
whatever answers, as `WM_COPYDATA` with `dwData == 1`. Owning that class is
therefore the whole mechanism, and it is the only way to obtain an application's
actual `HICON` — UI Automation can name Explorer's buttons but never draw them,
and it reports nothing at all once Explorer's taskbar is hidden, which is
AltDWM's default configuration.

`src/tray.rs` implements the receiving side:

* **Window chain.** `Shell_TrayWnd` plus the `TrayNotifyWnd` / `SysPager` /
  `ToolbarWindow32` descendants applications probe for. It is created without
  `WS_VISIBLE` and with `WS_EX_TRANSPARENT`, sized like a taskbar because
  applications read its rectangle to place balloons.
* **Z-order.** `FindWindow` walks top-level windows in Z-order, so hiding
  Explorer's taskbar is not enough — `src/shell.rs` also drops it out of the
  topmost band, and the host re-asserts `HWND_TOPMOST` on its sweep timer.
  Both are undone on shutdown.
* **Wire format.** The payload is the sender's struct in the *sender's* layout,
  so a 32-bit application on 64-bit Windows sends 4-byte handles at different
  offsets. `wire_layout` reconstructs the offsets from the sender's pointer
  width (probed with `IsWow64Process`) and the `cbSize` it declared; two of the
  four published versions collide on `cbSize` across widths, which is why the
  bitness is probed rather than inferred.
* **Identity.** `NIM_ADD` / `NIM_MODIFY` / `NIM_DELETE` / `NIM_SETVERSION`
  address an icon by `(hWnd, uID)` or by `guidItem`, and both are honoured. A
  modify for an icon never seen is treated as an add — that application
  registered with Explorer before AltDWM existed.
* **Ownership.** Icons are `CopyIcon`'d on arrival; the sender is free to
  destroy its own. Icons whose owner window is gone are swept every two seconds,
  because an application that is killed never sends `NIM_DELETE`.
* **Callbacks.** Clicks are posted back as the message the application
  registered, in the `NOTIFYICON_VERSION_4` shape when it asked for one, and the
  owner is granted `AllowSetForegroundWindow` first — a tray context menu will
  not dismiss otherwise.
* **Handshake.** `TaskbarCreated` is broadcast once the taskbar is out of the
  way, and again after the host window is destroyed, so icons arrive at startup
  and go back to Explorer on exit.

Not implemented: balloon notifications (`NIF_INFO`), and
`Shell_NotifyIconGetRect` (`dwData == 3`), whose reply packs a `RECT` into an
`LRESULT` in a way that is neither documented nor stable — callers fall back to
the cursor position, which is where AltDWM's icons are anyway.

## Deferred: Explorer-free shell

Explorer is still running. Its taskbar is transparent, demoted, and ignored for
work-area purposes, and it no longer backs the tray — but a complete
shell-replacement mode should also:

1. Start AltDWM as the user's shell without starting `explorer.exe`.
2. Provide startup failure recovery and an explicit command that restores the
   normal Explorer shell before enabling this mode persistently.
3. Decide how folder browsing works: starting `explorer.exe` as a file manager
   can also recreate shell UI, so Explorer process lifetime must be controlled
   or a separate file manager must be used.

This keeps easy tasks easy (edit TOML) and hard tasks possible (Rhai/Rust) — same model as Linux WMs but native to Windows.

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
| `custom`       | Rhai-drawn widget                                                             | `script`, `interval`         |

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

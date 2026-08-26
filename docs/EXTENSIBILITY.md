# AltDWM Extensibility — Custom Language for Widgets/Panels

Goal: let users configure **and change anything** without forking Rust code — like `awesomeWM (Lua)`, `Qtile (Python)`, `Hyprland (hyprlang)` or `Eww (yuck)`.

## Design principles

1. **No recompilation for most changes** — edit `config.toml` + optional `*.rhai` scripts and hit `Win+Shift+C` to hot-reload.
2. **Progressive disclosure** — simple TOML for 80% of users, full scripting for power users. Same file format powers both.
3. **Stable Rust core, pluggable edges** — `Panel`, `Widget`, `Layout`, `Rule`, `Keybind` are traits. New types can be added as Rust `cdylib` plugins or as Rhai scripts without touching core.
4. **Fail-safe** — bad config never crashes WM; falls back to defaults and logs. Shell replacement must not brick login.

## Stack choice — Why TOML + Rhai (not Lua/Python)?

| Option | Pros | Cons |
|---|---|---|
| **Lua (`mlua`)** | Familiar to awesomeWM users, large ecosystem | Requires Lua DLL, `unsafe` FFI, non-Rust error handling |
| **Python** | Familiar | Heavy runtime, distribution hell on Windows |
| **Rhai** | Pure Rust, sandboxed, `serde` friendly, no DLL, `cargo` only, JS-like syntax | Less known, smaller ecosystem |

**Decision: `TOML` declarative layer + optional `Rhai` scripting.**  
- TOML is already used for `cargo` and Windows users expect it. Strict schema, good error messages.
- Rhai snippets live inside TOML strings (`on_click = "rhai: launch('notepad.exe')"`) or external `scripts/*.rhai`. No extra runtime — compiled into `alt-dwm.exe`.

Users who want full Rust can still use the plugin API (compile-time).

## What you can customize (v0.2 DSL)

### 1) Panels = taskbars / bars / docks

```toml
[[panels]]
name = "bottom"
position = "bottom"      # top | bottom | left | right
height = 40              # or width for vertical bars
monitor = "all"          # all | primary | 1 | 2 | "Dell U2720Q"
margin = [0,0,0,0]       # top,right,bottom,left
widgets = ["workspaces", "spacer", "window_title", "tray", "clock"]
# each panel is a Win32 WS_POPUP | WS_EX_TOPMOST window, drawn via GDI/Direct2D
```

Multiple panels allowed — e.g. top bar + side dock.

### 2) Widgets — composable units inside panels

Built-ins (v0.2):

| widget | description | key config |
|---|---|---|
| `clock` | `Format: 09:41` | `format`, `interval` |
| `workspaces` | Virtual desktop / workspace pills | `show_icons` |
| `window_title` | Active window title | `max_len` |
| `tray` | System tray (`Shell_NotifyIcon`) | `icon_size` |
| `spacer` | Flexible gap | `size` |
| `launcher` | App grid / start button | `icon`, `command` |
| `custom` | Rhai-drawn widget | `script`, `interval` |

Widget registry is extensible:

```rust
// src/widgets.rs:14
pub trait Widget: Send {
  fn name(&self) -> &str;
  fn width(&self, ctx: &PanelCtx) -> i32;          // 0 = flex
  fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx);
  fn on_click(&self, btn: MouseButton) -> Option<Action>;
  fn interval_ms(&self) -> Option<u32>;
}
```

Adding a widget = implement trait + `inventory::submit!{ WidgetFactory }` or drop a `plugins/my_widget.dll` built against `alt_dwm_api`.

### 3) Rules — auto-manage windows (like bspwm `bspc rule` / Hyprland `windowrule`)

```toml
[[rules]]
match_class = "Discord"
match_title = ".*YouTube.*"
monitor = 2
layout = "floating"
opacity = 0.9
on_create = "rhai: move_to_workspace(2)"
```

Matcher uses class/title/process regex; action can be declarative or Rhai.

### 4) Layouts — pluggable tiling algorithms

Built-ins: `MasterStack`, `Grid`, `Monocle`, `Floating`. Custom:

```toml
[layouts.my_spiral]
script = "scripts/spiral.rhai"   # fn layout(wins, area) -> [rects]
```

Rhai `layout` receives `windows.len()` + `area` rect and returns rect array — same engine as resize.

### 5) Keybinds & Actions — fully data-driven

```toml
[[keybinds]]
keys = "Win+Shift+R"
action = "retile"

[[keybinds]]
keys = "Win+1"
action = "rhai: focus_workspace(1)"
```

Actions: `retile`, `toggle_tiling`, `set_layout("grid")`, `launch("wt.exe")`, `rhai: <code>`.

### 6) Scripting — Rhai callbacks

Any `on_*` field can be `rhai: <expr>`:

```toml
[[widgets]]
type = "custom"
name = "cpu"
interval = 1000
script = "scripts/cpu.rhai"      # or inline: on_update = "rhai: get_cpu()"

# scripts/cpu.rhai
let cpu = get_cpu_usage();
`CPU ${cpu}%`;
```

Exposed API (via `rhai::Engine` in `src/scripting.rs`):

```
launch(cmd)           // CreateProcess
get_cpu_usage() -> int
get_mem() -> map
focused_title() -> string
windows() -> array    // manageable HWNDs
retile() / set_layout(str)
move_to_workspace(n)
log(msg)
```

Sandboxed: no file write / network unless whitelisted.

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

- `Win+Shift+C` or `FileSystemWatcher` on `config.toml` → `Config::load_or_default` → diff → recreate panels without killing WM.
- Parse errors → log + `MessageBoxW` (if not headless) + keep last good config. Never leave user without a shell.
- `alt-dwm --check-config` validates without applying.

## Example full config

See `examples/config.example.toml` (generated by `cargo run -- --generate-config`).

```toml
[general]
gap = 8
layout = "MasterStack"
taskbar = true  # legacy, maps to panels[0] for compat

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
match_class = "Spotify"
floating = true

[[keybinds]]
keys = "Win+Return"
action = "launch('wt.exe')"
```

## Roadmap (commits)

1. **Done**: basic `config.toml` (`general`+`ignore`) + path discovery (`src/config.rs:109`)
2. **Next (this PR)**: extend to `[[panels]]`/`[[widgets]]`/`[[rules]]`/`[[keybinds]]`, widget trait scaffold, Panel manager, TOML-only widgets
3. **Follow-up**: `rhai` engine + `scripts/*.rhai` for custom widgets/layouts/callbacks
4. **Later**: Rust `cdylib` plugin loader + systray + virtual-desktop workspaces + `uiAccess` manifest

This keeps easy tasks easy (edit TOML) and hard tasks possible (Rhai/Rust) — same model as Linux WMs but native to Windows.

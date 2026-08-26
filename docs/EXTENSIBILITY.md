# AltDWM Extensibility — Custom Language for Widgets/Panels

Goal: let users configure **and change anything** without forking Rust code — like `awesomeWM (Lua)`, `Qtile (Python)`, `Hyprland (hyprlang)` or `Eww (yuck)`.

## Design principles

1. **No recompilation for most changes** — edit `config.toml` + optional `*.rhai` scripts and hit `Alt+Shift+C` (default, `Win+Shift` collides with Snipping Tool) to hot-reload.
2. **Progressive disclosure** — simple TOML for 80% of users, full scripting for power users. Same file format powers both.
3. **Stable Rust core, pluggable edges** — `Panel`, `Widget`, `Layout`, `Rule`, `Keybind` form the extension boundary. New widget types are added in Rust through `create_widget`; Rhai covers text widgets, layouts, and actions.
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
| `workspaces` | Current tiling layout and live tilable/floating count | `width` |
| `window_list` | Clickable list of current desktop's tiled windows | `width` |
| `window_title` | Active window title | `max_len` |
| `tray` | Visual system-tray placeholder (notification-area hosting is not implemented) | `width` |
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

Adding a widget = implement the trait and add its branch in `create_widget`. Dynamic DLL plugins are not implemented.

### 3) Rules — auto-manage windows (like bspwm `bspc rule` / Hyprland `windowrule`)

```toml
[[rules]]
match_class = "Discord"
match_title = ".*YouTube.*"
monitor = 2
floating = true
opacity = 0.9
on_create = "rhai: focus_next()"
```

Matcher uses class/title/process regex; action can be declarative or Rhai.

### 4) Layouts — pluggable tiling algorithms

Built-ins: `MasterStack`, `Grid`, `Monocle`, `Floating`. Custom:

```toml
[layouts.my_spiral]
script = "scripts/spiral.rhai"   # fn layout(n, left, top, right, bottom, gap) -> [rects]
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

# scripts/cpu.rhai
let cpu = get_cpu_usage();
`CPU ${cpu}%`;
```

Exposed API (via `rhai::Engine` in `src/scripting.rs`):

```
launch(cmd)           // CreateProcess
get_cpu_usage() -> int
get_mem_usage() -> int / get_mem() -> map
focused_title() -> string
window_count() / tilable_count() -> int
retile() / set_layout(str)
focus_next() / focus_prev() / move_to_next_monitor()
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

- `Alt+Shift+C` (default) or `FileSystemWatcher` on `config.toml` → `Config::load_or_default` → diff → recreate panels without killing WM.
- Parse errors → log + default configuration fallback. The current implementation does not show a MessageBox.
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
keys = "Alt+Shift+Return"
action = "launch('wt.exe')"
```

## Roadmap (commits)

1. **Done**: `config.toml` (`general`+`ignore`) + path discovery.
2. **Done**: `[[panels]]`/`[[widgets]]`/`[[rules]]`/`[[keybinds]]`, widget trait scaffold, panel manager, TOML widgets.
3. **Done**: Rhai engine + `scripts/*.rhai` for custom widgets/layouts/callbacks.
4. **Later**: real notification-area hosting, virtual-desktop workspace switching, elevated-window hook DLL, optional plugin ABI.

This keeps easy tasks easy (edit TOML) and hard tasks possible (Rhai/Rust) — same model as Linux WMs but native to Windows.

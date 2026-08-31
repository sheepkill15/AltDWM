//! Generic Rhai widget host.
//!
//! Rust owns only the stable shell capabilities (snapshots, GDI rendering and
//! Win32 actions). Widget policy and appearance live in `scripts/widgets/*.rhai`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use rhai::{Array, Dynamic, Map, Scope, AST};
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, HDC, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetForegroundWindow, IsIconic, DI_NORMAL, HICON,
};

use crate::config::WidgetConfig;
use crate::tray::{Button, TrayEntry, TrayId};
use crate::ui::{
    draw_label, fill_rect, fill_round_rect, measure_label, point_in_rect, rect_height, rect_width,
};
use crate::widgets::{HoverPaint, PanelCtx, Widget};

const EMBEDDED: &[(&str, &str)] = &[
    ("spacer", include_str!("../scripts/widgets/spacer.rhai")),
    ("clock", include_str!("../scripts/widgets/clock.rhai")),
    (
        "window_title",
        include_str!("../scripts/widgets/window_title.rhai"),
    ),
    ("layout", include_str!("../scripts/widgets/layout.rhai")),
    (
        "workspaces",
        include_str!("../scripts/widgets/workspaces.rhai"),
    ),
    (
        "window_list",
        include_str!("../scripts/widgets/window_list.rhai"),
    ),
    ("launcher", include_str!("../scripts/widgets/launcher.rhai")),
    ("tray", include_str!("../scripts/widgets/tray.rhai")),
    ("volume", include_str!("../scripts/widgets/volume.rhai")),
    ("battery", include_str!("../scripts/widgets/battery.rhai")),
    ("network", include_str!("../scripts/widgets/network.rhai")),
    ("input", include_str!("../scripts/widgets/input.rhai")),
    ("system", include_str!("../scripts/widgets/system.rhai")),
];

pub fn builtin_script_name(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "spacer" => "spacer",
        "clock" => "clock",
        "window_title" | "title" => "window_title",
        "layout" | "layout_status" => "layout",
        "workspaces" | "workspaces_pills" => "workspaces",
        "window_list" | "tasklist" => "window_list",
        "launcher" | "start" => "launcher",
        "tray" | "systray" => "tray",
        "volume" | "audio" => "volume",
        "battery" | "power" => "battery",
        "network" | "wifi" => "network",
        "input" | "keyboard" | "language" => "input",
        "system_status" | "status" | "system" => "system",
        _ => return None,
    })
}

pub fn builtin_script_path(kind: &str) -> Option<String> {
    builtin_script_name(kind).map(|name| format!("scripts/widgets/{name}.rhai"))
}

/// Materialize the built-ins beside a generated config. Existing files are
/// deliberately preserved: after generation they belong to the user.
pub fn export_builtin_scripts(config_path: &std::path::Path) -> Result<usize, String> {
    let root = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let directory = root.join("scripts").join("widgets");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let mut written = 0;
    for (name, source) in EMBEDDED {
        let path = directory.join(format!("{name}.rhai"));
        if path.exists() {
            continue;
        }
        std::fs::write(&path, source)
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        written += 1;
    }
    Ok(written)
}

pub fn validate_config_scripts(config: &crate::config::Config) -> Vec<String> {
    let mut widgets = config.widgets.clone();
    for panel in &config.panels {
        for name in &panel.widgets {
            if widgets.iter().any(|widget| widget.name == *name) {
                continue;
            }
            if let Some(widget) = crate::config::builtin_widget_config(name) {
                widgets.push(widget);
            }
        }
    }
    widgets
        .into_iter()
        .filter_map(|cfg| {
            let widget = ScriptWidget::new(cfg.clone());
            if cfg.widget_type != "custom"
                && builtin_script_name(&cfg.widget_type).is_none()
                && cfg.script.is_none()
            {
                return None;
            }
            match widget.source().and_then(|source| {
                crate::scripting::engine()
                    .compile(&source)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }) {
                Ok(()) => None,
                Err(error) => Some(format!("widget '{}' script: {error}", cfg.name)),
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
struct DrawCommand {
    kind: String,
    rect: [i32; 4],
    text: String,
    color: String,
    background: String,
    hover_background: String,
    font: String,
    font_size: i32,
    font_weight: i32,
    align: String,
    radius: i32,
    icon: isize,
    action: Option<String>,
    right_action: Option<String>,
    double_action: Option<String>,
    scroll_up: Option<String>,
    scroll_down: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct RenderPlan {
    width: i32,
    interval: u32,
    hover: HoverPaint,
    commands: Vec<DrawCommand>,
    error: Option<String>,
}

impl Default for RenderPlan {
    fn default() -> Self {
        Self {
            width: 120,
            interval: 1000,
            hover: HoverPaint::None,
            commands: Vec::new(),
            error: None,
        }
    }
}

#[derive(Default)]
struct ScriptState {
    source: String,
    ast: Option<AST>,
    plan: RenderPlan,
    evaluated_at: Option<Instant>,
}

pub struct ScriptWidget {
    cfg: WidgetConfig,
    state: Mutex<ScriptState>,
}

impl ScriptWidget {
    pub fn new(cfg: WidgetConfig) -> Self {
        let plan = RenderPlan {
            width: cfg.width.unwrap_or_else(|| default_width(&cfg.widget_type)),
            interval: cfg
                .interval
                .unwrap_or_else(|| default_interval(&cfg.widget_type)),
            ..Default::default()
        };
        Self {
            cfg,
            state: Mutex::new(ScriptState {
                plan,
                ..Default::default()
            }),
        }
    }

    fn script_name(&self) -> Option<&str> {
        self.cfg.script.as_deref().or_else(|| {
            builtin_script_name(&self.cfg.widget_type).map(|name| {
                // The returned string is used only to form candidate paths; the
                // embedded source below remains the final fallback.
                match name {
                    "spacer" => "scripts/widgets/spacer.rhai",
                    "clock" => "scripts/widgets/clock.rhai",
                    "window_title" => "scripts/widgets/window_title.rhai",
                    "layout" => "scripts/widgets/layout.rhai",
                    "workspaces" => "scripts/widgets/workspaces.rhai",
                    "window_list" => "scripts/widgets/window_list.rhai",
                    "launcher" => "scripts/widgets/launcher.rhai",
                    "tray" => "scripts/widgets/tray.rhai",
                    "volume" => "scripts/widgets/volume.rhai",
                    "battery" => "scripts/widgets/battery.rhai",
                    "network" => "scripts/widgets/network.rhai",
                    "input" => "scripts/widgets/input.rhai",
                    "system" => "scripts/widgets/system.rhai",
                    _ => unreachable!(),
                }
            })
        })
    }

    fn source(&self) -> Result<String, String> {
        let Some(script) = self.script_name() else {
            return Ok(self.cfg.label.clone().unwrap_or_else(|| "custom".into()));
        };
        if let Some(inline) = script.strip_prefix("rhai:") {
            return Ok(inline.trim().to_string());
        }
        for path in script_candidates(script) {
            if let Ok(source) = std::fs::read_to_string(&path) {
                return Ok(source);
            }
        }
        if let Some(name) = builtin_script_name(&self.cfg.widget_type) {
            if let Some((_, source)) = EMBEDDED.iter().find(|(candidate, _)| *candidate == name) {
                return Ok((*source).to_string());
            }
        }
        Err(format!("script not found: {script}"))
    }

    fn evaluate(&self, ctx: &PanelCtx, rect: RECT) -> RenderPlan {
        let source = match self.source() {
            Ok(source) => source,
            Err(error) => return error_plan(&self.cfg, error),
        };
        let mut ast = {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            (state.source == source)
                .then(|| state.ast.clone())
                .flatten()
        };
        if ast.is_none() {
            match crate::scripting::engine().compile(&source) {
                Ok(compiled) => ast = Some(compiled),
                Err(error) => return error_plan(&self.cfg, error.to_string()),
            }
        }
        let ast = ast.expect("compiled AST");
        let context = widget_context(&self.cfg, ctx, rect);
        let mut scope = Scope::new();
        let output =
            crate::scripting::engine().call_fn::<Dynamic>(&mut scope, &ast, "render", (context,));
        let mut plan = match output {
            Ok(value) => parse_plan(value, &self.cfg),
            Err(error) => {
                // Compatibility: an old custom script may simply evaluate to a
                // string rather than declaring `render(ctx)`.
                match crate::scripting::engine().eval_ast::<Dynamic>(&ast) {
                    Ok(value) => text_plan(&self.cfg, value.to_string()),
                    Err(_) => error_plan(&self.cfg, error.to_string()),
                }
            }
        };
        plan.width = self.cfg.width.unwrap_or(plan.width).max(0);
        plan.interval = self.cfg.interval.unwrap_or(plan.interval).max(16);
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.source = source;
        state.ast = Some(ast);
        plan
    }

    fn action_at(
        &self,
        point: (i32, i32),
        rect: RECT,
        scale: f32,
        field: ActionField,
    ) -> Option<String> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.plan.commands.iter().rev().find_map(|command| {
            let bounds = command_rect(command.rect, rect, scale);
            point_in_rect(point.0, point.1, &bounds)
                .then(|| match field {
                    ActionField::Left => command.action.clone(),
                    ActionField::Right => command.right_action.clone(),
                    ActionField::Double => command
                        .double_action
                        .clone()
                        .or_else(|| command.action.clone()),
                    ActionField::ScrollUp => command.scroll_up.clone(),
                    ActionField::ScrollDown => command.scroll_down.clone(),
                })
                .flatten()
        })
    }

    fn handle_action(&self, action: String, rect: RECT, ctx: &PanelCtx) -> Option<String> {
        if let Some(encoded) = action.strip_prefix("@tray:") {
            let mut parts = encoded.split(':');
            let button = match parts.next() {
                Some("right") => Button::Right,
                Some("double") => Button::DoubleLeft,
                _ => Button::Left,
            };
            if let Some(id) = decode_tray_id(&parts.collect::<Vec<_>>().join(":")) {
                crate::tray::invoke(id, button);
            }
            return None;
        }
        if let Some(raw) = action
            .strip_prefix("@window:")
            .and_then(|value| value.parse::<isize>().ok())
        {
            crate::focus::toggle_window_from_list(HWND(raw as *mut std::ffi::c_void));
            return None;
        }
        if action == "@quick_settings" {
            crate::quick_settings::toggle_from_panel(
                ctx.hwnd,
                crate::ui::client_rect_to_screen(ctx.hwnd, rect),
                &ctx.panel_name,
            );
            return None;
        }
        if action == "@tray_overflow" {
            let limit = self
                .cfg
                .extra
                .get("max_items")
                .and_then(toml::Value::as_integer)
                .unwrap_or(3)
                .max(0) as usize;
            let all = crate::tray::entries();
            let mut entries: Vec<TrayEntry> = all
                .iter()
                .filter(|entry| !entry.hidden)
                .skip(limit)
                .cloned()
                .collect();
            entries.extend(all.into_iter().filter(|entry| entry.hidden));
            crate::tray_overflow::toggle(crate::ui::client_rect_to_screen(ctx.hwnd, rect), entries);
            return None;
        }
        Some(action)
    }
}

#[derive(Clone, Copy)]
enum ActionField {
    Left,
    Right,
    Double,
    ScrollUp,
    ScrollDown,
}

impl Widget for ScriptWidget {
    fn name(&self) -> &str {
        &self.cfg.name
    }
    fn kind(&self) -> &'static str {
        "script"
    }
    fn width(&self, _ctx: &PanelCtx) -> i32 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .plan
            .width
    }
    fn hover_paint(&self) -> HoverPaint {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .plan
            .hover
    }
    fn interval_ms(&self) -> Option<u32> {
        Some(
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .plan
                .interval,
        )
    }
    fn refresh(&self, ctx: &PanelCtx, rect: RECT) -> bool {
        let due = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.evaluated_at.is_none_or(|last| {
                last.elapsed() >= Duration::from_millis(state.plan.interval as u64)
            })
        };
        if !due {
            return false;
        }
        let plan = self.evaluate(ctx, rect);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let changed = state.plan != plan;
        state.plan = plan;
        state.evaluated_at = Some(Instant::now());
        changed
    }
    fn draw(&self, hdc: HDC, rect: RECT, ctx: &PanelCtx) {
        let plan = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .plan
            .clone();
        for command in &plan.commands {
            draw_command(hdc, rect, ctx, command);
        }
    }
    fn on_click(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        self.action_at(point, rect, ctx.scale, ActionField::Left)
            .and_then(|a| self.handle_action(a, rect, ctx))
            .or_else(|| self.cfg.action.clone())
    }
    fn on_right_click(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        self.action_at(point, rect, ctx.scale, ActionField::Right)
            .and_then(|a| self.handle_action(a, rect, ctx))
    }
    fn on_double_click(&self, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> Option<String> {
        self.action_at(point, rect, ctx.scale, ActionField::Double)
            .and_then(|a| self.handle_action(a, rect, ctx))
    }
    fn on_scroll(&self, delta: i32, point: (i32, i32), rect: RECT, ctx: &PanelCtx) -> bool {
        let field = if delta > 0 {
            ActionField::ScrollUp
        } else {
            ActionField::ScrollDown
        };
        if let Some(action) = self.action_at(point, rect, ctx.scale, field) {
            let _ = self
                .handle_action(action, rect, ctx)
                .map(|a| crate::scripting::dispatch_action(&a));
            true
        } else {
            false
        }
    }
}

fn default_width(kind: &str) -> i32 {
    match builtin_script_name(kind) {
        Some("spacer" | "window_title" | "window_list") => 0,
        Some("clock") => 112,
        Some("layout") => 178,
        Some("workspaces") => 100,
        Some("launcher") => 112,
        Some("tray") => 104,
        Some("system") => 230,
        Some(_) => 104,
        None => 120,
    }
}
fn default_interval(kind: &str) -> u32 {
    match builtin_script_name(kind) {
        Some("clock") => 1000,
        Some("input") => 500,
        Some(
            "window_title" | "layout" | "workspaces" | "window_list" | "tray" | "launcher"
            | "spacer",
        ) => 250,
        _ => 1000,
    }
}

fn script_candidates(script: &str) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if let Some(dir) = crate::CONFIG_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|p| p.parent())
    {
        paths.push(dir.join(script));
    }
    paths.push(std::path::PathBuf::from(script));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join(script));
        }
    }
    paths
}

fn map_insert(map: &mut Map, key: &str, value: impl Into<Dynamic>) {
    map.insert(key.into(), value.into());
}
fn array_map(items: impl IntoIterator<Item = Map>) -> Array {
    items.into_iter().map(Dynamic::from_map).collect()
}

fn widget_context(cfg: &WidgetConfig, ctx: &PanelCtx, rect: RECT) -> Map {
    let scale = ctx.scale.max(0.01);
    let mut root = Map::new();
    map_insert(&mut root, "name", cfg.name.clone());
    map_insert(&mut root, "kind", cfg.widget_type.clone());
    map_insert(
        &mut root,
        "width",
        (rect_width(&rect) as f32 / scale).round() as i64,
    );
    map_insert(
        &mut root,
        "height",
        (rect_height(&rect) as f32 / scale).round() as i64,
    );
    map_insert(&mut root, "vertical", ctx.vertical);
    map_insert(&mut root, "monitor", ctx.monitor_key as i64);
    map_insert(&mut root, "panel", ctx.panel_name.clone());
    let foreground = unsafe { GetForegroundWindow() };
    let script_windows = if builtin_script_name(&cfg.widget_type) == Some("window_list") {
        crate::manager::collect_windows_including_minimized()
            .into_iter()
            .filter(|hwnd| unsafe {
                MonitorFromWindow(*hwnd, MONITOR_DEFAULTTONEAREST).0 as isize == ctx.monitor_key
            })
            .filter(|hwnd| crate::workspace::is_visible(*hwnd))
            .collect::<Vec<_>>()
    } else {
        ctx.windows.clone()
    };
    let windows = script_windows.iter().map(|hwnd| {
        let mut item = Map::new();
        let raw = hwnd.0 as isize;
        map_insert(&mut item, "id", raw as i64);
        map_insert(&mut item, "title", crate::util::get_window_title(*hwnd));
        map_insert(&mut item, "active", *hwnd == foreground);
        map_insert(&mut item, "minimized", unsafe { IsIconic(*hwnd).as_bool() });
        map_insert(
            &mut item,
            "floating",
            crate::rules::is_floating(*hwnd)
                || crate::focus::is_runtime_floating(*hwnd)
                || crate::manager::is_auto_floating(*hwnd),
        );
        map_insert(
            &mut item,
            "icon",
            crate::widgets::window_icon(*hwnd)
                .map(|icon| icon.0 as isize as i64)
                .unwrap_or(0),
        );
        item
    });
    map_insert(&mut root, "windows", array_map(windows));
    map_insert(
        &mut root,
        "focused_title",
        crate::util::get_window_title(foreground),
    );
    let workspaces = crate::workspace::summary(ctx.monitor_key, &ctx.windows)
        .into_iter()
        .map(|info| {
            let mut item = Map::new();
            map_insert(&mut item, "number", info.number as i64);
            map_insert(&mut item, "active", info.active);
            map_insert(&mut item, "occupied", info.occupied);
            item
        });
    map_insert(&mut root, "workspaces", array_map(workspaces));
    map_insert(
        &mut root,
        "layout",
        crate::CURRENT_LAYOUT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .name()
            .to_string(),
    );
    map_insert(
        &mut root,
        "tiling",
        crate::TILING_ENABLED.load(std::sync::atomic::Ordering::SeqCst),
    );
    let tray = crate::tray::entries().into_iter().map(|entry| {
        let mut item = Map::new();
        map_insert(&mut item, "id", encode_tray_id(entry.id));
        map_insert(&mut item, "name", entry.name);
        map_insert(&mut item, "icon", entry.icon as i64);
        map_insert(&mut item, "hidden", entry.hidden);
        map_insert(&mut item, "process", entry.process);
        item
    });
    map_insert(&mut root, "tray", array_map(tray));
    let status = crate::system::status();
    let mut system = Map::new();
    map_insert(
        &mut system,
        "volume",
        status.volume.map(|v| i64::from(v.percent())).unwrap_or(-1),
    );
    map_insert(&mut system, "muted", status.volume.is_some_and(|v| v.muted));
    map_insert(
        &mut system,
        "battery",
        status
            .battery
            .and_then(|v| v.percent)
            .map(i64::from)
            .unwrap_or(-1),
    );
    map_insert(
        &mut system,
        "charging",
        status.battery.is_some_and(|v| v.charging),
    );
    map_insert(
        &mut system,
        "on_ac",
        status.battery.is_some_and(|v| v.on_ac),
    );
    map_insert(&mut system, "network", status.network.label());
    let (network_kind, network_signal) = match &status.network {
        crate::system::NetworkStatus::WiFi { signal, .. } => ("wifi", i64::from(*signal)),
        crate::system::NetworkStatus::Wired => ("wired", -1),
        crate::system::NetworkStatus::Offline => ("offline", -1),
        crate::system::NetworkStatus::Unknown => ("unknown", -1),
    };
    map_insert(&mut system, "network_kind", network_kind);
    map_insert(&mut system, "network_signal", network_signal);
    map_insert(
        &mut system,
        "connected",
        !matches!(
            status.network,
            crate::system::NetworkStatus::Offline | crate::system::NetworkStatus::Unknown
        ),
    );
    map_insert(
        &mut system,
        "brightness",
        status
            .brightness
            .map(|v| i64::from(v.percent))
            .unwrap_or(-1),
    );
    map_insert(
        &mut system,
        "input",
        crate::input::current().map(|v| v.tag).unwrap_or_default(),
    );
    map_insert(&mut root, "system", system);
    let mut config = Map::new();
    if let Some(value) = &cfg.format {
        map_insert(&mut config, "format", value.clone());
    }
    if let Some(value) = &cfg.label {
        map_insert(&mut config, "label", value.clone());
    }
    if let Some(value) = &cfg.icon {
        map_insert(&mut config, "icon", value.clone());
    }
    if let Some(value) = &cfg.command {
        map_insert(&mut config, "command", value.clone());
    }
    if let Some(value) = &cfg.action {
        map_insert(&mut config, "action", value.clone());
    }
    for (key, value) in &cfg.extra {
        config.insert(key.clone().into(), toml_dynamic(value));
    }
    map_insert(&mut root, "config", config);
    root
}

fn toml_dynamic(value: &toml::Value) -> Dynamic {
    match value {
        toml::Value::String(v) => v.clone().into(),
        toml::Value::Integer(v) => (*v).into(),
        toml::Value::Float(v) => (*v).into(),
        toml::Value::Boolean(v) => (*v).into(),
        toml::Value::Array(v) => v.iter().map(toml_dynamic).collect::<Array>().into(),
        toml::Value::Table(v) => v
            .iter()
            .map(|(k, v)| (k.clone().into(), toml_dynamic(v)))
            .collect::<Map>()
            .into(),
        toml::Value::Datetime(v) => v.to_string().into(),
    }
}

fn parse_plan(value: Dynamic, cfg: &WidgetConfig) -> RenderPlan {
    if value.is::<Array>() {
        return RenderPlan {
            width: default_width(&cfg.widget_type),
            interval: default_interval(&cfg.widget_type),
            commands: parse_commands(value.cast::<Array>()),
            ..Default::default()
        };
    }
    let Some(map) = value.clone().try_cast::<Map>() else {
        return text_plan(cfg, value.to_string());
    };
    let hover = match string(&map, "hover").as_str() {
        "whole" => HoverPaint::Whole,
        "self" | "self_drawn" => HoverPaint::SelfDrawn,
        _ => HoverPaint::None,
    };
    RenderPlan {
        width: integer(&map, "width", default_width(&cfg.widget_type)),
        interval: integer(&map, "interval", default_interval(&cfg.widget_type) as i32).max(16)
            as u32,
        hover,
        commands: map
            .get("commands")
            .and_then(|v| v.clone().try_cast::<Array>())
            .map(parse_commands)
            .unwrap_or_default(),
        error: None,
    }
}
fn parse_commands(values: Array) -> Vec<DrawCommand> {
    values
        .into_iter()
        .filter_map(|v| v.try_cast::<Map>())
        .map(|m| DrawCommand {
            kind: string(&m, "type"),
            rect: [
                integer(&m, "x", 0),
                integer(&m, "y", 0),
                integer(&m, "w", 0),
                integer(&m, "h", 0),
            ],
            text: string(&m, "text"),
            color: string_or(&m, "color", "text"),
            background: string(&m, "background"),
            hover_background: string(&m, "hover_background"),
            font: string_or(&m, "font", "body"),
            font_size: integer(&m, "font_size", 0),
            font_weight: integer(&m, "font_weight", 400),
            align: string_or(&m, "align", "left"),
            radius: integer(&m, "radius", 0),
            icon: m.get("icon").and_then(|v| v.as_int().ok()).unwrap_or(0) as isize,
            action: optional_string(&m, "action"),
            right_action: optional_string(&m, "right_action"),
            double_action: optional_string(&m, "double_action"),
            scroll_up: optional_string(&m, "scroll_up"),
            scroll_down: optional_string(&m, "scroll_down"),
        })
        .collect()
}
fn integer(map: &Map, key: &str, default: i32) -> i32 {
    map.get(key)
        .and_then(|v| v.as_int().ok())
        .map(|v| v as i32)
        .unwrap_or(default)
}
fn string(map: &Map, key: &str) -> String {
    optional_string(map, key).unwrap_or_default()
}
fn string_or(map: &Map, key: &str, default: &str) -> String {
    optional_string(map, key).unwrap_or_else(|| default.into())
}
fn optional_string(map: &Map, key: &str) -> Option<String> {
    map.get(key)
        .and_then(|v| v.clone().into_string().ok())
        .filter(|value| !value.is_empty())
}
fn text_plan(cfg: &WidgetConfig, text: String) -> RenderPlan {
    RenderPlan {
        width: cfg.width.unwrap_or(default_width(&cfg.widget_type)),
        interval: cfg.interval.unwrap_or(default_interval(&cfg.widget_type)),
        commands: vec![DrawCommand {
            kind: "text".into(),
            rect: [10, 0, -20, -1],
            text,
            color: "text".into(),
            background: String::new(),
            hover_background: String::new(),
            font: "body".into(),
            font_size: 0,
            font_weight: 400,
            align: "left".into(),
            radius: 0,
            icon: 0,
            action: cfg.action.clone(),
            right_action: None,
            double_action: None,
            scroll_up: None,
            scroll_down: None,
        }],
        ..Default::default()
    }
}
fn error_plan(cfg: &WidgetConfig, error: String) -> RenderPlan {
    let mut plan = text_plan(cfg, format!("rhai: {error}"));
    plan.error = Some(error);
    plan
}

fn command_rect(values: [i32; 4], parent: RECT, scale: f32) -> RECT {
    let width = rect_width(&parent);
    let height = rect_height(&parent);
    let px = |v: i32| (v as f32 * scale).round() as i32;
    let w = if values[2] < 0 {
        width + px(values[2])
    } else {
        px(values[2])
    };
    let h = if values[3] < 0 {
        height + px(values[3])
    } else {
        px(values[3])
    };
    RECT {
        left: parent.left + px(values[0]),
        top: parent.top + px(values[1]),
        right: parent.left + px(values[0]) + w,
        bottom: parent.top + px(values[1]) + h,
    }
}
fn resolve_color(name: &str, ctx: &PanelCtx) -> COLORREF {
    match name {
        "text" => ctx.theme.text_color(),
        "text_dim" => ctx.theme.text_dim_color(),
        "surface" => ctx.theme.surface_color(),
        "surface_hover" => ctx.theme.surface_hover_color(),
        "accent" => ctx.theme.accent_active_color(),
        "border" => ctx.theme.border_color(),
        "panel" => ctx.theme.panel_bg(&ctx.panel_name),
        "" => COLORREF(0),
        other => ctx.theme.color(other),
    }
}
fn draw_command(hdc: HDC, parent: RECT, ctx: &PanelCtx, cmd: &DrawCommand) {
    let rect = command_rect(cmd.rect, parent, ctx.scale);
    let hovered = ctx.pointer.is_some_and(|p| point_in_rect(p.0, p.1, &rect));
    let bg = if hovered && !cmd.hover_background.is_empty() {
        &cmd.hover_background
    } else {
        &cmd.background
    };
    if !bg.is_empty() {
        if cmd.radius > 0 {
            fill_round_rect(hdc, &rect, ctx.px(cmd.radius), resolve_color(bg, ctx))
        } else {
            fill_rect(hdc, &rect, resolve_color(bg, ctx))
        }
    }
    match cmd.kind.as_str() {
        "icon" if cmd.icon != 0 => unsafe {
            let side = rect_width(&rect).min(rect_height(&rect));
            let _ = DrawIconEx(
                hdc,
                rect.left + (rect_width(&rect) - side) / 2,
                rect.top + (rect_height(&rect) - side) / 2,
                HICON(cmd.icon as *mut std::ffi::c_void),
                side,
                side,
                0,
                None,
                DI_NORMAL,
            );
        },
        "text" => {
            let font = if cmd.font_size > 0 {
                ctx.font(cmd.font_size, cmd.font_weight.clamp(100, 900))
            } else {
                match cmd.font.as_str() {
                    "strong" => ctx.strong_font(),
                    "small" => ctx.small_font(),
                    "symbol" => ctx.symbol_font(),
                    _ => ctx.body_font(),
                }
            };
            let measured = measure_label(hdc, &cmd.text, font);
            let area = match cmd.align.as_str() {
                "center" => RECT {
                    left: rect.left + (rect_width(&rect) - measured).max(0) / 2,
                    ..rect
                },
                "right" => RECT {
                    left: (rect.right - measured).max(rect.left),
                    ..rect
                },
                _ => rect,
            };
            draw_label(hdc, &area, &cmd.text, font, resolve_color(&cmd.color, ctx));
        }
        _ => {}
    }
}

fn encode_tray_id(id: TrayId) -> String {
    match id {
        TrayId::Native { owner, uid } => format!("native:{owner}:{uid}"),
        TrayId::Explorer { index } => format!("explorer:{index}"),
    }
}
fn decode_tray_id(value: &str) -> Option<TrayId> {
    let p = value.split(':').collect::<Vec<_>>();
    match p.as_slice() {
        ["native", owner, uid] => Some(TrayId::Native {
            owner: owner.parse().ok()?,
            uid: uid.parse().ok()?,
        }),
        ["explorer", index] => Some(TrayId::Explorer {
            index: index.parse().ok()?,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_aliases_resolve() {
        assert_eq!(builtin_script_name("audio"), Some("volume"));
        assert_eq!(builtin_script_name("tasklist"), Some("window_list"));
    }

    #[test]
    fn tray_ids_round_trip() {
        for id in [
            TrayId::Native { owner: -42, uid: 7 },
            TrayId::Explorer { index: 3 },
        ] {
            assert_eq!(decode_tray_id(&encode_tray_id(id)), Some(id));
        }
    }

    #[test]
    fn all_shipped_widgets_render() {
        let mut system = Map::new();
        map_insert(&mut system, "volume", 50_i64);
        map_insert(&mut system, "muted", false);
        map_insert(&mut system, "battery", 80_i64);
        map_insert(&mut system, "charging", false);
        map_insert(&mut system, "on_ac", false);
        map_insert(&mut system, "network", "Wired");
        map_insert(&mut system, "network_kind", "wired");
        map_insert(&mut system, "network_signal", -1_i64);
        map_insert(&mut system, "connected", true);
        map_insert(&mut system, "brightness", 75_i64);
        map_insert(&mut system, "input", "en-US");
        let mut context = Map::new();
        map_insert(&mut context, "name", "test");
        map_insert(&mut context, "kind", "test");
        map_insert(&mut context, "width", 240_i64);
        map_insert(&mut context, "height", 48_i64);
        map_insert(&mut context, "vertical", false);
        map_insert(&mut context, "monitor", 0_i64);
        map_insert(&mut context, "panel", "test");
        let mut window = Map::new();
        map_insert(&mut window, "id", 42_i64);
        map_insert(&mut window, "title", "Window");
        map_insert(&mut window, "icon", 0_i64);
        map_insert(&mut window, "active", true);
        map_insert(&mut window, "minimized", false);
        map_insert(&mut window, "floating", false);
        map_insert(&mut context, "windows", vec![Dynamic::from_map(window)]);
        map_insert(&mut context, "focused_title", "Window");
        let mut workspace = Map::new();
        map_insert(&mut workspace, "number", 1_i64);
        map_insert(&mut workspace, "active", true);
        map_insert(&mut workspace, "occupied", true);
        map_insert(
            &mut context,
            "workspaces",
            vec![Dynamic::from_map(workspace)],
        );
        map_insert(&mut context, "layout", "MasterStack");
        map_insert(&mut context, "tiling", true);
        let mut tray = Map::new();
        map_insert(&mut tray, "id", "explorer:0");
        map_insert(&mut tray, "name", "Example");
        map_insert(&mut tray, "icon", 0_i64);
        map_insert(&mut tray, "hidden", false);
        map_insert(&mut tray, "process", "example.exe");
        map_insert(&mut context, "tray", vec![Dynamic::from_map(tray)]);
        map_insert(&mut context, "system", system);
        map_insert(&mut context, "config", Map::new());
        for (name, source) in EMBEDDED {
            let ast = crate::scripting::engine()
                .compile(source)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            let output = crate::scripting::engine().call_fn::<Dynamic>(
                &mut Scope::new(),
                &ast,
                "render",
                (context.clone(),),
            );
            let output = output.unwrap_or_else(|error| panic!("{name}: {error}"));
            if *name == "system" {
                let plan = output.clone_cast::<Map>();
                let commands = plan["commands"].clone_cast::<Array>();
                let symbols = commands
                    .into_iter()
                    .map(|value| value.clone_cast::<Map>())
                    .filter(|command| {
                        command
                            .get("font")
                            .and_then(|value| value.clone().try_cast::<String>())
                            .is_some_and(|font| font == "symbol")
                    })
                    .count();
                assert_eq!(symbols, 4, "system widget lost one of its four icons");
            }
        }
    }
}

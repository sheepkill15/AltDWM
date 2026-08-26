use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::layout::Layout;

// ------------------------------------------------------------------
// Top-level Config — DSL root (see docs/EXTENSIBILITY.md)
// ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub ignore: Ignore,

    // v0.2 DSL — all optional, ignored if empty (backward compat with [general] only)
    #[serde(default)]
    pub panels: Vec<PanelConfig>,
    #[serde(default)]
    pub widgets: Vec<WidgetConfig>,
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
    #[serde(default)]
    pub keybinds: Vec<KeybindConfig>,
    #[serde(default)]
    pub layouts: HashMap<String, LayoutConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct General {
    #[serde(default = "default_gap")]
    pub gap: i32,
    #[serde(default = "default_layout")]
    pub layout: String,
    #[serde(default = "default_taskbar")]
    pub taskbar: bool,
    #[serde(default = "default_taskbar_height")]
    pub taskbar_height: i32,
    #[serde(default = "default_true")]
    pub auto_tile: bool,
    /// extra height for vertical panels (not used yet)
    #[serde(default)]
    pub outer_gap: Option<i32>,
    /// filter tiling to current virtual desktop only (requires IVirtualDesktopManager)
    #[serde(default = "default_filter_vd")]
    pub filter_virtual_desktop: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Ignore {
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub titles: Vec<String>,
    #[serde(default)]
    pub processes: Vec<String>,
}

// ------------------------------------------------------------------
// DSL pieces — Panel / Widget / Rule / Keybind / Layout
// ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelConfig {
    pub name: String,
    #[serde(default = "default_panel_position")]
    pub position: String, // top | bottom | left | right
    #[serde(default = "default_panel_height")]
    pub height: i32,
    #[serde(default = "default_monitor")]
    pub monitor: String, // all | primary | 1 | 2 | "Dell U2720Q"
    #[serde(default)]
    pub margin: Option<[i32; 4]>, // top,right,bottom,left
    #[serde(default)]
    pub background: Option<String>, // hex "#202020" or "rhai: ..."
    #[serde(default)]
    pub widgets: Vec<String>, // ordered widget names
    /// allow arbitrary future keys without parse error (extensibility)
    #[serde(default, flatten)]
    pub extra: HashMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetConfig {
    /// widget type: clock | workspaces | window_title | tray | spacer | launcher | custom | ...
    #[serde(rename = "type", alias = "widget_type", alias = "kind")]
    pub widget_type: String,
    pub name: String,
    /// strftime-like for clock, or template for custom
    #[serde(default)]
    pub format: Option<String>,
    /// update interval ms
    #[serde(default)]
    pub interval: Option<u32>,
    /// Rhai script path or inline `rhai: ...`
    #[serde(default)]
    pub script: Option<String>,
    /// click action: launch cmd or rhai: ...
    #[serde(default)]
    pub action: Option<String>,
    /// command for launcher / on_click
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    /// fixed width (0 = flex)
    #[serde(default)]
    pub width: Option<i32>,
    #[serde(default)]
    pub tooltip: Option<String>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleConfig {
    #[serde(default)]
    pub match_class: Option<String>,
    #[serde(default)]
    pub match_title: Option<String>,
    #[serde(default)]
    pub match_process: Option<String>,
    /// regex variant (if regex crate used)
    #[serde(default)]
    pub match_class_regex: Option<String>,
    #[serde(default)]
    pub match_title_regex: Option<String>,
    #[serde(default)]
    pub monitor: Option<String>,
    #[serde(default)]
    pub floating: Option<bool>,
    #[serde(default)]
    pub opacity: Option<f32>,
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub on_create: Option<String>, // rhai: ...
    #[serde(default, flatten)]
    pub extra: HashMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeybindConfig {
    /// "Win+Shift+R" etc
    pub keys: String,
    /// "retile" | "toggle_tiling" | "set_layout(\"grid\")" | "launch('wt.exe')" | "rhai: ..."
    pub action: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LayoutConfig {
    /// path to rhai script: fn layout(n, area) -> [rects]
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub gap: Option<i32>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, toml::Value>,
}

// ------------------------------------------------------------------
// Defaults helpers
// ------------------------------------------------------------------

fn default_gap() -> i32 { 8 }
fn default_taskbar() -> bool { true }
fn default_taskbar_height() -> i32 { 40 }
fn default_true() -> bool { true }
fn default_filter_vd() -> bool { false }
fn default_layout() -> String { "MasterStack".to_string() }
fn default_panel_position() -> String { "bottom".to_string() }
fn default_panel_height() -> i32 { 40 }
fn default_monitor() -> String { "all".to_string() }

impl Default for General {
    fn default() -> Self {
        Self {
            gap: default_gap(),
            layout: default_layout(),
            taskbar: default_taskbar(),
            taskbar_height: default_taskbar_height(),
            auto_tile: default_true(),
            outer_gap: None,
            filter_virtual_desktop: default_filter_vd(),
        }
    }
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            name: "bottom".into(),
            position: default_panel_position(),
            height: default_panel_height(),
            monitor: default_monitor(),
            margin: None,
            background: None,
            widgets: vec!["workspaces".into(), "window_title".into(), "clock".into()],
            extra: HashMap::new(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General::default(),
            ignore: Ignore {
                classes: vec![
                    "Progman".into(),
                    "WorkerW".into(),
                    "Shell_TrayWnd".into(),
                    "Shell_SecondaryTrayWnd".into(),
                    "AltDWM_Taskbar".into(),
                    "AltDWM_Host".into(),
                ],
                titles: vec![],
                processes: vec![],
            },
            panels: vec![],
            widgets: vec![],
            rules: vec![],
            keybinds: vec![
                // Alt+Shift chosen because Win+Shift collides with system (Win+Shift+S = Snipping Tool, etc.)
                // Users can set keys = "Win+Shift+R" etc in config.toml if they prefer — parser supports Win/Ctrl/Alt/Shift combos
                KeybindConfig { keys: "Alt+Shift+R".into(), action: "retile".into(), description: Some("Retile windows".into()) },
                KeybindConfig { keys: "Alt+Shift+T".into(), action: "toggle_tiling".into(), description: Some("Toggle tiling".into()) },
                KeybindConfig { keys: "Alt+Shift+Q".into(), action: "quit".into(), description: Some("Quit AltDWM".into()) },
                KeybindConfig { keys: "Alt+Shift+G".into(), action: "set_layout(\"Grid\")".into(), description: Some("Grid layout".into()) },
                KeybindConfig { keys: "Alt+Shift+M".into(), action: "set_layout(\"Monocle\")".into(), description: None },
                KeybindConfig { keys: "Alt+Shift+F".into(), action: "set_layout(\"Floating\")".into(), description: None },
                KeybindConfig { keys: "Alt+Shift+S".into(), action: "set_layout(\"MasterStack\")".into(), description: None },
                KeybindConfig { keys: "Alt+Shift+C".into(), action: "reload_config".into(), description: Some("Hot-reload config".into()) },
            ],
            layouts: HashMap::new(),
        }
    }
}

// ------------------------------------------------------------------
// Config helpers
// ------------------------------------------------------------------

impl Config {
    pub fn layout_enum(&self) -> Layout {
        match self.general.layout.to_lowercase().as_str() {
            "grid" => Layout::Grid,
            "monocle" => Layout::Monocle,
            "floating" => Layout::Floating,
            "masterstack" | "master" | "bsp" | "tiling" => Layout::MasterStack,
            other => {
                if self.layouts.contains_key(other) {
                    // custom layout — handled by try_compute_custom, keep MasterStack as fallback enum
                } else {
                    eprintln!("[config] unknown layout '{}' -> MasterStack", self.general.layout);
                }
                Layout::MasterStack
            }
        }
    }

    pub fn set_layout(&mut self, l: Layout) {
        self.general.layout = l.name().to_string();
    }

    pub fn is_legacy_taskbar_mode(&self) -> bool {
        self.panels.is_empty()
    }

    pub fn widget_by_name(&self, name: &str) -> Option<&WidgetConfig> {
        self.widgets.iter().find(|w| w.name == name)
    }

    pub fn validate(&self) -> Vec<String> {
        let mut warns = Vec::new();
        for p in &self.panels {
            if !["top","bottom","left","right"].contains(&p.position.as_str()) {
                warns.push(format!("panel '{}' invalid position '{}'", p.name, p.position));
            }
            for w in &p.widgets {
                if self.widget_by_name(w).is_none() && !is_builtin_widget(w) {
                    warns.push(format!("panel '{}' references unknown widget '{}'", p.name, w));
                }
            }
        }
        warns
    }
}

fn is_builtin_widget(name: &str) -> bool {
    matches!(name, "spacer" | "workspaces" | "window_title" | "tray" | "clock" | "launcher")
}

// ------------------------------------------------------------------
// Path discovery & I/O (unchanged, now supports expanded Config)
// ------------------------------------------------------------------

pub fn find_config_path(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        if p.exists() {
            return Some(p.to_path_buf());
        }
        return Some(p.to_path_buf());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("config.toml");
            if p.exists() { return Some(p); }
        }
    }
    if let Some(cfg_dir) = dirs::config_dir() {
        let p = cfg_dir.join("AltDWM").join("config.toml");
        if p.exists() { return Some(p); }
    }
    let cwd = PathBuf::from("config.toml");
    if cwd.exists() { return Some(cwd); }
    if let Some(cfg_dir) = dirs::config_dir() {
        return Some(cfg_dir.join("AltDWM").join("config.toml"));
    }
    Some(cwd)
}

pub fn default_config_path() -> PathBuf {
    find_config_path(None).unwrap_or_else(|| PathBuf::from("config.toml"))
}

pub fn load_from_path(path: &Path) -> Result<Config, String> {
    let data = std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    toml::from_str::<Config>(&data).map_err(|e| format!("parse {}: {}", path.display(), e))
}

pub fn load_or_default(explicit: Option<&Path>) -> (Config, Option<PathBuf>) {
    let path = find_config_path(explicit);
    if let Some(ref p) = path {
        if p.exists() {
            match load_from_path(p) {
                Ok(cfg) => {
                    println!("[config] loaded {}", p.display());
                    for w in cfg.validate() { eprintln!("[config] warn: {}", w); }
                    return (cfg, Some(p.clone()));
                }
                Err(e) => eprintln!("[config] failed to load {}: {} -> using defaults", p.display(), e),
            }
        } else {
            println!("[config] no config at {} -> using defaults", p.display());
        }
    }
    (Config::default(), path)
}

pub fn save_to_path(cfg: &Config, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }
    let s = toml::to_string_pretty(cfg).map_err(|e| format!("serialize: {}", e))?;
    std::fs::write(path, s).map_err(|e| format!("write {}: {}", path.display(), e))?;
    println!("[config] wrote {}", path.display());
    Ok(())
}

pub fn ensure_default_file(explicit: Option<&Path>) -> Result<PathBuf, String> {
    let path = find_config_path(explicit).unwrap_or_else(default_config_path);
    if !path.exists() {
        let cfg = example_config_with_panels();
        save_to_path(&cfg, &path)?;
    }
    Ok(path)
}

/// Example that demonstrates full DSL — used by --generate-config
pub fn example_config_with_panels() -> Config {
    let mut cfg = Config::default();
    cfg.general.gap = 8;
    cfg.general.layout = "MasterStack".into();
    // add sample panels/widgets/rules so generated file is illustrative
    cfg.panels = vec![
        PanelConfig {
            name: "bottom".into(),
            position: "bottom".into(),
            height: 40,
            monitor: "all".into(),
            margin: None,
            background: Some("#202020".into()),
            widgets: vec!["workspaces".into(), "window_title".into(), "spacer".into(), "tray".into(), "clock".into()],
            extra: HashMap::new(),
        },
        PanelConfig {
            name: "top".into(),
            position: "top".into(),
            height: 28,
            monitor: "primary".into(),
            margin: None,
            background: Some("#1a1a1a".into()),
            widgets: vec!["launcher".into(), "spacer".into(), "cpu".into()],
            extra: HashMap::new(),
        },
    ];
    cfg.widgets = vec![
        WidgetConfig { widget_type: "workspaces".into(), name: "workspaces".into(), format: None, interval: None, script: None, action: None, command: None, label: None, icon: None, width: None, tooltip: None, extra: HashMap::new() },
        WidgetConfig { widget_type: "window_title".into(), name: "window_title".into(), format: None, interval: None, script: None, action: None, command: None, label: None, icon: None, width: None, tooltip: None, extra: HashMap::new() },
        WidgetConfig { widget_type: "tray".into(), name: "tray".into(), format: None, interval: None, script: None, action: None, command: None, label: None, icon: None, width: Some(200), tooltip: None, extra: HashMap::new() },
        WidgetConfig { widget_type: "clock".into(), name: "clock".into(), format: Some("%H:%M:%S".into()), interval: Some(1000), script: None, action: Some("rhai: launch(\"explorer.exe\")".into()), command: None, label: None, icon: None, width: Some(160), tooltip: None, extra: HashMap::new() },
        WidgetConfig { widget_type: "spacer".into(), name: "spacer".into(), format: None, interval: None, script: None, action: None, command: None, label: None, icon: None, width: None, tooltip: None, extra: HashMap::new() },
        WidgetConfig { widget_type: "launcher".into(), name: "launcher".into(), format: None, interval: None, script: None, action: Some("launch('explorer.exe')".into()), command: None, label: Some("Menu".into()), icon: None, width: Some(40), tooltip: Some("Launcher".into()), extra: HashMap::new() },
        WidgetConfig { widget_type: "custom".into(), name: "cpu".into(), format: None, interval: Some(2000), script: Some("scripts/cpu.rhai".into()), action: None, command: None, label: None, icon: None, width: Some(120), tooltip: None, extra: HashMap::new() },
    ];
    cfg.rules = vec![
        RuleConfig { match_class: Some("Spotify".into()), match_title: None, match_process: None, match_class_regex: None, match_title_regex: None, monitor: None, floating: Some(true), opacity: None, layout: None, on_create: None, extra: HashMap::new() },
    ];
    cfg.layouts.insert("spiral".into(), LayoutConfig { script: Some("scripts/spiral.rhai".into()), gap: None, extra: HashMap::new() });
    // To use custom layout: set general.layout = "spiral"
    cfg
}

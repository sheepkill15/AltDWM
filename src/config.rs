use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::layout::Layout;
use crate::theme::Theme;
use regex::Regex;

// ------------------------------------------------------------------
// Top-level Config — DSL root (see docs/EXTENSIBILITY.md)
// ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub ignore: Ignore,
    #[serde(default)]
    pub theme: Theme,

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
    /// Provide wallpaper, Desktop-folder icons, and desktop context menus.
    /// The surface stays at the bottom of the Z order, underneath Explorer's
    /// own desktop when AltDWM is being tested without replacing the shell.
    #[serde(default = "default_true")]
    pub desktop: bool,
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
    /// Place a newly shown window synchronously and suppress its first DWM
    /// transition instead of waiting for the coalesced layout timer.
    #[serde(default = "default_true")]
    pub instant_first_layout: bool,
    /// Float owned, modal, and small non-resizable utility windows by default.
    /// An explicit matching rule with `floating = false` overrides this.
    #[serde(default = "default_true")]
    pub auto_float_utility_windows: bool,
    /// Query WM_GETMINMAXINFO and float a window when its assigned tile would
    /// violate the application's minimum tracking size.
    #[serde(default = "default_true")]
    pub respect_window_size_constraints: bool,
    /// Hide Explorer's primary/secondary taskbars while AltDWM is running.
    #[serde(default = "default_true")]
    pub hide_native_taskbar: bool,
    /// Where the `tray` widget's items come from.
    ///
    /// `auto` hosts the notification area when AltDWM owns the taskbar and
    /// mirrors Explorer's when it does not; `native` always hosts it, `explorer`
    /// always mirrors, `off` shows nothing. Hosting is what produces real icons
    /// and working clicks, but it also takes icons away from Explorer's tray,
    /// so it is not something to do behind the back of a user who kept it.
    #[serde(default = "default_tray")]
    pub tray: String,
    /// Process enabled Run entries and Startup-folder shortcuts when AltDWM is
    /// the configured Winlogon shell. Explorer normally owns this job.
    #[serde(default = "default_true")]
    pub launch_startup_apps: bool,
    /// Additional inset applied to every edge of each monitor's work area.
    #[serde(default)]
    pub outer_gap: Option<i32>,
    /// filter tiling to current virtual desktop only (requires IVirtualDesktopManager)
    #[serde(default = "default_filter_vd")]
    pub filter_virtual_desktop: bool,
    /// Workspaces per monitor. `1` disables the feature entirely.
    #[serde(default = "default_workspaces")]
    pub workspaces: usize,
    /// Fraction of the usable width the master column takes, 0.1–0.9.
    #[serde(default = "default_master_ratio")]
    pub master_ratio: f32,
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
    pub background: Option<String>, // hex "#202020"
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
    #[serde(default, flatten)]
    pub extra: HashMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    #[serde(skip)]
    #[serde(default)]
    pub compiled_class_regex: Option<Regex>,
    #[serde(skip)]
    #[serde(default)]
    pub compiled_title_regex: Option<Regex>,
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

fn default_gap() -> i32 {
    8
}
fn default_taskbar() -> bool {
    true
}
fn default_taskbar_height() -> i32 {
    40
}
fn default_true() -> bool {
    true
}
fn default_filter_vd() -> bool {
    false
}
fn default_tray() -> String {
    "auto".to_string()
}
/// Workspaces are opt-in.
///
/// Switching away from a workspace hides windows, and a window the user cannot
/// find is this program's worst failure mode. Someone who has not asked for
/// workspaces should never be able to reach that state, so the default leaves
/// the feature inert and the recovery paths untested-by-accident.
fn default_workspaces() -> usize {
    1
}
fn default_master_ratio() -> f32 {
    0.6
}
fn default_layout() -> String {
    "MasterStack".to_string()
}
fn default_panel_position() -> String {
    "bottom".to_string()
}
fn default_panel_height() -> i32 {
    40
}
fn default_monitor() -> String {
    "all".to_string()
}

impl Default for General {
    fn default() -> Self {
        Self {
            desktop: default_true(),
            gap: default_gap(),
            layout: default_layout(),
            taskbar: default_taskbar(),
            taskbar_height: default_taskbar_height(),
            auto_tile: default_true(),
            instant_first_layout: default_true(),
            auto_float_utility_windows: default_true(),
            respect_window_size_constraints: default_true(),
            hide_native_taskbar: default_true(),
            tray: default_tray(),
            launch_startup_apps: default_true(),
            outer_gap: None,
            filter_virtual_desktop: default_filter_vd(),
            workspaces: default_workspaces(),
            master_ratio: default_master_ratio(),
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

impl PanelConfig {
    pub fn margins(&self) -> [i32; 4] {
        self.margin.unwrap_or([0; 4]).map(|value| value.max(0))
    }

    pub fn edge_consumption(&self) -> i32 {
        let [top, right, bottom, left] = self.margins();
        match self.position.as_str() {
            "left" | "right" => left + self.height + right,
            _ => top + self.height + bottom,
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
                    "AltDWM_Panel".into(),
                    "AltDWM_CommandCenter".into(),
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
                // Users can set keys = "Win+Shift+R" etc. in config.toml if they prefer — parser supports Win/Ctrl/Alt/Shift combos
                KeybindConfig {
                    keys: "Alt+Shift+R".into(),
                    action: "retile".into(),
                    description: Some("Retile windows".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+T".into(),
                    action: "toggle_tiling".into(),
                    description: Some("Toggle tiling".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+Space".into(),
                    action: "command_center".into(),
                    description: Some("Open AltDWM command center".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+Q".into(),
                    action: "quit".into(),
                    description: Some("Quit AltDWM".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+G".into(),
                    action: "set_layout(\"Grid\")".into(),
                    description: Some("Grid layout".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+M".into(),
                    action: "set_layout(\"Monocle\")".into(),
                    description: None,
                },
                KeybindConfig {
                    keys: "Alt+Shift+F".into(),
                    action: "set_layout(\"Floating\")".into(),
                    description: None,
                },
                KeybindConfig {
                    keys: "Alt+Shift+S".into(),
                    action: "set_layout(\"MasterStack\")".into(),
                    description: None,
                },
                KeybindConfig {
                    keys: "Alt+Shift+C".into(),
                    action: "reload_config".into(),
                    description: Some("Hot-reload config".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+J".into(),
                    action: "focus_next()".into(),
                    description: Some("Focus next window".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+K".into(),
                    action: "focus_prev()".into(),
                    description: Some("Focus prev window".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+H".into(),
                    action: "focus_prev()".into(),
                    description: Some("Focus prev (left)".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+L".into(),
                    action: "focus_next()".into(),
                    description: Some("Focus next (right)".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+Y".into(),
                    action: "toggle_floating()".into(),
                    description: Some("Toggle floating for focused".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+N".into(),
                    action: "move_to_next_monitor()".into(),
                    description: Some("Move window to next monitor".into()),
                },
                KeybindConfig {
                    keys: "Alt+Shift+P".into(),
                    action: "move_to_prev_monitor()".into(),
                    description: Some("Move window to prev monitor".into()),
                },
            ],
            layouts: HashMap::new(),
            theme: Theme::default(),
        }
    }
}

// ------------------------------------------------------------------
// Config helpers
// ------------------------------------------------------------------

impl RuleConfig {
    pub fn compile_regexes(&mut self) {
        if let Some(rx) = &self.match_class_regex {
            match Regex::new(rx) {
                Ok(re) => self.compiled_class_regex = Some(re),
                Err(e) => eprintln!("[config] invalid match_class_regex '{}': {}", rx, e),
            }
        }
        if let Some(rx) = &self.match_title_regex {
            match Regex::new(rx) {
                Ok(re) => self.compiled_title_regex = Some(re),
                Err(e) => eprintln!("[config] invalid match_title_regex '{}': {}", rx, e),
            }
        }
    }
}

impl Config {
    pub fn normalize(&mut self) {
        self.general.gap = self.general.gap.max(0);
        self.general.taskbar_height = self.general.taskbar_height.max(1);
        self.general.outer_gap = self.general.outer_gap.map(|gap| gap.max(0));
        for panel in &mut self.panels {
            panel.height = panel.height.max(1);
            panel.position.make_ascii_lowercase();
            panel.margin = panel.margin.map(|values| values.map(|value| value.max(0)));
        }
        for widget in &mut self.widgets {
            widget.width = widget.width.map(|width| width.max(0));
            widget.interval = widget.interval.map(|interval| interval.max(16));
        }
    }

    pub fn compile_regexes(&mut self) {
        for r in &mut self.rules {
            r.compile_regexes();
        }
    }

    /// Names already reported as unknown.
    ///
    /// `layout_enum` is called on every retile, so an unadorned `eprintln` here
    /// printed the same complaint several times a second for as long as the typo
    /// stayed in the configuration.
    fn warn_unknown_layout_once(name: &str) {
        use std::collections::HashSet;
        use std::sync::{LazyLock, Mutex};
        static WARNED: LazyLock<Mutex<HashSet<String>>> =
            LazyLock::new(|| Mutex::new(HashSet::new()));
        if WARNED
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(name.to_string())
        {
            eprintln!("[config] unknown layout '{name}' -> MasterStack");
        }
    }

    pub fn layout_enum(&self) -> Layout {
        match self.general.layout.to_lowercase().as_str() {
            "grid" => Layout::Grid,
            "monocle" => Layout::Monocle,
            "floating" => Layout::Floating,
            "masterstack" | "master" | "bsp" | "tiling" => Layout::MasterStack,
            other => {
                if self
                    .layouts
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case(other))
                {
                    // custom layout — handled by try_compute_custom, keep MasterStack as fallback enum
                } else {
                    Self::warn_unknown_layout_once(&self.general.layout);
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
        if self.general.gap < 0 {
            warns.push(format!(
                "general.gap cannot be negative ({})",
                self.general.gap
            ));
        }
        if self.general.outer_gap.is_some_and(|gap| gap < 0) {
            warns.push("general.outer_gap cannot be negative".to_string());
        }
        if self.general.taskbar_height <= 0 {
            warns.push(format!(
                "general.taskbar_height must be positive ({})",
                self.general.taskbar_height
            ));
        }
        if self.theme.font_size <= 0 {
            warns.push(format!(
                "theme.font_size must be positive ({})",
                self.theme.font_size
            ));
        }
        if !(100..=900).contains(&self.theme.font_weight) {
            warns.push("theme.font_weight must be between 100 and 900".to_string());
        }
        if !(100..=900).contains(&self.theme.strong_font_weight) {
            warns.push("theme.strong_font_weight must be between 100 and 900".to_string());
        }
        if self.theme.rounding < 0 {
            warns.push("theme.rounding cannot be negative".to_string());
        }
        if !(1..=crate::workspace::MAX_WORKSPACES).contains(&self.general.workspaces) {
            warns.push(format!(
                "general.workspaces must be between 1 and {} ({})",
                crate::workspace::MAX_WORKSPACES,
                self.general.workspaces
            ));
        }
        if !(0.1..=0.9).contains(&self.general.master_ratio) {
            warns.push(format!(
                "general.master_ratio must be between 0.1 and 0.9 ({})",
                self.general.master_ratio
            ));
        }
        // Same reasoning as the layout name below: an unrecognised value falls
        // back to `auto`, which is indistinguishable from the setting being
        // ignored unless `--check-config` says so.
        if !matches!(
            self.general.tray.trim().to_ascii_lowercase().as_str(),
            "auto"
                | "native"
                | "host"
                | "shell"
                | "explorer"
                | "uia"
                | "mirror"
                | "off"
                | "none"
                | "disabled"
        ) {
            warns.push(format!(
                "general.tray '{}' is not one of auto|native|explorer|off — using auto",
                self.general.tray
            ));
        }
        // A typo'd layout name silently became MasterStack, which looks like
        // AltDWM ignoring the setting. `--check-config` should say so.
        let layout = self.general.layout.to_lowercase();
        let known_builtin = matches!(
            layout.as_str(),
            "grid" | "monocle" | "floating" | "masterstack" | "master" | "bsp" | "tiling"
        );
        let known_custom = self
            .layouts
            .keys()
            .any(|name| name.eq_ignore_ascii_case(&self.general.layout));
        if !known_builtin && !known_custom {
            warns.push(format!(
                "general.layout '{}' is neither a built-in layout nor a key in [layouts]",
                self.general.layout
            ));
        }
        let mut panel_names = std::collections::HashSet::new();
        for p in &self.panels {
            if p.name.trim().is_empty() {
                warns.push("panel name cannot be empty".to_string());
            } else if !panel_names.insert(&p.name) {
                warns.push(format!("duplicate panel name '{}'", p.name));
            }
            if !["top", "bottom", "left", "right"].contains(&p.position.as_str()) {
                warns.push(format!(
                    "panel '{}' invalid position '{}'",
                    p.name, p.position
                ));
            }
            if p.height <= 0 {
                warns.push(format!(
                    "panel '{}' height must be positive ({})",
                    p.name, p.height
                ));
            }
            if p.margins().iter().any(|margin| *margin < 0) {
                warns.push(format!("panel '{}' has a negative margin", p.name));
            }
            if p.widgets.is_empty() {
                warns.push(format!("panel '{}' declares no widgets", p.name));
            }
            for w in &p.widgets {
                if self.widget_by_name(w).is_none() && !is_builtin_widget(w) {
                    warns.push(format!(
                        "panel '{}' references unknown widget '{}'",
                        p.name, w
                    ));
                }
            }
        }
        let mut widget_names = std::collections::HashSet::new();
        for widget in &self.widgets {
            if widget.name.trim().is_empty() {
                warns.push("widget name cannot be empty".to_string());
            }
            if !widget_names.insert(&widget.name) {
                warns.push(format!("duplicate widget name '{}'", widget.name));
            }
            if widget.extra.contains_key("tooltip") {
                warns.push(format!(
                    "widget '{}' uses unsupported key 'tooltip'",
                    widget.name
                ));
            }
            if let Some(max_len) = widget
                .extra
                .get("max_len")
                .and_then(toml::Value::as_integer)
            {
                if max_len < 0 {
                    warns.push(format!("widget '{}' has a negative max_len", widget.name));
                }
            }
        }
        let mut keybinds = std::collections::HashSet::new();
        for keybind in &self.keybinds {
            let normalized = keybind.keys.to_ascii_lowercase();
            if !keybinds.insert(normalized) {
                warns.push(format!("duplicate keybind '{}'", keybind.keys));
            }
            if keybind.action.trim().is_empty() {
                warns.push(format!("keybind '{}' has an empty action", keybind.keys));
            }
        }
        for (index, rule) in self.rules.iter().enumerate() {
            if let Some(pattern) = &rule.match_class_regex {
                if let Err(error) = Regex::new(pattern) {
                    warns.push(format!("invalid class regex '{}': {}", pattern, error));
                }
            }
            if let Some(pattern) = &rule.match_title_regex {
                if let Err(error) = Regex::new(pattern) {
                    warns.push(format!("invalid title regex '{}': {}", pattern, error));
                }
            }
            // A rule with no conditions can never match, so it silently does
            // nothing — which reads as a rule engine that ignores the config.
            let has_condition = rule.match_class.is_some()
                || rule.match_class_regex.is_some()
                || rule.match_title.is_some()
                || rule.match_title_regex.is_some()
                || rule.match_process.is_some();
            if !has_condition {
                warns.push(format!(
                    "rule #{} has no match_class/match_title/match_process condition, so it matches nothing",
                    index + 1
                ));
            }
            if rule
                .opacity
                .is_some_and(|value| !(0.0..=1.0).contains(&value))
            {
                warns.push(format!(
                    "rule #{} opacity must be between 0.0 and 1.0",
                    index + 1
                ));
            }
        }
        for (name, layout) in &self.layouts {
            if layout.script.as_deref().is_none_or(str::is_empty) {
                warns.push(format!("custom layout '{}' has no script", name));
            }
        }
        warns
    }
}

/// `general.taskbar = true` with no `[[panels]]` declared is a request for a
/// bar, not a request for a second bar implementation. Synthesising a panel
/// here means the default shell goes through the same per-monitor placement,
/// DPI scaling, and widget pipeline as a hand-written one.
pub fn ensure_default_bar(cfg: &mut Config) {
    if !cfg.general.taskbar || !cfg.panels.is_empty() {
        return;
    }
    cfg.panels.push(PanelConfig {
        name: "taskbar".into(),
        position: "bottom".into(),
        height: cfg.general.taskbar_height.max(24),
        monitor: "all".into(),
        margin: None,
        background: None,
        widgets: vec![
            "launcher".into(),
            "layout".into(),
            "window_list".into(),
            "system".into(),
            "tray".into(),
            "power_menu".into(),
            "clock".into(),
        ],
        extra: HashMap::new(),
    });
}

pub fn builtin_widget_config(name: &str) -> Option<WidgetConfig> {
    let widget_type = match name {
        "spacer" | "workspaces" | "layout" | "window_title" | "tray" | "clock" | "launcher"
        | "window_list" | "volume" | "battery" | "network" | "input" | "system" | "power_menu" => {
            name
        }
        _ => return None,
    };
    Some(WidgetConfig {
        widget_type: widget_type.into(),
        name: name.into(),
        format: None,
        interval: None,
        script: crate::scripted_widget::builtin_script_path(widget_type),
        action: None,
        command: None,
        label: None,
        icon: None,
        width: None,
        extra: HashMap::new(),
    })
}

fn is_builtin_widget(name: &str) -> bool {
    builtin_widget_config(name).is_some()
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
            if p.exists() {
                return Some(p);
            }
        }
    }
    if let Some(cfg_dir) = dirs::config_dir() {
        let p = cfg_dir.join("AltDWM").join("config.toml");
        if p.exists() {
            return Some(p);
        }
    }
    let cwd = PathBuf::from("config.toml");
    if cwd.exists() {
        return Some(cwd);
    }
    if let Some(cfg_dir) = dirs::config_dir() {
        return Some(cfg_dir.join("AltDWM").join("config.toml"));
    }
    Some(cwd)
}

pub fn default_config_path() -> PathBuf {
    find_config_path(None).unwrap_or_else(|| PathBuf::from("config.toml"))
}

pub fn load_from_path(path: &Path) -> Result<Config, String> {
    let data =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let mut cfg =
        toml::from_str::<Config>(&data).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    cfg.normalize();
    cfg.compile_regexes();
    Ok(cfg)
}

pub fn load_or_default(explicit: Option<&Path>) -> (Config, Option<PathBuf>) {
    let path = find_config_path(explicit);
    if let Some(ref p) = path {
        if p.exists() {
            match load_from_path(p) {
                Ok(cfg) => {
                    println!("[config] loaded {}", p.display());
                    for w in cfg.validate() {
                        eprintln!("[config] warn: {}", w);
                    }
                    return (cfg, Some(p.clone()));
                }
                Err(e) => eprintln!(
                    "[config] failed to load {}: {} -> using defaults",
                    p.display(),
                    e
                ),
            }
        } else {
            println!("[config] no config at {} -> using defaults", p.display());
        }
    }
    (Config::default(), path)
}

/// Load an existing configuration without falling back. Runtime reloads and
/// `--check-config` use this so a transient parse error cannot replace a
/// working shell configuration with defaults.
pub fn load_existing(explicit: Option<&Path>) -> Result<(Config, PathBuf), String> {
    let path = find_config_path(explicit).ok_or_else(|| "no config path available".to_string())?;
    if !path.exists() {
        return Err(format!("config does not exist: {}", path.display()));
    }
    load_from_path(&path).map(|cfg| (cfg, path))
}

pub fn save_to_path(cfg: &Config, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }
    let s = toml::to_string_pretty(cfg).map_err(|e| format!("serialize: {}", e))?;
    std::fs::write(path, s).map_err(|e| format!("write {}: {}", path.display(), e))?;
    println!("[config] wrote {}", path.display());
    Ok(())
}

/// Example that demonstrates full DSL — used by --generate-config
pub fn example_config_with_panels() -> Config {
    let mut cfg = Config::default();
    cfg.general.gap = 8;
    cfg.general.layout = "MasterStack".into();
    // add sample panels/widgets/rules so generated file is illustrative
    cfg.panels = vec![PanelConfig {
        name: "shell".into(),
        position: "bottom".into(),
        height: 58,
        monitor: "all".into(),
        margin: Some([0, 8, 8, 8]),
        background: None,
        widgets: vec![
            "launcher".into(),
            "layout".into(),
            "window_list".into(),
            "cpu".into(),
            "tray".into(),
            "power_menu".into(),
            "clock".into(),
        ],
        extra: HashMap::new(),
    }];
    cfg.widgets = vec![
        WidgetConfig {
            widget_type: "launcher".into(),
            name: "launcher".into(),
            format: None,
            interval: None,
            script: crate::scripted_widget::builtin_script_path("launcher"),
            action: None,
            command: None,
            label: Some("AltDWM".into()),
            icon: None,
            width: Some(120),
            extra: HashMap::new(),
        },
        WidgetConfig {
            widget_type: "layout".into(),
            name: "layout".into(),
            format: None,
            interval: None,
            script: crate::scripted_widget::builtin_script_path("layout"),
            action: None,
            command: None,
            label: None,
            icon: None,
            width: Some(216),
            extra: HashMap::new(),
        },
        WidgetConfig {
            widget_type: "window_list".into(),
            name: "window_list".into(),
            format: None,
            interval: None,
            script: crate::scripted_widget::builtin_script_path("window_list"),
            action: None,
            command: None,
            label: None,
            icon: None,
            width: None,
            extra: HashMap::new(),
        },
        WidgetConfig {
            widget_type: "tray".into(),
            name: "tray".into(),
            format: None,
            interval: None,
            script: crate::scripted_widget::builtin_script_path("tray"),
            action: None,
            command: None,
            label: None,
            icon: None,
            // No fixed width: the tray sizes itself to however many icons the
            // session actually has.
            width: None,
            extra: HashMap::new(),
        },
        WidgetConfig {
            widget_type: "power_menu".into(),
            name: "power_menu".into(),
            format: None,
            interval: None,
            script: crate::scripted_widget::builtin_script_path("power_menu"),
            action: None,
            command: None,
            label: None,
            icon: None,
            width: None,
            extra: HashMap::new(),
        },
        WidgetConfig {
            widget_type: "clock".into(),
            name: "clock".into(),
            format: Some("%H:%M".into()),
            interval: Some(1000),
            script: crate::scripted_widget::builtin_script_path("clock"),
            action: None,
            command: None,
            label: None,
            icon: None,
            width: Some(136),
            extra: HashMap::new(),
        },
        WidgetConfig {
            widget_type: "custom".into(),
            name: "cpu".into(),
            format: None,
            interval: Some(2000),
            script: Some("scripts/cpu.rhai".into()),
            action: None,
            command: None,
            label: None,
            icon: None,
            width: Some(150),
            extra: HashMap::new(),
        },
    ];
    cfg.rules = vec![
        RuleConfig {
            // match_class is exact; the wildcard is what makes this fire against
            // Spotify's real class (SpotifyMainWindow).
            match_class: Some("*Spotify*".into()),
            match_title: None,
            match_process: None,
            match_class_regex: None,
            match_title_regex: None,
            compiled_class_regex: None,
            compiled_title_regex: None,
            monitor: None,
            floating: Some(true),
            opacity: None,
            layout: None,
            on_create: None,
            extra: HashMap::new(),
        },
        RuleConfig {
            match_class: None,
            match_title: None,
            match_process: Some("steamwebhelper.exe".into()),
            match_class_regex: None,
            match_title_regex: Some("(?i)(friends list|steam chat|friends & chat)".into()),
            compiled_class_regex: None,
            compiled_title_regex: None,
            monitor: None,
            floating: Some(true),
            opacity: None,
            layout: None,
            on_create: None,
            extra: HashMap::new(),
        },
    ];
    cfg.layouts.insert(
        "spiral".into(),
        LayoutConfig {
            script: Some("scripts/spiral.rhai".into()),
            gap: None,
            extra: HashMap::new(),
        },
    );
    // To use custom layout: set general.layout = "spiral"
    cfg
}

#[cfg(test)]
mod tests {
    use super::{load_existing, Config, KeybindConfig, PanelConfig};

    #[test]
    fn normalization_clamps_geometry_and_intervals() {
        let mut config = Config::default();
        config.general.gap = -4;
        config.general.outer_gap = Some(-2);
        config.panels.push(PanelConfig {
            position: "RIGHT".into(),
            height: -10,
            margin: Some([-1, 2, -3, 4]),
            ..PanelConfig::default()
        });
        config.normalize();
        assert_eq!(config.general.gap, 0);
        assert_eq!(config.general.outer_gap, Some(0));
        assert_eq!(config.panels[0].position, "right");
        assert_eq!(config.panels[0].height, 1);
        assert_eq!(config.panels[0].margin, Some([0, 2, 0, 4]));
    }

    #[test]
    fn missing_instant_first_layout_setting_defaults_to_enabled() {
        let config: Config = toml::from_str("[general]\nauto_tile = true\n").unwrap();
        assert!(config.general.instant_first_layout);
        assert!(config.general.auto_float_utility_windows);
        assert!(config.general.respect_window_size_constraints);
    }

    #[test]
    fn missing_desktop_setting_defaults_to_enabled() {
        let config: super::Config = toml::from_str("[general]\ngap = 12").unwrap();
        assert!(config.general.desktop);
    }

    #[test]
    fn validation_rejects_duplicate_keybinds_and_bad_regex() {
        let mut config = Config::default();
        config.keybinds.push(KeybindConfig {
            keys: "alt+shift+r".into(),
            action: "retile".into(),
            description: None,
        });
        config.rules.push(super::RuleConfig {
            match_class_regex: Some("[".into()),
            ..Default::default()
        });
        let warnings = config.validate().join("\n");
        assert!(warnings.contains("duplicate keybind"));
        assert!(warnings.contains("invalid class regex"));
    }

    #[test]
    fn validation_reports_out_of_range_and_unknown_values() {
        let mut config = Config::default();
        config.general.layout = "Gird".into();
        config.general.workspaces = 12;
        config.general.master_ratio = 1.5;
        config.general.gap = -4;
        let warns = config.validate();
        let joined = warns.join(" | ");
        // A typo'd layout silently became MasterStack, which reads as AltDWM
        // ignoring the setting rather than as a mistake in the file.
        assert!(joined.contains("general.layout 'Gird'"), "{joined}");
        assert!(joined.contains("general.workspaces"), "{joined}");
        assert!(joined.contains("general.master_ratio"), "{joined}");
        assert!(joined.contains("general.gap"), "{joined}");
    }

    #[test]
    fn a_custom_layout_name_is_not_reported_as_unknown() {
        let mut config = Config::default();
        config.general.layout = "spiral".into();
        config.layouts.insert(
            "spiral".into(),
            crate::config::LayoutConfig {
                script: Some("scripts/spiral.rhai".into()),
                gap: None,
                ..Default::default()
            },
        );
        assert!(
            !config
                .validate()
                .iter()
                .any(|w| w.contains("general.layout")),
            "a layout declared in [layouts] is legitimate"
        );
    }

    #[test]
    fn strict_load_rejects_invalid_toml() {
        let path = std::env::temp_dir().join(format!(
            "alt-dwm-invalid-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::write(&path, "[general\ngap = nope").expect("write fixture");
        let result = load_existing(Some(&path));
        let _ = std::fs::remove_file(path);
        assert!(result.is_err());
    }
}

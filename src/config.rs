use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::layout::Layout;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub ignore: Ignore,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct General {
    /// gap between windows (inner + outer uniform for MVP)
    #[serde(default = "default_gap")]
    pub gap: i32,
    /// initial layout name
    #[serde(default = "default_layout")]
    pub layout: String,
    #[serde(default = "default_taskbar")]
    pub taskbar: bool,
    #[serde(default = "default_taskbar_height")]
    pub taskbar_height: i32,
    /// auto-tile on window events
    #[serde(default = "default_true")]
    pub auto_tile: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Ignore {
    /// class names to never tile (exact match)
    #[serde(default)]
    pub classes: Vec<String>,
    /// substring match on title to ignore
    #[serde(default)]
    pub titles: Vec<String>,
    /// process exe names to ignore (future, not used yet)
    #[serde(default)]
    pub processes: Vec<String>,
}

fn default_gap() -> i32 { 8 }
fn default_taskbar() -> bool { true }
fn default_taskbar_height() -> i32 { 40 }
fn default_true() -> bool { true }
fn default_layout() -> String { "MasterStack".to_string() }

impl Default for General {
    fn default() -> Self {
        Self {
            gap: default_gap(),
            layout: default_layout(),
            taskbar: default_taskbar(),
            taskbar_height: default_taskbar_height(),
            auto_tile: default_true(),
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
        }
    }
}

impl Config {
    pub fn layout_enum(&self) -> Layout {
        match self.general.layout.to_lowercase().as_str() {
            "grid" => Layout::Grid,
            "monocle" => Layout::Monocle,
            "floating" => Layout::Floating,
            "masterstack" | "master" | "bsp" | "tiling" => Layout::MasterStack,
            _ => {
                eprintln!("[config] unknown layout '{}' -> MasterStack", self.general.layout);
                Layout::MasterStack
            }
        }
    }

    pub fn set_layout(&mut self, l: Layout) {
        self.general.layout = l.name().to_string();
    }
}

// --- path discovery ---

/// Search order:
/// 1. explicit --config path
/// 2. ./config.toml (next to exe)
/// 3. %APPDATA%/AltDWM/config.toml  (dirs::config_dir)
/// 4. ~/.config/altdwm/config.toml (dirs::config_dir fallback)
/// 5. ./config.toml (cwd)
pub fn find_config_path(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        if p.exists() {
            return Some(p.to_path_buf());
        }
        // explicit given but not found -> still return that path so caller can create there
        return Some(p.to_path_buf());
    }

    // next to exe
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("config.toml");
            if p.exists() {
                return Some(p);
            }
            // also check AltDWM/config.toml next to exe dir? skip
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

    // default create location: %APPDATA%/AltDWM/config.toml if that dir exists, else ./config.toml
    if let Some(cfg_dir) = dirs::config_dir() {
        let p = cfg_dir.join("AltDWM").join("config.toml");
        // return this as default even if not exists, so generate there
        return Some(p);
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
                    return (cfg, Some(p.clone()));
                }
                Err(e) => {
                    eprintln!("[config] failed to load {}: {} -> using defaults", p.display(), e);
                }
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
        let cfg = Config::default();
        save_to_path(&cfg, &path)?;
    }
    Ok(path)
}

use std::path::Path;
use windows::Win32::Foundation::RECT;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Layout {
    /// Master on left 60%, stack on right
    MasterStack,
    /// Grid - auto rows/cols
    Grid,
    /// Fullscreen monocle (one window maximized)
    Monocle,
    /// Floating - do nothing
    Floating,
}

impl Layout {
    pub fn name(&self) -> &'static str {
        match self {
            Layout::MasterStack => "MasterStack",
            Layout::Grid => "Grid",
            Layout::Monocle => "Monocle",
            Layout::Floating => "Floating",
        }
    }
}

/// Compute tiled rectangles for `n` windows within `area`
/// Returns vec of RECTs in same order as input windows
pub fn compute_layout(n: usize, area: RECT, gap: i32, layout: Layout) -> Vec<RECT> {
    if n == 0 {
        return Vec::new();
    }
    match layout {
        Layout::Floating => Vec::new(),
        Layout::Monocle => vec![shrink_rect(area, gap); n],
        Layout::Grid => grid_layout(n, area, gap),
        Layout::MasterStack => master_stack_layout(n, area, gap),
    }
}

/// Try to compute via custom Rhai layout script if `general.layout` names a key in `layouts`
/// Script must define `fn layout(n, left, top, right, bottom, gap)` returning array of maps with left/top/right/bottom
pub fn try_compute_custom(n: usize, area: RECT, gap: i32, cfg: &crate::config::Config) -> Option<Vec<RECT>> {
    let name = cfg.general.layout.as_str();
    let lc = cfg.layouts.get(name)?;
    let script_path = lc.script.as_deref()?;
    // resolve script path: try as given, then relative to config dir, then exe dir, then cwd
    let candidate_paths = {
        let mut v = vec![std::path::PathBuf::from(script_path)];
        if let Some(cfg_path) = crate::CONFIG_PATH.lock().unwrap().as_ref().and_then(|p| p.parent().map(|p| p.to_path_buf())) {
            v.push(cfg_path.join(script_path));
        }
        if let Ok(exe) = std::env::current_exe().and_then(|p| Ok(p.parent().map(|p| p.to_path_buf()).unwrap_or_default())) {
            v.push(exe.join(script_path));
        }
        v.push(std::path::PathBuf::from("scripts").join(script_path));
        v
    };
    let script_code = candidate_paths.iter().find_map(|p| std::fs::read_to_string(p).ok());
    let code = match script_code {
        Some(c) => c,
        None => {
            eprintln!("[layout] custom '{}' script not found: {} (tried {:?})", name, script_path, candidate_paths);
            return None;
        }
    };
    let engine = crate::scripting::engine().lock().ok()?;
    let mut scope = rhai::Scope::new();
    let ast = match engine.compile(&code) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[layout] custom '{}' compile error: {}", name, e);
            return None;
        }
    };
    if let Err(e) = engine.eval_ast_with_scope::<()>(&mut scope, &ast) {
        eprintln!("[layout] custom '{}' eval error: {}", name, e);
        return None;
    }
    // call fn layout(n, left, top, right, bottom, gap)
    let res: Result<rhai::Array, _> = engine.call_fn(
        &mut scope,
        &ast,
        "layout",
        (n as i64, area.left as i64, area.top as i64, area.right as i64, area.bottom as i64, gap as i64),
    );
    match res {
        Ok(arr) => {
            let mut rects = Vec::with_capacity(arr.len());
            for v in arr {
                if let Some(map) = v.clone().try_cast::<rhai::Map>() {
                    let left = map.get("left").and_then(|d| d.as_int().ok()).unwrap_or(area.left as i64) as i32;
                    let top = map.get("top").and_then(|d| d.as_int().ok()).unwrap_or(area.top as i64) as i32;
                    let right = map.get("right").and_then(|d| d.as_int().ok()).unwrap_or(area.right as i64) as i32;
                    let bottom = map.get("bottom").and_then(|d| d.as_int().ok()).unwrap_or(area.bottom as i64) as i32;
                    rects.push(RECT { left, top, right, bottom });
                } else if let Some(arr2) = v.try_cast::<rhai::Array>() {
                    // allow [left, top, right, bottom] array
                    if arr2.len() == 4 {
                        let l = arr2[0].as_int().unwrap_or(area.left as i64) as i32;
                        let t = arr2[1].as_int().unwrap_or(area.top as i64) as i32;
                        let r = arr2[2].as_int().unwrap_or(area.right as i64) as i32;
                        let b = arr2[3].as_int().unwrap_or(area.bottom as i64) as i32;
                        rects.push(RECT { left: l, top: t, right: r, bottom: b });
                    }
                }
            }
            if rects.len() != n {
                eprintln!("[layout] custom '{}' returned {} rects for {} windows, padding/truncating", name, rects.len(), n);
                // pad or truncate to n
                while rects.len() < n { rects.push(shrink_rect(area, gap)); }
                rects.truncate(n);
            }
            Some(rects)
        }
        Err(e) => {
            eprintln!("[layout] custom '{}' call error: {} — is fn layout(n,left,top,right,bottom,gap) defined?", name, e);
            None
        }
    }
}

fn shrink_rect(r: RECT, gap: i32) -> RECT {
    RECT {
        left: r.left + gap,
        top: r.top + gap,
        right: r.right - gap,
        bottom: r.bottom - gap,
    }
}

fn master_stack_layout(n: usize, area: RECT, gap: i32) -> Vec<RECT> {
    if n == 1 {
        return vec![shrink_rect(area, gap / 2)];
    }

    let width = area.right - area.left;
    let height = area.bottom - area.top;

    let master_w = width * 60 / 100 - gap / 2;
    let stack_w = width - master_w - gap * 2;

    let mut rects = Vec::with_capacity(n);

    rects.push(RECT {
        left: area.left + gap,
        top: area.top + gap,
        right: area.left + gap + master_w,
        bottom: area.bottom - gap,
    });

    let stack_x = area.left + gap + master_w + gap;
    let stack_count = n - 1;
    let total_stack_h = height - gap * 2;
    let gap_total = gap * (stack_count as i32 - 1);
    let win_h = (total_stack_h - gap_total) / stack_count as i32;

    for i in 0..stack_count {
        let y = area.top + gap + i as i32 * (win_h + gap);
        let mut bottom = y + win_h;
        if i == stack_count - 1 {
            bottom = area.bottom - gap;
        }
        rects.push(RECT {
            left: stack_x,
            top: y,
            right: stack_x + stack_w,
            bottom,
        });
    }

    rects
}

fn grid_layout(n: usize, area: RECT, gap: i32) -> Vec<RECT> {
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = (n + cols - 1) / cols;

    let width = area.right - area.left;
    let height = area.bottom - area.top;

    let cell_w = (width - gap * (cols as i32 + 1)) / cols as i32;
    let cell_h = (height - gap * (rows as i32 + 1)) / rows as i32;

    let mut rects = Vec::with_capacity(n);
    for i in 0..n {
        let col = i % cols;
        let row = i / cols;
        let left = area.left + gap + col as i32 * (cell_w + gap);
        let top = area.top + gap + row as i32 * (cell_h + gap);
        rects.push(RECT {
            left,
            top,
            right: left + cell_w,
            bottom: top + cell_h,
        });
    }
    rects
}

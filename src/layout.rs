use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;
use windows::Win32::Foundation::RECT;

use crate::ui::split_span;

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
    compute_layout_with_ratio(n, area, gap, layout, current_master_ratio())
}

/// The live master ratio, clamped to something that always leaves both columns
/// usable.
pub fn current_master_ratio() -> f32 {
    let configured = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .general
        .master_ratio;
    sane_master_ratio(configured)
}

/// `f32::clamp` propagates NaN, and TOML will happily parse `master_ratio = nan`,
/// which would otherwise reduce the master column to a single pixel.
pub fn sane_master_ratio(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.1, 0.9)
    } else {
        0.6
    }
}

pub fn compute_layout_with_ratio(
    n: usize,
    area: RECT,
    gap: i32,
    layout: Layout,
    master_ratio: f32,
) -> Vec<RECT> {
    if n == 0 {
        return Vec::new();
    }
    match layout {
        Layout::Floating => Vec::new(),
        Layout::Monocle => vec![shrink_rect(area, gap); n],
        Layout::Grid => grid_layout(n, area, gap),
        Layout::MasterStack => master_stack_layout(n, area, gap, master_ratio),
    }
}

/// The script body is evaluated once when the AST is compiled and the resulting
/// scope is cached with it. Re-evaluating the whole body on every retile, for
/// every monitor, was pure overhead.
type LayoutCacheEntry = (SystemTime, rhai::AST, PathBuf, rhai::Scope<'static>);
type LayoutCache = HashMap<String, LayoutCacheEntry>;

static LAYOUT_CACHE: LazyLock<Mutex<LayoutCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Report a layout problem the first time only.
///
/// These paths run on every retile, for every monitor. A missing script or a
/// script that fails to compile would otherwise print the same message five or
/// ten times a second for as long as the configuration stayed broken, which
/// buries everything else in the log.
fn warn_once(key: String, message: String) {
    use std::collections::HashSet;
    static WARNED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
    if WARNED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(key)
    {
        eprintln!("{message}");
    }
}

/// True when `general.layout` names an entry in `[layouts]` that carries a
/// script. Callers need this to tell "no layout" apart from "a custom layout
/// that happens to share a built-in name".
pub fn has_custom_layout(cfg: &crate::config::Config) -> bool {
    let name = cfg.general.layout.as_str();
    cfg.layouts
        .get(name)
        .or_else(|| {
            cfg.layouts
                .iter()
                .find(|(layout_name, _)| layout_name.eq_ignore_ascii_case(name))
                .map(|(_, layout)| layout)
        })
        .is_some_and(|layout| layout.script.as_deref().is_some_and(|s| !s.is_empty()))
}

/// Try to compute via custom Rhai layout script if `general.layout` names a key in `layouts`
/// Script must define `fn layout(n, left, top, right, bottom, gap)` returning array of maps with left/top/right/bottom
pub fn try_compute_custom(
    n: usize,
    area: RECT,
    gap: i32,
    cfg: &crate::config::Config,
) -> Option<Vec<RECT>> {
    let name = cfg.general.layout.as_str();
    let lc = cfg.layouts.get(name).or_else(|| {
        cfg.layouts
            .iter()
            .find(|(layout_name, _)| layout_name.eq_ignore_ascii_case(name))
            .map(|(_, layout)| layout)
    })?;
    let script_path = lc.script.as_deref()?;
    let layout_gap = lc.gap.unwrap_or(gap).max(0);
    // resolve script path: try as given, then relative to config dir, then exe dir, then cwd
    let candidate_paths = {
        let mut v = vec![PathBuf::from(script_path)];
        if let Some(cfg_path) = crate::CONFIG_PATH
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        {
            v.push(cfg_path.join(script_path));
        }
        if let Ok(exe) =
            std::env::current_exe().map(|p| p.parent().map(|p| p.to_path_buf()).unwrap_or_default())
        {
            v.push(exe.join(script_path));
        }
        v.push(PathBuf::from("scripts").join(script_path));
        v
    };
    let (code_path, code_mtime, code) = {
        let mut found: Option<(PathBuf, SystemTime, String)> = None;
        for p in &candidate_paths {
            if let Ok(meta) = std::fs::metadata(p) {
                if let Ok(mtime) = meta.modified() {
                    if let Ok(c) = std::fs::read_to_string(p) {
                        found = Some((p.clone(), mtime, c));
                        break;
                    }
                }
            }
        }
        // fallback to old find_map (for non-existent but readable)
        if found.is_none() {
            if let Some(c) = candidate_paths
                .iter()
                .find_map(|p| std::fs::read_to_string(p).ok())
            {
                // use first candidate as path with dummy mtime
                found = Some((candidate_paths[0].clone(), SystemTime::UNIX_EPOCH, c));
            }
        }
        match found {
            Some((p, t, c)) => (p, t, c),
            None => {
                warn_once(
                    format!("missing:{name}:{script_path}"),
                    format!(
                        "[layout] custom '{name}' script not found: {script_path} (tried {candidate_paths:?})"
                    ),
                );
                return None;
            }
        }
    };
    // check cache
    let (ast, mut scope) = {
        let mut cache = LAYOUT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let cached =
            cache
                .get(name)
                .and_then(|(cached_mtime, cached_ast, cached_path, cached_scope)| {
                    (*cached_path == code_path && *cached_mtime == code_mtime)
                        .then(|| (cached_ast.clone(), cached_scope.clone()))
                });
        if let Some(cached) = cached {
            cached
        } else {
            let engine = crate::scripting::engine();
            let new_ast = match engine.compile(&code) {
                Ok(a) => a,
                Err(e) => {
                    warn_once(
                        format!("compile:{name}:{code_mtime:?}"),
                        format!("[layout] custom '{name}' compile error: {e}"),
                    );
                    return None;
                }
            };
            let mut new_scope = rhai::Scope::new();
            if let Err(e) = engine.eval_ast_with_scope::<()>(&mut new_scope, &new_ast) {
                warn_once(
                    format!("eval:{name}:{code_mtime:?}"),
                    format!("[layout] custom '{name}' eval error: {e}"),
                );
                return None;
            }
            cache.insert(
                name.to_string(),
                (
                    code_mtime,
                    new_ast.clone(),
                    code_path.clone(),
                    new_scope.clone(),
                ),
            );
            (new_ast, new_scope)
        }
    };
    let engine = crate::scripting::engine();
    // call fn layout(n, left, top, right, bottom, gap)
    let res: Result<rhai::Array, _> = engine.call_fn(
        &mut scope,
        &ast,
        "layout",
        (
            n as i64,
            area.left as i64,
            area.top as i64,
            area.right as i64,
            area.bottom as i64,
            layout_gap as i64,
        ),
    );
    match res {
        Ok(arr) => {
            let mut rects = Vec::with_capacity(arr.len());
            for v in arr {
                if let Some(map) = v.clone().try_cast::<rhai::Map>() {
                    let left = map
                        .get("left")
                        .and_then(|d| d.as_int().ok())
                        .unwrap_or(area.left as i64) as i32;
                    let top = map
                        .get("top")
                        .and_then(|d| d.as_int().ok())
                        .unwrap_or(area.top as i64) as i32;
                    let right = map
                        .get("right")
                        .and_then(|d| d.as_int().ok())
                        .unwrap_or(area.right as i64) as i32;
                    let bottom = map
                        .get("bottom")
                        .and_then(|d| d.as_int().ok())
                        .unwrap_or(area.bottom as i64) as i32;
                    rects.push(RECT {
                        left,
                        top,
                        right,
                        bottom,
                    });
                } else if let Some(arr2) = v.try_cast::<rhai::Array>() {
                    // allow [left, top, right, bottom] array
                    if arr2.len() == 4 {
                        let l = arr2[0].as_int().unwrap_or(area.left as i64) as i32;
                        let t = arr2[1].as_int().unwrap_or(area.top as i64) as i32;
                        let r = arr2[2].as_int().unwrap_or(area.right as i64) as i32;
                        let b = arr2[3].as_int().unwrap_or(area.bottom as i64) as i32;
                        rects.push(RECT {
                            left: l,
                            top: t,
                            right: r,
                            bottom: b,
                        });
                    }
                }
            }
            if rects.len() != n {
                eprintln!(
                    "[layout] custom '{}' returned {} rects for {} windows, padding/truncating",
                    name,
                    rects.len(),
                    n
                );
                // pad or truncate to n
                while rects.len() < n {
                    rects.push(shrink_rect(area, layout_gap));
                }
                rects.truncate(n);
            }
            Some(rects)
        }
        Err(e) => {
            warn_once(
                format!("call:{name}"),
                format!(
                    "[layout] custom '{name}' call error: {e} — is fn layout(n,left,top,right,bottom,gap) defined?"
                ),
            );
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

/// Master column on the left, the rest stacked on the right.
///
/// Both columns are derived from one inner rectangle inset by `gap` on all four
/// sides. The previous arithmetic subtracted the gap budget from the stack width
/// only, which put the stack's right edge exactly on the area boundary — the
/// layout had a gap on its left and none on its right.
fn master_stack_layout(n: usize, area: RECT, gap: i32, master_ratio: f32) -> Vec<RECT> {
    let master_ratio = sane_master_ratio(master_ratio);
    let inner = shrink_rect(area, gap);
    if n <= 1 {
        return vec![inner];
    }
    let width = (inner.right - inner.left).max(1);
    // Split the space that remains once the inner gap is accounted for.
    let content = width - gap;
    let master_w = ((content as f32 * master_ratio).round() as i32).clamp(1, content.max(1));
    let master_right = inner.left + master_w;
    let stack_left = master_right + gap;

    let mut rects = Vec::with_capacity(n);
    rects.push(RECT {
        left: inner.left,
        top: inner.top,
        right: master_right,
        bottom: inner.bottom,
    });
    let height = (inner.bottom - inner.top).max(1);
    for (top, bottom) in split_span(inner.top, height, n - 1, gap) {
        rects.push(RECT {
            left: stack_left,
            top,
            right: inner.right,
            bottom,
        });
    }
    rects
}

/// Row counts for a grid of `n` windows, chosen so the last row is filled
/// rather than left with a hole. `ceil(sqrt(n))` columns leaves three windows in
/// a 2x2 with an empty cell; distributing the remainder across rows instead
/// gives 2 + 1, with the lone window spanning the full width.
fn grid_row_counts(n: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    let rows = ((n as f64).sqrt().round() as usize).clamp(1, n);
    let base = n / rows;
    let remainder = n % rows;
    (0..rows)
        .map(|row| base + usize::from(row < remainder))
        .collect()
}

fn grid_layout(n: usize, area: RECT, gap: i32) -> Vec<RECT> {
    let inner = shrink_rect(area, gap);
    let row_counts = grid_row_counts(n);
    if row_counts.is_empty() {
        return Vec::new();
    }
    let height = (inner.bottom - inner.top).max(1);
    let width = (inner.right - inner.left).max(1);
    let rows = split_span(inner.top, height, row_counts.len(), gap);
    let mut rects = Vec::with_capacity(n);
    for (row_index, columns_in_row) in row_counts.iter().enumerate() {
        let (top, bottom) = rows[row_index];
        for (left, right) in split_span(inner.left, width, *columns_in_row, gap) {
            rects.push(RECT {
                left,
                top,
                right,
                bottom,
            });
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::{compute_layout, grid_row_counts, split_span, Layout};
    use windows::Win32::Foundation::RECT;

    const AREA: RECT = RECT {
        left: 0,
        top: 0,
        right: 1000,
        bottom: 800,
    };

    #[test]
    fn a_nonsense_master_ratio_falls_back_instead_of_collapsing_the_column() {
        use super::sane_master_ratio;
        assert_eq!(sane_master_ratio(0.6), 0.6);
        assert_eq!(sane_master_ratio(0.05), 0.1, "clamped, not rejected");
        assert_eq!(sane_master_ratio(2.0), 0.9);
        // TOML parses `nan` happily, and f32::clamp propagates it — which would
        // have reduced the master column to a single pixel.
        assert_eq!(sane_master_ratio(f32::NAN), 0.6);
        assert_eq!(sane_master_ratio(f32::INFINITY), 0.6);
        let rects = compute_layout(2, AREA, 10, Layout::MasterStack);
        let master_width = rects[0].right - rects[0].left;
        assert!(
            master_width > 100,
            "master column collapsed: {master_width}"
        );
    }

    #[test]
    fn split_span_uses_every_pixel() {
        let tracks = split_span(0, 100, 3, 10);
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].0, 0);
        assert_eq!(tracks[2].1, 100, "last track must end on the far edge");
        for pair in tracks.windows(2) {
            assert_eq!(pair[1].0 - pair[0].1, 10, "gap between tracks");
        }
    }

    #[test]
    fn master_stack_gaps_are_symmetric() {
        let gap = 10;
        let rects = compute_layout(3, AREA, gap, Layout::MasterStack);
        assert_eq!(rects.len(), 3);
        // Outer inset is the same on all four sides.
        assert_eq!(rects[0].left - AREA.left, gap);
        assert_eq!(rects[0].top - AREA.top, gap);
        assert_eq!(
            AREA.right - rects[1].right,
            gap,
            "stack must keep its right gap"
        );
        assert_eq!(AREA.bottom - rects[0].bottom, gap);
        assert_eq!(AREA.bottom - rects[2].bottom, gap);
        // Master and stack are separated by exactly one gap.
        assert_eq!(rects[1].left - rects[0].right, gap);
        // Stack members are separated by exactly one gap.
        assert_eq!(rects[2].top - rects[1].bottom, gap);
    }

    #[test]
    fn single_window_uses_the_same_inset_as_many() {
        let gap = 12;
        let one = compute_layout(1, AREA, gap, Layout::MasterStack);
        let many = compute_layout(2, AREA, gap, Layout::MasterStack);
        assert_eq!(one[0].left, many[0].left);
        assert_eq!(one[0].top, many[0].top);
        assert_eq!(one[0].right, AREA.right - gap);
        assert_eq!(one[0].bottom, AREA.bottom - gap);
    }

    #[test]
    fn grid_fills_the_last_row_instead_of_leaving_a_hole() {
        assert_eq!(grid_row_counts(3), vec![2, 1]);
        assert_eq!(grid_row_counts(4), vec![2, 2]);
        assert_eq!(grid_row_counts(5), vec![3, 2]);
        assert_eq!(grid_row_counts(7), vec![3, 2, 2]);
        let rects = compute_layout(3, AREA, 10, Layout::Grid);
        assert_eq!(rects.len(), 3);
        // The lone window on the last row spans the full inner width.
        assert_eq!(rects[2].left, 10);
        assert_eq!(rects[2].right, AREA.right - 10);
    }

    #[test]
    fn crowded_layouts_never_produce_inverted_rects() {
        for n in 1..40usize {
            for layout in [Layout::MasterStack, Layout::Grid, Layout::Monocle] {
                let rects = compute_layout(n, AREA, 8, layout);
                for rect in &rects {
                    assert!(
                        rect.right > rect.left && rect.bottom > rect.top,
                        "n={n} {layout:?} produced {rect:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn monocle_positions_every_window_in_the_same_area() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 1200,
            bottom: 800,
        };
        let rects = compute_layout(3, area, 10, Layout::Monocle);
        assert_eq!(rects.len(), 3);
        assert!(rects.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!((rects[0].left, rects[0].top), (10, 10));
    }
}

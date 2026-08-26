use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, DeferWindowPos, EndDeferWindowPos, EnumWindows, HWND_TOP, SWP_NOACTIVATE,
    SWP_NOZORDER,
};

use crate::layout::{compute_layout, Layout};
use crate::util::is_manageable;

/// Collect all manageable windows, grouped by monitor
pub fn collect_windows(taskbar_hwnd: Option<HWND>) -> Vec<HWND> {
    let mut windows: Vec<HWND> = Vec::new();
    unsafe {
        let ptr = &mut windows as *mut Vec<HWND> as isize;
        let _ = EnumWindows(Some(enum_cb), LPARAM(ptr));
    }
    windows
        .into_iter()
        .filter(|hwnd| is_manageable(*hwnd, taskbar_hwnd))
        .collect()
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let vec = &mut *(lparam.0 as *mut Vec<HWND>);
    vec.push(hwnd);
    BOOL(1)
}

fn is_primary_monitor(mi: &MONITORINFO) -> bool {
    // MONITORINFOF_PRIMARY = 1
    (mi.dwFlags & 1) != 0
}

pub fn get_all_monitors() -> Vec<HMONITOR> {
    let mut mons: Vec<HMONITOR> = Vec::new();
    unsafe extern "system" fn enum_cb(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let v = &mut *(lparam.0 as *mut Vec<HMONITOR>);
        v.push(hmon);
        BOOL(1)
    }
    unsafe {
        let ptr = &mut mons as *mut Vec<HMONITOR> as isize;
        let _ = EnumDisplayMonitors(None, None, Some(enum_cb), LPARAM(ptr));
    }
    mons
}

pub fn hmonitor_for_target(target: &str) -> Option<HMONITOR> {
    let mons = get_all_monitors();
    if mons.is_empty() {
        return None;
    }
    let lower = target.to_lowercase();
    if lower == "primary" || lower == "1" {
        // primary is at (0,0), find it, else first
        for &h in &mons {
            unsafe {
                let mut mi = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(h, &mut mi as *mut _ as *mut _).as_bool()
                    && is_primary_monitor(&mi)
                {
                    return Some(h);
                }
            }
        }
        return Some(mons[0]);
    }
    if let Ok(idx) = lower.parse::<usize>() {
        if idx >= 1 && idx <= mons.len() {
            return Some(mons[idx - 1]);
        }
    }
    if lower == "all" {
        return None;
    } // don't override
      // try substring match on device name via MONITORINFOEXW
    for &h in &mons {
        unsafe {
            let mut ex = MONITORINFOEXW {
                monitorInfo: MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                    ..Default::default()
                },
                szDevice: [0; 32],
            };
            if GetMonitorInfoW(h, &mut ex as *mut _ as *mut _ as *mut MONITORINFO).as_bool() {
                let dev = String::from_utf16_lossy(&ex.szDevice)
                    .trim_matches(char::from(0))
                    .to_string();
                if dev.to_lowercase().contains(&lower) {
                    return Some(h);
                }
            }
        }
    }
    None
}

fn panel_reserves_for_monitor(hmon: HMONITOR, cfg: &crate::config::Config) -> (i32, i32, i32, i32) {
    let mut left = 0;
    let mut top = 0;
    let mut right = 0;
    let mut bottom = 0;
    for panel in &cfg.panels {
        let applies = panel.monitor.eq_ignore_ascii_case("all")
            || hmonitor_for_target(&panel.monitor).is_some_and(|target| target == hmon);
        if !applies {
            continue;
        }
        match panel.position.as_str() {
            "top" => top += panel.edge_consumption(),
            "right" => right += panel.edge_consumption(),
            "bottom" => bottom += panel.edge_consumption(),
            "left" => left += panel.edge_consumption(),
            _ => {}
        }
    }
    (left, top, right, bottom)
}

fn apply_window_chrome(hwnd: HWND, cfg: &crate::config::Config) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE,
    };
    const DWMWCP_ROUND: u32 = 2;
    unsafe {
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as _,
            std::mem::size_of_val(&corner) as u32,
        );
        let border = cfg.theme.border_color();
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border.0 as *const _ as _,
            std::mem::size_of_val(&border) as u32,
        );
    }
}

fn get_work_area_for_hmonitor(
    hmon: HMONITOR,
    top_reserve: i32,
    bottom_reserve: i32,
    taskbar_hwnd: Option<HWND>,
    cfg: &crate::config::Config,
) -> RECT {
    unsafe {
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut mi as *mut _ as *mut _).as_bool() {
            return RECT {
                left: 0,
                top: top_reserve,
                right: 1920,
                bottom: 1080 - bottom_reserve,
            };
        }
        let mut work = mi.rcWork;
        let is_primary = is_primary_monitor(&mi);
        let (panel_left, panel_top, panel_right, panel_bottom) = if cfg.panels.is_empty() {
            (0, top_reserve, 0, bottom_reserve)
        } else {
            panel_reserves_for_monitor(hmon, cfg)
        };
        if panel_left > 0 || panel_top > 0 || panel_right > 0 || panel_bottom > 0 {
            let apply = if taskbar_hwnd.is_some() {
                if let Some(tb) = taskbar_hwnd {
                    MonitorFromWindow(tb, MONITOR_DEFAULTTONEAREST) == hmon
                } else {
                    false
                }
            } else {
                !cfg.panels.is_empty() || is_primary
            };
            if apply {
                work.left += panel_left;
                work.top += panel_top;
                work.right -= panel_right;
                work.bottom -= panel_bottom;
            }
        }
        let outer_gap = cfg.general.outer_gap.unwrap_or(0);
        work.left += outer_gap;
        work.top += outer_gap;
        work.right -= outer_gap;
        work.bottom -= outer_gap;
        if work.bottom <= work.top {
            work.bottom = work.top + 100;
        }
        if work.right <= work.left {
            work.right = work.left + 100;
        }
        work
    }
}

/// Tile with explicit top/bottom reserves (for panels DSL)
pub fn tile_windows_reserved(
    taskbar_hwnd: Option<HWND>,
    top_reserve: i32,
    bottom_reserve: i32,
    layout: Layout,
    gap: i32,
) {
    // snapshot config once per tick to avoid repeated locking
    let cfg_snapshot = crate::CURRENT_CONFIG
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let verbose = std::env::var_os("ALT_DWM_VERBOSE").is_some();
    let all_windows = collect_windows(taskbar_hwnd);
    if all_windows.is_empty() {
        return;
    }

    // virtual desktop filter (if enabled)
    let before_vd = all_windows.len();
    let all_windows: Vec<HWND> = if cfg_snapshot.general.filter_virtual_desktop {
        all_windows
            .into_iter()
            .filter(|hwnd| crate::virtual_desktop::is_on_current_desktop(*hwnd))
            .collect()
    } else {
        all_windows
    };
    if verbose && all_windows.len() != before_vd {
        println!(
            "[manager] filtered {} windows not on current virtual desktop",
            before_vd - all_windows.len()
        );
    }
    if all_windows.is_empty() {
        return;
    }

    // apply rules — floating windows are excluded from tiling (including runtime floating via Alt+Shift+Y)
    let (windows, floating): (Vec<_>, Vec<_>) = all_windows.into_iter().partition(|hwnd| {
        !crate::rules::is_floating(*hwnd) && !crate::focus::is_runtime_floating(*hwnd)
    });
    if verbose && !floating.is_empty() {
        println!("[manager] floating {} window(s) per rules:", floating.len());
        for hwnd in &floating {
            let cls = crate::util::get_class_name(*hwnd);
            let title = crate::util::get_window_title(*hwnd);
            println!(
                "  ~ {:?} class={} title=\"{}\" (floating)",
                hwnd.0, cls, title
            );
        }
    }
    if windows.is_empty() {
        if verbose {
            println!("[manager] no tilable windows (all floating)");
        }
        // still apply opacity to floating windows
        for hwnd in &floating {
            if let Some(op) = crate::rules::rule_opacity(*hwnd) {
                if verbose {
                    println!("[manager] opacity {} for {:?}", op, hwnd.0);
                }
                crate::rules::apply_opacity(*hwnd, op);
            }
        }
        return;
    }

    // apply opacity per rules (both tilable and floating)
    for hwnd in windows.iter().chain(floating.iter()) {
        if let Some(op) = crate::rules::rule_opacity(*hwnd) {
            if verbose {
                println!("[manager] opacity {} for {:?}", op, hwnd.0);
            }
            crate::rules::apply_opacity(*hwnd, op);
        }
    }

    let layout_name = cfg_snapshot.general.layout.clone();
    if verbose {
        println!(
            "[manager] tiling {} windows with layout {} gap={} ({} floating skipped)",
            windows.len(),
            layout_name,
            gap,
            floating.len()
        );
    }
    if verbose {
        for hwnd in &windows {
            let cls = crate::util::get_class_name(*hwnd);
            let title = crate::util::get_window_title(*hwnd);
            println!("  - {:?} class={} title=\"{}\"", hwnd.0, cls, title);
        }
    }

    use std::collections::HashMap;
    let mut per_monitor: HashMap<isize, Vec<HWND>> = HashMap::new();
    let mut monitor_rects: HashMap<isize, RECT> = HashMap::new();

    for hwnd in windows {
        // check rule for monitor override
        let target_hmon = if let Some(mon_str) = crate::rules::rule_monitor(hwnd) {
            if let Some(h) = hmonitor_for_target(&mon_str) {
                if verbose {
                    println!(
                        "[manager] {:?} rule monitor='{}' -> hmon 0x{:x}",
                        hwnd.0, mon_str, h.0 as usize
                    );
                }
                h
            } else {
                eprintln!(
                    "[manager] rule monitor '{}' not found for {:?}",
                    mon_str, hwnd.0
                );
                unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }
            }
        } else {
            unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }
        };
        let key = target_hmon.0 as isize;
        per_monitor.entry(key).or_default().push(hwnd);
        monitor_rects.entry(key).or_insert_with(|| {
            get_work_area_for_hmonitor(
                target_hmon,
                top_reserve,
                bottom_reserve,
                taskbar_hwnd,
                &cfg_snapshot,
            )
        });
    }

    let total: usize = per_monitor.values().map(Vec::len).sum();
    let hdwp = unsafe {
        match BeginDeferWindowPos(total as i32) {
            Ok(h) => h,
            Err(e) => {
                println!("[manager] BeginDeferWindowPos failed: {:?}", e);
                return;
            }
        }
    };
    let mut hdwp = hdwp;

    for (mon, wins) in per_monitor {
        let area = monitor_rects.get(&mon).copied().unwrap_or(RECT {
            left: 0,
            top: top_reserve,
            right: 1920,
            bottom: 1080 - bottom_reserve,
        });
        if verbose {
            println!(
                "[manager] monitor 0x{:x} area {:?}",
                mon,
                crate::util::rect_to_string(&area)
            );
        }
        let mut monitor_cfg = cfg_snapshot.clone();
        let mut monitor_layout = layout;
        if let Some(rule_layout) = crate::rules::layout_for_windows(&wins) {
            monitor_cfg.general.layout = rule_layout;
            monitor_layout = monitor_cfg.layout_enum();
        }
        // try custom layout first (if general.layout names a key in layouts with script)
        let rects = if let Some(custom) =
            crate::layout::try_compute_custom(wins.len(), area, gap, &monitor_cfg)
        {
            if verbose {
                println!("[manager] custom layout '{}'", monitor_cfg.general.layout);
            }
            custom
        } else {
            compute_layout(wins.len(), area, gap, monitor_layout)
        };
        for (i, hwnd) in wins.iter().enumerate() {
            let r = if i < rects.len() {
                rects[i]
            } else {
                continue;
            };
            let w = r.right - r.left;
            let h = r.bottom - r.top;
            if w <= 0 || h <= 0 {
                continue;
            }
            apply_window_chrome(*hwnd, &cfg_snapshot);
            if verbose {
                println!(
                    "[manager] -> {:?} => {}x{} @ {},{}",
                    hwnd.0, w, h, r.left, r.top
                );
            }
            unsafe {
                match DeferWindowPos(
                    hdwp,
                    *hwnd,
                    Some(HWND_TOP),
                    r.left,
                    r.top,
                    w,
                    h,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                ) {
                    Ok(h) => hdwp = h,
                    Err(e) => println!("[manager] DeferWindowPos failed for {:?}: {:?}", hwnd.0, e),
                }
            }
        }
    }

    unsafe {
        match EndDeferWindowPos(hdwp) {
            Ok(_) => {
                if verbose {
                    println!("[manager] tiling committed");
                }
            }
            Err(e) => println!("[manager] EndDeferWindowPos failed: {:?}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::panel_reserves_for_monitor;
    use crate::config::{Config, PanelConfig};
    use windows::Win32::Graphics::Gdi::HMONITOR;

    #[test]
    fn reserves_all_four_panel_edges_with_margins() {
        let config = Config {
            panels: [
                ("left", 10, [0, 2, 0, 3]),
                ("top", 20, [4, 0, 5, 0]),
                ("right", 30, [0, 7, 0, 6]),
                ("bottom", 40, [8, 0, 9, 0]),
            ]
            .into_iter()
            .map(|(position, height, margin)| PanelConfig {
                position: position.into(),
                height,
                margin: Some(margin),
                ..PanelConfig::default()
            })
            .collect(),
            ..Config::default()
        };
        let reserves = panel_reserves_for_monitor(HMONITOR::default(), &config);
        assert_eq!(reserves, (15, 29, 43, 57));
    }
}

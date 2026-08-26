use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::core::BOOL;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, DeferWindowPos, EndDeferWindowPos, EnumWindows,
    HWND_TOP, SWP_NOACTIVATE, SWP_NOZORDER, SWP_FRAMECHANGED,
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
    mi.rcMonitor.left == 0 && mi.rcMonitor.top == 0
}

fn get_work_area_for_hwnd(hwnd: HWND, top_reserve: i32, bottom_reserve: i32, taskbar_hwnd: Option<HWND>) -> RECT {
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
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
        // Reserve for panels / taskbar
        // For legacy taskbar (taskbar_hwnd Some), only reserve on that monitor; for panels (None + reserves), reserve on primary / "all"
        if top_reserve > 0 || bottom_reserve > 0 {
            let apply = if taskbar_hwnd.is_some() {
                // legacy: only monitor containing taskbar
                if let Some(tb) = taskbar_hwnd {
                    MonitorFromWindow(tb, MONITOR_DEFAULTTONEAREST) == hmon
                } else { false }
            } else {
                // panels mode: apply to primary (and optionally all if config says "all")
                // For MVP we check config panels; if any panel has monitor=="all", apply to all, else primary only
                let cfg = crate::CURRENT_CONFIG.lock().unwrap();
                let has_all = cfg.panels.iter().any(|p| p.monitor == "all");
                has_all || is_primary
            };
            if apply {
                work.top += top_reserve;
                work.bottom -= bottom_reserve;
            }
        }
        if work.bottom <= work.top {
            work.bottom = work.top + 100;
        }
        if work.right <= work.left {
            work.right = work.left + 100;
        }
        work
    }
}

/// Tile all windows using the given layout.
/// Legacy wrapper — only bottom reserved.
pub fn tile_windows(taskbar_hwnd: Option<HWND>, taskbar_height: i32, layout: Layout, gap: i32) {
    tile_windows_reserved(taskbar_hwnd, 0, taskbar_height, layout, gap)
}

/// Tile with explicit top/bottom reserves (for panels DSL)
pub fn tile_windows_reserved(taskbar_hwnd: Option<HWND>, top_reserve: i32, bottom_reserve: i32, layout: Layout, gap: i32) {
    let all_windows = collect_windows(taskbar_hwnd);
    if all_windows.is_empty() {
        println!("[manager] no manageable windows");
        return;
    }

    // virtual desktop filter (if enabled in config)
    let before_vd = all_windows.len();
    let all_windows: Vec<HWND> = all_windows.into_iter().filter(|hwnd| crate::virtual_desktop::is_on_current_desktop(*hwnd)).collect();
    if all_windows.len() != before_vd {
        println!("[manager] filtered {} windows not on current virtual desktop", before_vd - all_windows.len());
    }
    if all_windows.is_empty() {
        println!("[manager] no windows on current virtual desktop");
        return;
    }

    // apply rules — floating windows are excluded from tiling
    let (windows, floating): (Vec<_>, Vec<_>) = all_windows.into_iter().partition(|hwnd| !crate::rules::is_floating(*hwnd));
    if !floating.is_empty() {
        println!("[manager] floating {} window(s) per rules:", floating.len());
        for hwnd in &floating {
            let cls = crate::util::get_class_name(*hwnd);
            let title = crate::util::get_window_title(*hwnd);
            println!("  ~ {:?} class={} title=\"{}\" (floating)", hwnd.0, cls, title);
        }
    }
    if windows.is_empty() {
        println!("[manager] no tilable windows (all floating)");
        return;
    }

    let layout_name = crate::CURRENT_CONFIG.lock().unwrap().general.layout.clone();
    println!("[manager] tiling {} windows with layout {} gap={} ({} floating skipped)", windows.len(), layout_name, gap, floating.len());
    for hwnd in &windows {
        let cls = crate::util::get_class_name(*hwnd);
        let title = crate::util::get_window_title(*hwnd);
        println!("  - {:?} class={} title=\"{}\"", hwnd.0, cls, title);
    }

    if layout == Layout::Floating {
        println!("[manager] floating - skipping tile");
        return;
    }

    use std::collections::HashMap;
    let mut per_monitor: HashMap<isize, Vec<HWND>> = HashMap::new();
    let mut monitor_rects: HashMap<isize, RECT> = HashMap::new();

    for hwnd in windows {
        unsafe {
            let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let key = hmon.0 as isize;
            per_monitor.entry(key).or_default().push(hwnd);
            if !monitor_rects.contains_key(&key) {
                let area = get_work_area_for_hwnd(hwnd, top_reserve, bottom_reserve, taskbar_hwnd);
                monitor_rects.insert(key, area);
            }
        }
    }

    let total: usize = per_monitor.values().map(|v| v.len()).sum();
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
        let area = monitor_rects.get(&mon).copied().unwrap_or(RECT { left: 0, top: top_reserve, right: 1920, bottom: 1080 - bottom_reserve });
        println!("[manager] monitor 0x{:x} area {:?}", mon, crate::util::rect_to_string(&area));
        // try custom layout first (if general.layout names a key in layouts with script)
        let cfg = crate::CURRENT_CONFIG.lock().unwrap().clone();
        let rects = if let Some(custom) = crate::layout::try_compute_custom(wins.len(), area, gap, &cfg) {
            println!("[manager] custom layout '{}'", cfg.general.layout);
            custom
        } else {
            compute_layout(wins.len(), area, gap, layout)
        };
        let effective_rects = if layout == Layout::Monocle && !rects.is_empty() {
            vec![rects[0]]
        } else {
            rects
        };

        for (i, hwnd) in wins.iter().enumerate() {
            if layout == Layout::Monocle && i != 0 {
                continue;
            }
            let r = if layout == Layout::Monocle {
                effective_rects[0]
            } else {
                if i < effective_rects.len() {
                    effective_rects[i]
                } else {
                    continue;
                }
            };
            let w = r.right - r.left;
            let h = r.bottom - r.top;
            if w <= 0 || h <= 0 {
                continue;
            }
            println!("[manager] -> {:?} => {}x{} @ {},{}", hwnd.0, w, h, r.left, r.top);
            unsafe {
                match DeferWindowPos(
                    hdwp,
                    *hwnd,
                    Some(HWND_TOP),
                    r.left,
                    r.top,
                    w,
                    h,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                ) {
                    Ok(h) => hdwp = h,
                    Err(e) => println!("[manager] DeferWindowPos failed for {:?}: {:?}", hwnd.0, e),
                }
            }
        }
    }

    unsafe {
        match EndDeferWindowPos(hdwp) {
            Ok(_) => println!("[manager] tiling committed"),
            Err(e) => println!("[manager] EndDeferWindowPos failed: {:?}", e),
        }
    }
}

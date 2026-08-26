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

fn get_work_area_for_hwnd(hwnd: HWND, taskbar_height: i32, taskbar_hwnd: Option<HWND>) -> RECT {
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut mi as *mut _ as *mut _).as_bool() {
            return RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080 - taskbar_height,
            };
        }
        let mut work = mi.rcWork;
        // Reserve taskbar space only on monitor that contains taskbar
        if taskbar_height > 0 {
            let mut subtract = false;
            if let Some(tb) = taskbar_hwnd {
                let tb_mon = MonitorFromWindow(tb, MONITOR_DEFAULTTONEAREST);
                if tb_mon == hmon {
                    subtract = true;
                }
            } else {
                // no taskbar window: subtract from primary (origin 0,0)
                subtract = mi.rcMonitor.left == 0 && mi.rcMonitor.top == 0;
            }
            if subtract {
                work.bottom -= taskbar_height;
            }
        }
        if work.bottom <= work.top {
            work.bottom = work.top + 100;
        }
        work
    }
}

/// Tile all windows using the given layout.
/// If taskbar_hwnd is Some, its height is reserved.
pub fn tile_windows(taskbar_hwnd: Option<HWND>, taskbar_height: i32, layout: Layout, gap: i32) {
    let windows = collect_windows(taskbar_hwnd);
    if windows.is_empty() {
        println!("[manager] no manageable windows");
        return;
    }

    println!("[manager] tiling {} windows with layout {} gap={}", windows.len(), layout.name(), gap);
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
                let area = get_work_area_for_hwnd(hwnd, taskbar_height, taskbar_hwnd);
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
        let area = monitor_rects.get(&mon).copied().unwrap_or(RECT { left: 0, top: 0, right: 1920, bottom: 1080 - taskbar_height });
        println!("[manager] monitor 0x{:x} area {:?}", mon, crate::util::rect_to_string(&area));
        let rects = compute_layout(wins.len(), area, gap, layout);
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

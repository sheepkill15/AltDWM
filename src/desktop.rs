//! Windows-style desktop surface for shell-replacement sessions.
//!
//! Explorer normally owns three jobs that are easy to miss when replacing it:
//! painting the configured wallpaper, presenting the user/public Desktop
//! folders, and providing the background/item context menus.  This module owns
//! those jobs without starting Explorer.  One bottom-of-Z-order window spans the
//! virtual screen; desktop items use a vertical Windows grid on the primary
//! display and open through the shell's normal file associations.

use std::collections::HashSet;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, EndPaint, FillRect, InvalidateRect, SelectObject, SetBkMode,
    SetTextColor, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX, DT_WORDBREAK, HDC, PAINTSTRUCT, SRCCOPY,
    TRANSPARENT,
};
use windows::Win32::UI::Shell::{
    FOLDERID_PublicDesktop, SHGetFileInfoW, ShellExecuteW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DrawIconEx, GetClientRect, GetSystemMetrics, KillTimer, SetForegroundWindow, SetTimer,
    SetWindowPos, ShowWindow, TrackPopupMenu, DI_NORMAL, HWND_BOTTOM, MF_SEPARATOR, MF_STRING,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SW_SHOWNA, SW_SHOWNORMAL, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WM_COMMAND,
    WM_CONTEXTMENU, WM_CREATE, WM_DESTROY, WM_DISPLAYCHANGE, WM_ERASEBKGND, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_PAINT, WM_SETTINGCHANGE, WM_TIMER, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP,
};

const CLASS_NAME: windows::core::PCWSTR = w!("AltDWM_Desktop");
const TIMER_REFRESH: usize = 1;
const TIMER_WALLPAPER: usize = 2;
const CMD_OPEN: usize = 1001;
const CMD_REFRESH: usize = 1002;
const CMD_PERSONALIZE: usize = 1003;

const GRID_LEFT: i32 = 12;
const GRID_TOP: i32 = 12;
const CELL_WIDTH: i32 = 96;
const CELL_HEIGHT: i32 = 104;
const ICON_SIZE: i32 = 48;

#[derive(Clone)]
struct DesktopItem {
    path: PathBuf,
    label: String,
    icon: isize,
    rect: RECT,
}

#[derive(Default)]
struct DesktopState {
    hwnd: isize,
    items: Vec<DesktopItem>,
    selected: Option<usize>,
    signature: Vec<(PathBuf, u64, u64)>,
}

static STATE: LazyLock<Mutex<DesktopState>> = LazyLock::new(|| Mutex::new(DesktopState::default()));

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn virtual_screen() -> RECT {
    unsafe {
        let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
        RECT {
            left,
            top,
            right: left + GetSystemMetrics(SM_CXVIRTUALSCREEN),
            bottom: top + GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

fn display_name(path: &Path) -> String {
    let mut name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if path.is_file() {
        if let Some(stem) = path.file_stem() {
            name = stem.to_string_lossy().into_owned();
        }
    }
    name
}

fn public_desktop() -> Option<PathBuf> {
    unsafe {
        let value = windows::Win32::UI::Shell::SHGetKnownFolderPath(
            &FOLDERID_PublicDesktop,
            Default::default(),
            None,
        )
        .ok()?;
        let path = value.to_string().ok().map(PathBuf::from);
        windows::Win32::System::Com::CoTaskMemFree(Some(value.0 as *const c_void));
        path
    }
}

fn desktop_paths() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(path) = dirs::desktop_dir() {
        roots.push(path);
    }
    if let Some(path) = public_desktop() {
        if !roots.iter().any(|root| {
            root.to_string_lossy()
                .eq_ignore_ascii_case(&path.to_string_lossy())
        }) {
            roots.push(path);
        }
    }

    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let attributes = entry
                .metadata()
                .map(|metadata| metadata.file_attributes())
                .unwrap_or(0);
            if attributes
                & (windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN.0
                    | windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_SYSTEM.0)
                != 0
            {
                continue;
            }
            let key = path
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !key.is_empty() && seen.insert(key) {
                paths.push(path);
            }
        }
    }
    paths.sort_by_key(|path| (path.is_file(), display_name(path).to_lowercase()));
    paths
}

fn file_signature(paths: &[PathBuf]) -> Vec<(PathBuf, u64, u64)> {
    paths
        .iter()
        .map(|path| {
            let metadata = std::fs::metadata(path).ok();
            let len = metadata.as_ref().map_or(0, std::fs::Metadata::len);
            let modified = metadata
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_secs());
            (path.clone(), len, modified)
        })
        .collect()
}

fn load_icon(path: &Path) -> isize {
    let path = wide(path.as_os_str());
    let mut info = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(path.as_ptr()),
            Default::default(),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if ok == 0 {
        0
    } else {
        info.hIcon.0 as isize
    }
}

fn release_items(items: &mut Vec<DesktopItem>) {
    for item in items.drain(..) {
        if item.icon != 0 {
            unsafe {
                let _ = DestroyIcon(windows::Win32::UI::WindowsAndMessaging::HICON(
                    item.icon as *mut c_void,
                ));
            }
        }
    }
}

fn refresh_items(force: bool) {
    let paths = desktop_paths();
    let signature = file_signature(&paths);
    let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
    if !force && signature == state.signature {
        return;
    }
    release_items(&mut state.items);
    let screen = virtual_screen();
    // Coordinates in the desktop client start at the virtual-screen origin.
    // The primary monitor begins at screen coordinate (0, 0), even when a
    // secondary display extends left/up into negative coordinates.
    let (left_reserve, top_reserve, _right_reserve, bottom_reserve) =
        crate::manager::hmonitor_for_target("primary")
            .map(|monitor| {
                let config = crate::CURRENT_CONFIG
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                crate::manager::panel_reserves_for_monitor(monitor, &config)
            })
            .unwrap_or_default();
    let primary_left = -screen.left + left_reserve + GRID_LEFT;
    let primary_top = -screen.top + top_reserve + GRID_TOP;
    let primary_height =
        unsafe { GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN) }
            - top_reserve
            - bottom_reserve;
    let rows = ((primary_height - GRID_TOP * 2) / CELL_HEIGHT).max(1) as usize;
    state.items = paths
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let column = index / rows;
            let row = index % rows;
            let left = primary_left + column as i32 * CELL_WIDTH;
            let top = primary_top + row as i32 * CELL_HEIGHT;
            DesktopItem {
                label: display_name(&path),
                icon: load_icon(&path),
                path,
                rect: RECT {
                    left,
                    top,
                    right: left + CELL_WIDTH,
                    bottom: top + CELL_HEIGHT,
                },
            }
        })
        .collect();
    state.signature = signature;
    state.selected = None;
    let hwnd = HWND(state.hwnd as *mut c_void);
    drop(state);
    if !hwnd.0.is_null() {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

fn hit_test(point: POINT) -> Option<usize> {
    let state = STATE.lock().unwrap_or_else(|error| error.into_inner());
    state.items.iter().position(|item| {
        point.x >= item.rect.left
            && point.x < item.rect.right
            && point.y >= item.rect.top
            && point.y < item.rect.bottom
    })
}

fn point_from_lparam(lparam: LPARAM) -> POINT {
    POINT {
        x: lparam.0 as i16 as i32,
        y: (lparam.0 >> 16) as i16 as i32,
    }
}

fn selected_path() -> Option<PathBuf> {
    let state = STATE.lock().unwrap_or_else(|error| error.into_inner());
    state
        .selected
        .and_then(|index| state.items.get(index))
        .map(|item| item.path.clone())
}

fn open_path(hwnd: HWND, path: &Path) {
    let encoded = wide(path.as_os_str());
    unsafe {
        // The desktop uses WS_EX_NOACTIVATE so an ordinary background click
        // does not steal keyboard focus. A launch is different: it is explicit
        // user activation. Briefly making the shell surface foreground gives
        // ShellExecute (including reused URI handlers such as Settings) the
        // foreground permission needed to reveal its destination window.
        let _ = SetForegroundWindow(hwnd);
        let result = ShellExecuteW(
            Some(hwnd),
            w!("open"),
            PCWSTR(encoded.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if result.0 as isize <= 32 {
            eprintln!(
                "[desktop] failed to open {} (code {:?})",
                path.display(),
                result.0
            );
        }
        // Keep the desktop a true background surface even though it briefly
        // brokered foreground activation for the launched application.
        let screen = virtual_screen();
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            screen.left,
            screen.top,
            screen.right - screen.left,
            screen.bottom - screen.top,
            SWP_NOACTIVATE,
        );
    }
}

fn show_context_menu(hwnd: HWND, screen_point: POINT, item: Option<usize>) {
    {
        let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
        state.selected = item;
    }
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        if item.is_some() {
            let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("Open"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        }
        let _ = AppendMenuW(menu, MF_STRING, CMD_REFRESH, w!("Refresh"));
        if item.is_none() {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, CMD_PERSONALIZE, w!("Personalize"));
        }
        let _ = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_RIGHTBUTTON,
            screen_point.x,
            screen_point.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
    }
}

fn paint_wallpaper(hdc: HDC, bounds: RECT) {
    use windows::Win32::Graphics::GdiPlus::{
        GdipCreateFromHDC, GdipDeleteGraphics, GdipDisposeImage, GdipDrawImageRectRectI,
        GdipGetImageHeight, GdipGetImageWidth, GdipLoadImageFromFile, GdiplusStartup,
        GdiplusStartupInput, GpGraphics, GpImage, Ok as GdiPlusOk, UnitPixel,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    use windows::Win32::UI::Shell::{
        DesktopWallpaper, IDesktopWallpaper, DWPOS_CENTER, DWPOS_FILL, DWPOS_FIT, DWPOS_SPAN,
        DWPOS_STRETCH, DWPOS_TILE,
    };

    static GDIPLUS: LazyLock<Option<usize>> = LazyLock::new(|| {
        let mut token = 0usize;
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            ..Default::default()
        };
        (unsafe { GdiplusStartup(&mut token, &input, std::ptr::null_mut()) } == GdiPlusOk)
            .then_some(token)
    });
    // A solid fallback is important in recovery/early-login sessions where the
    // wallpaper COM server has not started yet; an uninitialized back buffer
    // would otherwise paint random pixels across the entire desktop.
    unsafe {
        let brush = CreateSolidBrush(COLORREF(0));
        let _ = FillRect(hdc, &bounds, brush);
        let _ = DeleteObject(brush.into());
    }
    let wallpaper: IDesktopWallpaper =
        match unsafe { CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL) } {
            Ok(value) => value,
            Err(_) => return,
        };
    let background = unsafe { wallpaper.GetBackgroundColor() }.unwrap_or(COLORREF(0));
    unsafe {
        let brush = CreateSolidBrush(background);
        let _ = FillRect(hdc, &bounds, brush);
        let _ = DeleteObject(brush.into());
    }
    if GDIPLUS.is_none() {
        return;
    }
    let mut graphics: *mut GpGraphics = std::ptr::null_mut();
    if unsafe { GdipCreateFromHDC(hdc, &mut graphics) } != GdiPlusOk || graphics.is_null() {
        return;
    }
    let screen = virtual_screen();
    let position = unsafe { wallpaper.GetPosition() }.unwrap_or(DWPOS_FILL);
    let count = unsafe { wallpaper.GetMonitorDevicePathCount() }.unwrap_or(0);

    for index in 0..count {
        let Ok(monitor_id) = (unsafe { wallpaper.GetMonitorDevicePathAt(index) }) else {
            continue;
        };
        let monitor_rect = unsafe { wallpaper.GetMonitorRECT(PCWSTR(monitor_id.0)) };
        let wallpaper_path = unsafe { wallpaper.GetWallpaper(PCWSTR(monitor_id.0)) };
        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(monitor_id.0 as *const c_void));
        }
        let (Ok(monitor_rect), Ok(wallpaper_path)) = (monitor_rect, wallpaper_path) else {
            continue;
        };
        let path = unsafe { wallpaper_path.to_string() }.unwrap_or_default();
        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(wallpaper_path.0 as *const c_void));
        }
        if path.is_empty() {
            continue;
        }
        let encoded = wide(std::ffi::OsStr::new(&path));
        let mut image: *mut GpImage = std::ptr::null_mut();
        if unsafe { GdipLoadImageFromFile(PCWSTR(encoded.as_ptr()), &mut image) } != GdiPlusOk
            || image.is_null()
        {
            continue;
        }
        let mut image_width = 0u32;
        let mut image_height = 0u32;
        unsafe {
            let _ = GdipGetImageWidth(image, &mut image_width);
            let _ = GdipGetImageHeight(image, &mut image_height);
        }
        if image_width == 0 || image_height == 0 {
            unsafe {
                let _ = GdipDisposeImage(image);
            }
            continue;
        }
        let mut destination = RECT {
            left: monitor_rect.left - screen.left,
            top: monitor_rect.top - screen.top,
            right: monitor_rect.right - screen.left,
            bottom: monitor_rect.bottom - screen.top,
        };
        if position == DWPOS_SPAN {
            destination = bounds;
        }
        let dw = destination.right - destination.left;
        let dh = destination.bottom - destination.top;
        let iw = image_width as i32;
        let ih = image_height as i32;
        let (mut dx, mut dy, mut draw_w, mut draw_h, mut sx, mut sy, mut src_w, mut src_h) =
            (destination.left, destination.top, dw, dh, 0, 0, iw, ih);
        if position == DWPOS_CENTER {
            draw_w = iw.min(dw);
            draw_h = ih.min(dh);
            dx += (dw - draw_w) / 2;
            dy += (dh - draw_h) / 2;
            sx = (iw - draw_w) / 2;
            sy = (ih - draw_h) / 2;
            src_w = draw_w;
            src_h = draw_h;
        } else if position == DWPOS_FIT {
            let scale = (dw as f64 / iw as f64).min(dh as f64 / ih as f64);
            draw_w = (iw as f64 * scale).round() as i32;
            draw_h = (ih as f64 * scale).round() as i32;
            dx += (dw - draw_w) / 2;
            dy += (dh - draw_h) / 2;
        } else if position == DWPOS_FILL {
            let scale = (dw as f64 / iw as f64).max(dh as f64 / ih as f64);
            src_w = (dw as f64 / scale).round() as i32;
            src_h = (dh as f64 / scale).round() as i32;
            sx = (iw - src_w) / 2;
            sy = (ih - src_h) / 2;
        } else if position == DWPOS_TILE {
            let mut y = destination.top;
            while y < destination.bottom {
                let mut x = destination.left;
                while x < destination.right {
                    unsafe {
                        let _ = GdipDrawImageRectRectI(
                            graphics,
                            image,
                            x,
                            y,
                            iw.min(destination.right - x),
                            ih.min(destination.bottom - y),
                            0,
                            0,
                            iw.min(destination.right - x),
                            ih.min(destination.bottom - y),
                            UnitPixel,
                            std::ptr::null(),
                            0,
                            std::ptr::null_mut(),
                        );
                    }
                    x += iw;
                }
                y += ih;
            }
            unsafe {
                let _ = GdipDisposeImage(image);
            }
            continue;
        } else if position == DWPOS_STRETCH || position == DWPOS_SPAN {
            // The initialized full-destination/full-source rectangles are right.
        }
        unsafe {
            let _ = GdipDrawImageRectRectI(
                graphics,
                image,
                dx,
                dy,
                draw_w,
                draw_h,
                sx,
                sy,
                src_w,
                src_h,
                UnitPixel,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
            );
            let _ = GdipDisposeImage(image);
        }
        if position == DWPOS_SPAN {
            break;
        }
    }
    unsafe {
        let _ = GdipDeleteGraphics(graphics);
    }
}

fn paint(hwnd: HWND) {
    let mut paint = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
    let mut bounds = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut bounds);
    }
    let width = (bounds.right - bounds.left).max(1);
    let height = (bounds.bottom - bounds.top).max(1);
    unsafe {
        let buffer = CreateCompatibleDC(Some(hdc));
        let bitmap = CreateCompatibleBitmap(hdc, width, height);
        let previous = SelectObject(buffer, bitmap.into());
        paint_wallpaper(buffer, bounds);

        let state = STATE.lock().unwrap_or_else(|error| error.into_inner());
        let _ = SetBkMode(buffer, TRANSPARENT);
        let _ = SetTextColor(buffer, COLORREF(0x00ff_ffff));
        for (index, item) in state.items.iter().enumerate() {
            if state.selected == Some(index) {
                let brush = CreateSolidBrush(COLORREF(0x0080_4020));
                let _ = FillRect(buffer, &item.rect, brush);
                let _ = DeleteObject(brush.into());
            }
            if item.icon != 0 {
                let icon_x = item.rect.left + (CELL_WIDTH - ICON_SIZE) / 2;
                let icon_y = item.rect.top + 4;
                let _ = DrawIconEx(
                    buffer,
                    icon_x,
                    icon_y,
                    windows::Win32::UI::WindowsAndMessaging::HICON(item.icon as *mut c_void),
                    ICON_SIZE,
                    ICON_SIZE,
                    0,
                    None,
                    DI_NORMAL,
                );
            }
            let mut text_rect = RECT {
                left: item.rect.left + 2,
                top: item.rect.top + ICON_SIZE + 8,
                right: item.rect.right - 2,
                bottom: item.rect.bottom - 2,
            };
            let mut label: Vec<u16> = item.label.encode_utf16().collect();
            let mut shadow_rect = text_rect;
            shadow_rect.left += 1;
            shadow_rect.top += 1;
            shadow_rect.right += 1;
            shadow_rect.bottom += 1;
            let _ = SetTextColor(buffer, COLORREF(0));
            let _ = DrawTextW(
                buffer,
                &mut label,
                &mut shadow_rect,
                DT_CENTER | DT_WORDBREAK | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            let _ = SetTextColor(buffer, COLORREF(0x00ff_ffff));
            let _ = DrawTextW(
                buffer,
                &mut label,
                &mut text_rect,
                DT_CENTER | DT_WORDBREAK | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        }
        drop(state);
        let _ = BitBlt(hdc, 0, 0, width, height, Some(buffer), 0, 0, SRCCOPY);
        let _ = SelectObject(buffer, previous);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(buffer);
        let _ = EndPaint(hwnd, &paint);
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => {
            STATE.lock().unwrap_or_else(|error| error.into_inner()).hwnd = hwnd.0 as isize;
            refresh_items(true);
            let _ = SetTimer(Some(hwnd), TIMER_REFRESH, 2000, None);
            // Slideshow changes do not reliably broadcast WM_SETTINGCHANGE to
            // replacement shells, so periodically repaint from IDesktopWallpaper.
            let _ = SetTimer(Some(hwnd), TIMER_WALLPAPER, 15000, None);
            LRESULT(0)
        }
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            let point = point_from_lparam(lparam);
            let selected = hit_test(point);
            STATE
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .selected = selected;
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let point = point_from_lparam(lparam);
            if let Some(index) = hit_test(point) {
                STATE
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .selected = Some(index);
                if let Some(path) = selected_path() {
                    open_path(hwnd, &path);
                }
            }
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            let mut screen_point = point_from_lparam(lparam);
            if screen_point.x == -1 && screen_point.y == -1 {
                screen_point = POINT { x: 24, y: 24 };
            }
            let screen = virtual_screen();
            let client_point = POINT {
                x: screen_point.x - screen.left,
                y: screen_point.y - screen.top,
            };
            show_context_menu(hwnd, screen_point, hit_test(client_point));
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_COMMAND => {
            match wparam.0 & 0xffff {
                CMD_OPEN => {
                    if let Some(path) = selected_path() {
                        open_path(hwnd, &path);
                    }
                }
                CMD_REFRESH => refresh_items(true),
                CMD_PERSONALIZE => {
                    open_path(hwnd, Path::new("ms-settings:personalization-background"))
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_REFRESH {
                refresh_items(false);
            } else if wparam.0 == TIMER_WALLPAPER {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            reposition();
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(Some(hwnd), TIMER_REFRESH);
            let _ = KillTimer(Some(hwnd), TIMER_WALLPAPER);
            let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
            release_items(&mut state.items);
            state.hwnd = 0;
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

pub fn start(enabled: bool) -> Result<Option<HWND>, String> {
    if !enabled {
        return Ok(None);
    }
    let existing = STATE.lock().unwrap_or_else(|error| error.into_inner()).hwnd;
    if existing != 0 {
        return Ok(Some(HWND(existing as *mut c_void)));
    }
    crate::util::register_window_class(CLASS_NAME, wndproc, "Desktop")?;
    let screen = virtual_screen();
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS_NAME,
            w!("AltDWM Desktop"),
            WS_POPUP,
            screen.left,
            screen.top,
            screen.right - screen.left,
            screen.bottom - screen.top,
            None,
            None,
            Some(HINSTANCE(std::ptr::null_mut())),
            None,
        )
        .map_err(|error| format!("Desktop CreateWindowExW failed: {error:?}"))?
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            screen.left,
            screen.top,
            screen.right - screen.left,
            screen.bottom - screen.top,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    println!(
        "[desktop] surface ready with {} item(s)",
        STATE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .items
            .len()
    );
    Ok(Some(hwnd))
}

pub fn reposition() {
    let state = STATE.lock().unwrap_or_else(|error| error.into_inner());
    if state.hwnd == 0 {
        return;
    }
    let hwnd = HWND(state.hwnd as *mut c_void);
    drop(state);
    let screen = virtual_screen();
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            screen.left,
            screen.top,
            screen.right - screen.left,
            screen.bottom - screen.top,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
    refresh_items(true);
}

pub fn configure(enabled: bool) {
    if enabled {
        if let Err(error) = start(true) {
            eprintln!("[desktop] {error}");
        }
    } else {
        shutdown();
    }
}

pub fn shutdown() {
    let hwnd = STATE.lock().unwrap_or_else(|error| error.into_inner()).hwnd;
    if hwnd != 0 {
        unsafe {
            let _ =
                windows::Win32::UI::WindowsAndMessaging::DestroyWindow(HWND(hwnd as *mut c_void));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{display_name, file_signature};
    use std::path::{Path, PathBuf};

    #[test]
    fn desktop_labels_hide_file_extensions() {
        assert_eq!(display_name(Path::new("Cargo.toml")), "Cargo");
        assert_eq!(
            display_name(Path::new(r"C:\Users\Test\Desktop\Folder")),
            "Folder"
        );
    }

    #[test]
    fn missing_files_still_have_stable_signatures() {
        let path = PathBuf::from(r"Z:\definitely-missing\item.txt");
        assert_eq!(
            file_signature(std::slice::from_ref(&path)),
            vec![(path, 0, 0)]
        );
    }
}

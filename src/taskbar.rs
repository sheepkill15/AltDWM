use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, SYSTEMTIME, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, SetBkMode, SetTextColor,
    TextOutW, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetSystemMetrics, SetWindowPos, ShowWindow,
    HMENU, HWND_TOPMOST, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW,
    WM_CREATE, WM_DESTROY, WM_LBUTTONDOWN, WM_PAINT, WM_TIMER, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

pub const TASKBAR_HEIGHT: i32 = 40;

static TASKBAR_HWND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn get_taskbar_hwnd() -> Option<HWND> {
    let v = TASKBAR_HWND.load(std::sync::atomic::Ordering::SeqCst);
    if v == 0 {
        None
    } else {
        Some(HWND(v as *mut std::ffi::c_void))
    }
}

unsafe extern "system" fn taskbar_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(hwnd), 1, 1000, None);
            LRESULT(0)
        }
        WM_TIMER => {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, true);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            let theme = crate::CURRENT_CONFIG
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .theme
                .clone();
            let bg = theme.panel_bg("bottom");
            let brush = CreateSolidBrush(bg);
            FillRect(hdc, &rect, brush);
            let _ = DeleteObject(brush.into());
            let font = crate::theme::create_font(&theme);
            let old_font = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, theme.text_color());
            let st: SYSTEMTIME = GetLocalTime();
            let time_str = format!(
                "AltDWM | {:02}:{:02}:{:02} | Alt+Shift+R: retile | T:toggle Q:quit G:grid M:monocle F:float S:master J/K:focus",
                st.wHour, st.wMinute, st.wSecond
            );
            let wide: Vec<u16> = time_str.encode_utf16().collect();
            let _ = TextOutW(hdc, 10, 12, &wide);
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old_font);
            let _ = DeleteObject(font.into());
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            println!("[taskbar] click at {:?}", lparam);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(hwnd), 1);
            TASKBAR_HWND.store(0, std::sync::atomic::Ordering::SeqCst);
            // don't PostQuitMessage — only host posts quit (taskbar is not main)
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub fn create_taskbar() -> Result<HWND, String> {
    unsafe {
        let hinstance = HINSTANCE(std::ptr::null_mut());

        let class_name = w!("AltDWM_Taskbar");
        crate::util::register_window_class(class_name, taskbar_wndproc, "Taskbar")?;

        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let screen_h = GetSystemMetrics(SM_CYSCREEN);

        let y_final = screen_h - TASKBAR_HEIGHT;

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_APPWINDOW,
            class_name,
            w!("AltDWM Taskbar"),
            WS_POPUP | WS_VISIBLE,
            0,
            y_final,
            screen_w,
            TASKBAR_HEIGHT,
            None,
            Some(HMENU(std::ptr::null_mut())),
            Some(hinstance),
            None,
        )
        .map_err(|e| format!("CreateWindowExW failed: {:?}", e))?;

        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            y_final,
            screen_w,
            TASKBAR_HEIGHT,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        TASKBAR_HWND.store(hwnd.0 as usize, std::sync::atomic::Ordering::SeqCst);
        println!(
            "[taskbar] created hwnd={:?} {}x{} @ 0,{} (screen {}x{})",
            hwnd.0, screen_w, TASKBAR_HEIGHT, y_final, screen_w, screen_h
        );
        Ok(hwnd)
    }
}

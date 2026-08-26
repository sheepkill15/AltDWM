use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM, RECT, SYSTEMTIME};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetMonitorInfoW, MonitorFromWindow,
    SetBkMode, SetTextColor, TextOutW, HBRUSH, PAINTSTRUCT, TRANSPARENT, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetSystemMetrics, RegisterClassExW, ShowWindow,
    PostQuitMessage, SystemParametersInfoW, SPI_GETWORKAREA, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    HMENU, MSG, SW_SHOW, WM_CREATE, WM_DESTROY, WM_PAINT, WM_TIMER, WM_LBUTTONDOWN, WNDCLASSEXW,
    WS_EX_APPWINDOW, WS_EX_TOPMOST, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE, SM_CXSCREEN, SM_CYSCREEN,
    SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, HWND_TOPMOST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

pub const TASKBAR_HEIGHT: i32 = 40;

static mut TASKBAR_HWND: HWND = HWND(std::ptr::null_mut());

pub fn get_taskbar_hwnd() -> Option<HWND> {
    unsafe {
        if TASKBAR_HWND.0.is_null() { None } else { Some(TASKBAR_HWND) }
    }
}

unsafe extern "system" fn taskbar_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
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
            let brush = CreateSolidBrush(COLORREF(0x00202020));
            FillRect(hdc, &rect, brush);
            let _ = DeleteObject(brush.into());

            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00FFFFFF));
            let st: SYSTEMTIME = GetLocalTime();
            let time_str = format!(
                "AltDWM | {:02}:{:02}:{:02} | Win+Shift+R: retile | T:toggle Q:quit G:grid M:monocle F:float S:master",
                st.wHour, st.wMinute, st.wSecond
            );
            let wide: Vec<u16> = time_str.encode_utf16().collect();
            TextOutW(hdc, 10, 12, &wide);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            println!("[taskbar] click at {:?}", lparam);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(hwnd), 1);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub fn create_taskbar() -> Result<HWND, String> {
    unsafe {
        let hinstance = HINSTANCE(std::ptr::null_mut());

        let class_name = w!("AltDWM_Taskbar");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(taskbar_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance.into(),
            hIcon: Default::default(),
            hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(Some(hinstance), windows::Win32::UI::WindowsAndMessaging::IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: windows::core::PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: Default::default(),
        };
        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            let err = windows::Win32::Foundation::GetLastError();
            if err.0 != 1410 {
                return Err(format!("RegisterClassExW failed: {:?}", err));
            }
        }

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
        ).map_err(|e| format!("CreateWindowExW failed: {:?}", e))?;

        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, y_final, screen_w, TASKBAR_HEIGHT, SWP_NOACTIVATE | SWP_NOZORDER);
        let _ = ShowWindow(hwnd, SW_SHOW);
        TASKBAR_HWND = hwnd;
        println!("[taskbar] created hwnd={:?} {}x{} @ 0,{} (screen {}x{})", hwnd.0, screen_w, TASKBAR_HEIGHT, y_final, screen_w, screen_h);
        Ok(hwnd)
    }
}

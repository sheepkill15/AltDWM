//! Virtual desktop awareness — filter tiling to current desktop only.
//! Uses documented IVirtualDesktopManager (Win10+). Gracefully degrades if COM unavailable.
//! Toggle via config.toml `general.filter_virtual_desktop = true`
use windows::core::GUID;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::IVirtualDesktopManager;

// CLSID_VirtualDesktopManager = {AA509086-5CA9-4C25-8F95-589D3C07B48A}
const CLSID_VIRTUAL_DESKTOP_MANAGER: GUID = GUID::from_u128(0xaa509086_5ca9_4c25_8f95_589d3c07b48a);

/// Call once at startup to ensure COM initialized on main thread
pub fn init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    // probe to warm up — ignore result
    let _ = get_vdm();
}

thread_local! {
    static VDM_CACHE: std::cell::RefCell<Option<Option<IVirtualDesktopManager>>> = const { std::cell::RefCell::new(None) };
}

fn get_vdm() -> Option<IVirtualDesktopManager> {
    VDM_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if let Some(cached) = &*cache {
            return cached.clone();
        }
        // not yet cached — create
        let res = unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            CoCreateInstance::<_, IVirtualDesktopManager>(&CLSID_VIRTUAL_DESKTOP_MANAGER, None, CLSCTX_ALL)
        };
        let opt = match res {
            Ok(m) => Some(m),
            Err(e) => {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    eprintln!("[vdesktop] CoCreateInstance failed: {:?} — virtual desktop filtering disabled", e);
                });
                None
            }
        };
        *cache = Some(opt.clone());
        opt
    })
}

/// Returns true if window is on current virtual desktop, or if COM unavailable / filter disabled.
pub fn is_on_current_desktop(hwnd: HWND) -> bool {
    let filter = {
        crate::CURRENT_CONFIG
            .lock()
            .map(|c| c.general.filter_virtual_desktop)
            .unwrap_or(false)
    };
    if !filter {
        return true;
    }
    if let Some(vdm) = get_vdm() {
        unsafe {
            return match vdm.IsWindowOnCurrentVirtualDesktop(hwnd) {
                Ok(b) => b.as_bool(),
                Err(_) => true, // not a top-level or other error -> treat as visible
            };
        }
    }
    true
}

#[allow(dead_code)]
pub fn move_to_desktop(_hwnd: HWND, _desktop_id: &str) {
    println!(
        "[vdesktop] move_to_desktop stub — needs IVirtualDesktopManagerInternal undocumented COM"
    );
}

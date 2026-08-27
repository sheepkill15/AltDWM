//! Live system state for the shell's status widgets and quick settings.
//!
//! Everything here is polled on a dedicated worker thread and published as an
//! immutable snapshot, for the same reason the tray bridge works that way:
//! Core Audio, WLAN, and DDC/CI calls are all slow enough — and COM calls
//! reentrant enough — that issuing them from `WM_PAINT` would stall the shell's
//! message loop. Widgets read the snapshot; commands are sent to the worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_millis(1000);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeStatus {
    /// 0.0–1.0 on the endpoint's own scalar scale.
    pub level: f32,
    pub muted: bool,
}

impl VolumeStatus {
    pub fn percent(&self) -> u8 {
        (self.level.clamp(0.0, 1.0) * 100.0).round() as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryStatus {
    pub percent: Option<u8>,
    pub charging: bool,
    pub on_ac: bool,
    pub minutes_remaining: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum NetworkStatus {
    #[default]
    Unknown,
    Offline,
    Wired,
    WiFi {
        ssid: String,
        /// Signal quality, 0–100, as reported by the WLAN service.
        signal: u8,
    },
}

impl NetworkStatus {
    pub fn label(&self) -> String {
        match self {
            NetworkStatus::WiFi { ssid, .. } if !ssid.is_empty() => ssid.clone(),
            NetworkStatus::WiFi { .. } => "Wi-Fi".into(),
            NetworkStatus::Wired => "Wired".into(),
            NetworkStatus::Offline => "Offline".into(),
            NetworkStatus::Unknown => "Network".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrightnessStatus {
    pub percent: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemStatus {
    /// `None` when there is no audio endpoint, or it could not be reached.
    pub volume: Option<VolumeStatus>,
    /// `None` on a desktop with no battery.
    pub battery: Option<BatteryStatus>,
    pub network: NetworkStatus,
    /// `None` when no monitor answers DDC/CI — common for internal laptop
    /// panels, which expose brightness through WMI instead.
    pub brightness: Option<BrightnessStatus>,
    pub wifi_radio_on: Option<bool>,
}

enum Command {
    SetVolume(f32),
    AdjustVolume(f32),
    ToggleMute,
    SetBrightness(u8),
    AdjustBrightness(i32),
    SetWiFiRadio(bool),
    Refresh,
}

struct Worker {
    status: Arc<Mutex<SystemStatus>>,
    commands: Sender<Command>,
}

static WORKER: LazyLock<Worker> = LazyLock::new(start_worker);
/// Set once any widget or surface asks for system state, so the worker is not
/// started for configurations that never display it.
static WANTED: AtomicBool = AtomicBool::new(false);

fn start_worker() -> Worker {
    let status = Arc::new(Mutex::new(SystemStatus::default()));
    let worker_status = status.clone();
    let (commands, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("AltDWM-system".into())
        .spawn(move || worker_loop(worker_status, receiver))
        .expect("failed to start system worker");
    Worker { status, commands }
}

/// Current snapshot. Cheap: a clone of a small struct behind a mutex.
pub fn status() -> SystemStatus {
    WANTED.store(true, Ordering::SeqCst);
    WORKER
        .status
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn send(command: Command) {
    WANTED.store(true, Ordering::SeqCst);
    let _ = WORKER.commands.send(command);
}

pub fn set_volume(level: f32) {
    send(Command::SetVolume(level));
}

pub fn adjust_volume(delta: f32) {
    send(Command::AdjustVolume(delta));
}

pub fn toggle_mute() {
    send(Command::ToggleMute);
}

pub fn set_brightness(percent: u8) {
    send(Command::SetBrightness(percent));
}

pub fn adjust_brightness(delta: i32) {
    send(Command::AdjustBrightness(delta));
}

pub fn set_wifi_radio(enabled: bool) {
    send(Command::SetWiFiRadio(enabled));
}

pub fn refresh() {
    send(Command::Refresh);
}

fn worker_loop(status: Arc<Mutex<SystemStatus>>, receiver: Receiver<Command>) {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    unsafe {
        // The worker owns its own MTA apartment: it must never marshal into the
        // shell's STA, or a slow endpoint could block the message loop.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut audio = audio::Endpoint::new();
    loop {
        let next = SystemStatus {
            volume: audio.read(),
            battery: power::read(),
            network: net::read(),
            brightness: brightness::read(),
            wifi_radio_on: net::radio_state(),
        };
        let changed = {
            let mut current = status.lock().unwrap_or_else(|error| error.into_inner());
            let changed = *current != next;
            if changed {
                *current = next;
            }
            changed
        };
        if changed {
            crate::panel::invalidate_all();
            crate::quick_settings::invalidate();
        }

        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(command) => {
                match command {
                    Command::SetVolume(level) => audio.set_level(level),
                    Command::AdjustVolume(delta) => audio.adjust(delta),
                    Command::ToggleMute => audio.toggle_mute(),
                    Command::SetBrightness(percent) => brightness::write(percent),
                    Command::AdjustBrightness(delta) => brightness::adjust(delta),
                    Command::SetWiFiRadio(enabled) => net::set_radio(enabled),
                    Command::Refresh => {}
                }
                // Re-poll immediately so the UI reflects the change on the next
                // paint rather than up to a second later.
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

// ---------------------------------------------------------------- audio ----

mod audio {
    use super::VolumeStatus;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

    /// The default render endpoint, re-resolved whenever it goes away — the user
    /// can change or unplug the default device at any time.
    pub struct Endpoint {
        volume: Option<IAudioEndpointVolume>,
    }

    impl Endpoint {
        pub fn new() -> Self {
            Self { volume: None }
        }

        fn resolve(&mut self) -> Option<&IAudioEndpointVolume> {
            if self.volume.is_none() {
                self.volume = unsafe {
                    let enumerator: IMMDeviceEnumerator =
                        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
                    let device = enumerator
                        .GetDefaultAudioEndpoint(eRender, eMultimedia)
                        .ok()?;
                    device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok()
                };
            }
            self.volume.as_ref()
        }

        pub fn read(&mut self) -> Option<VolumeStatus> {
            let volume = self.resolve()?;
            let level = unsafe { volume.GetMasterVolumeLevelScalar() };
            let muted = unsafe { volume.GetMute() };
            match (level, muted) {
                (Ok(level), Ok(muted)) => Some(VolumeStatus {
                    level,
                    muted: muted.as_bool(),
                }),
                _ => {
                    // The endpoint went away; drop it so the next poll re-resolves.
                    self.volume = None;
                    None
                }
            }
        }

        pub fn set_level(&mut self, level: f32) {
            let Some(volume) = self.resolve() else {
                return;
            };
            unsafe {
                if volume
                    .SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null())
                    .is_err()
                {
                    self.volume = None;
                }
            }
        }

        pub fn adjust(&mut self, delta: f32) {
            let Some(current) = self.read() else {
                return;
            };
            // Nudging the volume un-mutes, which is what every volume key does.
            if current.muted && delta > 0.0 {
                self.set_mute(false);
            }
            self.set_level(current.level + delta);
        }

        fn set_mute(&mut self, muted: bool) {
            let Some(volume) = self.resolve() else {
                return;
            };
            unsafe {
                if volume.SetMute(muted, std::ptr::null()).is_err() {
                    self.volume = None;
                }
            }
        }

        pub fn toggle_mute(&mut self) {
            let Some(current) = self.read() else {
                return;
            };
            self.set_mute(!current.muted);
        }
    }
}

// ---------------------------------------------------------------- power ----

mod power {
    use super::BatteryStatus;
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    /// `BATTERY_FLAG_NO_BATTERY` — a desktop reports this and has no battery UI.
    const NO_BATTERY: u8 = 128;
    const UNKNOWN_PERCENT: u8 = 255;
    const UNKNOWN_TIME: u32 = u32::MAX;

    pub fn read() -> Option<BatteryStatus> {
        let mut status = SYSTEM_POWER_STATUS::default();
        unsafe {
            GetSystemPowerStatus(&mut status).ok()?;
        }
        if status.BatteryFlag & NO_BATTERY != 0 {
            return None;
        }
        let percent = (status.BatteryLifePercent != UNKNOWN_PERCENT)
            .then_some(status.BatteryLifePercent.min(100));
        let on_ac = status.ACLineStatus == 1;
        Some(BatteryStatus {
            percent,
            // BATTERY_FLAG_CHARGING
            charging: status.BatteryFlag & 8 != 0,
            on_ac,
            minutes_remaining: (status.BatteryLifeTime != UNKNOWN_TIME)
                .then_some(status.BatteryLifeTime / 60),
        })
    }
}

// -------------------------------------------------------------- network ----

mod net {
    use super::NetworkStatus;
    use windows::Win32::NetworkManagement::WiFi::{
        WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface,
        WlanSetInterface, WLAN_API_VERSION_2_0,
        WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST, WLAN_INTERFACE_STATE,
        WLAN_PHY_RADIO_STATE, WLAN_RADIO_STATE,
        dot11_radio_state_off, dot11_radio_state_on, wlan_interface_state_connected,
        wlan_intf_opcode_current_connection, wlan_intf_opcode_radio_state,
    };

    /// A WLAN client handle plus the interfaces it found, released together.
    struct Wlan {
        handle: windows::Win32::Foundation::HANDLE,
    }

    impl Wlan {
        fn open() -> Option<Self> {
            let mut negotiated = 0u32;
            let mut handle = windows::Win32::Foundation::HANDLE::default();
            let result = unsafe {
                WlanOpenHandle(WLAN_API_VERSION_2_0, None, &mut negotiated, &mut handle)
            };
            (result == 0).then_some(Self { handle })
        }

        /// GUID of the first WLAN interface, if the machine has one.
        fn first_interface(&self) -> Option<(windows::core::GUID, WLAN_INTERFACE_STATE)> {
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
            let result = unsafe { WlanEnumInterfaces(self.handle, None, &mut list) };
            if result != 0 || list.is_null() {
                return None;
            }
            let found = unsafe {
                let count = (*list).dwNumberOfItems as usize;
                (count > 0).then(|| {
                    let info = &(*list).InterfaceInfo[0];
                    (info.InterfaceGuid, info.isState)
                })
            };
            unsafe { WlanFreeMemory(list as *mut _) };
            found
        }
    }

    impl Drop for Wlan {
        fn drop(&mut self) {
            unsafe {
                WlanCloseHandle(self.handle, None);
            }
        }
    }

    fn query<T>(wlan: &Wlan, guid: &windows::core::GUID, opcode: i32) -> Option<T>
    where
        T: Copy,
    {
        let mut size = 0u32;
        let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
        let result = unsafe {
            WlanQueryInterface(
                wlan.handle,
                guid,
                windows::Win32::NetworkManagement::WiFi::WLAN_INTF_OPCODE(opcode),
                None,
                &mut size,
                &mut data,
                None,
            )
        };
        if result != 0 || data.is_null() || (size as usize) < size_of::<T>() {
            if !data.is_null() {
                unsafe { WlanFreeMemory(data) };
            }
            return None;
        }
        let value = unsafe { *(data as *const T) };
        unsafe { WlanFreeMemory(data) };
        Some(value)
    }

    pub fn read() -> NetworkStatus {
        if let Some(wlan) = Wlan::open() {
            if let Some((guid, state)) = wlan.first_interface() {
                if state == wlan_interface_state_connected {
                    if let Some(attributes) = query::<WLAN_CONNECTION_ATTRIBUTES>(
                        &wlan,
                        &guid,
                        wlan_intf_opcode_current_connection.0,
                    ) {
                        let association = attributes.wlanAssociationAttributes;
                        let ssid = &association.dot11Ssid;
                        let length = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
                        let name = String::from_utf8_lossy(&ssid.ucSSID[..length]).to_string();
                        return NetworkStatus::WiFi {
                            ssid: name,
                            signal: association.wlanSignalQuality.min(100) as u8,
                        };
                    }
                    return NetworkStatus::WiFi {
                        ssid: String::new(),
                        signal: 0,
                    };
                }
            }
        }
        // No Wi-Fi association. Fall back to whether the machine has any route
        // to the internet at all, which distinguishes a wired desktop from one
        // that is genuinely offline.
        match internet_connected() {
            Some(true) => NetworkStatus::Wired,
            Some(false) => NetworkStatus::Offline,
            None => NetworkStatus::Unknown,
        }
    }

    /// `INetworkListManager` is the documented connectivity oracle and does not
    /// generate traffic of its own.
    fn internet_connected() -> Option<bool> {
        use windows::core::GUID;
        use windows::Win32::Networking::NetworkListManager::{
            INetworkListManager, NLM_CONNECTIVITY_IPV4_INTERNET, NLM_CONNECTIVITY_IPV6_INTERNET,
        };
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
        const CLSID_NETWORK_LIST_MANAGER: GUID =
            GUID::from_u128(0xDCB00C01_570F_4A9B_8D69_199FDBA5723B);
        unsafe {
            let manager: INetworkListManager =
                CoCreateInstance(&CLSID_NETWORK_LIST_MANAGER, None, CLSCTX_ALL).ok()?;
            let connectivity = manager.GetConnectivity().ok()?;
            Some(
                connectivity.0 & (NLM_CONNECTIVITY_IPV4_INTERNET.0 | NLM_CONNECTIVITY_IPV6_INTERNET.0)
                    != 0,
            )
        }
    }

    pub fn radio_state() -> Option<bool> {
        let wlan = Wlan::open()?;
        let (guid, _) = wlan.first_interface()?;
        let state =
            query::<WLAN_RADIO_STATE>(&wlan, &guid, wlan_intf_opcode_radio_state.0)?;
        let count = (state.dwNumberOfPhys as usize).min(state.PhyRadioState.len());
        Some(
            state.PhyRadioState[..count]
                .iter()
                .any(|phy| phy.dot11SoftwareRadioState == dot11_radio_state_on),
        )
    }

    pub fn set_radio(enabled: bool) {
        let Some(wlan) = Wlan::open() else {
            return;
        };
        let Some((guid, _)) = wlan.first_interface() else {
            return;
        };
        let mut state = WLAN_PHY_RADIO_STATE {
            dwPhyIndex: 0,
            dot11SoftwareRadioState: if enabled {
                dot11_radio_state_on
            } else {
                dot11_radio_state_off
            },
            dot11HardwareRadioState: Default::default(),
        };
        unsafe {
            WlanSetInterface(
                wlan.handle,
                &guid,
                windows::Win32::NetworkManagement::WiFi::WLAN_INTF_OPCODE(
                    wlan_intf_opcode_radio_state.0,
                ),
                size_of::<WLAN_PHY_RADIO_STATE>() as u32,
                &mut state as *mut _ as *const core::ffi::c_void,
                None,
            );
        }
    }
}

// ----------------------------------------------------------- brightness ----

mod brightness {
    use super::BrightnessStatus;
    use windows::Win32::Devices::Display::{
        DestroyPhysicalMonitor, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
        GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, PHYSICAL_MONITOR,
    };
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};

    /// Physical monitors for the primary display.
    ///
    /// This is the DDC/CI path. It works for most external displays; internal
    /// laptop panels usually answer only through WMI, and report `None` here.
    /// Callers must treat brightness as optional rather than assume it exists.
    fn primary_monitors() -> Vec<PHYSICAL_MONITOR> {
        let monitor = unsafe {
            MonitorFromPoint(
                windows::Win32::Foundation::POINT { x: 0, y: 0 },
                MONITOR_DEFAULTTOPRIMARY,
            )
        };
        let mut count = 0u32;
        unsafe {
            if GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &mut count).is_err() || count == 0 {
                return Vec::new();
            }
        }
        let mut monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
        unsafe {
            if GetPhysicalMonitorsFromHMONITOR(monitor, &mut monitors).is_err() {
                return Vec::new();
            }
        }
        monitors
    }

    fn release(monitors: Vec<PHYSICAL_MONITOR>) {
        for monitor in monitors {
            unsafe {
                let _ = DestroyPhysicalMonitor(monitor.hPhysicalMonitor);
            }
        }
    }

    pub fn read() -> Option<BrightnessStatus> {
        let monitors = primary_monitors();
        if monitors.is_empty() {
            return None;
        }
        let mut found = None;
        for monitor in &monitors {
            let mut minimum = 0u32;
            let mut current = 0u32;
            let mut maximum = 0u32;
            let ok = unsafe {
                GetMonitorBrightness(
                    monitor.hPhysicalMonitor,
                    &mut minimum,
                    &mut current,
                    &mut maximum,
                ) != 0
            };
            if ok && maximum > minimum {
                let span = (maximum - minimum) as f32;
                let percent = ((current.saturating_sub(minimum)) as f32 / span * 100.0).round();
                found = Some(BrightnessStatus {
                    percent: percent.clamp(0.0, 100.0) as u8,
                });
                break;
            }
        }
        release(monitors);
        found
    }

    pub fn write(percent: u8) {
        let monitors = primary_monitors();
        for monitor in &monitors {
            let mut minimum = 0u32;
            let mut current = 0u32;
            let mut maximum = 0u32;
            let ok = unsafe {
                GetMonitorBrightness(
                    monitor.hPhysicalMonitor,
                    &mut minimum,
                    &mut current,
                    &mut maximum,
                ) != 0
            };
            if !ok || maximum <= minimum {
                continue;
            }
            let span = (maximum - minimum) as f32;
            let target = minimum + (span * f32::from(percent.min(100)) / 100.0).round() as u32;
            unsafe {
                SetMonitorBrightness(monitor.hPhysicalMonitor, target);
            }
        }
        release(monitors);
    }

    pub fn adjust(delta: i32) {
        if let Some(current) = read() {
            let next = (i32::from(current.percent) + delta).clamp(0, 100) as u8;
            write(next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NetworkStatus, VolumeStatus};

    #[test]
    fn volume_reports_whole_percentages() {
        assert_eq!(VolumeStatus { level: 0.0, muted: false }.percent(), 0);
        assert_eq!(VolumeStatus { level: 0.245, muted: false }.percent(), 25);
        assert_eq!(VolumeStatus { level: 1.0, muted: false }.percent(), 100);
        // Endpoints have been observed reporting slightly out-of-range scalars.
        assert_eq!(VolumeStatus { level: 1.4, muted: false }.percent(), 100);
        assert_eq!(VolumeStatus { level: -0.2, muted: false }.percent(), 0);
    }

    #[test]
    fn network_labels_prefer_the_ssid() {
        assert_eq!(
            NetworkStatus::WiFi {
                ssid: "Kitchen".into(),
                signal: 70
            }
            .label(),
            "Kitchen"
        );
        // A connected interface that will not disclose its SSID still reads as Wi-Fi.
        assert_eq!(
            NetworkStatus::WiFi {
                ssid: String::new(),
                signal: 0
            }
            .label(),
            "Wi-Fi"
        );
        assert_eq!(NetworkStatus::Wired.label(), "Wired");
        assert_eq!(NetworkStatus::Offline.label(), "Offline");
    }
}

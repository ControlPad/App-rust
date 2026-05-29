//! Windows audio backend (WASAPI via the `windows` crate).
//!
//! Per-process volume/mute uses `IAudioSessionManager2` to enumerate sessions
//! and match by `GetProcessId()`. Endpoint (system/mic) volume/mute uses
//! `IAudioEndpointVolume`. Device lookup is by friendly name; `None` resolves
//! to the default endpoint for the role.

#![cfg(target_os = "windows")]

use std::sync::Mutex;

use windows::core::Interface;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eCapture, eMultimedia, eRender, IAudioSessionControl, IAudioSessionControl2,
    IAudioSessionEnumerator, IAudioSessionManager2, IMMDevice, IMMDeviceCollection,
    IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

use super::{AudioBackend, MuteTarget, VolumeTarget};

pub struct WasapiBackend {
    _com: ComGuard,
    enumerator: IMMDeviceEnumerator,
    // The `windows` interfaces are !Send/!Sync, but in practice each call is
    // on the UI thread + serial worker thread. Wrap in a Mutex.
    lock: Mutex<()>,
}

struct ComGuard;

impl ComGuard {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            // S_OK (0), S_FALSE (already STA — fine), or RPC_E_CHANGED_MODE (winit
            // beat us to it — fine, we just don't own the apartment). Anything else
            // is a real failure.
            const RPC_E_CHANGED_MODE: windows::core::HRESULT =
                windows::core::HRESULT(0x80010106u32 as i32);
            if hr.is_err() && hr != RPC_E_CHANGED_MODE {
                hr.ok()?;
            }
        }
        Ok(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

unsafe impl Send for WasapiBackend {}
unsafe impl Sync for WasapiBackend {}

impl WasapiBackend {
    pub fn new() -> anyhow::Result<Self> {
        let com = ComGuard::new().map_err(|e| anyhow::anyhow!("CoInitializeEx: {e}"))?;
        let enumerator: IMMDeviceEnumerator = unsafe {
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| anyhow::anyhow!("CoCreateInstance MMDeviceEnumerator: {e}"))?
        };
        Ok(Self { _com: com, enumerator, lock: Mutex::new(()) })
    }

    fn default_device(&self, flow: u32) -> windows::core::Result<IMMDevice> {
        // flow: 0 = eRender, 1 = eCapture
        let data_flow = if flow == 0 { eRender } else { eCapture };
        unsafe { self.enumerator.GetDefaultAudioEndpoint(data_flow, eMultimedia) }
    }

    fn find_device(&self, flow: u32, name: &str) -> windows::core::Result<IMMDevice> {
        let data_flow = if flow == 0 { eRender } else { eCapture };
        let needle = name.to_lowercase();
        unsafe {
            let coll: IMMDeviceCollection = self
                .enumerator
                .EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE)?;
            let count = coll.GetCount()?;
            // First pass: exact friendly-name match. Handles unique names, the
            // 1st of a duplicated name (stored bare), and any real device whose
            // name happens to end in " (N)".
            for i in 0..count {
                let dev = coll.Item(i)?;
                if let Ok(friendly) = device_friendly_name(&dev) {
                    if friendly.to_lowercase() == needle {
                        return Ok(dev);
                    }
                }
            }
            // Disambiguated duplicate: "<base> (N)" → Nth endpoint named <base>.
            if let Some((base, n)) = parse_dup_suffix(name) {
                let base = base.to_lowercase();
                let mut seen = 0u32;
                for i in 0..count {
                    let dev = coll.Item(i)?;
                    if let Ok(friendly) = device_friendly_name(&dev) {
                        if friendly.to_lowercase() == base {
                            seen += 1;
                            if seen == n {
                                return Ok(dev);
                            }
                        }
                    }
                }
            }
            // Second pass: substring match.
            for i in 0..count {
                let dev = coll.Item(i)?;
                if let Ok(friendly) = device_friendly_name(&dev) {
                    if friendly.to_lowercase().contains(&needle) {
                        return Ok(dev);
                    }
                }
            }
        }
        log::warn!("output device {name:?} not found; using default endpoint");
        self.default_device(flow)
    }

    fn endpoint_volume(&self, dev: &IMMDevice) -> windows::core::Result<IAudioEndpointVolume> {
        unsafe { dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }
    }

    fn session_manager(&self, dev: &IMMDevice) -> windows::core::Result<IAudioSessionManager2> {
        unsafe { dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }
    }

    fn for_each_process_session<F: FnMut(&IAudioSessionControl2, u32)>(
        &self,
        process: &str,
        mut visit: F,
    ) {
        let _guard = self.lock.lock().unwrap();
        let needle = strip_exe(&process.to_lowercase());
        unsafe {
            let Ok(dev) = self.default_device(0) else { return };
            let Ok(mgr) = self.session_manager(&dev) else { return };
            let Ok(enumerator): Result<IAudioSessionEnumerator, _> = mgr.GetSessionEnumerator()
            else {
                return;
            };
            let Ok(count) = enumerator.GetCount() else { return };
            for i in 0..count {
                let Ok(ctl): Result<IAudioSessionControl, _> = enumerator.GetSession(i) else {
                    continue;
                };
                let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else { continue };
                let Ok(pid) = ctl2.GetProcessId() else { continue };
                if pid == 0 {
                    continue;
                }
                let Some(name) = process_name(pid) else { continue };
                if strip_exe(&name.to_lowercase()).contains(&needle)
                    || needle.contains(&strip_exe(&name.to_lowercase()))
                {
                    visit(&ctl2, pid);
                }
            }
        }
    }
}

fn strip_exe(s: &str) -> String {
    let s = s.strip_suffix(".exe").unwrap_or(s);
    s.rsplit(['/', '\\']).next().unwrap_or(s).to_string()
}

impl AudioBackend for WasapiBackend {
    fn set_volume(&self, target: VolumeTarget<'_>, value: f32) {
        let v = value.clamp(0.0, 1.0);
        match target {
            VolumeTarget::Process(p) => {
                self.for_each_process_session(p, |ctl, _| unsafe {
                    if let Ok(vol) = ctl.cast::<ISimpleAudioVolume>() {
                        let _ = vol.SetMasterVolume(v, std::ptr::null());
                    }
                });
            }
            VolumeTarget::Mic(m) => {
                let _g = self.lock.lock().unwrap();
                if let Ok(dev) = self.find_device(1, m) {
                    if let Ok(ep) = self.endpoint_volume(&dev) {
                        unsafe {
                            let _ = ep.SetMasterVolumeLevelScalar(v, std::ptr::null());
                        }
                    }
                }
            }
            VolumeTarget::System(name) => {
                let _g = self.lock.lock().unwrap();
                let dev = match name {
                    Some(n) => self.find_device(0, n),
                    None => self.default_device(0),
                };
                if let Ok(dev) = dev {
                    if let Ok(ep) = self.endpoint_volume(&dev) {
                        unsafe {
                            let _ = ep.SetMasterVolumeLevelScalar(v, std::ptr::null());
                        }
                    }
                }
            }
        }
    }

    fn set_mute(&self, target: MuteTarget<'_>, muted: bool) {
        let m = BOOL::from(muted);
        match target {
            MuteTarget::Process(p) => {
                self.for_each_process_session(p, |ctl, _| unsafe {
                    if let Ok(vol) = ctl.cast::<ISimpleAudioVolume>() {
                        let _ = vol.SetMute(m, std::ptr::null());
                    }
                });
            }
            MuteTarget::Mic(name) => {
                let _g = self.lock.lock().unwrap();
                if let Ok(dev) = self.find_device(1, name) {
                    if let Ok(ep) = self.endpoint_volume(&dev) {
                        unsafe {
                            let _ = ep.SetMute(m, std::ptr::null());
                        }
                    }
                }
            }
            MuteTarget::System(name) => {
                let _g = self.lock.lock().unwrap();
                let dev = match name {
                    Some(n) => self.find_device(0, n),
                    None => self.default_device(0),
                };
                if let Ok(dev) = dev {
                    if let Ok(ep) = self.endpoint_volume(&dev) {
                        unsafe {
                            let _ = ep.SetMute(m, std::ptr::null());
                        }
                    }
                }
            }
        }
    }

    fn toggle_mute(&self, target: MuteTarget<'_>) {
        let cur = self.is_muted(target);
        self.set_mute(target, !cur);
    }

    fn is_muted(&self, target: MuteTarget<'_>) -> bool {
        match target {
            MuteTarget::Process(p) => {
                let mut muted = false;
                self.for_each_process_session(p, |ctl, _| unsafe {
                    if let Ok(vol) = ctl.cast::<ISimpleAudioVolume>() {
                        if let Ok(b) = vol.GetMute() {
                            muted = b.as_bool();
                        }
                    }
                });
                muted
            }
            MuteTarget::Mic(name) => {
                let _g = self.lock.lock().unwrap();
                let Ok(dev) = self.find_device(1, name) else { return false };
                let Ok(ep) = self.endpoint_volume(&dev) else { return false };
                unsafe { ep.GetMute().map(|b| b.as_bool()).unwrap_or(false) }
            }
            MuteTarget::System(name) => {
                let _g = self.lock.lock().unwrap();
                let dev = match name {
                    Some(n) => self.find_device(0, n),
                    None => self.default_device(0),
                };
                let Ok(dev) = dev else { return false };
                let Ok(ep) = self.endpoint_volume(&dev) else { return false };
                unsafe { ep.GetMute().map(|b| b.as_bool()).unwrap_or(false) }
            }
        }
    }

    fn list_outputs(&self) -> Vec<String> {
        list_devices(&self.enumerator, 0)
    }

    fn list_mics(&self) -> Vec<String> {
        list_devices(&self.enumerator, 1)
    }
}

fn list_devices(enumerator: &IMMDeviceEnumerator, flow: u32) -> Vec<String> {
    let data_flow = if flow == 0 { eRender } else { eCapture };
    let mut out = Vec::new();
    unsafe {
        let Ok(coll) = enumerator.EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE) else {
            return out;
        };
        let Ok(count) = coll.GetCount() else { return out };
        for i in 0..count {
            if let Ok(dev) = coll.Item(i) {
                if let Ok(name) = device_friendly_name(&dev) {
                    out.push(name);
                }
            }
        }
    }
    // Two endpoints can report the identical friendly name (e.g. two monitors on
    // one GPU's "NVIDIA High Definition Audio"). Disambiguate by appending
    // " (2)", " (3)", … to the 2nd+ occurrence so each is shown and selectable.
    // The 1st stays bare for backward compatibility with already-saved configs.
    // `find_device` parses the suffix back to the Nth same-named endpoint.
    disambiguate(&mut out);
    out
}

/// Append " (N)" to repeated names (N = 2 for the 2nd occurrence, etc.).
fn disambiguate(names: &mut [String]) {
    use std::collections::HashMap;
    let mut seen: HashMap<String, u32> = HashMap::new();
    for n in names.iter_mut() {
        let count = seen.entry(n.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            *n = format!("{n} ({count})");
        }
    }
}

/// Split a disambiguated name "<base> (N)" (N >= 2) into (base, N).
fn parse_dup_suffix(name: &str) -> Option<(&str, u32)> {
    let stripped = name.strip_suffix(')')?;
    let open = stripped.rfind(" (")?;
    let n: u32 = stripped[open + 2..].parse().ok()?;
    if n >= 2 {
        Some((&stripped[..open], n))
    } else {
        None
    }
}

fn device_friendly_name(dev: &IMMDevice) -> windows::core::Result<String> {
    unsafe {
        let store = dev.OpenPropertyStore(windows::Win32::System::Com::STGM_READ)?;
        let var = store.GetValue(&PKEY_Device_FriendlyName)?;
        Ok(format!("{var}"))
    }
}

fn process_name(pid: u32) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::QueryFullProcessImageNameW;
    use windows::Win32::System::Threading::PROCESS_NAME_WIN32;
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        // QueryFullProcessImageNameW works with PROCESS_QUERY_LIMITED_INFORMATION,
        // unlike GetModuleBaseNameW which often needs VM_READ and fails silently.
        let ok = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(h);
        if !ok || len == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        // Return just the file name (e.g. "Spotify.exe").
        Some(full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string())
    }
}

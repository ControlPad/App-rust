//! Proof of concept: per-process loopback capture for the planned mixer.
//!
//! Captures one program's audio via the Windows process-loopback virtual device
//! (`VAD\Process_Loopback`, Windows 10 2004+) and answers the question the
//! mixer design hinges on: does the capture still carry audio once the
//! program's own session is muted / turned down to 0 in the volume mixer?
//! If yes, Slidr can silence the original and play its own mix without a
//! virtual speaker device.
//!
//! Runs three phases (normal, session muted, session volume 0), prints the
//! level captured in each, then restores the session state. Optionally plays
//! the capture to an output device (substring of its name) so it can be heard
//! / fed into a virtual cable.
//!
//!     cargo run --example loopback_poc -- spotify
//!     cargo run --example loopback_poc -- spotify "Voicemeeter Input" 6
//!
//! `--route <pid> <device-a> <device-b> [secs]` instead tests per-app output
//! routing: moves the program to device A, captures, moves it to device B
//! mid-stream, captures again, then clears the override.
//!
//!     cargo run --example loopback_poc -- --route 1234 "Steam Streaming Speakers" "Voicemeeter In 1"

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("loopback_poc is Windows-only");
}

#[cfg(target_os = "windows")]
fn main() -> anyhow::Result<()> {
    poc::run()
}

#[cfg(target_os = "windows")]
mod poc {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use anyhow::{anyhow, bail, Context};
    use windows::core::{implement, Interface, IUnknown, HRESULT, PWSTR};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Media::Audio::{
        eRender, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
        IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
        IAudioCaptureClient, IAudioClient, IAudioRenderClient, IAudioSessionControl2,
        IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator, ISimpleAudioVolume,
        MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
        AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
        AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, DEVICE_STATE_ACTIVE,
        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        WAVEFORMATEX,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
    };
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    use windows::Win32::System::Threading::{
        CreateEventW, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;
    const BITS: u16 = 16;
    const BLOCK: usize = (CHANNELS as usize) * (BITS as usize / 8);
    /// 100 ms in REFERENCE_TIME (100 ns) units.
    const BUFFER_100MS: i64 = 1_000_000;

    static STOP: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn on_ctrl(_: u32) -> BOOL {
        STOP.store(true, Ordering::SeqCst);
        true.into()
    }

    pub fn run() -> anyhow::Result<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("--route") {
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
                SetConsoleCtrlHandler(Some(on_ctrl), true)?;
            }
            return route::run(&args[1..]);
        }
        let Some(target) = args.first().map(|s| s.to_lowercase()) else {
            bail!("usage: loopback_poc <process-name> [output-device-substring] [seconds-per-phase]");
        };
        let out_name = args.get(1).filter(|s| !s.is_empty()).cloned();
        let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);

        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            SetConsoleCtrlHandler(Some(on_ctrl), true)?;
        }
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };

        let sessions = find_sessions(&enumerator, &target)?;
        if sessions.is_empty() {
            bail!("no audio session found for '{target}' - is it running and has it played sound yet?");
        }
        let pid = sessions[0].pid;
        println!(
            "target: {} session(s), capturing process tree of pid {pid} ({})",
            sessions.len(),
            sessions[0].exe
        );

        let capture = activate_process_loopback(pid)?;
        let render = match &out_name {
            Some(name) => Some(open_render(&enumerator, name)?),
            None => None,
        };

        let original: Vec<(BOOL, f32)> = sessions
            .iter()
            .map(|s| unsafe { (s.vol.GetMute().unwrap_or_default(), s.vol.GetMasterVolume().unwrap_or(1.0)) })
            .collect();

        let phases: [(&str, Option<bool>, Option<f32>); 3] = [
            ("normal", Some(false), None),
            ("session muted", Some(true), None),
            ("session volume 0", Some(false), Some(0.0)),
        ];
        let mut results = Vec::new();
        let outcome = (|| -> anyhow::Result<()> {
            for (label, mute, vol) in phases {
                for (s, (_, orig_vol)) in sessions.iter().zip(&original) {
                    unsafe {
                        if let Some(m) = mute {
                            s.vol.SetMute(m, std::ptr::null())?;
                        }
                        s.vol.SetMasterVolume(vol.unwrap_or(*orig_vol), std::ptr::null())?;
                    }
                }
                println!("phase '{label}' for {secs}s ...");
                let stats = capture.pump(Duration::from_secs(secs), render.as_ref())?;
                println!("  -> {stats}");
                results.push((label, stats));
                if STOP.load(Ordering::SeqCst) {
                    break;
                }
            }
            Ok(())
        })();

        for (s, (mute, vol)) in sessions.iter().zip(&original) {
            unsafe {
                let _ = s.vol.SetMute(*mute, std::ptr::null());
                let _ = s.vol.SetMasterVolume(*vol, std::ptr::null());
            }
        }
        println!("session state restored");
        outcome?;

        println!("\nsummary:");
        for (label, stats) in &results {
            println!("  {label:<18} {stats}");
        }
        Ok(())
    }

    struct Session {
        pid: u32,
        exe: String,
        vol: ISimpleAudioVolume,
    }

    /// Sessions of `target` on every active render endpoint (the program may not
    /// be playing on the default device).
    fn find_sessions(enumerator: &IMMDeviceEnumerator, target: &str) -> anyhow::Result<Vec<Session>> {
        let needle = target.trim_end_matches(".exe");
        let mut out = Vec::new();
        unsafe {
            let coll = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            for i in 0..coll.GetCount()? {
                let dev = coll.Item(i)?;
                let Ok(mgr) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                let list = mgr.GetSessionEnumerator()?;
                for j in 0..list.GetCount()? {
                    let ctl = list.GetSession(j)?;
                    let ctl2: IAudioSessionControl2 = ctl.cast()?;
                    let pid = ctl2.GetProcessId().unwrap_or(0);
                    if pid == 0 {
                        continue;
                    }
                    let Some(exe) = process_name(pid) else { continue };
                    if exe.to_lowercase().trim_end_matches(".exe").contains(needle) {
                        out.push(Session { pid, exe, vol: ctl.cast()? });
                    }
                }
            }
        }
        Ok(out)
    }

    fn process_name(pid: u32) -> Option<String> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
            let _ = CloseHandle(h);
            if !ok {
                return None;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            path.rsplit('\\').next().map(str::to_string)
        }
    }

    fn pcm_format() -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: 1, // WAVE_FORMAT_PCM
            nChannels: CHANNELS,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * BLOCK as u32,
            nBlockAlign: BLOCK as u16,
            wBitsPerSample: BITS,
            cbSize: 0,
        }
    }

    #[implement(IActivateAudioInterfaceCompletionHandler)]
    struct Done(mpsc::Sender<()>);

    impl IActivateAudioInterfaceCompletionHandler_Impl for Done_Impl {
        fn ActivateCompleted(
            &self,
            _op: Option<&IActivateAudioInterfaceAsyncOperation>,
        ) -> windows::core::Result<()> {
            let _ = self.0.send(());
            Ok(())
        }
    }

    /// Raw PROPVARIANT holding a VT_BLOB (x64 layout: 8-byte header + 16-byte union).
    #[repr(C)]
    struct BlobVariant {
        vt: u16,
        _r: [u16; 3],
        cb: u32,
        ptr: *const u8,
        _pad: u64,
    }
    const VT_BLOB: u16 = 65;

    struct Capture {
        client: IAudioClient,
        cap: IAudioCaptureClient,
        event: HANDLE,
    }

    fn activate_process_loopback(pid: u32) -> anyhow::Result<Capture> {
        let params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        let var = BlobVariant {
            vt: VT_BLOB,
            _r: [0; 3],
            cb: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            ptr: &params as *const _ as *const u8,
            _pad: 0,
        };
        let (tx, rx) = mpsc::channel();
        let handler: IActivateAudioInterfaceCompletionHandler = Done(tx).into();
        unsafe {
            let op = ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(&var as *const _ as *const windows::core::PROPVARIANT),
                &handler,
            )
            .context("ActivateAudioInterfaceAsync")?;
            rx.recv_timeout(Duration::from_secs(5)).map_err(|_| anyhow!("activation timed out"))?;
            let mut hr = HRESULT(0);
            let mut unk: Option<IUnknown> = None;
            op.GetActivateResult(&mut hr, &mut unk)?;
            hr.ok().context("process loopback activation")?;
            let client: IAudioClient = unk.ok_or_else(|| anyhow!("no interface"))?.cast()?;

            let fmt = pcm_format();
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK
                        | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                    BUFFER_100MS,
                    0,
                    &fmt,
                    None,
                )
                .context("capture Initialize")?;
            let event = CreateEventW(None, false, false, None)?;
            client.SetEventHandle(event)?;
            let cap: IAudioCaptureClient = client.GetService()?;
            Ok(Capture { client, cap, event })
        }
    }

    struct Render {
        client: IAudioClient,
        rc: IAudioRenderClient,
        frames: u32,
        name: String,
    }

    fn friendly_name(dev: &IMMDevice) -> anyhow::Result<String> {
        unsafe {
            let store = dev.OpenPropertyStore(STGM_READ)?;
            Ok(format!("{}", store.GetValue(&PKEY_Device_FriendlyName)?))
        }
    }

    /// First active output endpoint whose friendly name contains `name` (case-insensitive).
    fn find_render(enumerator: &IMMDeviceEnumerator, name: &str) -> anyhow::Result<(IMMDevice, String)> {
        let needle = name.to_lowercase();
        unsafe {
            let coll = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            for i in 0..coll.GetCount()? {
                let dev = coll.Item(i)?;
                let friendly = friendly_name(&dev)?;
                if friendly.to_lowercase().contains(&needle) {
                    return Ok((dev, friendly));
                }
            }
        }
        bail!("no output device matching '{name}'")
    }

    fn open_render(enumerator: &IMMDeviceEnumerator, name: &str) -> anyhow::Result<Render> {
        unsafe {
            let (dev, friendly) = find_render(enumerator, name)?;
            let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
            let fmt = pcm_format();
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    BUFFER_100MS * 2,
                    0,
                    &fmt,
                    None,
                )
                .context("render Initialize")?;
            let frames = client.GetBufferSize()?;
            let rc: IAudioRenderClient = client.GetService()?;
            println!("playing capture on: {friendly}");
            Ok(Render { client, rc, frames, name: friendly })
        }
    }

    impl Render {
        /// Queue as much of `data` as fits; the rest is dropped (POC, no resampling/drift handling).
        fn write(&self, data: &[u8]) -> windows::core::Result<()> {
            unsafe {
                let free = self.frames - self.client.GetCurrentPadding()?;
                let n = free.min((data.len() / BLOCK) as u32);
                if n == 0 {
                    return Ok(());
                }
                let buf = self.rc.GetBuffer(n)?;
                std::ptr::copy_nonoverlapping(data.as_ptr(), buf, n as usize * BLOCK);
                self.rc.ReleaseBuffer(n, 0)
            }
        }
    }

    #[derive(Default)]
    pub struct Stats {
        frames: u64,
        sum_sq: f64,
        peak: f32,
        silent_packets: u32,
        packets: u32,
    }

    impl std::fmt::Display for Stats {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let samples = self.frames * CHANNELS as u64;
            let rms = if samples == 0 { 0.0 } else { (self.sum_sq / samples as f64).sqrt() };
            let db = |v: f64| if v <= 0.0 { f64::NEG_INFINITY } else { 20.0 * v.log10() };
            write!(
                f,
                "frames {:>7}  packets {:>4} (silent flag {:>4})  rms {:>7.1} dBFS  peak {:>7.1} dBFS",
                self.frames,
                self.packets,
                self.silent_packets,
                db(rms),
                db(self.peak as f64)
            )
        }
    }

    impl Capture {
        fn pump(&self, dur: Duration, render: Option<&Render>) -> anyhow::Result<Stats> {
            const SILENT: u32 = 0x2; // AUDCLNT_BUFFERFLAGS_SILENT
            let mut stats = Stats::default();
            let mut scratch: Vec<u8> = Vec::new();
            unsafe {
                self.client.Start()?;
                if let Some(r) = render {
                    r.client.Start()?;
                }
                let end = Instant::now() + dur;
                while Instant::now() < end && !STOP.load(Ordering::SeqCst) {
                    if WaitForSingleObject(self.event, 100) != WAIT_OBJECT_0 {
                        continue;
                    }
                    loop {
                        let mut data = std::ptr::null_mut();
                        let mut n = 0u32;
                        let mut flags = 0u32;
                        self.cap.GetBuffer(&mut data, &mut n, &mut flags, None, None)?;
                        if n == 0 {
                            self.cap.ReleaseBuffer(0)?;
                            break;
                        }
                        stats.packets += 1;
                        stats.frames += n as u64;
                        let bytes = n as usize * BLOCK;
                        scratch.clear();
                        if flags & SILENT != 0 {
                            stats.silent_packets += 1;
                            scratch.resize(bytes, 0);
                        } else {
                            scratch.extend_from_slice(std::slice::from_raw_parts(data, bytes));
                        }
                        self.cap.ReleaseBuffer(n)?;
                        for s in scratch.chunks_exact(2) {
                            let v = i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0;
                            stats.sum_sq += (v * v) as f64;
                            stats.peak = stats.peak.max(v.abs());
                        }
                        if let Some(r) = render {
                            r.write(&scratch).with_context(|| format!("render to {}", r.name))?;
                        }
                    }
                }
                self.client.Stop()?;
                if let Some(r) = render {
                    r.client.Stop()?;
                }
            }
            Ok(stats)
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.event);
            }
        }
    }

    /// Per-app default output, i.e. what Settings > "App volume and device
    /// preferences" changes. Windows persists it per executable until cleared.
    #[allow(non_snake_case)]
    mod route {
        use super::*;
        use std::ffi::c_void;
        use windows::core::{interface, IUnknown_Vtbl, HSTRING};
        use windows::Win32::Media::Audio::{
            eConsole, eMultimedia, AudioSessionStateActive, EDataFlow, ERole,
        };
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::System::WinRT::RoGetActivationFactory;

        /// KSCATEGORY_AUDIO render interface suffix of an MMDevice interface path.
        const RENDER_INTERFACE: &str = "{e6327cad-dcec-4949-ae8a-991e976a79d2}";

        // Undocumented activation factory behind `Windows.Media.Internal.AudioPolicyConfig`
        // (Win10 21H2+ IID; EarTrumpet uses the same layout). Only the three
        // persisted-endpoint methods are called; the rest are vtable placeholders.
        #[interface("ab3d4648-e242-459f-b02f-541c70306324")]
        unsafe trait IAudioPolicyConfigFactory: IUnknown {
            // IInspectable
            fn GetIids(&self) -> HRESULT;
            fn GetRuntimeClassName(&self) -> HRESULT;
            fn GetTrustLevel(&self) -> HRESULT;
            fn p00(&self) -> HRESULT;
            fn p01(&self) -> HRESULT;
            fn p02(&self) -> HRESULT;
            fn p03(&self) -> HRESULT;
            fn p04(&self) -> HRESULT;
            fn p05(&self) -> HRESULT;
            fn p06(&self) -> HRESULT;
            fn p07(&self) -> HRESULT;
            fn p08(&self) -> HRESULT;
            fn p09(&self) -> HRESULT;
            fn p10(&self) -> HRESULT;
            fn p11(&self) -> HRESULT;
            fn p12(&self) -> HRESULT;
            fn p13(&self) -> HRESULT;
            fn p14(&self) -> HRESULT;
            fn p15(&self) -> HRESULT;
            fn p16(&self) -> HRESULT;
            fn p17(&self) -> HRESULT;
            fn p18(&self) -> HRESULT;
            fn SetPersistedDefaultAudioEndpoint(
                &self,
                pid: u32,
                flow: EDataFlow,
                role: ERole,
                device: *mut c_void,
            ) -> HRESULT;
            fn GetPersistedDefaultAudioEndpoint(
                &self,
                pid: u32,
                flow: EDataFlow,
                role: ERole,
                device: *mut *mut c_void,
            ) -> HRESULT;
            fn ClearAllPersistedApplicationDefaultEndpoints(&self) -> HRESULT;
        }

        fn factory() -> anyhow::Result<IAudioPolicyConfigFactory> {
            unsafe {
                RoGetActivationFactory(&HSTRING::from("Windows.Media.Internal.AudioPolicyConfig"))
                    .context("AudioPolicyConfig factory")
            }
        }

        fn interface_path(dev: &IMMDevice) -> anyhow::Result<String> {
            unsafe {
                let id = dev.GetId()?;
                let s = id.to_string();
                CoTaskMemFree(Some(id.0 as _));
                Ok(format!(r"\\?\SWD#MMDEVAPI#{}#{RENDER_INTERFACE}", s?))
            }
        }

        /// Route `pid`'s output to `dev`, or back to the system default with `None`.
        fn set(f: &IAudioPolicyConfigFactory, pid: u32, dev: Option<&IMMDevice>) -> anyhow::Result<()> {
            let path = dev.map(interface_path).transpose()?.map(HSTRING::from);
            for role in [eConsole, eMultimedia] {
                unsafe {
                    let raw: *mut c_void = match &path {
                        Some(h) => std::mem::transmute_copy(h),
                        None => std::ptr::null_mut(),
                    };
                    f.SetPersistedDefaultAudioEndpoint(pid, eRender, role, raw)
                        .ok()
                        .context("SetPersistedDefaultAudioEndpoint")?;
                }
            }
            Ok(())
        }

        fn get(f: &IAudioPolicyConfigFactory, pid: u32) -> String {
            unsafe {
                let mut raw: *mut c_void = std::ptr::null_mut();
                if let Err(e) = f.GetPersistedDefaultAudioEndpoint(pid, eRender, eMultimedia, &mut raw).ok() {
                    return format!("<error {e}>");
                }
                let h: HSTRING = std::mem::transmute(raw);
                if h.is_empty() { "<none>".into() } else { h.to_string() }
            }
        }

        /// "device [active|idle]" for every session of `pid`.
        fn session_devices(enumerator: &IMMDeviceEnumerator, pid: u32) -> anyhow::Result<Vec<String>> {
            let mut out = Vec::new();
            unsafe {
                let coll = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
                for i in 0..coll.GetCount()? {
                    let dev = coll.Item(i)?;
                    let Ok(mgr) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                    let list = mgr.GetSessionEnumerator()?;
                    for j in 0..list.GetCount()? {
                        let ctl = list.GetSession(j)?;
                        let ctl2: IAudioSessionControl2 = ctl.cast()?;
                        if ctl2.GetProcessId().unwrap_or(0) == pid {
                            let state = if ctl.GetState()? == AudioSessionStateActive { "active" } else { "idle" };
                            out.push(format!("{} [{state}]", friendly_name(&dev)?));
                        }
                    }
                }
            }
            Ok(out)
        }

        pub fn run(args: &[String]) -> anyhow::Result<()> {
            let (Some(pid), Some(a), Some(b)) = (args.first(), args.get(1), args.get(2)) else {
                bail!("usage: loopback_poc --route <pid> <device-a> <device-b> [seconds]");
            };
            let pid: u32 = pid.parse().context("pid")?;
            let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4);
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
            let f = factory()?;
            let (dev_a, name_a) = find_render(&enumerator, a)?;
            let (dev_b, name_b) = find_render(&enumerator, b)?;
            let show = |label: &str| {
                println!(
                    "{label}: override {} | sessions {:?}",
                    get(&f, pid),
                    session_devices(&enumerator, pid).unwrap_or_default()
                );
            };

            show("before");
            let outcome = (|| -> anyhow::Result<()> {
                set(&f, pid, Some(&dev_a))?;
                show(&format!("routed to '{name_a}'"));
                // Lets a test player wait until it is routed before it makes a sound.
                if let Ok(flag) = std::env::var("POC_READY_FILE") {
                    std::fs::write(flag, b"")?;
                }
                let capture = activate_process_loopback(pid)?;
                let s = capture.pump(Duration::from_secs(secs), None)?;
                show("after capture A");
                println!("  capture A -> {s}");
                set(&f, pid, Some(&dev_b))?;
                std::thread::sleep(Duration::from_millis(500));
                show(&format!("moved to '{name_b}' mid-stream"));
                let s = capture.pump(Duration::from_secs(secs), None)?;
                show("after capture B");
                println!("  capture B -> {s}");
                Ok(())
            })();
            set(&f, pid, None)?;
            show("cleared");
            outcome
        }
    }
}

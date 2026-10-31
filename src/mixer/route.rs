//! Per-program output routing on Windows — what Settings > "App volume and
//! device preferences" changes.
//!
//! Windows persists such an override per executable until it is cleared, so a
//! program Slidr moved to the sink would stay silent if Slidr died. Every
//! override therefore goes into a journal first; on the next run the stale
//! entries are cleared as soon as their program shows up again.

// The COM method names below are Windows'.
#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ffi::c_void;

use serde::{Deserialize, Serialize};
use windows::core::{interface, IUnknown, IUnknown_Vtbl, HRESULT, HSTRING};
use windows::Win32::Media::Audio::{eConsole, eMultimedia, eRender, EDataFlow, ERole, IMMDevice};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::WinRT::RoGetActivationFactory;

/// KSCATEGORY_AUDIO render interface suffix of an MMDevice interface path.
const RENDER_INTERFACE: &str = "{e6327cad-dcec-4949-ae8a-991e976a79d2}";

// Undocumented activation factory behind `Windows.Media.Internal.AudioPolicyConfig`
// (Win10 21H2+ IID; EarTrumpet uses the same layout). Only the persisted-endpoint
// method is called; the rest are vtable placeholders, starting with
// IInspectable's three (GetIids, GetRuntimeClassName, GetTrustLevel).
#[interface("ab3d4648-e242-459f-b02f-541c70306324")]
unsafe trait IAudioPolicyConfigFactory: IUnknown {
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
    fn p19(&self) -> HRESULT;
    fn p20(&self) -> HRESULT;
    fn p21(&self) -> HRESULT;
    fn SetPersistedDefaultAudioEndpoint(
        &self,
        pid: u32,
        flow: EDataFlow,
        role: ERole,
        device: *mut c_void,
    ) -> HRESULT;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    pid: u32,
    /// Image path, to find the same program again under a new pid.
    exe: String,
}

pub struct Router {
    factory: IAudioPolicyConfigFactory,
    /// Overrides Slidr owns, by pid; mirrored in the journal file.
    journal: Vec<Entry>,
}

impl Router {
    pub fn new() -> Option<Self> {
        let factory = unsafe {
            RoGetActivationFactory::<IAudioPolicyConfigFactory>(&HSTRING::from(
                "Windows.Media.Internal.AudioPolicyConfig",
            ))
        };
        match factory {
            Ok(factory) => Some(Self { factory, journal: load_journal() }),
            Err(e) => {
                log::warn!("mixer: per-program routing unavailable ({e}); nothing will be rerouted");
                None
            }
        }
    }

    pub fn is_routed(&self, pid: u32) -> bool {
        self.journal.iter().any(|e| e.pid == pid)
    }

    /// Move `pid`'s output to `dev`. Fails while the program has no audio
    /// session yet — the caller retries on its next scan.
    pub fn route(&mut self, pid: u32, dev: &IMMDevice) -> windows::core::Result<()> {
        let path = HSTRING::from(interface_path(dev)?);
        // Journal first: if we die between the two, the next run clears an
        // override that may not exist, which is harmless.
        if !self.is_routed(pid) {
            let exe = crate::app_groups::process_path(pid).unwrap_or_default();
            self.journal.push(Entry { pid, exe });
            save_journal(&self.journal);
        }
        self.set(pid, Some(&path))
    }

    /// Clear overrides left over from an earlier run (crash, kill) for programs
    /// that are playing right now and that Slidr is not routing on purpose.
    /// `sessions` = (pid, image path) of everything with an audio session.
    pub fn recover(&mut self, sessions: &HashMap<u32, String>, keep: &dyn Fn(u32) -> bool) {
        let mut changed = false;
        for entry in std::mem::take(&mut self.journal) {
            let live = sessions.contains_key(&entry.pid);
            if live && keep(entry.pid) {
                self.journal.push(entry);
                continue;
            }
            // Same pid still playing, or the same program under a new pid —
            // the override belongs to the executable, so any of its pids works.
            let pid = if live {
                Some(entry.pid)
            } else {
                sessions
                    .iter()
                    .find(|(_, exe)| !entry.exe.is_empty() && exe.eq_ignore_ascii_case(&entry.exe))
                    .map(|(pid, _)| *pid)
            };
            match pid {
                // Not playing right now; try again when it is.
                None => self.journal.push(entry),
                // Routed on purpose under its new pid, which has its own entry.
                Some(p) if keep(p) => changed = true,
                Some(p) => {
                    log::info!("mixer: restoring output of {} (pid {p})", entry.exe);
                    if self.set(p, None).is_ok() {
                        changed = true;
                    } else {
                        self.journal.push(entry);
                    }
                }
            }
        }
        if changed {
            save_journal(&self.journal);
        }
    }

    /// Restore everything Slidr routed whose program is still running.
    pub fn clear_all(&mut self) {
        for pid in self.journal.iter().map(|e| e.pid).collect::<Vec<_>>() {
            if self.set(pid, None).is_ok() {
                self.journal.retain(|e| e.pid != pid);
            }
        }
        save_journal(&self.journal);
    }

    pub fn pending(&self) -> bool {
        !self.journal.is_empty()
    }

    fn set(&self, pid: u32, path: Option<&HSTRING>) -> windows::core::Result<()> {
        for role in [eConsole, eMultimedia] {
            unsafe {
                let raw: *mut c_void = match path {
                    Some(h) => std::mem::transmute_copy(h),
                    None => std::ptr::null_mut(),
                };
                self.factory.SetPersistedDefaultAudioEndpoint(pid, eRender, role, raw).ok()?;
            }
        }
        Ok(())
    }
}

fn interface_path(dev: &IMMDevice) -> windows::core::Result<String> {
    unsafe {
        let id = dev.GetId()?;
        let s = id.to_string();
        CoTaskMemFree(Some(id.0 as _));
        Ok(format!(r"\\?\SWD#MMDEVAPI#{}#{RENDER_INTERFACE}", s?))
    }
}

fn journal_path() -> std::path::PathBuf {
    crate::storage::config_root().join(super::JOURNAL_FILE)
}

fn load_journal() -> Vec<Entry> {
    std::fs::read(journal_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_journal(entries: &[Entry]) {
    let path = journal_path();
    let result = if entries.is_empty() {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        serde_json::to_vec_pretty(entries)
            .map_err(std::io::Error::other)
            .and_then(|json| std::fs::write(&path, json))
    };
    if let Err(e) = result {
        log::warn!("mixer: could not write {}: {e}", path.display());
    }
}

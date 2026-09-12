//! Cross-platform key simulation with key-repeat semantics matching the reference.
//!
//! `hold_start(vk)` emits the initial key-down and, if the key is "tap-only"
//! (media keys etc.), schedules repeats at the system repeat rate. Regular
//! keys are held physically and rely on the OS auto-repeat.
//!
//! The wire format everywhere in Slidr (presets, `keys_library`, the board
//! protocol) is a **Windows Virtual-Key code**. That is only directly usable by
//! `enigo` on Windows — on Linux `Key::Other` is interpreted as an *X11 keysym*,
//! which shares numbering with VK codes for ASCII letters/digits and for nothing
//! else. So VK codes are translated at the enigo boundary; see `platform_key`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use enigo::{Direction, Enigo, Key, Keyboard, Settings as EnigoSettings};

/// Translate a Windows VK code into the key representation the platform's
/// `enigo` backend expects. `None` = no sensible mapping (caller skips the key).
#[cfg(target_os = "windows")]
fn platform_key(vk: u32) -> Option<Key> {
    Some(Key::Other(vk))
}

/// On Linux `Key::Other(v)` is fed to `Keysym::from(v)`, so we must hand enigo
/// an X11 keysym, not a VK code. Without this, F-keys type letters, arrows type
/// punctuation and every media/modifier key is a silent no-op.
#[cfg(not(target_os = "windows"))]
fn platform_key(vk: u32) -> Option<Key> {
    vk_to_keysym(vk).map(Key::Other)
}

#[cfg(not(target_os = "windows"))]
use crate::keys_vk::vk_to_keysym;


/// Initial delay before a tap-only key starts repeating, and the repeat period.
const REPEAT_DELAY: Duration = Duration::from_millis(500);
const REPEAT_PERIOD: Duration = Duration::from_millis(33);

pub struct KeyController {
    enigo: Enigo,
    holds: HashMap<u32, Arc<AtomicBool>>,
}

impl KeyController {
    pub fn new() -> anyhow::Result<Self> {
        warn_if_no_x11();
        let enigo = Enigo::new(&EnigoSettings::default())
            .map_err(|e| anyhow::anyhow!("Enigo init failed: {e:?}"))?;
        Ok(Self { enigo, holds: HashMap::new() })
    }

    pub fn hold_start(&mut self, vk: u32) {
        if self.holds.contains_key(&vk) {
            return;
        }
        let Some(key) = platform_key(vk) else {
            log::warn!("key {vk} (0x{vk:02X}) has no mapping on this platform; ignored");
            return;
        };
        if let Err(e) = self.enigo.key(key, Direction::Press) {
            log::warn!("key press {vk} failed: {e:?}");
            return;
        }

        // For tap-style media keys we emit a quick release and arm a repeat
        // timer. Detection here is conservative: only keys in the media range.
        let tap_only = matches!(vk, 0xAE..=0xB7);
        if tap_only {
            let _ = self.enigo.key(key, Direction::Release);
            let cancel = Arc::new(AtomicBool::new(false));
            self.holds.insert(vk, cancel.clone());
            spawn_repeater(vk, cancel);
        } else {
            self.holds.insert(vk, Arc::new(AtomicBool::new(false)));
        }
    }

    pub fn hold_stop(&mut self, vk: u32) {
        if let Some(cancel) = self.holds.remove(&vk) {
            cancel.store(true, Ordering::Relaxed);
        }
        let Some(key) = platform_key(vk) else { return };
        if let Err(e) = self.enigo.key(key, Direction::Release) {
            log::debug!("key release {vk}: {e:?}");
        }
    }
}

/// Repeat `vk` until cancelled. `Enigo` is not `Send`, so the thread builds its
/// own and **keeps** it for the whole hold — the previous version constructed a
/// fresh one per tick, which on Linux meant opening an X11 connection and
/// re-uploading a keymap 30x/s.
fn spawn_repeater(vk: u32, cancel: Arc<AtomicBool>) {
    let _ = thread::Builder::new()
        .name("slidr-key-repeat".into())
        .spawn(move || {
            thread::sleep(REPEAT_DELAY);
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let Some(key) = platform_key(vk) else { return };
            let Ok(mut enigo) = Enigo::new(&EnigoSettings::default()) else {
                log::warn!("key repeat {vk}: cannot open input connection");
                return;
            };
            while !cancel.load(Ordering::Relaxed) {
                if let Err(e) = enigo.key(key, Direction::Click) {
                    log::debug!("key repeat {vk} stopped: {e:?}");
                    return;
                }
                thread::sleep(REPEAT_PERIOD);
            }
        });
}

/// enigo is built with the `x11rb` backend only, which reaches native X11
/// clients and XWayland clients but *not* native Wayland clients. Say so once,
/// loudly, instead of letting key actions silently do nothing.
#[cfg(not(target_os = "windows"))]
fn warn_if_no_x11() {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").map(|v| v == "wayland").unwrap_or(false);
    if wayland {
        log::warn!(
            "Wayland session detected: simulated keys are delivered over XTEST and will only \
             reach XWayland (X11) windows, not native Wayland windows. Log in with an X11/Xorg \
             session if you need key actions to work everywhere."
        );
    }
    if std::env::var_os("DISPLAY").is_none() {
        log::warn!("$DISPLAY is unset; key actions will be unavailable");
    }
}

#[cfg(target_os = "windows")]
fn warn_if_no_x11() {}

//! Cross-platform key simulation with key-repeat semantics matching the reference.
//!
//! `hold_start(vk)` emits the initial key-down and, if the key is "tap-only"
//! (media keys etc.), schedules repeats at the system repeat rate. Regular
//! keys are held physically and rely on the OS auto-repeat.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use enigo::{Direction, Enigo, Key, Keyboard, Settings as EnigoSettings};

pub struct KeyController {
    enigo: Enigo,
    holds: HashMap<u32, Arc<AtomicBool>>,
}

impl KeyController {
    pub fn new() -> anyhow::Result<Self> {
        let enigo = Enigo::new(&EnigoSettings::default())
            .map_err(|e| anyhow::anyhow!("Enigo init failed: {e:?}"))?;
        Ok(Self { enigo, holds: HashMap::new() })
    }

    pub fn hold_start(&mut self, vk: u32) {
        if self.holds.contains_key(&vk) {
            return;
        }
        let key = Key::Other(vk);
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
            // Spawn a repeater. Conservative defaults: 500ms initial, 33ms repeat.
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(500));
                while !cancel.load(Ordering::Relaxed) {
                    // We can't reuse `Enigo` here (not Send); spawn a fresh one.
                    if let Ok(mut e) = Enigo::new(&EnigoSettings::default()) {
                        let _ = e.key(Key::Other(vk), Direction::Click);
                    }
                    thread::sleep(Duration::from_millis(33));
                }
            });
        } else {
            self.holds.insert(vk, Arc::new(AtomicBool::new(false)));
        }
    }

    pub fn hold_stop(&mut self, vk: u32) {
        if let Some(cancel) = self.holds.remove(&vk) {
            cancel.store(true, Ordering::Relaxed);
        }
        let key = Key::Other(vk);
        if let Err(e) = self.enigo.key(key, Direction::Release) {
            log::debug!("key release {vk}: {e:?}");
        }
    }
}

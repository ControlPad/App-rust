//! `slidr --quit`: ask a running Slidr to exit the normal way — the same path
//! as its Exit button, so the mixer hands rerouted programs back first.
//! Killing the process skips that (the next start repairs it, but until then
//! those programs play into the silent output).

#![cfg(target_os = "windows")]

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE, INFINITE,
};

const EVENT: PCWSTR = w!("Local\\SlidrQuit");

/// Wait (on a background thread) for a `--quit` request and leave the event
/// loop when one comes.
pub fn listen() {
    let spawned = std::thread::Builder::new().name("quit-signal".into()).spawn(|| unsafe {
        let Ok(event) = CreateEventW(None, false, false, EVENT) else {
            log::warn!("--quit listener unavailable");
            return;
        };
        if WaitForSingleObject(event, INFINITE) == WAIT_OBJECT_0 {
            log::info!("quit requested by another process");
            let _ = slint::invoke_from_event_loop(|| {
                let _ = slint::quit_event_loop();
            });
        }
        let _ = CloseHandle(event);
    });
    if let Err(e) = spawned {
        log::warn!("--quit listener not started: {e}");
    }
}

/// Signal a running Slidr to quit. `false` if none is listening.
pub fn signal() -> bool {
    unsafe {
        match OpenEventW(EVENT_MODIFY_STATE, false, EVENT) {
            Ok(event) => {
                let ok = SetEvent(event).is_ok();
                let _ = CloseHandle(event);
                ok
            }
            Err(_) => false,
        }
    }
}

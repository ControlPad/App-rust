//! OS media-session state ("is music playing?") as an LED condition source.
//!
//! Windows exposes every app that publishes transport controls — Spotify, a
//! browser tab, a desktop player — through the System Media Transport Controls
//! (SMTC). Reading that instead of talking to Spotify's own API means no login,
//! no API key, and it works for whatever the user actually plays music with.
//!
//! The WinRT calls block on an async operation, so they run on a dedicated
//! poller thread and everything else reads the cached snapshot. Non-Windows
//! builds report "nothing playing" (the MPRIS equivalent would go here).

use std::sync::OnceLock;
use std::time::Duration;

use parking_lot::Mutex;

/// How often the media session list is re-read.
const POLL: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone, Default)]
pub struct MediaState {
    /// Source ids (Windows: the app user-model id) that are playing right now.
    pub playing: Vec<String>,
    /// All known media sources, playing or not — used to populate the picker.
    pub sources: Vec<String>,
}

impl MediaState {
    /// Is anything playing? With `filter` set, only that source counts
    /// (case-insensitive substring, so "spotify" matches "Spotify.exe").
    pub fn is_playing(&self, filter: Option<&str>) -> bool {
        match filter.map(str::trim).filter(|f| !f.is_empty()) {
            None => !self.playing.is_empty(),
            Some(f) => {
                let f = f.to_lowercase();
                self.playing.iter().any(|s| s.to_lowercase().contains(&f))
            }
        }
    }
}

static STATE: OnceLock<Mutex<MediaState>> = OnceLock::new();

/// Latest snapshot. The first call starts the poller thread, so a caller that
/// asks immediately after startup may see an empty state for up to one poll.
pub fn state() -> MediaState {
    let cell = STATE.get_or_init(|| {
        start_poller();
        Mutex::new(MediaState::default())
    });
    cell.lock().clone()
}

/// Media sources the OS knows about, for the condition's target picker.
pub fn sources() -> Vec<String> {
    state().sources
}

fn start_poller() {
    let _ = std::thread::Builder::new()
        .name("slidr-media".into())
        .spawn(|| {
            // Own apartment: the WinRT calls below block on an async operation,
            // which must not happen on the UI thread's STA.
            #[cfg(target_os = "windows")]
            unsafe {
                use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            loop {
                let next = read_sessions();
                if let Some(cell) = STATE.get() {
                    *cell.lock() = next;
                }
                std::thread::sleep(POLL);
            }
        });
}

#[cfg(target_os = "windows")]
fn read_sessions() -> MediaState {
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager as SessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    };

    let mut out = MediaState::default();
    let manager = match SessionManager::RequestAsync().and_then(|op| op.get()) {
        Ok(m) => m,
        Err(e) => {
            log::debug!("media: session manager unavailable: {e}");
            return out;
        }
    };
    let Ok(sessions) = manager.GetSessions() else { return out };
    for session in sessions {
        let Ok(id) = session.SourceAppUserModelId() else { continue };
        let id = id.to_string();
        if id.is_empty() {
            continue;
        }
        if !out.sources.contains(&id) {
            out.sources.push(id.clone());
        }
        let playing = session
            .GetPlaybackInfo()
            .and_then(|info| info.PlaybackStatus())
            .map(|status| status == PlaybackStatus::Playing)
            .unwrap_or(false);
        if playing && !out.playing.contains(&id) {
            out.playing.push(id);
        }
    }
    out
}

#[cfg(not(target_os = "windows"))]
fn read_sessions() -> MediaState {
    // Linux would read MPRIS over D-Bus (org.mpris.MediaPlayer2.*); Slidr has no
    // D-Bus dependency today, so the condition simply never fires there.
    MediaState::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matching() {
        let s = MediaState {
            playing: vec!["Spotify.exe".into()],
            sources: vec!["Spotify.exe".into(), "firefox.exe".into()],
        };
        assert!(s.is_playing(None));
        assert!(s.is_playing(Some("spotify")));
        assert!(s.is_playing(Some("  ")), "blank filter means any source");
        assert!(!s.is_playing(Some("firefox")));
        assert!(!MediaState::default().is_playing(None));
    }
}

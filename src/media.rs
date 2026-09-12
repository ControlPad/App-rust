//! OS media-session state as an LED condition source.
//!
//! Windows exposes every app that publishes transport controls — Spotify, a
//! browser tab, a desktop player — through the System Media Transport Controls
//! (SMTC). Reading that instead of talking to Spotify's own API means no login,
//! no API key, and it works for whatever the user actually plays music with.
//!
//! The WinRT calls block on an async operation, so they run on a dedicated
//! poller thread and everything else reads the cached snapshot. Non-Windows
//! builds report "nothing" (the MPRIS equivalent would go here).

use std::sync::OnceLock;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// How often the media session list is re-read.
const POLL: Duration = Duration::from_millis(1000);

/// Playback state of a media source, as an LED condition asks about it. The four
/// are mutually exclusive, so a condition is a plain equality test.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaStatus {
    #[default]
    Playing,
    Paused,
    /// A player is open but neither playing nor paused — stopped, or between
    /// tracks while it loads the next one.
    Stopped,
    /// No media session at all: nothing is even open.
    Nothing,
}

impl MediaStatus {
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Paused,
            2 => Self::Stopped,
            3 => Self::Nothing,
            _ => Self::Playing,
        }
    }
    pub fn to_index(self) -> i32 {
        match self {
            Self::Playing => 0,
            Self::Paused => 1,
            Self::Stopped => 2,
            Self::Nothing => 3,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Playing => "playing",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
            Self::Nothing => "nothing",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MediaState {
    /// Every media session the OS reports, as (source id, status).
    pub sessions: Vec<(String, MediaStatus)>,
}

impl MediaState {
    /// The single status a condition compares against. With several players the
    /// liveliest one wins — something actually playing outranks something merely
    /// paused — so "Playing" means "music is on" regardless of what else is
    /// open. With `filter` set only matching sources count (case-insensitive
    /// substring, so "spotify" matches "Spotify.exe"); if none match, the answer
    /// is `Nothing`.
    pub fn status(&self, filter: Option<&str>) -> MediaStatus {
        let filter = filter
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_lowercase);
        let mut best: Option<MediaStatus> = None;
        for (id, st) in &self.sessions {
            if let Some(f) = &filter {
                if !id.to_lowercase().contains(f.as_str()) {
                    continue;
                }
            }
            best = Some(match (best, *st) {
                (Some(MediaStatus::Playing), _) | (_, MediaStatus::Playing) => MediaStatus::Playing,
                (Some(MediaStatus::Paused), _) | (_, MediaStatus::Paused) => MediaStatus::Paused,
                _ => MediaStatus::Stopped,
            });
        }
        best.unwrap_or(MediaStatus::Nothing)
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

/// Media sources the OS knows about with their status, for the UI hint.
pub fn sources() -> Vec<(String, MediaStatus)> {
    state().sessions
}

fn start_poller() {
    // Nothing to poll where there is no media-session API — don't burn a thread
    // (and a wakeup per second) re-reading an empty answer.
    if cfg!(not(target_os = "windows")) {
        return;
    }
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
        let status = match session.GetPlaybackInfo().and_then(|info| info.PlaybackStatus()) {
            Ok(PlaybackStatus::Playing) => MediaStatus::Playing,
            Ok(PlaybackStatus::Paused) => MediaStatus::Paused,
            // Stopped / Opened / Changing all mean "a player is there but no
            // sound is coming out"; Closed sessions are not enumerated.
            Ok(_) => MediaStatus::Stopped,
            Err(_) => continue,
        };
        // One entry per source — a player can register more than one session,
        // and the livelier one is the interesting one.
        match out.sessions.iter_mut().find(|(known, _)| *known == id) {
            Some(slot) => {
                if status == MediaStatus::Playing {
                    slot.1 = status;
                }
            }
            None => out.sessions.push((id, status)),
        }
    }
    out
}

#[cfg(not(target_os = "windows"))]
fn read_sessions() -> MediaState {
    // Linux would read MPRIS over D-Bus (org.mpris.MediaPlayer2.*); Slidr has no
    // D-Bus dependency today, so the condition reports "nothing" there.
    MediaState::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_of(v: &[(&str, MediaStatus)]) -> MediaState {
        MediaState { sessions: v.iter().map(|(a, b)| (a.to_string(), *b)).collect() }
    }

    #[test]
    fn nothing_without_a_matching_session() {
        assert_eq!(MediaState::default().status(None), MediaStatus::Nothing);
        let s = state_of(&[("Spotify.exe", MediaStatus::Playing)]);
        assert_eq!(s.status(Some("vlc")), MediaStatus::Nothing);
    }

    #[test]
    fn filter_is_a_case_insensitive_substring() {
        let s = state_of(&[
            ("Spotify.exe", MediaStatus::Playing),
            ("firefox.exe", MediaStatus::Paused),
        ]);
        assert_eq!(s.status(Some("spotify")), MediaStatus::Playing);
        assert_eq!(s.status(Some("firefox")), MediaStatus::Paused);
        assert_eq!(s.status(Some("   ")), MediaStatus::Playing, "blank filter = any source");
    }

    #[test]
    fn the_liveliest_source_wins() {
        // A paused browser tab next to a playing player still means "playing".
        let s = state_of(&[
            ("firefox.exe", MediaStatus::Paused),
            ("Spotify.exe", MediaStatus::Playing),
        ]);
        assert_eq!(s.status(None), MediaStatus::Playing);

        let s = state_of(&[
            ("firefox.exe", MediaStatus::Stopped),
            ("Spotify.exe", MediaStatus::Paused),
        ]);
        assert_eq!(s.status(None), MediaStatus::Paused);

        let s = state_of(&[("firefox.exe", MediaStatus::Stopped)]);
        assert_eq!(s.status(None), MediaStatus::Stopped);
    }

    #[test]
    fn status_index_round_trip() {
        for st in [
            MediaStatus::Playing,
            MediaStatus::Paused,
            MediaStatus::Stopped,
            MediaStatus::Nothing,
        ] {
            assert_eq!(MediaStatus::from_index(st.to_index()), st);
        }
    }
}

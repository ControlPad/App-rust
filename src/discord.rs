//! Discord voice state ("am I muted?") as an LED condition source.
//!
//! Discord's desktop client exposes a local RPC socket — a named pipe on
//! Windows, a unix socket elsewhere — that carries the same voice state the
//! client shows. Nothing else reports Discord's *self*-mute: the OS only knows
//! about the microphone endpoint, which stays unmuted while Discord mutes you
//! internally. So the socket it is.
//!
//! Access is gated on an OAuth application, which the user creates once in the
//! Discord developer portal and enters in Slidr's settings:
//!
//! 1. <https://discord.com/developers/applications> → New Application,
//! 2. OAuth2 → add the redirect `http://localhost`,
//! 3. copy the Client ID and Client Secret into Slidr.
//!
//! On first connect Discord shows an authorize prompt; the resulting token is
//! cached in the config directory so later starts are silent. The secret is only
//! needed to obtain a token — once one is cached, it is no longer read, and
//! `SLIDR_DISCORD_CLIENT_SECRET` can supply it instead of the settings file.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Scopes we need: `rpc` to talk to the client at all, `rpc.voice.read` for the
/// voice settings. Both are read-only.
const SCOPES: [&str; 2] = ["rpc", "rpc.voice.read"];
const REDIRECT_URI: &str = "http://localhost";
const TOKEN_URL: &str = "https://discord.com/api/v10/oauth2/token";

/// Wait between reconnect attempts when Discord isn't running (or rejected us).
const RECONNECT_DELAY: Duration = Duration::from_secs(10);

/// What an LED condition can ask about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscordSignal {
    /// Self-muted in Discord (the client's own mute, not the OS mic).
    #[default]
    Muted,
    /// Self-deafened.
    Deafened,
    /// Connected to a voice channel.
    InVoice,
}

impl DiscordSignal {
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Deafened,
            2 => Self::InVoice,
            _ => Self::Muted,
        }
    }
    pub fn to_index(self) -> i32 {
        match self {
            Self::Muted => 0,
            Self::Deafened => 1,
            Self::InVoice => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DiscordState {
    /// The RPC socket is open and authenticated.
    pub connected: bool,
    pub muted: bool,
    pub deafened: bool,
    pub in_voice: bool,
}

impl DiscordState {
    pub fn get(&self, signal: DiscordSignal) -> bool {
        if !self.connected {
            return false;
        }
        match signal {
            DiscordSignal::Muted => self.muted,
            DiscordSignal::Deafened => self.deafened,
            DiscordSignal::InVoice => self.in_voice,
        }
    }
}

static STATE: OnceLock<Mutex<DiscordState>> = OnceLock::new();
/// Why the last connection attempt failed, for the settings/LED status line.
/// Without this the UI can only say "not connected", which tells the user
/// nothing about whether Discord is closed, the id is wrong, or the redirect
/// URI is missing from their application.
static LAST_ERROR: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static CONFIG: OnceLock<Mutex<Option<Credentials>>> = OnceLock::new();
static RUNNING: AtomicBool = AtomicBool::new(false);
/// Bumped whenever the credentials change, so an older worker retires itself.
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, PartialEq, Eq)]
struct Credentials {
    client_id: String,
    client_secret: String,
}

fn state_cell() -> &'static Mutex<DiscordState> {
    STATE.get_or_init(|| Mutex::new(DiscordState::default()))
}

fn config_cell() -> &'static Mutex<Option<Credentials>> {
    CONFIG.get_or_init(|| Mutex::new(None))
}

pub fn state() -> DiscordState {
    *state_cell().lock()
}

fn error_cell() -> &'static Mutex<Option<String>> {
    LAST_ERROR.get_or_init(|| Mutex::new(None))
}

/// Why the worker is not connected right now, if it has tried and failed.
pub fn last_error() -> Option<String> {
    error_cell().lock().clone()
}

/// Does this look like a Discord application id? They are snowflakes — a run of
/// digits — and settings are saved on every keystroke, so a half-typed id must
/// not send us off to open a socket and fail.
pub fn is_valid_id(client_id: &str) -> bool {
    let id = client_id.trim();
    id.len() >= 15 && id.len() <= 25 && id.chars().all(|c| c.is_ascii_digit())
}

/// Point the worker at a Discord application. Called whenever settings change;
/// a blank (or not-yet-complete) client id stops the worker. The secret may also
/// come from `SLIDR_DISCORD_CLIENT_SECRET`, which takes precedence.
pub fn configure(client_id: &str, client_secret: &str) {
    let secret = std::env::var("SLIDR_DISCORD_CLIENT_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| client_secret.to_string());
    let next = is_valid_id(client_id).then(|| Credentials {
        client_id: client_id.trim().to_string(),
        client_secret: secret,
    });

    let mut slot = config_cell().lock();
    if *slot == next {
        return;
    }
    *slot = next.clone();
    GENERATION.fetch_add(1, Ordering::SeqCst);
    *state_cell().lock() = DiscordState::default();
    *error_cell().lock() = None;
    drop(slot);

    if next.is_none() {
        log::info!("discord: no usable client id; voice state disabled");
        return;
    }
    // One worker for the process; it re-reads the credentials each round.
    if !RUNNING.swap(true, Ordering::SeqCst) {
        let _ = std::thread::Builder::new()
            .name("slidr-discord".into())
            .spawn(worker);
    }
}

fn worker() {
    loop {
        let generation = GENERATION.load(Ordering::SeqCst);
        let creds = config_cell().lock().clone();
        let Some(creds) = creds else {
            std::thread::sleep(RECONNECT_DELAY);
            continue;
        };
        match session(&creds, generation) {
            Ok(()) => log::info!("discord: session ended"),
            Err(e) => {
                log::warn!("discord: {e}");
                *error_cell().lock() = Some(e.to_string());
            }
        }
        *state_cell().lock() = DiscordState::default();
        std::thread::sleep(RECONNECT_DELAY);
    }
}

/// One connect → authenticate → subscribe → read-loop cycle. Returns when the
/// socket closes or the credentials change under us.
fn session(creds: &Credentials, generation: u64) -> anyhow::Result<()> {
    let mut conn = Connection::open(&creds.client_id)?;
    log::info!("discord: connected to the local RPC socket");

    let token = authenticate(&mut conn, creds)?;
    save_token(&token);
    state_cell().lock().connected = true;
    *error_cell().lock() = None;
    log::info!("discord: authenticated");

    // Current values first, then the live updates.
    conn.send_cmd("GET_VOICE_SETTINGS", json!({}))?;
    conn.send_cmd("GET_SELECTED_VOICE_CHANNEL", json!({}))?;
    conn.subscribe("VOICE_SETTINGS_UPDATE")?;
    conn.subscribe("VOICE_CHANNEL_SELECT")?;

    loop {
        if GENERATION.load(Ordering::SeqCst) != generation {
            return Ok(()); // credentials changed; the worker restarts us
        }
        let (_, msg) = conn.recv()?;
        apply_message(&msg);
    }
}

/// Fold one RPC message into the shared state. Voice settings arrive both as a
/// command response and as a subscribed event, with the same payload shape.
fn apply_message(msg: &Value) {
    let evt = msg.get("evt").and_then(Value::as_str).unwrap_or("");
    let cmd = msg.get("cmd").and_then(Value::as_str).unwrap_or("");
    let data = msg.get("data");

    if evt == "VOICE_SETTINGS_UPDATE" || cmd == "GET_VOICE_SETTINGS" {
        if let Some(d) = data {
            let mut s = state_cell().lock();
            if let Some(m) = d.get("mute").and_then(Value::as_bool) {
                s.muted = m;
            }
            if let Some(d2) = d.get("deaf").and_then(Value::as_bool) {
                s.deafened = d2;
                // Deafening implies muted in the client's own display.
                if d2 {
                    s.muted = true;
                }
            }
        }
    }
    if evt == "VOICE_CHANNEL_SELECT" || cmd == "GET_SELECTED_VOICE_CHANNEL" {
        // `data` is null (or has no channel id) when leaving a channel.
        let in_voice = data
            .and_then(|d| d.get("channel_id").or_else(|| d.get("id")))
            .is_some_and(|v| !v.is_null());
        state_cell().lock().in_voice = in_voice;
    }
}

// ── authentication ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredToken {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
}

fn token_path() -> std::path::PathBuf {
    crate::storage::config_root().join("discord_token.json")
}

fn load_token() -> StoredToken {
    std::fs::read_to_string(token_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_token(token: &StoredToken) {
    let path = token_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(json) = serde_json::to_string_pretty(token) else { return };
    if std::fs::write(&path, json).is_err() {
        log::warn!("discord: could not cache the access token; re-authorising next start");
        return;
    }
    restrict(&path);
}

/// Make the token file owner-only where the platform expresses that in the mode
/// bits. On Windows the config directory is already per-user.
#[cfg(unix)]
fn restrict(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) {}

/// Authenticate the open connection, re-using the cached token when it still
/// works and falling back to a fresh authorize flow (which prompts the user in
/// Discord) otherwise.
fn authenticate(conn: &mut Connection, creds: &Credentials) -> anyhow::Result<StoredToken> {
    let cached = load_token();
    if !cached.access_token.is_empty() && try_authenticate(conn, &cached.access_token)? {
        return Ok(cached);
    }
    if !cached.refresh_token.is_empty() {
        match exchange(creds, [("grant_type", "refresh_token"), ("refresh_token", &cached.refresh_token)]) {
            Ok(fresh) => {
                if try_authenticate(conn, &fresh.access_token)? {
                    return Ok(fresh);
                }
            }
            Err(e) => log::info!("discord: token refresh failed ({e}); asking for authorisation"),
        }
    }

    if creds.client_secret.is_empty() {
        anyhow::bail!(
            "no cached token and no Client Secret — paste the secret once so Slidr \
             can complete the authorisation"
        );
    }
    log::info!("discord: requesting authorisation — confirm the prompt in Discord");
    conn.send_cmd(
        "AUTHORIZE",
        json!({ "client_id": creds.client_id, "scopes": SCOPES }),
    )?;
    let code = loop {
        let (_, msg) = conn.recv()?;
        if msg.get("cmd").and_then(Value::as_str) == Some("AUTHORIZE") {
            if let Some(err) = rpc_error(&msg) {
                anyhow::bail!("authorisation refused: {err}");
            }
            match msg.pointer("/data/code").and_then(Value::as_str) {
                Some(c) => break c.to_string(),
                None => anyhow::bail!("authorisation response carried no code"),
            }
        }
        apply_message(&msg);
    };

    let token = exchange(
        creds,
        [("grant_type", "authorization_code"), ("code", &code)],
    )?;
    if !try_authenticate(conn, &token.access_token)? {
        anyhow::bail!("the freshly issued token was rejected");
    }
    Ok(token)
}

/// Send AUTHENTICATE and report whether it was accepted.
fn try_authenticate(conn: &mut Connection, access_token: &str) -> anyhow::Result<bool> {
    conn.send_cmd("AUTHENTICATE", json!({ "access_token": access_token }))?;
    loop {
        let (_, msg) = conn.recv()?;
        if msg.get("cmd").and_then(Value::as_str) == Some("AUTHENTICATE") {
            if let Some(err) = rpc_error(&msg) {
                log::info!("discord: cached token rejected ({err})");
                return Ok(false);
            }
            return Ok(true);
        }
        apply_message(&msg);
    }
}

fn rpc_error(msg: &Value) -> Option<String> {
    if msg.get("evt").and_then(Value::as_str) != Some("ERROR") {
        return None;
    }
    Some(
        msg.pointer("/data/message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_string(),
    )
}

/// OAuth token endpoint. `extra` carries the grant-specific fields.
fn exchange(creds: &Credentials, extra: [(&str, &str); 2]) -> anyhow::Result<StoredToken> {
    let mut form: Vec<(&str, &str)> = vec![
        ("client_id", creds.client_id.as_str()),
        ("client_secret", creds.client_secret.as_str()),
        ("redirect_uri", REDIRECT_URI),
    ];
    form.extend_from_slice(&extra);
    let resp = ureq::post(TOKEN_URL)
        .send_form(&form)
        .map_err(|e| match e {
            // The body can echo request fields — never let it reach a log.
            ureq::Error::Status(401, _) => anyhow::anyhow!(
                "Discord rejected the Client ID/Secret (HTTP 401) — check both in Settings"
            ),
            ureq::Error::Status(400, _) => anyhow::anyhow!(
                "Discord rejected the authorisation (HTTP 400) — the application needs \
                 the redirect http://localhost registered under OAuth2"
            ),
            ureq::Error::Status(code, _) => {
                anyhow::anyhow!("token endpoint returned HTTP {code}")
            }
            other => anyhow::anyhow!("token request failed: {other}"),
        })?;
    // ureq's `into_json` needs its optional json feature; we already depend on
    // serde_json, so parse the body ourselves.
    let body: Value = serde_json::from_str(&resp.into_string()?)?;
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if access_token.is_empty() {
        anyhow::bail!("token endpoint returned no access_token");
    }
    Ok(StoredToken {
        access_token,
        refresh_token: body
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

// ── the socket ──────────────────────────────────────────────────────────────

/// Discord's IPC framing: little-endian opcode, little-endian length, JSON body.
const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;

/// Discord's own cap is 64 KiB per frame; refuse anything larger rather than
/// allocating whatever a length prefix claims.
const MAX_FRAME: u32 = 64 * 1024;

struct Connection {
    io: Box<dyn ReadWrite>,
    nonce: u64,
}

trait ReadWrite: Read + Write + Send {}
impl<T: Read + Write + Send> ReadWrite for T {}

impl Connection {
    /// Open the first available RPC socket and complete the handshake. Discord
    /// numbers its sockets 0-9; several can exist with more than one client
    /// (stable next to PTB/Canary).
    fn open(client_id: &str) -> anyhow::Result<Self> {
        let mut last: Option<String> = None;
        for i in 0..10 {
            match connect_socket(i) {
                Ok(io) => {
                    let mut conn = Self { io, nonce: 0 };
                    conn.send(OP_HANDSHAKE, &json!({ "v": 1, "client_id": client_id }))?;
                    let (_, msg) = conn.recv()?;
                    if let Some(err) = rpc_error(&msg) {
                        anyhow::bail!("handshake refused: {err}");
                    }
                    return Ok(conn);
                }
                Err(e) => last = Some(e.to_string()),
            }
        }
        anyhow::bail!(
            "no Discord RPC socket found (is Discord running?){}",
            last.map(|e| format!(": {e}")).unwrap_or_default()
        )
    }

    fn next_nonce(&mut self) -> String {
        self.nonce += 1;
        format!("slidr-{}", self.nonce)
    }

    fn send_cmd(&mut self, cmd: &str, args: Value) -> anyhow::Result<()> {
        let nonce = self.next_nonce();
        self.send(OP_FRAME, &json!({ "cmd": cmd, "args": args, "nonce": nonce }))
    }

    fn subscribe(&mut self, evt: &str) -> anyhow::Result<()> {
        let nonce = self.next_nonce();
        self.send(
            OP_FRAME,
            &json!({ "cmd": "SUBSCRIBE", "evt": evt, "args": {}, "nonce": nonce }),
        )
    }

    fn send(&mut self, op: u32, payload: &Value) -> anyhow::Result<()> {
        let body = serde_json::to_vec(payload)?;
        let mut frame = Vec::with_capacity(8 + body.len());
        frame.extend_from_slice(&op.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(&body);
        self.io.write_all(&frame)?;
        self.io.flush()?;
        Ok(())
    }

    /// Read one frame, answering pings inline so the caller only ever sees data.
    fn recv(&mut self) -> anyhow::Result<(u32, Value)> {
        loop {
            let mut header = [0u8; 8];
            self.io.read_exact(&mut header)?;
            let op = u32::from_le_bytes(header[..4].try_into().unwrap());
            let len = u32::from_le_bytes(header[4..].try_into().unwrap());
            if len > MAX_FRAME {
                anyhow::bail!("oversized RPC frame ({len} bytes)");
            }
            let mut body = vec![0u8; len as usize];
            self.io.read_exact(&mut body)?;
            match op {
                OP_CLOSE => anyhow::bail!("the client closed the connection"),
                OP_PING => {
                    let echo: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
                    self.send(OP_PONG, &echo)?;
                }
                OP_PONG => {}
                _ => {
                    let msg: Value = serde_json::from_slice(&body)?;
                    return Ok((op, msg));
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn connect_socket(index: u32) -> anyhow::Result<Box<dyn ReadWrite>> {
    use std::fs::OpenOptions;
    let path = format!(r"\\.\pipe\discord-ipc-{index}");
    let pipe = OpenOptions::new().read(true).write(true).open(&path)?;
    Ok(Box::new(pipe))
}

#[cfg(not(target_os = "windows"))]
fn connect_socket(index: u32) -> anyhow::Result<Box<dyn ReadWrite>> {
    use std::os::unix::net::UnixStream;
    // Flatpak and Snap relocate the socket one directory down.
    let base = ["XDG_RUNTIME_DIR", "TMPDIR"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .unwrap_or_else(|| "/tmp".to_string());
    let mut last = None;
    for sub in ["", "app/com.discordapp.Discord/", "snap.discord/"] {
        let path = format!("{}/{sub}discord-ipc-{index}", base.trim_end_matches('/'));
        match UnixStream::connect(&path) {
            Ok(s) => return Ok(Box::new(s)),
            Err(e) => last = Some(e),
        }
    }
    Err(last
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow::anyhow!("no socket path tried")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_validation() {
        assert!(is_valid_id("123456789012345678"));
        assert!(is_valid_id("  123456789012345678  "));
        // A half-typed id must not start a connection attempt.
        assert!(!is_valid_id("1234"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("not-a-snowflake-1234"));
    }

    #[test]
    fn signal_index_round_trip() {
        for s in [DiscordSignal::Muted, DiscordSignal::Deafened, DiscordSignal::InVoice] {
            assert_eq!(DiscordSignal::from_index(s.to_index()), s);
        }
    }

    #[test]
    fn state_is_false_while_disconnected() {
        let s = DiscordState { connected: false, muted: true, deafened: true, in_voice: true };
        assert!(!s.get(DiscordSignal::Muted));
        let s = DiscordState { connected: true, ..s };
        assert!(s.get(DiscordSignal::Muted));
    }

    #[test]
    fn voice_settings_update_folds_in() {
        *state_cell().lock() = DiscordState { connected: true, ..Default::default() };
        apply_message(&json!({ "evt": "VOICE_SETTINGS_UPDATE", "data": { "mute": true, "deaf": false } }));
        assert!(state().muted);
        assert!(!state().deafened);

        // Deafening implies muted, the way the client shows it.
        apply_message(&json!({ "evt": "VOICE_SETTINGS_UPDATE", "data": { "mute": false, "deaf": true } }));
        assert!(state().deafened);
        assert!(state().muted);

        apply_message(&json!({ "evt": "VOICE_CHANNEL_SELECT", "data": { "channel_id": "123" } }));
        assert!(state().in_voice);
        apply_message(&json!({ "evt": "VOICE_CHANNEL_SELECT", "data": null }));
        assert!(!state().in_voice);
    }

    #[test]
    fn error_frames_are_recognised() {
        let msg = json!({ "cmd": "AUTHENTICATE", "evt": "ERROR", "data": { "message": "nope" } });
        assert_eq!(rpc_error(&msg).as_deref(), Some("nope"));
        assert!(rpc_error(&json!({ "cmd": "AUTHENTICATE" })).is_none());
    }
}

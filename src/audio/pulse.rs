//! Linux audio backend.
//!
//! Implemented over `pactl(1)` — present on every modern Linux desktop and
//! transparently supported by PipeWire via `pipewire-pulse`. Shelling out
//! keeps the implementation tiny vs. binding to libpulse's async C API,
//! and the action rate (≤60Hz, throttled) is well within budget.

use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{AudioBackend, MuteTarget, VolumeTarget};

pub struct PulseBackend {
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    sink_inputs: Vec<SinkInput>,
    sources: Vec<NamedId>,
    sinks: Vec<NamedId>,
    stamp: Option<Instant>,
}

#[derive(Clone)]
struct SinkInput {
    id: String,
    process_binary: String,
    app_name: String,
}

#[derive(Clone)]
struct NamedId {
    name: String,
    description: String,
}

const CACHE_TTL: Duration = Duration::from_millis(500);

impl PulseBackend {
    pub fn new() -> anyhow::Result<Self> {
        // Probe pactl presence + a working server.
        let out = Command::new("pactl")
            .arg("info")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| anyhow::anyhow!("pactl not found: {e}"))?;
        if !out.status.success() {
            anyhow::bail!("pactl info failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(Self { cache: Mutex::new(Cache::default()) })
    }

    fn refresh(&self) {
        let mut c = self.cache.lock().unwrap();
        if let Some(t) = c.stamp {
            if t.elapsed() < CACHE_TTL {
                return;
            }
        }
        c.sink_inputs = list_sink_inputs();
        c.sources = list_endpoints("sources");
        c.sinks = list_endpoints("sinks");
        c.stamp = Some(Instant::now());
    }

    fn sink_inputs_for(&self, process: &str) -> Vec<String> {
        self.refresh();
        let c = self.cache.lock().unwrap();
        let needle = process.to_lowercase();
        let needle = strip_exe(&needle);
        c.sink_inputs
            .iter()
            .filter(|si| {
                si.process_binary.to_lowercase().contains(&needle)
                    || si.app_name.to_lowercase().contains(&needle)
            })
            .map(|si| si.id.clone())
            .collect()
    }

    fn match_endpoint(&self, kind: EndpointKind, name: Option<&str>) -> Option<String> {
        self.refresh();
        let c = self.cache.lock().unwrap();
        let list = match kind {
            EndpointKind::Source => &c.sources,
            EndpointKind::Sink => &c.sinks,
        };
        let name = name?;
        let n = name.to_lowercase();
        list.iter()
            .find(|e| e.name.to_lowercase().contains(&n) || e.description.to_lowercase().contains(&n))
            .map(|e| e.name.clone())
    }
}

#[derive(Copy, Clone)]
enum EndpointKind {
    Source,
    Sink,
}

fn strip_exe(s: &str) -> String {
    let s = s.trim_end_matches(".exe");
    // Use the basename if a path was provided.
    s.rsplit(['/', '\\']).next().unwrap_or(s).to_string()
}

impl AudioBackend for PulseBackend {
    fn set_volume(&self, target: VolumeTarget<'_>, value: f32) {
        let pct = format!("{}%", (value.clamp(0.0, 1.0) * 100.0).round() as i32);
        match target {
            VolumeTarget::Process(p) => {
                for id in self.sink_inputs_for(p) {
                    run("set-sink-input-volume", &[&id, &pct]);
                }
            }
            VolumeTarget::Mic(m) => {
                let name = self
                    .match_endpoint(EndpointKind::Source, Some(m))
                    .unwrap_or_else(|| "@DEFAULT_SOURCE@".into());
                run("set-source-volume", &[&name, &pct]);
            }
            VolumeTarget::System(d) => {
                let name = self
                    .match_endpoint(EndpointKind::Sink, d)
                    .unwrap_or_else(|| "@DEFAULT_SINK@".into());
                run("set-sink-volume", &[&name, &pct]);
            }
        }
    }

    fn set_mute(&self, target: MuteTarget<'_>, muted: bool) {
        let flag = if muted { "1" } else { "0" };
        match target {
            MuteTarget::Process(p) => {
                for id in self.sink_inputs_for(p) {
                    run("set-sink-input-mute", &[&id, flag]);
                }
            }
            MuteTarget::Mic(m) => {
                let name = self
                    .match_endpoint(EndpointKind::Source, Some(m))
                    .unwrap_or_else(|| "@DEFAULT_SOURCE@".into());
                run("set-source-mute", &[&name, flag]);
            }
            MuteTarget::System(d) => {
                let name = self
                    .match_endpoint(EndpointKind::Sink, d)
                    .unwrap_or_else(|| "@DEFAULT_SINK@".into());
                run("set-sink-mute", &[&name, flag]);
            }
        }
    }

    fn toggle_mute(&self, target: MuteTarget<'_>) {
        match target {
            MuteTarget::Process(p) => {
                for id in self.sink_inputs_for(p) {
                    run("set-sink-input-mute", &[&id, "toggle"]);
                }
            }
            MuteTarget::Mic(m) => {
                let name = self
                    .match_endpoint(EndpointKind::Source, Some(m))
                    .unwrap_or_else(|| "@DEFAULT_SOURCE@".into());
                run("set-source-mute", &[&name, "toggle"]);
            }
            MuteTarget::System(d) => {
                let name = self
                    .match_endpoint(EndpointKind::Sink, d)
                    .unwrap_or_else(|| "@DEFAULT_SINK@".into());
                run("set-sink-mute", &[&name, "toggle"]);
            }
        }
    }

    fn is_muted(&self, target: MuteTarget<'_>) -> bool {
        let (subcmd, name) = match target {
            MuteTarget::Process(p) => {
                let ids = self.sink_inputs_for(p);
                let Some(id) = ids.first() else { return false };
                ("get-sink-input-mute", id.clone())
            }
            MuteTarget::Mic(m) => (
                "get-source-mute",
                self.match_endpoint(EndpointKind::Source, Some(m))
                    .unwrap_or_else(|| "@DEFAULT_SOURCE@".into()),
            ),
            MuteTarget::System(d) => (
                "get-sink-mute",
                self.match_endpoint(EndpointKind::Sink, d)
                    .unwrap_or_else(|| "@DEFAULT_SINK@".into()),
            ),
        };
        capture(subcmd, &[&name])
            .map(|o| o.contains("yes"))
            .unwrap_or(false)
    }

    fn list_processes(&self) -> Vec<String> {
        self.refresh();
        let c = self.cache.lock().unwrap();
        let mut names: Vec<String> = c
            .sink_inputs
            .iter()
            .map(|si| {
                if !si.process_binary.is_empty() {
                    si.process_binary.clone()
                } else {
                    si.app_name.clone()
                }
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    fn list_mics(&self) -> Vec<String> {
        self.refresh();
        self.cache.lock().unwrap().sources.iter().map(|e| e.description.clone()).collect()
    }

    fn list_outputs(&self) -> Vec<String> {
        self.refresh();
        self.cache.lock().unwrap().sinks.iter().map(|e| e.description.clone()).collect()
    }
}

fn run(subcmd: &str, args: &[&str]) {
    let _ = Command::new("pactl")
        .arg(subcmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn capture(subcmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("pactl")
        .arg(subcmd)
        .args(args)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn list_sink_inputs() -> Vec<SinkInput> {
    let Some(text) = capture("list", &["sink-inputs"]) else { return Vec::new() };
    let mut out = Vec::new();
    let mut cur: Option<SinkInput> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Sink Input #") {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            cur = Some(SinkInput {
                id: rest.trim().to_string(),
                process_binary: String::new(),
                app_name: String::new(),
            });
        } else if let Some(c) = cur.as_mut() {
            if let Some(v) = property(line, "application.process.binary") {
                c.process_binary = v;
            } else if let Some(v) = property(line, "application.name") {
                if c.app_name.is_empty() {
                    c.app_name = v;
                }
            }
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

fn list_endpoints(kind: &str) -> Vec<NamedId> {
    let Some(text) = capture("list", &[kind]) else { return Vec::new() };
    let mut out = Vec::new();
    let mut name = String::new();
    let mut desc = String::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        if line.starts_with("Name: ") {
            if !name.is_empty() {
                out.push(NamedId { name: std::mem::take(&mut name), description: std::mem::take(&mut desc) });
            }
            name = line[6..].trim().to_string();
        } else if line.starts_with("Description: ") {
            desc = line[13..].trim().to_string();
        }
    }
    if !name.is_empty() {
        out.push(NamedId { name, description: desc });
    }
    out
}

fn property(line: &str, key: &str) -> Option<String> {
    // pactl prints `key = "value"` (with leading whitespace).
    let prefix = format!("{key} = \"");
    let idx = line.find(&prefix)?;
    let rest = &line[idx + prefix.len()..];
    let end = rest.rfind('"')?;
    Some(rest[..end].to_string())
}

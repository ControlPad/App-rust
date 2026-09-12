//! Linux audio backend.
//!
//! Implemented over `pactl(1)` — present on every modern Linux desktop and
//! transparently supported by PipeWire via `pipewire-pulse`. Shelling out
//! keeps the implementation tiny vs. binding to libpulse's async C API,
//! and the action rate (≤60Hz, throttled) is well within budget.
//!
//! Two things matter for correctness here:
//!
//! * **Locale.** `pactl`'s human-readable output is translated, so every parse
//!   below would break on a non-English desktop ("Sink-Eingang #", "Stumm: ja").
//!   Every invocation therefore forces `LC_ALL=C`.
//! * **Query cost.** A `pactl` call is a fork+exec. The LED engine polls mute
//!   and volume 10×/s per LED, so answering those from a live process each time
//!   meant dozens of processes per second at idle. `Volume:`/`Mute:` are already
//!   part of the `pactl list` output we refresh anyway, so reads are served from
//!   the same short-TTL cache and cost nothing extra.

use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::pulse_parse::{parse_defaults, parse_records, strip_exe, Record};
use super::{AudioBackend, MuteTarget, VolumeTarget};

pub struct PulseBackend {
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    sink_inputs: Vec<Record>,
    sources: Vec<Record>,
    sinks: Vec<Record>,
    /// Names reported by `pactl info`, cached alongside the lists so resolving
    /// "System (default)" costs no extra process per query.
    default_sink: String,
    default_source: String,
    stamp: Option<Instant>,
}

const CACHE_TTL: Duration = Duration::from_millis(500);

impl PulseBackend {
    pub fn new() -> anyhow::Result<Self> {
        // Probe pactl presence + a working server.
        let out = pactl()
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
        // Collect outside the lock so concurrent readers (UI thread listing
        // devices while the actuator ticks) never queue behind three subprocesses.
        {
            let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(t) = c.stamp {
                if t.elapsed() < CACHE_TTL {
                    return;
                }
            }
        }
        let sink_inputs = list("sink-inputs", "Sink Input #");
        let sources = list("sources", "Source #");
        let sinks = list("sinks", "Sink #");
        let (default_sink, default_source) = defaults();
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.sink_inputs = sink_inputs;
        c.sources = sources;
        c.sinks = sinks;
        c.default_sink = default_sink;
        c.default_source = default_source;
        c.stamp = Some(Instant::now());
    }

    /// Drop the cache so the next read reflects a change we just made. Used
    /// after mute/default-sink changes so LED feedback is immediate; *not* used
    /// after volume writes, which arrive at up to 60Hz during a slider drag and
    /// whose ≤500ms staleness is invisible.
    fn invalidate(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).stamp = None;
    }

    fn sink_inputs_for(&self, process: &str) -> Vec<Record> {
        self.refresh();
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let needle = strip_exe(&process.to_lowercase());
        if needle.is_empty() {
            return Vec::new();
        }
        c.sink_inputs
            .iter()
            .filter(|si| {
                si.process_binary.to_lowercase().contains(&needle)
                    || si.app_name.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect()
    }

    fn match_endpoint(&self, kind: EndpointKind, name: Option<&str>) -> Option<Record> {
        self.refresh();
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let list = match kind {
            EndpointKind::Source => &c.sources,
            EndpointKind::Sink => &c.sinks,
        };
        let name = name?;
        let n = name.to_lowercase();
        if n.is_empty() {
            return None;
        }
        list.iter()
            .find(|e| e.name.to_lowercase().contains(&n) || e.description.to_lowercase().contains(&n))
            .cloned()
    }

    /// Resolve an endpoint to its pactl name, falling back to the server's
    /// default when the configured device isn't present.
    fn endpoint_name(&self, kind: EndpointKind, name: Option<&str>) -> String {
        self.match_endpoint(kind, name).map(|e| e.name).unwrap_or_else(|| {
            match kind {
                EndpointKind::Source => "@DEFAULT_SOURCE@".into(),
                EndpointKind::Sink => "@DEFAULT_SINK@".into(),
            }
        })
    }

    /// Cached record for an endpoint, resolving `None`/unknown to the server
    /// default by looking up the name `pactl get-default-{sink,source}` reports.
    fn endpoint_record(&self, kind: EndpointKind, name: Option<&str>) -> Option<Record> {
        if let Some(r) = self.match_endpoint(kind, name) {
            return Some(r);
        }
        // Unconfigured or unresolvable: fall back to the server default, read
        // from the same cache (no subprocess).
        self.refresh();
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let (list, default) = match kind {
            EndpointKind::Source => (&c.sources, &c.default_source),
            EndpointKind::Sink => (&c.sinks, &c.default_sink),
        };
        list.iter().find(|e| &e.name == default).cloned()
    }

    fn run(&self, subcmd: &str, args: &[&str]) {
        let status = pactl()
            .arg(subcmd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => log::debug!("pactl {subcmd} {args:?} exited with {s}"),
            Err(e) => log::warn!("pactl {subcmd} failed to run: {e}"),
        }
    }
}

#[derive(Copy, Clone)]
enum EndpointKind {
    Source,
    Sink,
}


impl AudioBackend for PulseBackend {
    fn set_volume(&self, target: VolumeTarget<'_>, value: f32) {
        let pct = format!("{}%", (value.clamp(0.0, 1.0) * 100.0).round() as i32);
        match target {
            VolumeTarget::Process(p) => {
                for si in self.sink_inputs_for(p) {
                    self.run("set-sink-input-volume", &[&si.id, &pct]);
                }
            }
            VolumeTarget::Mic(m) => {
                let name = self.endpoint_name(EndpointKind::Source, Some(m));
                self.run("set-source-volume", &[&name, &pct]);
            }
            VolumeTarget::System(d) => {
                let name = self.endpoint_name(EndpointKind::Sink, d);
                self.run("set-sink-volume", &[&name, &pct]);
            }
        }
    }

    fn set_mute(&self, target: MuteTarget<'_>, muted: bool) {
        let flag = if muted { "1" } else { "0" };
        self.apply_mute(target, flag);
    }

    fn toggle_mute(&self, target: MuteTarget<'_>) {
        self.apply_mute(target, "toggle");
    }

    fn is_muted(&self, target: MuteTarget<'_>) -> bool {
        // Served entirely from the refresh cache — no subprocess.
        match target {
            MuteTarget::Process(p) => {
                self.sink_inputs_for(p).first().map(|si| si.muted).unwrap_or(false)
            }
            MuteTarget::Mic(m) => self
                .endpoint_record(EndpointKind::Source, Some(m))
                .map(|e| e.muted)
                .unwrap_or(false),
            MuteTarget::System(d) => self
                .endpoint_record(EndpointKind::Sink, d)
                .map(|e| e.muted)
                .unwrap_or(false),
        }
    }

    fn get_volume(&self, target: VolumeTarget<'_>) -> Option<f32> {
        match target {
            VolumeTarget::Process(p) => self.sink_inputs_for(p).first()?.volume,
            VolumeTarget::Mic(m) => self.endpoint_record(EndpointKind::Source, Some(m))?.volume,
            VolumeTarget::System(d) => self.endpoint_record(EndpointKind::Sink, d)?.volume,
        }
    }

    fn list_processes(&self) -> Vec<String> {
        self.refresh();
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let mut names: Vec<String> = c
            .sink_inputs
            .iter()
            .map(Record::process_label)
            .filter(|n| !n.is_empty())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    fn list_mics(&self) -> Vec<String> {
        self.refresh();
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        // Skip `.monitor` sources: those are loopbacks of output devices, not
        // capture hardware, and only clutter the microphone picker.
        let mut names: Vec<String> = c
            .sources
            .iter()
            .filter(|s| !s.monitor)
            .map(Record::label)
            .filter(|n| !n.is_empty())
            .collect();
        names.dedup();
        names
    }

    fn list_outputs(&self) -> Vec<String> {
        self.refresh();
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.sinks.iter().map(Record::label).filter(|n| !n.is_empty()).collect()
    }

    fn cycle_output(&self, devices: &[String]) {
        self.refresh();
        let sinks = self.cache.lock().unwrap_or_else(|e| e.into_inner()).sinks.clone();
        // Cycle order as pactl sink names. Empty list = every sink; otherwise map
        // each configured description (or name) to its sink, skipping any missing.
        let order: Vec<String> = if devices.is_empty() {
            sinks.iter().map(|s| s.name.clone()).collect()
        } else {
            devices
                .iter()
                .filter_map(|d| {
                    let dl = d.to_lowercase();
                    sinks
                        .iter()
                        .find(|s| {
                            s.description.to_lowercase().contains(&dl)
                                || s.name.to_lowercase().contains(&dl)
                        })
                        .map(|s| s.name.clone())
                })
                .collect()
        };
        if order.is_empty() {
            return;
        }
        let cur = capture("get-default-sink", &[]).map(|s| s.trim().to_string());
        let cur_idx = cur.as_ref().and_then(|c| order.iter().position(|n| n == c));
        let next = cur_idx.map_or(0, |i| (i + 1) % order.len());
        self.run("set-default-sink", &[&order[next]]);
        self.invalidate();
    }
}

impl PulseBackend {
    fn apply_mute(&self, target: MuteTarget<'_>, flag: &str) {
        match target {
            MuteTarget::Process(p) => {
                for si in self.sink_inputs_for(p) {
                    self.run("set-sink-input-mute", &[&si.id, flag]);
                }
            }
            MuteTarget::Mic(m) => {
                let name = self.endpoint_name(EndpointKind::Source, Some(m));
                self.run("set-source-mute", &[&name, flag]);
            }
            MuteTarget::System(d) => {
                let name = self.endpoint_name(EndpointKind::Sink, d);
                self.run("set-sink-mute", &[&name, flag]);
            }
        }
        // Mute changes are user-initiated and rare; refresh now so the LED
        // engine reflects the new state on its next tick rather than up to
        // CACHE_TTL later.
        self.invalidate();
    }
}

/// A `pactl` command with the locale pinned. Without this every `starts_with`
/// below fails on a translated desktop.
fn pactl() -> Command {
    let mut c = Command::new("pactl");
    c.env("LC_ALL", "C").env("LANG", "C").env_remove("LANGUAGE");
    c
}

fn capture(subcmd: &str, args: &[&str]) -> Option<String> {
    let out = pactl()
        .arg(subcmd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `pactl list <kind>`. All three entity kinds share one layout: a header
/// line `<header_prefix><id>` followed by indented `Key: value` fields and a
/// `Properties:` block of `key = "value"` lines.
fn list(kind: &str, header_prefix: &str) -> Vec<Record> {
    let Some(text) = capture("list", &[kind]) else { return Vec::new() };
    parse_records(&text, header_prefix)
}

/// `(default_sink, default_source)` from a single `pactl info` call.
fn defaults() -> (String, String) {
    let Some(text) = capture("info", &[]) else { return (String::new(), String::new()) };
    parse_defaults(&text)
}



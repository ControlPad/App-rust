//! Parsers for `pactl` textual output.
//!
//! Split out of `pulse.rs` and compiled on every platform so the parsing --
//! the part most likely to regress, and the part that silently broke on
//! non-English desktops -- is unit-testable anywhere, including Windows CI.
//! Everything here is pure: no process spawning, no I/O.

#![allow(dead_code)] // only the Linux backend consumes these

#[derive(Clone, Default)]
pub struct Record {
    pub id: String,
    pub name: String,
    pub description: String,
    pub process_binary: String,
    pub app_name: String,
    pub volume: Option<f32>,
    pub muted: bool,
    /// Sources only: true for the `.monitor` loopback of an output device.
    pub monitor: bool,
}

impl Record {
    /// Best human-readable label for a device.
    pub fn label(&self) -> String {
        if self.description.is_empty() {
            self.name.clone()
        } else {
            self.description.clone()
        }
    }

    /// Best human-readable label for a playback stream.
    pub fn process_label(&self) -> String {
        if self.process_binary.is_empty() {
            self.app_name.clone()
        } else {
            self.process_binary.clone()
        }
    }
}

pub fn strip_exe(s: &str) -> String {
    let s = s.strip_suffix(".exe").unwrap_or(s);
    // Use the basename if a path was provided.
    s.rsplit(['/', '\\']).next().unwrap_or(s).to_string()
}

pub fn parse_defaults(text: &str) -> (String, String) {
    let mut sink = String::new();
    let mut source = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(v) = line.strip_prefix("Default Sink: ") {
            sink = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("Default Source: ") {
            source = v.trim().to_string();
        }
    }
    (sink, source)
}

pub fn parse_records(text: &str, header_prefix: &str) -> Vec<Record> {
    let mut out: Vec<Record> = Vec::new();
    let mut cur: Option<Record> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix(header_prefix) {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            cur = Some(Record { id: rest.trim().to_string(), ..Default::default() });
            continue;
        }
        let Some(c) = cur.as_mut() else { continue };

        if let Some(v) = line.strip_prefix("Name: ") {
            c.name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("Description: ") {
            c.description = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("Mute: ") {
            c.muted = v.trim().eq_ignore_ascii_case("yes");
        } else if let Some(v) = line.strip_prefix("Volume: ") {
            // Only the plain `Volume:` line — `Base Volume:` also carries a
            // percentage and would otherwise win for sinks/sources.
            c.volume = parse_first_percent(v).map(|p| (p as f32 / 100.0).clamp(0.0, 1.0));
        } else if let Some(v) = line.strip_prefix("Monitor of Sink: ") {
            c.monitor = v.trim() != "n/a";
        } else if let Some(v) = property(line, "application.process.binary") {
            c.process_binary = v;
        } else if let Some(v) = property(line, "application.name") {
            if c.app_name.is_empty() {
                c.app_name = v;
            }
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

fn parse_first_percent(s: &str) -> Option<u32> {
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'%' {
            let mut j = i;
            while j > 0 && bytes[j - 1].is_ascii_digit() {
                j -= 1;
            }
            if j < i {
                return s[j..i].parse().ok();
            }
        }
    }
    None
}

fn property(line: &str, key: &str) -> Option<String> {
    // pactl prints `key = "value"` (with leading whitespace).
    let prefix = format!("{key} = \"");
    let idx = line.find(&prefix)?;
    let rest = &line[idx + prefix.len()..];
    let end = rest.rfind('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINKS: &str = "\
Sink #0
\tState: RUNNING
\tName: alsa_output.pci-0000_00_1f.3.analog-stereo
\tDescription: Built-in Audio Analog Stereo
\tMute: no
\tVolume: front-left: 42926 /  65% / -11.20 dB,   front-right: 42926 /  65% / -11.20 dB
\t        balance 0.00
\tBase Volume: 65536 / 100% / 0.00 dB
\tMonitor Source: alsa_output.pci-0000_00_1f.3.analog-stereo.monitor
\tProperties:
\t\tdevice.description = \"Built-in Audio Analog Stereo\"
Sink #1
\tState: SUSPENDED
\tName: alsa_output.usb-headset
\tDescription: USB Headset
\tMute: yes
\tVolume: front-left: 65536 / 100% / 0.00 dB
\tBase Volume: 65536 / 100% / 0.00 dB
";

    const SOURCES: &str = "\
Source #0
\tName: alsa_output.pci-0000_00_1f.3.analog-stereo.monitor
\tDescription: Monitor of Built-in Audio
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB
\tMonitor of Sink: alsa_output.pci-0000_00_1f.3.analog-stereo
Source #1
\tName: alsa_input.pci-0000_00_1f.3.analog-stereo
\tDescription: Built-in Microphone
\tMute: yes
\tVolume: front-left: 32768 /  50% / -18.06 dB
\tMonitor of Sink: n/a
";

    const SINK_INPUTS: &str = "\
Sink Input #412
\tDriver: protocol-native.c
\tSink: 0
\tMute: no
\tVolume: front-left: 45875 /  70% / -9.30 dB
\t        balance 0.00
\tProperties:
\t\tapplication.name = \"Spotify\"
\t\tapplication.process.binary = \"spotify\"
Sink Input #413
\tSink: 0
\tMute: yes
\tVolume: front-left: 65536 / 100% / 0.00 dB
\tProperties:
\t\tapplication.name = \"Firefox\"
";

    #[test]
    fn parses_sinks_with_volume_and_mute() {
        let r = parse_records(SINKS, "Sink #");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].id, "0");
        assert_eq!(r[0].description, "Built-in Audio Analog Stereo");
        assert!(!r[0].muted);
        assert_eq!(r[0].volume, Some(0.65));
        assert!(r[1].muted);
        assert_eq!(r[1].volume, Some(1.0));
    }

    /// `Base Volume:` must not override the live `Volume:` line.
    #[test]
    fn base_volume_is_ignored() {
        let r = parse_records(SINKS, "Sink #");
        assert_eq!(r[0].volume, Some(0.65));
    }

    #[test]
    fn detects_monitor_sources() {
        let r = parse_records(SOURCES, "Source #");
        assert_eq!(r.len(), 2);
        assert!(r[0].monitor, "monitor source not flagged");
        assert!(!r[1].monitor);
        assert!(r[1].muted);
        assert_eq!(r[1].volume, Some(0.5));
    }

    #[test]
    fn parses_sink_inputs_with_properties() {
        let r = parse_records(SINK_INPUTS, "Sink Input #");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].id, "412");
        assert_eq!(r[0].process_binary, "spotify");
        assert_eq!(r[0].app_name, "Spotify");
        assert_eq!(r[0].volume, Some(0.7));
        assert!(!r[0].muted);
        // No process.binary property -> falls back to the application name.
        assert_eq!(r[1].process_label(), "Firefox");
        assert!(r[1].muted);
    }

    #[test]
    fn sink_header_does_not_swallow_sink_inputs() {
        assert!(parse_records(SINK_INPUTS, "Sink #").is_empty());
    }

    #[test]
    fn strip_exe_handles_paths_and_repeats() {
        assert_eq!(strip_exe("spotify.exe"), "spotify");
        assert_eq!(strip_exe("c:\\apps\\discord.exe"), "discord");
        assert_eq!(strip_exe("/usr/bin/firefox"), "firefox");
        // Only one suffix is stripped (trim_end_matches used to eat them all).
        assert_eq!(strip_exe("weird.exe.exe"), "weird.exe");
    }

    #[test]
    fn parses_pactl_info_defaults() {
        let info = "\
Server String: /run/user/1000/pulse/native
Server Name: PulseAudio (on PipeWire 1.0.5)
Default Sink: alsa_output.usb-headset
Default Source: alsa_input.pci-0000_00_1f.3.analog-stereo
Cookie: 1234:5678
";
        let (sink, source) = parse_defaults(info);
        assert_eq!(sink, "alsa_output.usb-headset");
        assert_eq!(source, "alsa_input.pci-0000_00_1f.3.analog-stereo");
    }

    #[test]
    fn percent_parsing() {
        assert_eq!(parse_first_percent("front-left: 42926 /  65% / -11.20 dB"), Some(65));
        assert_eq!(parse_first_percent("no percentage here"), None);
    }
}

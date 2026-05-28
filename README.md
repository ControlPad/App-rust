# Slidr — cross-platform rework

Lightweight, cross-platform reimplementation of the original Windows-only
Slidr (ControlPad) WPF app. One Rust binary, ~10 MB on disk, no .NET, no
WebView, runs on Linux (PulseAudio/PipeWire) and Windows (WASAPI — backend
stubbed, see below).

## Status

| Area                                       | State          |
| ------------------------------------------ | -------------- |
| Serial protocol parser                     | ✅ Done + tests |
| Frame streaming + reconnect/hot-plug       | ✅ Done         |
| Bézier slider curves (5 presets + custom)  | ✅ Done + tests |
| Event engine (deadzone, 16 ms throttle)    | ✅ Done         |
| Linux audio (pactl: per-app, mic, system)  | ✅ Done         |
| Windows audio (WASAPI)                     | 🟡 Stubbed     |
| Key simulation w/ hold-repeat              | ✅ Done         |
| Persistence (settings + presets)           | ✅ Done         |
| Autostart (XDG / Windows registry)         | ✅ Done         |
| UI: Home, Sliders, Buttons, Settings pages | ✅ Done         |
| Assignment popups                          | ✅ Done         |
| System tray                                | 🟡 Deferred*   |
| Single-instance lock                       | ⏳ Deferred    |

\* tray-icon crate pulls GTK3 in on Linux; defer until tray UX is designed.

## Layout

```
src/
├── main.rs            Entry, spawns serial thread, builds app
├── app.rs             eframe App, side-nav, frame loop
├── protocol.rs        Wire-format parser (FrameReader)
├── serial.rs          Port discovery, read loop, reconnect
├── events.rs          Edge detection, deadzone, throttle, dispatch
├── curve.rs           Slider normalization + cubic Bézier curves
├── keys.rs            enigo wrapper with hold/repeat semantics
├── storage.rs         JSON settings + per-preset folders
├── autostart.rs       XDG desktop entry / HKCU\Run
├── model.rs           Preset, Categories, Actions, AudioStream, Settings
├── audio/
│  ├── mod.rs          AudioBackend trait + factory
│  ├── pulse.rs        Linux: shells to pactl (works under PipeWire too)
│  ├── wasapi.rs       Windows: stub
│  └── null.rs         Fallback
└── ui/
   ├── home.rs         Live sliders + buttons + edit-mode assignments
   ├── categories.rs   Manage slider & button categories
   ├── settings_page.rs Theme, curve, deadzone, autostart, presets
   ├── popups.rs       Assignment dialogs
   └── theme.rs        Fluent-ish dark/light/system themes
```

## Build & run

```sh
# Linux dev deps (Ubuntu 24.04 names):
sudo apt install build-essential pkg-config libudev-dev libxkbcommon-dev \
    libwayland-dev libxcb1-dev libxdo-dev libssl-dev libfontconfig1-dev

cargo run --release
```

Set `SLIDR_CONFIG_DIR=/path/to/dir` to override the default config location
(`$XDG_CONFIG_HOME/slidr`, e.g. `~/.config/slidr`).

## Wire protocol

CSV, `\n`-terminated, 115 200 baud, 8N1:

```
<board:int>,<badge:int>,<s1>,<s2>,<s3>,<s4>,<s5>,<s6>,<b1>,...,<b11>\n
```

* board: -1 None / 0 Left / 1 Right
* badge: 0 None / 1 Supporter / 2 Premium
* sliders: 1..=1024 (raw ADC)
* buttons: 0/1

The Arduino sketch from the reference project is wire-compatible — no
firmware changes required.

## License

Same terms as the original project (see `../reference/LICENSE`).

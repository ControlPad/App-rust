# Slidr — cross-platform rework

Cross-platform reimplementation of the original Windows-only Slidr (ControlPad)
WPF app. A single native Rust binary — no .NET, no Electron/WebView — that turns
an Arduino-based control pad (6 sliders + 11 buttons) into a per-app audio mixer
and macro board.

- **UI:** [Slint](https://slint.dev) with the Material backend, Inter typeface,
  Tabler icons. Custom themed controls (select, toggle, slider), accent-colour
  presets, an interactive Bézier curve editor, and a retractable sidebar.
- **Platforms:** Linux (PulseAudio / PipeWire-Pulse via `pactl`) and Windows
  (WASAPI — per-process, mic and endpoint volume/mute).

## Features

- **Per-app volume** — assign processes, microphones, or output devices to
  physical sliders; volume mapped through a configurable response curve.
- **Button actions** — mute process / main audio / mic, open an application,
  open a website, or simulate a key (full key library ported from the original).
- **Program categories** — target *Browsers* or *Games* instead of one
  executable, so a newly installed game or browser is picked up without editing
  anything. Membership comes from `assets/app_groups.json`, from path heuristics
  (anything under a Steam/Epic/GOG library is a game), and from the OS itself
  (browsers registered with Windows). Extend or add categories with an
  `app_groups.json` in the config directory.
- **Profile safety** — a profile records the newest Slidr that wrote it. Opening
  one from a *newer* build (or one this build cannot parse at all) shows a
  warning and, crucially, stops Slidr writing to it — an older version would
  otherwise silently save the profile back without the fields it does not know.
  Upgrades are never flagged.
- **Profiles** — each profile carries its own categories, slider/button
  assignments, **and** appearance + slider settings (theme, accent, dead-zone,
  curve, unmute-on-change). Create / rename / import / export / switch.
- **Step-by-step add wizard** with live process / audio-device pickers (search).
- **Interactive curve editor** — drag the two control points for a custom curve;
  presets are visualised read-only.
- **System integration** — minimise-to-tray (Windows), autostart, start
  minimised, light/dark/system theme.
- **Output-switch resync** — when the default output device changes, the volumes
  Slidr last applied are re-sent, so the sliders and the actual levels don't
  drift apart (per-app volumes otherwise follow the *new* endpoint's remembered
  levels).
- **LED state sources** (experimental) — besides mute/volume/HTTP, an LED can
  follow the OS media session (playing / paused / stopped / nothing detected,
  for any player that publishes transport controls, optionally filtered to one)
  or Discord's voice state (self-mute / deafen / in a voice channel). Discord
  needs a one-off OAuth application — see [`src/discord.rs`](src/discord.rs) for
  the three setup steps, or the "Open developer portal" button under
  Settings → LEDs.
- **Hot-plug** — automatic Arduino detection, reconnect with backoff.
- **Update check** — one request to the GitHub releases API per launch (opt-out
  under Settings → Updates, plus a "Check now" button). When a newer release
  exists, a notice appears in the sidebar and opens
  [the releases page](https://github.com/ControlPad/App-rust/releases/latest) in
  the browser. Nothing is ever downloaded or installed automatically; the
  comparison uses the same version parsing as the profile stamp
  ([`src/update.rs`](src/update.rs)).

## Architecture

The UI thread only diffs incoming frames and renders; all audio/key I/O runs on
a dedicated **actuator thread** (commands over a channel, volume writes
coalesced) so the UI never stutters under a moving slider.

```
serial thread ──frames──▶ UI thread (Slint) ──commands──▶ actuator thread
                          (diff + render)                  (audio + keys)
```

### Source layout

```
ui/app.slint          Entire UI (components, pages, popups, curve editor)
src/
├── main.rs           Entry: spawn serial + actuator, build window, event loop
├── glue.rs           Slint ⇄ core bridge (callbacks, models, timers)
├── protocol.rs       Wire-format parser (FrameReader)              [tests]
├── serial.rs         Port discovery, read loop, reconnect/backoff
├── events.rs         Edge detection, dead-zone, throttle → Cmd list
├── actuator.rs       Worker thread applying audio/key commands
├── curve.rs          Slider normalization + cubic Bézier curves    [tests]
├── keys.rs           enigo wrapper with hold/repeat semantics
├── keys_library.rs   Virtual-key catalogue (media, F-keys, …)
├── app_groups.rs     Curated program categories + OS discovery      [tests]
├── media.rs          OS media session ("is music playing")          [tests]
├── discord.rs        Discord RPC voice state (local socket)         [tests]
├── led.rs            LED engine: conditions → board commands
├── storage.rs        JSON: global settings + per-profile folders
├── autostart.rs      XDG desktop entry / HKCU\Run
├── tray.rs           Windows system tray (cfg-gated)
├── model.rs          Preset, ProfileSettings, Categories, Actions, Settings
└── audio/
   ├── mod.rs         AudioBackend trait + factory
   ├── pulse.rs       Linux: pactl (works under PipeWire-Pulse too)
   ├── wasapi.rs      Windows: IMMDevice / sessions / endpoint volume
   └── null.rs        Fallback when no backend is available
installer/slidr.nsi   Interactive NSIS installer
.github/workflows/release.yml   Tag/manual → draft release (3 artifacts)
```

## Build & run

```sh
# Linux dev deps (Ubuntu 24.04 names):
sudo apt install build-essential pkg-config libudev-dev \
    libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev \
    libfontconfig1-dev

cargo run --release
```

### Linux runtime requirements

* **`pactl`** (Ubuntu/Debian/Fedora: `pulseaudio-utils`). The audio backend
  shells out to it; it also ships with `pipewire-pulse`. Without it Slidr
  starts but every volume/mute action is a no-op.
* **Serial port access.** Opening `/dev/ttyUSB*` / `/dev/ttyACM*` needs
  permission, or the connection banner shows "Access denied". Either install
  the bundled udev rule (preferred - applies on replug, no group, no re-login):

  ```sh
  sudo install -m644 packaging/99-slidr.rules /etc/udev/rules.d/99-slidr.rules
  sudo udevadm control --reload-rules && sudo udevadm trigger
  ```

  or add yourself to the port's group and log out and back in:

  ```sh
  sudo usermod -aG dialout "$USER"
  ```
* **Key actions need an X11 session.** Simulated keypresses go out over XTEST,
  which reaches X11 and XWayland windows but *not* native Wayland windows - a
  Wayland compositor does not let an ordinary application inject input. On a
  Wayland session Slidr logs a warning at startup and key actions will appear
  to do nothing in Wayland-native apps; volume, mute, LED and API actions are
  unaffected. Pick the "Xorg"/"X11" session at login if you need key actions.
* **No system tray.** The tray (`src/tray.rs`) is Windows-only, so the
  tray-related settings are hidden on Linux and `--hidden` is ignored - it
  would otherwise leave a window-less process that cannot be reopened.

Override the config location with `SLIDR_CONFIG_DIR=/path` (default
`$XDG_CONFIG_HOME/slidr`, i.e. `~/.config/slidr`; `%APPDATA%/slidr` on Windows).

### Cross-compiling the Windows build (from Linux)

```sh
sudo apt install mingw-w64 nsis
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
# interactive installer:
cp target/x86_64-pc-windows-gnu/release/slidr.exe installer/slidr.exe
cp assets/logo.ico installer/logo.ico
(cd installer && makensis -DVERSION=0.2.2 slidr.nsi)
```

`deploy.sh` does all of the above and publishes the three artifacts plus a
`build-info.json` to the web root.

## Releases

Pushing a `v*` tag (or running the **Release** workflow manually with a tag
input) builds Linux + Windows in parallel and drafts a GitHub release with:

- `Slidr-windows-setup.exe` — interactive installer (install dir, shortcuts,
  uninstaller)
- `Slidr-windows-portable.exe` — standalone executable
- `Slidr-linux-x86_64` — Linux binary

## Wire protocol

CSV, `\n`-terminated, 115 200 baud, 8N1:

```
<board:int>,<badge:int>,<s1>,<s2>,<s3>,<s4>,<s5>,<s6>,<b1>,…,<b11>\n
```

| field   | meaning                                   |
| ------- | ----------------------------------------- |
| board   | -1 None · 0 Left · 1 Right                |
| badge   | 0 None · 1 Supporter · 2 Premium          |
| sliders | raw ADC, 1..=1024                         |
| buttons | 0 / 1                                      |

The Arduino sketch from the reference project is wire-compatible — no firmware
changes required.

## Tests

```sh
cargo test            # protocol parser, Bézier curve, model helpers
```

## License

MIT (see `Cargo.toml`). Original project terms in `../reference/LICENSE`.

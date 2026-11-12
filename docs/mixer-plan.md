# Mixer plan (Wave-Link-style routing, mixing and EQ)

Status: planned. Proof of concept in `examples/loopback_poc.rs`.

## Goal

- Programs are assigned to **channels** ("Music" = Spotify, "Game", "Soundboard", …).
- Slidr mixes the channels (fader, mute, EQ) and plays the result on the
  user's headset — the original stream is no longer heard.
- A **Slidr Mic** carries the real microphone (with a voice chain) plus any
  channels the user sends to it; Discord etc. use it as their input.
- The mixer runs whether or not a board is connected. The board only controls it.

## What the proof of concept established (Windows 11 22631)

| Question | Result |
|---|---|
| Can one program's audio be captured on its own? | Yes — `VAD\Process_Loopback` (Win10 2004+), steady 48 kHz, no driver. |
| Does the capture survive muting the program's session / volume 0? | **No** — the capture is post-session-volume and goes digitally silent. Hiding the original therefore needs rerouting. |
| Can a running program be moved to another output? | Yes — `IAudioPolicyConfigFactory::SetPersistedDefaultAudioEndpoint` moved a playing Spotify between endpoints live, and clearing it moved it back. |
| Does the capture follow the move? | Yes — uninterrupted while the program switched endpoints. |
| Caveats | The override is refused (`E_INVALIDARG`) until the program has an audio session. Legacy WinMM streams (`Media.SoundPlayer`) did not move live. |

## Architecture

```
                      ┌──────────── Slidr audio engine (own thread) ────────────┐
Spotify ─rerouted───► │ "speaker" sink (nobody listens)                          │
  (per-process capture)► channel "Music" ─ fader/mute/EQ ─┬─► monitor mix ──────┼─► headset
Game ───────────────► │ channel "Game"  ─ fader/mute/EQ ─┤                      │
real mic ───────────► │ voice ─ HPF/EQ/gate/comp ────────┴─► mic mix ───────────┼─► cable ─► Discord
                      └──────────────────────────────────────────────────────────┘
```

New module `src/mixer/`:

- `dsp.rs` — RBJ biquads (peak, shelf, high/low pass), EQ presets, gain, mixing.
  Pure Rust, not platform-gated, fully unit-tested (like `audio/pulse_parse.rs`).
- `engine.rs` — real-time thread, 48 kHz f32, 10 ms blocks. One ring buffer per
  source absorbs clock drift between devices. Controlled through a command
  channel, like `actuator`.
- `capture.rs` — per-process loopback capture (process tree mode) and the real
  mic input.
- `route.rs` — reroutes channel programs to the speaker sink once their first
  session appears. Every override goes into a journal file and is cleared on
  exit, on the next start after a crash, from a tray entry
  ("Reset audio routing") and on uninstall (`slidr.exe --restore-audio`).
- Per-channel option **"don't reroute"**: the program keeps playing normally
  and is only copied into the mic mix (for games, where the extra 20–40 ms of
  monitor latency would hurt).
- Guard: the monitor device may never be the speaker sink (feedback loop).

### Devices: how many virtual cables?

- **Mic:** needs a real capture endpoint → one virtual cable (free VB-CABLE).
- **Speaker sink:** only needs *some* output endpoint nobody listens to. That
  can be a second cable, but equally an existing unused output (e.g. a monitor's
  HDMI audio without speakers, "Steam Streaming Speakers"). The sink is a
  device setting; Slidr suggests candidates and only recommends a second cable
  when none exists.
- Degraded modes: mic cable only → Slidr Mic without rerouting; sink only →
  mixing/EQ for the user's own ears.

## Data model

- **Per profile** (`Preset`): `MixerConfig` with
  - channels: name, programs (process / path / `@group:` refs from
    `app_groups`), level, mute, EQ preset + enabled, send to mic + send level,
    reroute on/off;
  - voice chain (EQ, gate, compressor);
  - EQ preset list.
  Switching profiles re-applies routing (programs leaving all channels are
  restored to the system default).
- **Global** (`Settings`): hardware only — monitor device, mic input, speaker
  sink, mic cable. These are properties of the machine, not of a profile.
- New profile fields go through the existing profile version guard.

## Board integration

- Slider targets in `AudioStream`: "mixer channel" and "mic send of channel X".
  The actuator forwards these to the engine instead of WASAPI.
- New `ActionKind`s: mute channel, toggle EQ, cycle EQ presets, set EQ preset,
  toggle "channel → mic" (e.g. soundboard on/off in Discord).
- LED conditions: channel muted, EQ active.
- Mic volume via the engine is software gain on the Slidr Mic and therefore
  independent of the capture driver.

## UI

New "Mixer" page in `ui/app.slint`: channel strips (fader, mute, EQ, → mic with
send level, program list), a voice strip, device selection, and an EQ editor
(5-band presets first, curve view later). Missing cables show a hint with an
install button.

## Installer

VB-CABLE is Windows-only and not installed by Slidr's installer: the end-user
licence in its package says it may not be integrated into another software's
installation procedure without VB-Audio's agreement (the website's licensing
page is more permissive; the package licence wins until VB-Audio agrees).

So the installer's finish page points to it instead: a short note naming
VB-Audio, www.vb-cable.com and "donationware, all participations are welcome",
and an unchecked "Open the VB-CABLE download page" box. Both are hidden when
the driver is installed already (its `VBAudioVACMME` / `VBAudioVACWDM` service
is registered). The Mixer page shows the same hint with a download button when
no cable is found. Setup needs administrator rights and a restart; that stays
VB-Audio's own setup, run by the user.

Linux needs no driver (see below).

## Linux

`src/mixer/pulse_io.rs`. PipeWire/Pulse needs no driver and has no cable
limit. Driven through `pactl`, `parec` and `pacat` (all in `pulseaudio-utils`,
which Slidr already needs), so the binary links no audio library:

- each channel is a null sink `slidr_mixer_ch_<id>`; its programs' streams are
  moved there (`move-sink-input`) and its monitor is recorded into the channel.
  With "Replace original" off, a `module-loopback` also plays the original;
- the mix plays as one `pacat` stream on the default output and follows it;
- the Slidr Mic is a null sink the mic bus plays into, plus a
  `module-remap-source` of its monitor named "Slidr Mic";
- the played streams run on the wall clock; their "Buffer Latency" (read in
  the 1 s scan) drops or doubles a block to keep them ~40 ms ahead;
- every module Slidr loads has `slidr_mixer` in its arguments. Unloading a
  null sink moves its streams back to the default output, so switching off,
  quitting and cleaning up after a crash (at start) are all "unload ours".

The engine, DSP, effects and switch sounds are shared with Windows. The
Settings "Mixer devices" card and the VB-CABLE hint are Windows-only.

## Phases

1. Fix mic volume on Windows (independent, own PR).
2. DSP + engine core without devices, unit-tested.
3. Windows I/O + routing with journal/restore.
4. Data model, persistence, Mixer page.
5. Board: slider targets, actions, LED conditions.
6. Installer section + device detection.
7. Linux backend.
8. Later: noise suppression (`nnnoiseless`), EQ curve editor.

## Risks

- Programs using legacy WinMM or exclusive mode can't be moved live / captured.
- If Slidr dies, rerouted programs stay on the sink until the next start or a
  manual reset (journal + tray entry mitigate this).
- Monitor latency of ~20–40 ms for rerouted programs.

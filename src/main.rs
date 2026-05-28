//! Slidr — cross-platform Arduino-based audio mixer / macro pad.
//!
//! Slint (Material style) UI, Rust event/audio/serial backend.

#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod actuator;
mod audio;
mod autostart;
mod curve;
mod events;
mod glue;
mod keys;
mod keys_library;
mod model;
mod protocol;
mod serial;
mod storage;
#[cfg(target_os = "windows")]
mod tray;

use std::sync::Arc;

use parking_lot::Mutex;

slint::include_modules!();

const BUILD_STAMP: &str = env!("SLIDR_BUILD_DATE");

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Preview / design-iteration mode: spawn a window pre-populated with mock state
    // so the UI can be screenshotted headlessly without an Arduino.
    //   --demo            seed demo data + mark connected
    //   --page=N          initial page (0-3)
    //   --popup=assign    open assign popup
    //   --popup=wizard    open wizard popup at step 0
    let args: Vec<String> = std::env::args().collect();
    let demo = args.iter().any(|a| a == "--demo");
    let initial_page: i32 = args.iter()
        .find_map(|a| a.strip_prefix("--page=").and_then(|s| s.parse().ok())).unwrap_or(0);
    let popup = args.iter().find_map(|a| a.strip_prefix("--popup=")).unwrap_or("");
    let collapsed = args.iter().any(|a| a == "--collapsed");
    let dark = args.iter().any(|a| a == "--dark");

    // Lock the main thread to an STA apartment before *any* COM-using crate
    // (WASAPI, winit's OleInitialize) gets a chance.
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    // Spawn the serial worker (skipped in --demo so mocked state isn't overwritten).
    let (tx, rx) = crossbeam_channel::bounded(256);
    let kicker = if demo {
        drop(tx);
        serial::RetryKicker::default()
    } else {
        serial::spawn(tx)
    };

    // Audio backend used only for *listing* devices/processes on the UI thread
    // (occasional, on wizard open). Volume/mute application happens on the
    // actuator thread with its own backend so the UI never blocks.
    let audio: Arc<dyn audio::AudioBackend> = Arc::from(audio::default_backend());
    let cmd_tx = actuator::spawn();

    // Load persisted state.
    let mut settings = storage::load_settings();
    let preset_name = if settings.active_preset.is_empty() {
        "Default".to_string()
    } else {
        settings.active_preset.clone()
    };
    let preset = storage::load_preset(&preset_name).unwrap_or_else(|_| model::Preset {
        id: 1,
        name: preset_name.clone(),
        ..Default::default()
    });
    settings.active_preset = preset.name.clone();

    let shared = Arc::new(Mutex::new(glue::Shared {
        settings,
        preset,
        engine: events::EventEngine::default(),
        audio,
        cmd_tx,
        kicker,
        pending_assign: None,
        pending_wizard: None,
        pending_retry_deadline: None,
        editing_idx: None,
    }));

    let ui = AppWindow::new()?;
    glue::wire(&ui, shared.clone(), rx, BUILD_STAMP);

    if demo {
        use slint::{ModelRc, SharedString, VecModel};
        use crate::glue::Shared;
        use slint::ComponentHandle;
        // Demo data
        let demo_sliders = vec![
            SliderCell { value: 700, percent: 0.66, category: "Music".into(), assigned: true },
            SliderCell { value: 600, percent: 0.59, category: "Chat".into(), assigned: true },
            SliderCell { value: 550, percent: 0.54, category: "Game".into(), assigned: true },
            SliderCell { value: 1024, percent: 1.0, category: "Browser".into(), assigned: true },
            SliderCell { value: 570, percent: 0.56, category: "".into(), assigned: false },
            SliderCell { value: 580, percent: 0.57, category: "System".into(), assigned: true },
        ];
        let demo_buttons = vec![
            ButtonCell { pressed: false, category: "Mute mic".into(), assigned: true },
            ButtonCell { pressed: true,  category: "Pause".into(), assigned: true },
            ButtonCell { pressed: false, category: "Discord".into(), assigned: true },
            ButtonCell { pressed: false, category: "OBS Rec".into(), assigned: true },
            ButtonCell { pressed: false, category: "".into(), assigned: false },
            ButtonCell { pressed: false, category: "".into(), assigned: false },
            ButtonCell { pressed: false, category: "Volume↑".into(), assigned: true },
            ButtonCell { pressed: false, category: "Volume↓".into(), assigned: true },
            ButtonCell { pressed: false, category: "".into(), assigned: false },
            ButtonCell { pressed: false, category: "Test".into(), assigned: true },
            ButtonCell { pressed: false, category: "".into(), assigned: false },
        ];
        ui.set_sliders(ModelRc::new(VecModel::from(demo_sliders)));
        let (top, right): (Vec<_>, Vec<_>) = demo_buttons.into_iter().enumerate()
            .partition(|(i, _)| *i < 6);
        ui.set_top_buttons(ModelRc::new(VecModel::from(
            top.into_iter().map(|(_, b)| b).collect::<Vec<_>>())));
        ui.set_right_buttons(ModelRc::new(VecModel::from(
            right.into_iter().map(|(_, b)| b).collect::<Vec<_>>())));
        let disconnected = args.iter().any(|a| a == "--disconnected");
        ui.set_connected(!disconnected);
        ui.set_connection_msg(if disconnected {
            "No Slidr device found".into()
        } else {
            slint::SharedString::from("Connected on /dev/ttyUSB0")
        });
        ui.set_badge_text("Supporter".into());
        ui.set_active_preset_name("Default".into());
        ui.set_current_page(initial_page);

        // seed two slider categories
        let scats = vec![
            CategorySummary { id: 1, name: "Music".into(), count: 1,
                lines: ModelRc::new(VecModel::from(vec![
                    LineItem { id: 0, primary: "spotify".into(), secondary: "process".into(), icon_kind: 0 },
                ])) },
            CategorySummary { id: 2, name: "Chat".into(), count: 2,
                lines: ModelRc::new(VecModel::from(vec![
                    LineItem { id: 0, primary: "discord".into(), secondary: "process".into(), icon_kind: 0 },
                    LineItem { id: 1, primary: "default mic".into(), secondary: "microphone".into(), icon_kind: 1 },
                ])) },
        ];
        ui.set_slider_categories(ModelRc::new(VecModel::from(scats)));
        let bcats = vec![
            CategorySummary { id: 1, name: "Toggle mute".into(), count: 1,
                lines: ModelRc::new(VecModel::from(vec![
                    LineItem { id: 0, primary: "Mute microphone".into(), secondary: "default mic".into(), icon_kind: 1 },
                ])) },
            CategorySummary { id: 2, name: "Media keys".into(), count: 2,
                lines: ModelRc::new(VecModel::from(vec![
                    LineItem { id: 0, primary: "Simulate key".into(), secondary: "Play/Pause (179)".into(), icon_kind: 4 },
                    LineItem { id: 1, primary: "Open website".into(), secondary: "https://example.com".into(), icon_kind: 3 },
                ])) },
        ];
        ui.set_button_categories(ModelRc::new(VecModel::from(bcats)));
        ui.set_preset_names(ModelRc::new(VecModel::from(vec![
            SharedString::from("Default"),
            SharedString::from("Gaming"),
            SharedString::from("Recording"),
        ])));
        ui.set_live_processes(ModelRc::new(VecModel::from(vec![
            SharedString::from("spotify"),
            SharedString::from("firefox"),
            SharedString::from("discord"),
            SharedString::from("obs"),
            SharedString::from("steam"),
        ])));
        ui.set_live_mics(ModelRc::new(VecModel::from(vec![
            SharedString::from("Built-in Microphone"),
            SharedString::from("USB Headset Mic"),
        ])));
        ui.set_live_outputs(ModelRc::new(VecModel::from(vec![
            SharedString::from("Built-in Speakers"),
            SharedString::from("USB Headset"),
            SharedString::from("HDMI Output"),
        ])));

        if collapsed { ui.set_sidebar_collapsed(true); }
        if dark { ui.invoke_apply_theme(2); }
        if args.iter().any(|a| a == "--edit") { ui.set_edit_mode(true); }
        if args.iter().any(|a| a == "--curve-custom") {
            ui.set_curve_preset_index(5);
            ui.set_curve_editable(true);
            ui.set_curve_x1(0.3); ui.set_curve_y1(0.3);
            ui.set_curve_x2(0.6); ui.set_curve_y2(0.6);
        } else {
            // visualise an ease-in-out preset by default
            ui.set_curve_x1(0.42); ui.set_curve_y1(0.0);
            ui.set_curve_x2(0.58); ui.set_curve_y2(1.0);
        }

        match popup {
            "preset" => {
                ui.set_preset_popup_open(true);
            }
            "assign" => {
                let opts: Vec<AssignPick> = vec![
                    AssignPick { id: 1, name: "Music".into() },
                    AssignPick { id: 2, name: "Chat".into() },
                ];
                ui.set_assign_options(ModelRc::new(VecModel::from(opts)));
                ui.set_assign_popup_open(true);
                ui.set_assign_target_kind(0);
                ui.set_assign_target_index(2);
            }
            "wizard" | "wizard0" => {
                ui.set_wizard_open(true);
                ui.set_wizard_target_kind(1);
                ui.set_wizard_step(0);
            }
            "wizard1" => {
                ui.set_wizard_open(true);
                ui.set_wizard_target_kind(1);
                ui.set_wizard_step(1);
                ui.set_wizard_kind(0);
            }
            "wizard2" => {
                ui.set_wizard_open(true);
                ui.set_wizard_target_kind(1);
                ui.set_wizard_step(2);
                ui.set_wizard_kind(0);
                ui.set_wizard_property("spotify".into());
                ui.set_wizard_display("spotify".into());
            }
            _ => {}
        }
    }

    // System tray + close-to-tray (Windows). Held for the process lifetime.
    #[cfg(target_os = "windows")]
    let _tray = {
        use slint::ComponentHandle;
        let tray = tray::install(&ui);
        let weak = ui.as_weak();
        let shared_close = shared.clone();
        // Read the setting live each time the window is closed.
        ui.window().on_close_requested(move || {
            let minimize = shared_close.lock().settings.minimize_to_tray;
            if minimize {
                if let Some(ui) = weak.upgrade() {
                    let _ = ui.hide();
                }
                slint::CloseRequestResponse::KeepWindowShown
            } else {
                slint::CloseRequestResponse::HideWindow
            }
        });
        tray
    };

    ui.run()?;

    // Persist on exit.
    let s = shared.lock();
    let _ = storage::save_settings(&s.settings);
    let _ = storage::save_preset(&s.preset);
    Ok(())
}

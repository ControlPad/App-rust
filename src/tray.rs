//! System tray integration (Windows only).
//!
//! On Linux, `tray-icon` pulls in GTK which conflicts with the Slint winit
//! backend, so the tray is compiled in only on Windows. The icon hosts a
//! Show / Exit menu and a left-click that restores the window.

#![cfg(target_os = "windows")]

use std::rc::Rc;

use slint::ComponentHandle;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::AppWindow;

pub struct Tray {
    _icon: TrayIcon,
    show_id: tray_icon::menu::MenuId,
    exit_id: tray_icon::menu::MenuId,
}

fn load_icon() -> Option<tray_icon::Icon> {
    let bytes = include_bytes!("../assets/logo.ico");
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    tray_icon::Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// Build the tray and wire its events to the window. Must run on the UI thread
/// after the event loop is available. Returns the tray (keep it alive).
pub fn install(ui: &AppWindow) -> Option<Rc<Tray>> {
    let menu = Menu::new();
    let show = MenuItem::new("Show Slidr", true, None);
    let exit = MenuItem::new("Exit", true, None);
    menu.append(&show).ok()?;
    menu.append(&exit).ok()?;
    let show_id = show.id().clone();
    let exit_id = exit.id().clone();

    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Slidr");
    if let Some(icon) = load_icon() {
        builder = builder.with_icon(icon);
    }
    let icon = builder.build().ok()?;

    let tray = Rc::new(Tray { _icon: icon, show_id, exit_id });

    // Poll tray + menu events on the UI thread via a Slint timer.
    let weak = ui.as_weak();
    let tray_for_timer = tray.clone();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(120),
        move || {
            // Menu clicks
            if let Ok(ev) = MenuEvent::receiver().try_recv() {
                if ev.id == tray_for_timer.show_id {
                    if let Some(ui) = weak.upgrade() {
                        let _ = ui.show();
                        ui.window().set_minimized(false);
                    }
                } else if ev.id == tray_for_timer.exit_id {
                    let _ = slint::quit_event_loop();
                }
            }
            // Left-click on the icon → restore
            if let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::DoubleClick { .. } = ev {
                    if let Some(ui) = weak.upgrade() {
                        let _ = ui.show();
                        ui.window().set_minimized(false);
                    }
                }
            }
        },
    );
    // Keep the timer alive for the process lifetime.
    std::mem::forget(timer);

    Some(tray)
}

//! System tray integration (Windows only).
//!
//! On Linux, `tray-icon` pulls in GTK which conflicts with the Slint winit
//! backend, so the tray is compiled in only on Windows. The icon hosts a
//! Minimize/Show toggle + Exit, and a double-click that restores the window.

#![cfg(target_os = "windows")]

use std::rc::Rc;

use slint::ComponentHandle;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::AppWindow;

pub struct Tray {
    _icon: TrayIcon,
    toggle: MenuItem,
    toggle_id: tray_icon::menu::MenuId,
    exit_id: tray_icon::menu::MenuId,
    weak: slint::Weak<AppWindow>,
}

impl Tray {
    /// Reflect window visibility in the toggle label.
    /// hidden=true → "Show Slidr"; hidden=false → "Minimize Slidr".
    pub fn set_window_hidden(&self, hidden: bool) {
        self.toggle
            .set_text(if hidden { "Show Slidr" } else { "Minimize Slidr" });
    }

    fn show_window(&self) {
        if let Some(ui) = self.weak.upgrade() {
            let _ = ui.show();
            ui.window().set_minimized(false);
        }
        self.set_window_hidden(false);
    }

    fn hide_window(&self) {
        if let Some(ui) = self.weak.upgrade() {
            let _ = ui.hide();
        }
        self.set_window_hidden(true);
    }

    fn toggle_window(&self) {
        let visible = self.weak.upgrade().map(|ui| ui.window().is_visible()).unwrap_or(false);
        if visible {
            self.hide_window();
        } else {
            self.show_window();
        }
    }
}

fn load_icon() -> Option<tray_icon::Icon> {
    let bytes = include_bytes!("../assets/logo.ico");
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    tray_icon::Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// Build the tray and wire its events. Run on the UI thread. Returns the tray
/// (keep it alive). The toggle starts as "Minimize Slidr" (window visible).
pub fn install(ui: &AppWindow) -> Option<Rc<Tray>> {
    let menu = Menu::new();
    let toggle = MenuItem::new("Minimize Slidr", true, None);
    let exit = MenuItem::new("Exit", true, None);
    menu.append(&toggle).ok()?;
    menu.append(&exit).ok()?;
    let toggle_id = toggle.id().clone();
    let exit_id = exit.id().clone();

    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Slidr");
    if let Some(icon) = load_icon() {
        builder = builder.with_icon(icon);
    }
    let icon = builder.build().ok()?;

    let tray = Rc::new(Tray {
        _icon: icon,
        toggle,
        toggle_id,
        exit_id,
        weak: ui.as_weak(),
    });

    // Poll tray + menu events on the UI thread via a Slint timer.
    let tray_for_timer = tray.clone();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(120),
        move || {
            if let Ok(ev) = MenuEvent::receiver().try_recv() {
                if ev.id == tray_for_timer.toggle_id {
                    tray_for_timer.toggle_window();
                } else if ev.id == tray_for_timer.exit_id {
                    let _ = slint::quit_event_loop();
                }
            }
            if let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::DoubleClick { .. } = ev {
                    tray_for_timer.show_window();
                }
            }
        },
    );
    std::mem::forget(timer);

    Some(tray)
}

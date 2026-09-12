//! System tray integration (Windows only).
//!
//! On Linux, `tray-icon` pulls in GTK which conflicts with the Slint winit
//! backend, so the tray is compiled in only on Windows. The icon hosts a
//! Minimize/Show toggle + Exit, and a double-click that restores the window.

#![cfg(target_os = "windows")]

use std::rc::Rc;

use slint::ComponentHandle;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::AppWindow;

pub struct Tray {
    _icon: TrayIcon,
    toggle: MenuItem,
    toggle_id: tray_icon::menu::MenuId,
    exit_id: tray_icon::menu::MenuId,
    weak: slint::Weak<AppWindow>,
    /// Last label state pushed to the menu item, so the visibility poll only
    /// touches the item when it actually changes.
    shown_label: std::cell::Cell<bool>,
}

impl Tray {
    /// Reflect window visibility in the toggle label.
    /// hidden=true → "Show Slidr"; hidden=false → "Minimize Slidr".
    pub fn set_window_hidden(&self, hidden: bool) {
        if self.shown_label.get() == !hidden {
            return;
        }
        self.shown_label.set(!hidden);
        self.toggle
            .set_text(if hidden { "Show Slidr" } else { "Minimize Slidr" });
    }

    /// True when the window is actually on screen — visible *and* not minimized
    /// to the taskbar. A minimized window is "not shown" for the toggle's
    /// purposes: the next click should bring it back, not hide it further.
    fn is_shown(&self) -> bool {
        self.weak
            .upgrade()
            .map(|ui| ui.window().is_visible() && !ui.window().is_minimized())
            .unwrap_or(false)
    }

    /// Re-derive the toggle label from the window's real state. Called every
    /// poll tick so the label can't drift out of sync — whether the window was
    /// never shown (autostart `--hidden`), minimized from the taskbar, or
    /// restored by the user outside our own code paths.
    fn sync_label(&self) {
        self.set_window_hidden(!self.is_shown());
    }

    fn show_window(&self) {
        if let Some(ui) = self.weak.upgrade() {
            let _ = ui.show();
            // Un-minimize covers the "sitting in the taskbar" case; `show()`
            // alone is a no-op for a window that is visible but minimized.
            ui.window().set_minimized(false);
        }
        raise_to_foreground();
        self.set_window_hidden(false);
    }

    fn hide_window(&self) {
        if let Some(ui) = self.weak.upgrade() {
            let _ = ui.hide();
        }
        self.set_window_hidden(true);
    }

    fn toggle_window(&self) {
        if self.is_shown() {
            self.hide_window();
        } else {
            self.show_window();
        }
    }
}

/// Bring this process's main window to the front.
///
/// Slint has no "focus the window" API, and `set_minimized(false)` restores the
/// window without necessarily raising it above whatever the user was in. We
/// locate our own top-level window by process id and ask the shell directly.
fn raise_to_foreground() {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow,
        ShowWindow, GW_OWNER, SW_RESTORE,
    };

    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Option<HWND>);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        // Ours, top-level (no owner), and actually mapped — skips the hidden
        // helper windows winit and the tray create alongside the real one.
        if pid == GetCurrentProcessId()
            && GetWindow(hwnd, GW_OWNER).is_err()
            && IsWindowVisible(hwnd).as_bool()
        {
            *out = Some(hwnd);
            return BOOL(0); // found it — stop enumerating
        }
        BOOL(1)
    }

    unsafe {
        let mut found: Option<HWND> = None;
        let _ = EnumWindows(Some(cb), LPARAM(&mut found as *mut _ as isize));
        if let Some(hwnd) = found {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
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
/// (keep it alive).
///
/// Left click opens the window, right click opens the menu. `start_hidden`
/// seeds the toggle label for a window that is never shown
/// (autostart-minimized); from then on the poll below keeps it in sync with the
/// window's real state.
pub fn install(ui: &AppWindow, start_hidden: bool) -> Option<Rc<Tray>> {
    let menu = Menu::new();
    let initial = if start_hidden { "Show Slidr" } else { "Minimize Slidr" };
    let toggle = MenuItem::new(initial, true, None);
    let exit = MenuItem::new("Exit", true, None);
    menu.append(&toggle).ok()?;
    menu.append(&exit).ok()?;
    let toggle_id = toggle.id().clone();
    let exit_id = exit.id().clone();

    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        // Left click belongs to "show the window" (handled below); the menu is
        // what right click is for.
        .with_menu_on_left_click(false)
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
        shown_label: std::cell::Cell::new(!start_hidden),
    });

    // Poll tray + menu events on the UI thread via a Slint timer.
    let tray_for_timer = tray.clone();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            // Keep the label honest even when the window state changed without
            // going through us (minimize button, Win+D, restore from taskbar).
            tray_for_timer.sync_label();

            // Drain both queues completely, then act once.
            //
            // `tray-icon` posts a Move event for every mouse movement across the
            // icon, so taking a single event per tick left a click queued behind
            // a flood of them: it arrived seconds late, and a second click
            // arrived a tick after that, making the window pop to the front
            // repeatedly. Collapsing a burst into one action fixes both.
            let mut exit = false;
            let mut toggle = false;
            let mut show = false;
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                if ev.id == tray_for_timer.exit_id {
                    exit = true;
                } else if ev.id == tray_for_timer.toggle_id {
                    toggle = true;
                }
            }
            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                match ev {
                    // Act on the release, so pressing on the icon and dragging
                    // away doesn't count as a click.
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                    | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => show = true,
                    _ => {}
                }
            }

            if exit {
                let _ = slint::quit_event_loop();
            } else if show {
                tray_for_timer.show_window();
            } else if toggle {
                tray_for_timer.toggle_window();
            }
        },
    );
    std::mem::forget(timer);

    Some(tray)
}

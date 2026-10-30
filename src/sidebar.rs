//! Sidebar tabs: their order and which are shown, set in Settings. Settings
//! itself can't be hidden — otherwise there'd be no way back.

use std::sync::Arc;

use parking_lot::Mutex;
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::glue::Shared;
use crate::model::Settings;
use crate::{AppWindow, NavRow};

/// (name stored in settings, page index in the UI, label).
const TABS: [(&str, i32, &str); 5] = [
    ("home", 0, "Home"),
    ("sliders", 1, "Sliders"),
    ("buttons", 2, "Buttons"),
    ("mixer", 4, "Mixer"),
    ("settings", 3, "Settings"),
];
const ALWAYS: &str = "settings";

/// Tabs in the user's order; tabs the saved order doesn't know (new ones
/// after an update) come after the known ones, in default order.
fn ordered(s: &Settings) -> Vec<(&'static str, i32, &'static str)> {
    let mut out: Vec<_> = s.nav_order.iter().filter_map(|n| TABS.iter().find(|t| t.0 == n)).copied().collect();
    for t in TABS {
        if !out.iter().any(|o| o.0 == t.0) {
            out.push(t);
        }
    }
    out
}

fn visible(s: &Settings, name: &str) -> bool {
    name == ALWAYS || !s.nav_hidden.iter().any(|h| h == name)
}

/// The pages the sidebar shows, in order.
fn pages(s: &Settings) -> Vec<i32> {
    ordered(s).into_iter().filter(|t| visible(s, t.0)).map(|t| t.1).collect()
}

pub fn push(ui: &AppWindow, s: &Settings) {
    let rows: Vec<NavRow> = ordered(s)
        .into_iter()
        .map(|(name, page, label)| NavRow { page, label: label.into(), visible: visible(s, name), fixed: name == ALWAYS })
        .collect();
    ui.set_nav_rows(ModelRc::new(VecModel::from(rows)));
    let pages = pages(s);
    // Hiding the open tab moves to the first one still shown.
    if !pages.contains(&ui.get_current_page()) {
        if let Some(first) = pages.first() {
            ui.set_current_page(*first);
            ui.invoke_page_changed(*first);
        }
    }
    ui.set_nav_pages(ModelRc::new(VecModel::from(pages)));
}

fn name_of(page: i32) -> Option<&'static str> {
    TABS.iter().find(|t| t.1 == page).map(|t| t.0)
}

pub fn wire(ui: &AppWindow, shared: Arc<Mutex<Shared>>) {
    push(ui, &shared.lock().settings);
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_nav_move(move |page, dir| {
            let Some(ui) = weak.upgrade() else { return };
            let mut s = shared.lock();
            let mut order: Vec<&str> = ordered(&s.settings).into_iter().map(|t| t.0).collect();
            let Some(i) = name_of(page).and_then(|n| order.iter().position(|o| *o == n)) else { return };
            let j = i as i32 + dir;
            if j < 0 || j as usize >= order.len() {
                return;
            }
            order.swap(i, j as usize);
            s.settings.nav_order = order.into_iter().map(str::to_string).collect();
            let _ = crate::storage::save_settings(&s.settings);
            push(&ui, &s.settings);
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_nav_visible(move |page, on| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(name) = name_of(page).filter(|n| *n != ALWAYS) else { return };
            let mut s = shared.lock();
            s.settings.nav_hidden.retain(|h| h != name);
            if !on {
                s.settings.nav_hidden.push(name.to_string());
            }
            let _ = crate::storage::save_settings(&s.settings);
            push(&ui, &s.settings);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_order_and_settings_cannot_be_hidden() {
        let mut s = Settings::default();
        assert_eq!(pages(&s), vec![0, 1, 2, 4, 3]);
        s.nav_hidden = vec!["settings".into(), "home".into()];
        assert_eq!(pages(&s), vec![1, 2, 4, 3]);
    }

    #[test]
    fn saved_order_wins_and_unknown_tabs_are_appended() {
        let s = Settings { nav_order: vec!["mixer".into(), "bogus".into(), "home".into()], ..Settings::default() };
        assert_eq!(pages(&s), vec![4, 0, 1, 2, 3]);
    }
}

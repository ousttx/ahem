//! The tray icon: a left click toggles the quick panel (`tray_panel`); the menu opens the
//! panel or the full window, chooses the mode of the main hotkey and quits. Closing the window
//! only hides it, so the app keeps listening to hotkeys.

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::commands;
use crate::domain::modes::{self, Mode};
use crate::i18n::{self, Strings};
use crate::platform;
use crate::storage::settings::Settings;
use crate::tray_glyph::Glyph;
use crate::{tray_panel, tray_status};

pub const ID: &str = "main";
const MODE_PREFIX: &str = "mode:";

pub fn create(app: &AppHandle, settings: &Settings) -> tauri::Result<()> {
    let strings = strings(settings);
    // `tray_status` redraws the icon with every change of what ahem is doing.
    let builder = TrayIconBuilder::with_id(ID)
        .icon(tray_status::icon(app, Glyph::Idle))
        .icon_as_template(platform::TRAY_ICON.template)
        .tooltip(strings.get("app.name"))
        .menu(&menu(app, settings, &strings)?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "panel" => {
                if let Some(panel) = tray_panel::get(app) {
                    panel.show_near_cursor();
                }
            }
            "open" => show_main(app),
            "quit" => app.exit(0),
            id => {
                if let Some(mode_id) = id.strip_prefix(MODE_PREFIX) {
                    commands::set_main_mode(app, mode_id);
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                match tray_panel::get(tray.app_handle()) {
                    Some(panel) => panel.toggle(rect),
                    None => show_main(tray.app_handle()),
                }
            }
        });
    builder.build(app)?;
    Ok(())
}

/// After the settings change: mode names, the checked mode and the language.
pub fn refresh(app: &AppHandle, settings: &Settings) {
    let Some(tray) = app.tray_by_id(ID) else { return };
    let strings = strings(settings);
    match menu(app, settings, &strings) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
        }
        Err(error) => log_line!("cannot rebuild the tray menu: {error}"),
    }
    let _ = tray.set_tooltip(Some(strings.get("app.name")));
}

pub fn show_main(app: &AppHandle) {
    // On the main thread, so the app is back in the Dock before the window activates it.
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let app = &handle;
        if let Some(panel) = tray_panel::get(app) {
            panel.hide();
        }
        if let Some(window) = app.get_webview_window("main") {
            platform::set_in_dock(app, true);
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    });
}

fn strings(settings: &Settings) -> Strings {
    let locales: Vec<String> = sys_locale::get_locales().collect();
    Strings::new(i18n::language(settings.ui_language, &locales))
}

fn mode_name(strings: &Strings, mode: &Mode) -> String {
    if mode.id == modes::PLAIN || mode.id == modes::ENGLISH {
        strings.get(&format!("modes.builtin.{}", mode.id))
    } else {
        mode.name.clone()
    }
}

fn menu(app: &AppHandle, settings: &Settings, strings: &Strings) -> tauri::Result<Menu<Wry>> {
    let panel = MenuItem::with_id(app, "panel", strings.get("tray.panel"), true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", strings.get("tray.open"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", strings.get("tray.quit"), true, None::<&str>)?;
    let modes = Submenu::with_id(app, "modes", strings.get("tray.main_mode"), true)?;
    // Translation to English is a hotkey of plain dictation, not a main mode.
    for mode in settings.modes.iter().filter(|mode| mode.id != modes::ENGLISH) {
        let item = CheckMenuItem::with_id(
            app,
            format!("{MODE_PREFIX}{}", mode.id),
            mode_name(strings, mode),
            true,
            mode.id == settings.main_mode_id,
            None::<&str>,
        )?;
        modes.append(&item)?;
    }
    Menu::with_items(app, &[&panel, &open, &modes, &PredefinedMenuItem::separator(app)?, &quit])
}

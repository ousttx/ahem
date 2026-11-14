// First: its `log_line!` is for every module below.
#[macro_use]
mod util;

pub mod audio;
mod commands;
pub mod dictation;
pub mod domain;
pub mod engines;
pub mod files;
pub mod hotkeys;
pub mod i18n;
mod indicator;
mod meeting_commands;
pub mod meetings;
pub mod platform;
pub mod runtimes;
pub mod storage;
mod tray;
mod tray_glyph;
mod tray_panel;
mod tray_status;
mod updates;

use std::io::IsTerminal;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use tauri::{Emitter, Manager, RunEvent, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_global_shortcut::ShortcutState;

use indicator::Indicator;
use tray_status::TrayStatus;

use commands::{CORE_EVENT, Dictation, Downloads, Hotkeys};
use dictation::services::{self, Paths, RealServices};
use dictation::{Controller, Message};
use domain::app_rules::RecentApps;
use engines::{llama_server, whisper_server};
use meetings::service::Meetings;
use meetings::speakers::SpeakerModels;
use meetings::store::SharedMeetings;
use storage::history::SharedHistory;
use storage::models::ModelStore;
use storage::secrets::SecretStore;
use storage::settings::SettingsStore;
use storage::stats::SharedStats;

/// Passed by the login entry: start in the tray without showing the window.
const HIDDEN_ARG: &str = "--hidden";
/// How long quitting waits for the controller: its own shutdown waits up to 2 s for the engine.
const QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

pub fn run() {
    let (inbox, messages) = mpsc::channel::<Message>();
    let hotkey_inbox = inbox.clone();
    let hotkeys = Hotkeys::default();
    let routes = hotkeys.clone();
    // Tauri's generated code may print with `eprintln!` itself.
    #[allow(clippy::disallowed_macros)]
    let context = tauri::generate_context!();
    let app = tauri::Builder::default()
        // Must come first: a second launch only shows the running app's window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| tray::show_main(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec![HIDDEN_ARG])))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // Only its checks and installation, called by `updates`; its own commands are not
        // granted to any window.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    let Some(action) = routes.0.lock().expect("hotkey lock").get(shortcut).cloned()
                    else {
                        return;
                    };
                    // As a menu goes at a key: the text goes to the program behind the quick
                    // panel, never into the panel. Without waiting for the main thread.
                    if event.state == ShortcutState::Pressed
                        && let Some(panel) = tray_panel::get(app)
                    {
                        panel.dismiss_if_shown();
                    }
                    let message = match event.state {
                        ShortcutState::Pressed => Message::HotkeyPressed(action),
                        ShortcutState::Released => Message::HotkeyReleased(action),
                    };
                    let _ = hotkey_inbox.send(message);
                })
                .build(),
        )
        .setup(move |app| {
            let config_dir = app.path().app_config_dir()?;
            let data_dir = app.path().app_local_data_dir()?;
            let moved = storage::move_folders_from_old_identifier(&config_dir, &data_dir);
            // After the move: a `logs` folder made before it would keep the old one's in place.
            if !cfg!(debug_assertions) && !std::io::stderr().is_terminal() {
                log_to_file(&data_dir.join("logs").join("app.log"));
            }
            for problem in moved {
                log_line!("{problem}");
            }
            let store = SettingsStore::open(config_dir.join("settings.json"));
            if let Some(error) = store.load_error() {
                log_line!("{error}; using defaults, saving disabled");
            }
            let settings = store.settings().clone();
            commands::refresh_autostart(app.handle(), settings.launch_at_login);
            let models = ModelStore::new(data_dir.join("models"));
            // The engines the installer carries; in a development run, as tools/build-engine and
            // tools/fetch-llama-cpp put them in runtimes/ of the source tree.
            let resources = app.path().resource_dir().ok();
            let executable =
                whisper_server::executable_in(&whisper_server::engine_dir(resources.as_deref()));
            let llama_executable =
                llama_server::executable_in(&llama_server::engine_dir(resources.as_deref()));
            let secrets = config_dir.join("secrets.json");
            let meeting_active = Arc::new(AtomicBool::new(false));
            // One for meetings and files alike, with one log.
            let speakers = SpeakerModels::new(models.root().join("speakers"))
                .with_log(data_dir.join("logs").join("speakers.log"));
            let paths = Paths {
                executable: executable.clone(),
                llama_executable: llama_executable.clone(),
                engine_log: data_dir.join("logs").join("engine.log"),
                mute_marker: data_dir.join("mute.marker"),
                secrets: secrets.clone(),
                meeting: Arc::clone(&meeting_active),
                speakers: speakers.clone(),
            };
            let history = Arc::new(SharedHistory::new(data_dir.join("history.sqlite3")));
            let stats = Arc::new(SharedStats::new(data_dir.join("stats.sqlite3")));
            // Meetings and transcribed files.
            let transcripts = Arc::new(SharedMeetings::new(
                data_dir.join("meetings.sqlite3"),
                data_dir.join("meetings"),
            ));
            let services = RealServices::new(
                models.clone(),
                paths,
                Arc::clone(&history),
                Arc::clone(&stats),
                Arc::clone(&transcripts),
                inbox.clone(),
            );
            let last_result = Arc::new(Mutex::new(None));
            let recent_apps = Arc::new(Mutex::new(RecentApps::default()));
            let indicator = match Indicator::create(app.handle()) {
                Ok(indicator) => {
                    indicator.set_shown(settings.show_indicator);
                    let indicator = Arc::new(indicator);
                    // For a save to turn it on or off (`commands::announce_settings`).
                    app.manage(Arc::clone(&indicator));
                    Some(indicator)
                }
                Err(error) => {
                    log_line!("cannot create the indicator: {error}");
                    None
                }
            };
            let handle = app.handle().clone();
            let tray_status = TrayStatus::new(handle.clone());
            app.manage(tray_status.clone());
            let emit = Box::new(move |event| {
                if let Some(indicator) = &indicator {
                    indicator.on_event(&event);
                }
                tray_status.on_event(&event);
                let _ = handle.emit(CORE_EVENT, event);
            });
            let controller = Controller::new(
                services,
                settings.clone(),
                Arc::clone(&last_result),
                Arc::clone(&recent_apps),
                emit,
            );
            let retry_available = controller.retry_available();
            let thread = thread::spawn(move || controller.run(messages));

            let core_handle = app.handle().clone();
            let meetings = Meetings::new(
                transcripts,
                speakers,
                inbox.clone(),
                Arc::new(move |event| {
                    let _ = core_handle.emit(CORE_EVENT, event);
                }),
                meeting_active,
            );
            app.manage(meetings);
            app.manage(Mutex::new(store));
            app.manage(SecretStore::new(secrets));
            app.manage(history);
            app.manage(stats);
            app.manage(models);
            app.manage(Downloads::default());
            app.manage(updates::Updates::default());
            updates::start(app.handle(), &data_dir);
            app.manage(commands::ChatGptSignIn::default());
            app.manage(hotkeys);
            app.manage(Dictation {
                inbox: inbox.clone(),
                last_result,
                retry_available,
                recent_apps,
                thread: Mutex::new(Some(thread)),
                whisper_executable: executable,
                llama_executable,
            });
            commands::register_startup_hotkeys(app.handle(), &settings);
            match tray_panel::TrayPanel::create(app.handle()) {
                Ok(panel) => {
                    app.manage(panel);
                }
                Err(error) => log_line!("cannot create the tray panel: {error}"),
            }
            // Built here, not by Tauri before `setup` (`"create": false`): the page calls commands
            // as soon as it loads, and they need the state managed above.
            if let Some(config) = app.config().app.windows.iter().find(|w| w.label == "main") {
                WebviewWindowBuilder::from_config(app.handle(), config)?.build()?;
            }
            for label in ["main", indicator::LABEL] {
                if let Some(webview) = app.get_webview_window(label)
                    && let Err(error) = webview.set_zoom(platform::UI_ZOOM)
                {
                    log_line!("cannot zoom the {label} window: {error}");
                }
            }
            if let Err(error) = tray::create(app.handle(), &settings) {
                log_line!("cannot create the tray icon: {error}");
            }
            // The window starts hidden (tauri.conf.json); at login the app stays in the tray.
            // `args_os`: a path that is not Unicode (the app's own, on Linux) must not panic.
            if std::env::args_os().any(|arg| arg == HIDDEN_ARG) {
                platform::set_in_dock(app.handle(), false);
            } else {
                tray::show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // The quick panel hides as soon as another window takes the focus.
            if let WindowEvent::Focused(focused) = event
                && window.label() == tray_panel::LABEL
            {
                platform::set_panel_keys(*focused);
                if !focused && let Some(panel) = tray_panel::get(window.app_handle()) {
                    panel.hide();
                }
            }
            // On a screen of another scale the OS resizes the panel by the old size; it gets its
            // size and place again once that is done. Asked from another thread, so the task is
            // queued behind this event: asked on the main thread, it would run at once, inside
            // the event, before the OS has set the size.
            if let WindowEvent::ScaleFactorChanged { .. } = event
                && window.label() == tray_panel::LABEL
            {
                let app = window.app_handle().clone();
                std::thread::spawn(move || {
                    let handle = app.clone();
                    let _ = app.run_on_main_thread(move || {
                        if let Some(panel) = tray_panel::get(&handle) {
                            panel.refit();
                        }
                    });
                });
            }
            // The Windows tray icon is colored for the taskbar's theme.
            if let WindowEvent::ThemeChanged(_) = event
                && window.label() == "main"
                && let Some(status) = window.app_handle().try_state::<TrayStatus>()
            {
                status.redraw();
            }
            // Closing hides the window; the app quits from the tray menu. The quick panel and
            // the indicator live as long as the app: Alt+F4 on Windows only hides the panel.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                match window.label() {
                    "main" => {
                        let _ = window.hide();
                        platform::set_in_dock(window.app_handle(), false);
                    }
                    tray_panel::LABEL => {
                        if let Some(panel) = tray_panel::get(window.app_handle()) {
                            panel.hide();
                        }
                    }
                    _ => {}
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::system_locales,
            commands::get_settings,
            commands::get_status,
            commands::save_settings,
            commands::set_hotkey_suspended,
            commands::get_last_result,
            commands::retry_available,
            commands::retry_dictation,
            commands::connect_chatgpt,
            commands::cancel_chatgpt_sign_in,
            commands::open_chatgpt_usage,
            commands::get_permissions,
            commands::request_permission,
            commands::recent_apps,
            commands::list_input_devices,
            commands::list_languages,
            commands::list_models,
            commands::download_model,
            commands::cancel_model_download,
            commands::delete_model,
            commands::data_folders,
            commands::open_data_folder,
            commands::list_cloud_engines,
            commands::add_connection,
            commands::remove_connection,
            commands::refresh_connection_models,
            commands::get_history,
            commands::delete_history_entry,
            commands::clear_history,
            commands::get_stats,
            commands::reset_stats,
            commands::delete_history_file,
            commands::fit_tray_panel,
            commands::hide_tray_panel,
            commands::open_main_window,
            commands::quit_app,
            commands::transcribe_file,
            commands::cancel_file,
            commands::save_held_result,
            commands::replace_connection_key,
            commands::export_replacements,
            commands::import_replacements,
            meeting_commands::get_meetings,
            meeting_commands::get_meeting,
            meeting_commands::start_meeting,
            meeting_commands::stop_meeting,
            meeting_commands::discard_meeting,
            meeting_commands::transcribe_meeting,
            meeting_commands::cancel_meeting_transcription,
            meeting_commands::delete_meeting,
            meeting_commands::rename_meeting,
            meeting_commands::export_meeting,
            meeting_commands::get_speaker_models,
            meeting_commands::download_speaker_models,
            meeting_commands::delete_speaker_models,
            updates::install_update
        ])
        .build(context)
        .expect("error while building tauri application");

    app.run(|app, event| {
        if platform::reopened(&event) {
            tray::show_main(app);
        }
        // Stop the engine process before the app exits, so it is never left running.
        if let RunEvent::Exit = event {
            shut_down(app);
        }
    });
}

/// What the app lets go before it ends, by quitting or by an update that restarts it: a meeting
/// being recorded is kept, the controller stops its engine.
fn shut_down(app: &tauri::AppHandle) {
    app.state::<Meetings>().shutdown();
    let dictation = app.state::<Dictation>();
    let _ = dictation.inbox.send(Message::Shutdown);
    // Not waited out: whatever holds the controller, quitting goes on.
    if let Some(thread) = dictation.thread.lock().expect("dictation lock").take() {
        services::join_for(thread, QUIT_WAIT);
    }
}

/// The app's own log: what it says on standard error (problems it gets over, never a key or a
/// dictated text) goes to `path`, the last run's kept as `app.previous.log`. For a release build
/// started without a terminal, whose standard error goes nowhere.
fn log_to_file(path: &std::path::Path) {
    let redirected = engines::local_server::open_log(path).and_then(platform::redirect_stderr);
    if let Err(error) = redirected {
        log_line!("cannot keep the app's log: {error}");
    }
}

#[cfg(test)]
mod tests {
    /// What the app says on standard error goes to its log, and the last run's log is kept.
    /// In a process of its own: the test process keeps its standard error.
    #[test]
    fn standard_error_goes_to_the_log() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/test-artifacts/app-log-{}", std::process::id()));
        let log = dir.join("app.log");
        if let Some(path) = std::env::var_os("AHEM_TEST_APP_LOG") {
            super::log_to_file(std::path::Path::new(&path));
            log_line!("said in the log");
            return;
        }
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&log, "the last run\n").unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["tests::standard_error_goes_to_the_log", "--exact", "--nocapture"])
            .env("AHEM_TEST_APP_LOG", &log)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(std::fs::read_to_string(&log).unwrap().contains("said in the log"));
        let previous = std::fs::read_to_string(dir.join("app.previous.log")).unwrap();
        assert_eq!(previous, "the last run\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The app's pages run only their own scripts and reach nothing but the backend.
    #[test]
    fn the_content_security_policy_is_strict() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let csp = config["app"]["security"]["csp"].as_str().expect("a policy");
        let directive = |name: &str| {
            csp.split(';').map(str::trim).find(|part| part.starts_with(name)).unwrap_or_default()
        };
        assert_eq!(directive("script-src"), "script-src 'self'");
        assert_eq!(directive("default-src"), "default-src 'self'");
        assert!(directive("connect-src").contains("ipc:"));
        assert!(!csp.contains("http://localhost") && !csp.contains("https:"), "{csp}");
    }
}

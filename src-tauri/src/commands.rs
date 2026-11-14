//! Tauri commands and the state they share.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

use crate::audio::capture::{self, InputDevice};
use crate::dictation::{FileRequest, FileSource, Message, services};
use crate::domain::app_rules::{self, AppId, RecentApps};
use crate::domain::clean_text::CleanText;
use crate::domain::events::{CoreEvent, DictationEvent, ModelEvent};
use crate::domain::modes::{self, Connection, HotkeyAction, ModesError, ProviderKind};
use crate::domain::replacements::{self, Replacement};
use crate::domain::services::Service;
use crate::domain::session::DictationResult;
use crate::engines::chatgpt::{self, AuthError, Credentials};
use crate::engines::cloud::{self, CloudError};
use crate::engines::llm::{self, LlmError};
use crate::engines::{self, languages};
use crate::files::ExportFormat;
use crate::hotkeys;
use crate::indicator::Indicator;
use crate::meetings::service::Meetings;
use crate::meetings::transcript::Labels;
use crate::runtimes;
use crate::storage::catalog::{self, ModelSpec};
use crate::storage::history::{self, Retention, SharedHistory};
use crate::storage::models::{ModelState, ModelStore};
use crate::storage::secrets::{self, SecretStore};
use crate::storage::settings::{SaveError, Settings, SettingsStore};
use crate::storage::stats::{self, SharedStats};
use crate::tray;
use crate::util::{now_ms, random_hex};
use tauri_plugin_autostart::ManagerExt;

/// Tauri event name for every `CoreEvent`.
pub const CORE_EVENT: &str = "core-event";
/// Sent with the saved settings after every save, also one made from the tray.
pub const SETTINGS_EVENT: &str = "settings-changed";
/// The registered hotkeys changed without a save (a recorded hotkey ended): read the status again.
pub const HOTKEYS_EVENT: &str = "hotkeys-changed";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// Passes a download's progress on at most every `PROGRESS_INTERVAL`, and at its end.
pub fn throttled(mut report: impl FnMut(u64, u64)) -> impl FnMut(u64, u64) {
    let mut last: Option<Instant> = None;
    move |done, total| {
        if done == total || last.is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL) {
            last = Some(Instant::now());
            report(done, total);
        }
    }
}

/// Running model downloads with their cancel flags, and models being deleted, by model id.
#[derive(Default)]
pub struct Downloads(Mutex<ModelJobs>);

#[derive(Default)]
struct ModelJobs {
    running: HashMap<String, Arc<AtomicBool>>,
    deleting: HashSet<String>,
}

impl Downloads {
    /// Notes a download with its stop flag; `false` if one with this id is already going or the
    /// model is being deleted.
    pub fn start(&self, id: &str, cancel: Arc<AtomicBool>) -> bool {
        let mut jobs = self.0.lock().expect("downloads lock");
        if jobs.running.contains_key(id) || jobs.deleting.contains(id) {
            return false;
        }
        jobs.running.insert(id.to_string(), cancel);
        true
    }

    pub fn finish(&self, id: &str) {
        self.0.lock().expect("downloads lock").running.remove(id);
    }

    pub fn running(&self, id: &str) -> bool {
        self.0.lock().expect("downloads lock").running.contains_key(id)
    }

    /// Notes that the model is being deleted; `false` while it downloads.
    fn start_deleting(&self, id: &str) -> bool {
        let mut jobs = self.0.lock().expect("downloads lock");
        !jobs.running.contains_key(id) && jobs.deleting.insert(id.to_string())
    }

    fn finish_deleting(&self, id: &str) {
        self.0.lock().expect("downloads lock").deleting.remove(id);
    }

    fn deleting(&self, id: &str) -> bool {
        self.0.lock().expect("downloads lock").deleting.contains(id)
    }

    /// Asks a running download to stop.
    pub fn cancel(&self, id: &str) {
        if let Some(cancel) = self.0.lock().expect("downloads lock").running.get(id) {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub struct Dictation {
    pub inbox: Sender<Message>,
    pub last_result: Arc<Mutex<Option<DictationResult>>>,
    /// A failed dictation's recording is kept to be recognized again.
    pub retry_available: Arc<AtomicBool>,
    pub recent_apps: Arc<Mutex<RecentApps>>,
    pub thread: Mutex<Option<JoinHandle<()>>>,
    /// `whisper-server` and `llama-server`, for the state of the chosen model's engine.
    pub whisper_executable: PathBuf,
    pub llama_executable: PathBuf,
}

/// The registered hotkeys and what each does; shared with the plugin's handler. Empty while
/// a new hotkey is being recorded in the interface.
#[derive(Default, Clone)]
pub struct Hotkeys(pub Arc<Mutex<HashMap<Shortcut, HotkeyAction>>>);

#[derive(Serialize)]
pub struct ModelInfo {
    #[serde(flatten)]
    spec: ModelSpec,
    state: ModelState,
    /// Bytes to download and to keep: a llama.cpp model's projector counts too.
    download_size: u64,
    /// Codes of the spoken languages it recognizes.
    languages: Vec<&'static str>,
    /// Its engine reads the dictionary.
    takes_dictionary: bool,
    /// Its engine takes the chosen spoken language instead of always detecting it.
    takes_language: bool,
}

/// A cloud engine for the recognition settings; mirrors `CloudEngineInfo` in `CloudPanel.tsx`.
#[derive(Serialize)]
pub struct CloudEngineInfo {
    id: &'static str,
    name: &'static str,
    /// The named service whose key it uses (`src/services.ts`).
    service: &'static str,
    translate: bool,
    /// It gives word times, so it transcribes audio and video files.
    files: bool,
    languages: Vec<&'static str>,
    takes_dictionary: bool,
    takes_language: bool,
}

#[derive(Serialize)]
pub struct Status {
    hotkey_registered: bool,
    /// Canonical hotkeys from the settings that are not registered (taken by another program).
    unregistered_hotkeys: Vec<String>,
    engine_available: bool,
    settings_error: Option<String>,
}

/// Every hotkey of the settings to register with its action, after checking the modes. The
/// translation hotkey waits for a dictation model that translates.
pub fn wanted_hotkeys(settings: &Settings) -> Result<Vec<(String, HotkeyAction)>, SaveError> {
    settings.hotkey_actions(engines::translates(settings.model_id.as_deref())).map_err(|error| {
        match error {
            // The interface says so as of a hotkey another program has.
            ModesError::HotkeyClash(hotkey) => SaveError::HotkeyTaken(hotkey),
            // The interface never sends the others.
            _ => SaveError::Invalid("modes".into()),
        }
    })
}

/// The hotkeys of `settings` to register: their set, or the main hotkey alone when they name no
/// valid set (a file edited by hand).
fn hotkey_set(settings: &Settings) -> Vec<(String, HotkeyAction)> {
    wanted_hotkeys(settings).unwrap_or_else(|_| vec![(settings.hotkey.clone(), HotkeyAction::Main)])
}

/// Every hotkey the saved `settings` name, the translation one too while the model does not
/// translate: none of them is newly chosen by a save. The full set holds `hotkey_set`.
fn known_hotkeys(settings: &Settings) -> Vec<(String, HotkeyAction)> {
    settings.hotkey_actions(true).unwrap_or_else(|_| hotkey_set(settings))
}

/// Set while the interface records a new hotkey: none is registered meanwhile.
static HOTKEYS_SUSPENDED: AtomicBool = AtomicBool::new(false);
/// The hotkeys another program held when the recording began, for the status to name meanwhile.
static TAKEN_WHEN_SUSPENDED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn shortcuts_of(
    wanted: &[(String, HotkeyAction)],
) -> Result<HashMap<Shortcut, HotkeyAction>, SaveError> {
    let mut shortcuts = HashMap::new();
    for (text, action) in wanted {
        let shortcut = hotkeys::parse(text).map_err(|error| SaveError::InvalidHotkey(error.0))?;
        shortcuts.insert(shortcut, action.clone());
    }
    Ok(shortcuts)
}

/// The hotkeys of a set, to tell the ones the user has just chosen from those already there.
fn known_shortcuts(known: &[(String, HotkeyAction)]) -> HashSet<Shortcut> {
    known.iter().filter_map(|(text, _)| hotkeys::parse(text).ok()).collect()
}

/// Registers `wanted`, replacing the current set. A hotkey of `known` (the set before) that
/// another program holds is left out and the rest still registers: it was taken already, and
/// refusing for it would refuse every save. A newly chosen hotkey that is taken refuses the
/// whole set: the previous set stays registered and the taken hotkey is reported.
pub fn register_hotkeys(
    app: &AppHandle,
    wanted: &[(String, HotkeyAction)],
    known: &[(String, HotkeyAction)],
) -> Result<(), SaveError> {
    let mut next = shortcuts_of(wanted)?;
    let state = app.state::<Hotkeys>();
    // The plugin is called without the lock: on Linux it waits for its own thread, which takes
    // the lock to route a key press.
    let (removed, added): (Vec<Shortcut>, Vec<Shortcut>) = {
        let routes = state.0.lock().expect("hotkey lock");
        (
            routes.keys().filter(|key| !next.contains_key(key)).copied().collect(),
            next.keys().filter(|key| !routes.contains_key(key)).copied().collect(),
        )
    };
    let shortcuts = app.global_shortcut();
    let left_out = register_with(
        &added,
        &removed,
        &known_shortcuts(known),
        &mut |shortcut| shortcuts.register(shortcut).is_ok(),
        &mut |shortcut| {
            let _ = shortcuts.unregister(shortcut);
        },
    )
    .map_err(|taken| SaveError::HotkeyTaken(taken.into_string()))?;
    for shortcut in &left_out {
        next.remove(shortcut);
    }
    *state.0.lock().expect("hotkey lock") = next;
    Ok(())
}

/// `register_hotkeys` apart from the plugin: `removed` go, `added` come; a taken one of `known`
/// is left out (returned), a taken new one undoes all of it and is the error.
fn register_with(
    added: &[Shortcut],
    removed: &[Shortcut],
    known: &HashSet<Shortcut>,
    register: &mut dyn FnMut(Shortcut) -> bool,
    unregister: &mut dyn FnMut(Shortcut),
) -> Result<Vec<Shortcut>, Shortcut> {
    for shortcut in removed {
        unregister(*shortcut);
    }
    let mut registered = Vec::new();
    let mut left_out = Vec::new();
    for shortcut in added {
        if register(*shortcut) {
            registered.push(*shortcut);
        } else if known.contains(shortcut) {
            left_out.push(*shortcut);
        } else {
            for undo in &registered {
                unregister(*undo);
            }
            for old in removed {
                register(*old);
            }
            return Err(*shortcut);
        }
    }
    Ok(left_out)
}

/// While a new hotkey is recorded: whether the newly chosen hotkeys of `wanted` are free. Each
/// is registered for a moment and let go; no key press is routed meanwhile.
fn try_hotkeys(
    app: &AppHandle,
    wanted: &[(String, HotkeyAction)],
    known: &[(String, HotkeyAction)],
) -> Result<(), SaveError> {
    let known = known_shortcuts(known);
    let shortcuts = app.global_shortcut();
    for shortcut in shortcuts_of(wanted)?.keys().filter(|shortcut| !known.contains(shortcut)) {
        if shortcuts.register(*shortcut).is_err() {
            return Err(SaveError::HotkeyTaken(shortcut.into_string()));
        }
        let _ = shortcuts.unregister(*shortcut);
    }
    Ok(())
}

/// At startup and after a hotkey was recorded: every hotkey that is free; the status names the
/// others. Settings that name no valid set (a file edited by hand) fall back to the main one.
pub fn register_startup_hotkeys(app: &AppHandle, settings: &Settings) {
    let all = hotkey_set(settings);
    if let Err(error) = register_hotkeys(app, &all, &all) {
        log_line!("cannot register the hotkeys: {error:?}");
    }
}

/// Preferred OS locales in priority order, e.g. `["ru-RU", "en-US"]`.
/// The interface language is chosen from them on the frontend.
#[tauri::command]
pub fn system_locales() -> Vec<String> {
    sys_locale::get_locales().collect()
}

#[tauri::command]
pub fn get_settings(store: State<'_, Mutex<SettingsStore>>) -> Settings {
    store.lock().expect("settings lock").settings().clone()
}

#[tauri::command]
pub fn get_status(
    hotkeys: State<'_, Hotkeys>,
    dictation: State<'_, Dictation>,
    store: State<'_, Mutex<SettingsStore>>,
) -> Status {
    let store = store.lock().expect("settings lock");
    let wanted: Vec<String> =
        hotkey_set(store.settings()).into_iter().map(|(hotkey, _)| hotkey).collect();
    // While a new hotkey is recorded none is registered: the ones taken before stay named, a
    // newly chosen one was tried when it was saved.
    let unregistered_hotkeys: Vec<String> = if HOTKEYS_SUSPENDED.load(Ordering::Relaxed) {
        let taken = TAKEN_WHEN_SUSPENDED.lock().expect("hotkey lock");
        wanted.into_iter().filter(|hotkey| taken.contains(hotkey)).collect()
    } else {
        let routes = hotkeys.0.lock().expect("hotkey lock");
        let registered: Vec<String> = routes.keys().map(|key| key.into_string()).collect();
        wanted.into_iter().filter(|hotkey| !registered.contains(hotkey)).collect()
    };
    Status {
        hotkey_registered: !unregistered_hotkeys.contains(&store.settings().hotkey),
        unregistered_hotkeys,
        engine_available: chosen_engine_available(
            store.settings().model_id.as_deref(),
            &dictation.whisper_executable,
            &dictation.llama_executable,
        ),
        settings_error: store.load_error().map(String::from),
    }
}

/// Whether the program of the chosen model is on this computer, as dictation checks it; a cloud
/// engine, an unknown model or none needs no program here.
pub(crate) fn chosen_engine_available(
    model_id: Option<&str>,
    whisper_server: &Path,
    llama_server: &Path,
) -> bool {
    model_id.and_then(catalog::find).is_none_or(|spec| {
        services::engine_installed(
            spec.sherpa.is_some(),
            spec.llama.is_some(),
            whisper_server,
            llama_server,
        )
    })
}

#[tauri::command]
pub fn save_settings(
    mut settings: Settings,
    app: AppHandle,
    store: State<'_, Mutex<SettingsStore>>,
    dictation: State<'_, Dictation>,
    history: State<'_, Arc<SharedHistory>>,
) -> Result<Settings, SaveError> {
    let mut store = store.lock().expect("settings lock");
    // Settings of a newer version are not written over: refused before the login item or the
    // hotkeys change.
    if store.load_error().is_some() {
        return Err(SaveError::ReadOnly);
    }
    // Connections change only through their own commands, which also handle the keys.
    settings.connections = store.settings().connections.clone();
    settings.check()?;
    let downloads = app.state::<Downloads>();
    if !choosable_model(settings.model_id.as_deref(), &settings.connections, |id| {
        downloads.deleting(id)
    }) {
        return Err(SaveError::Invalid("model_id".into()));
    }
    let wanted = wanted_hotkeys(&settings)?;
    // A removed mode takes its rules along.
    app_rules::tidy(&mut settings.app_rules, &settings.modes);
    let previous = hotkey_set(store.settings());
    let known = known_hotkeys(store.settings());
    let limit_changed = store.settings().history_limit != settings.history_limit;
    let autostart_changed = store.settings().launch_at_login != settings.launch_at_login;
    if autostart_changed {
        set_autostart(&app, settings.launch_at_login)?;
    }
    // While a new hotkey is recorded none is registered: the new ones are only tried, so a
    // taken one is still refused.
    let suspended = HOTKEYS_SUSPENDED.load(Ordering::Relaxed);
    let registered = if suspended {
        try_hotkeys(&app, &wanted, &known)
    } else {
        register_hotkeys(&app, &wanted, &known)
    };
    if let Err(error) = registered {
        if autostart_changed {
            let _ = set_autostart(&app, !settings.launch_at_login);
        }
        return Err(error);
    }
    let saved = match store.update(settings) {
        Ok(saved) => saved.clone(),
        Err(error) => {
            if !suspended {
                let _ = register_hotkeys(&app, &previous, &previous);
            }
            if autostart_changed {
                let _ = set_autostart(&app, store.settings().launch_at_login);
            }
            return Err(error);
        }
    };
    // A smaller limit removes the oldest entries at once, not only at the next dictation; off
    // the main thread (removing thousands of entries takes a while), and the lists read again.
    if limit_changed {
        let (history, app, retention) =
            (Arc::clone(&history), app.clone(), Retention::from_limit(saved.history_limit));
        thread::spawn(move || {
            if let Err(error) = history.trim(retention) {
                log_line!("cannot trim the history: {error}");
            }
            history_changed(&app);
        });
    }
    announce_settings(&app, &dictation, &saved);
    Ok(saved)
}

/// Runs `work` on the main thread and waits for its result; `None` when the app is closing.
/// Background commands change the settings only this way. Saving rebuilds the tray menu and
/// registers hotkeys, which wait for the main thread, while a command running there may wait
/// for the settings lock: holding that lock off the main thread would freeze the app. On the
/// main thread itself `work` runs at once.
fn on_main_thread<T: Send + 'static>(
    app: &AppHandle,
    work: impl FnOnce(&AppHandle) -> T + Send + 'static,
) -> Option<T> {
    let (send, receive) = mpsc::sync_channel(1);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = send.send(work(&handle));
    })
    .ok()?;
    receive.recv().ok()
}

/// The error of a change that did not run because the app is closing.
fn closing() -> SaveError {
    SaveError::Io("the app is closing".into())
}

/// Tells the controller, the tray and every window about the saved settings.
fn announce_settings(app: &AppHandle, dictation: &Dictation, saved: &Settings) {
    let _ = dictation.inbox.send(Message::Settings(Box::new(saved.clone())));
    if let Some(indicator) = app.try_state::<Arc<Indicator>>() {
        indicator.set_shown(saved.show_indicator);
    }
    tray::refresh(app, saved);
    let _ = app.emit(SETTINGS_EVENT, saved);
}

/// The OS entry that starts the app hidden in the tray at login.
fn set_autostart(app: &AppHandle, enabled: bool) -> Result<(), SaveError> {
    let autostart = app.autolaunch();
    let result = if enabled { autostart.enable() } else { autostart.disable() };
    result.map_err(|error| SaveError::Io(format!("autostart: {error}")))
}

/// At each start with "start at login" on: the entry is written again with where the app is
/// now, so one made while it ran from a disk image or Downloads (and moved since) still starts
/// it. Not when the system says it is off (turned off in Windows' Task Manager, which writing
/// it would turn on again), and not for a development build, which shares the entry.
pub fn refresh_autostart(app: &AppHandle, launch_at_login: bool) {
    let enabled = || app.autolaunch().is_enabled().unwrap_or(false);
    if refreshes_autostart(launch_at_login, cfg!(debug_assertions), enabled)
        && let Err(error) = set_autostart(app, true)
    {
        log_line!("cannot write the start at login again: {error:?}");
    }
}

/// Whether the login entry is written again: "start at login" on, a release build, and the
/// system says the entry is on (asked last: it reads the system).
fn refreshes_autostart(
    launch_at_login: bool,
    development: bool,
    enabled: impl FnOnce() -> bool,
) -> bool {
    launch_at_login && !development && enabled()
}

/// From the tray menu: the main hotkey dictates in this mode from now on.
pub fn set_main_mode(app: &AppHandle, mode_id: &str) {
    let current =
        app.state::<Mutex<SettingsStore>>().lock().expect("settings lock").settings().clone();
    let mut settings = current.clone();
    settings.main_mode_id = mode_id.to_string();
    if let Err(error) = save_settings(settings, app.clone(), app.state(), app.state(), app.state())
    {
        log_line!("cannot change the main mode: {error:?}");
        // The menu ticked the chosen mode itself: the tick goes back to the saved one.
        tray::refresh(app, &current);
    }
}

/// Pauses the hotkey while the user records a new one in the interface.
#[tauri::command]
pub fn set_hotkey_suspended(
    suspended: bool,
    app: AppHandle,
    store: State<'_, Mutex<SettingsStore>>,
) -> Result<(), SaveError> {
    if suspended {
        // A second call finds none registered: what was taken is known already.
        if HOTKEYS_SUSPENDED.load(Ordering::Relaxed) {
            return Ok(());
        }
        let state = app.state::<Hotkeys>();
        let registered: Vec<String> =
            state.0.lock().expect("hotkey lock").keys().map(|key| key.into_string()).collect();
        let settings = store.lock().expect("settings lock").settings().clone();
        *TAKEN_WHEN_SUSPENDED.lock().expect("hotkey lock") = hotkey_set(&settings)
            .into_iter()
            .map(|(hotkey, _)| hotkey)
            .filter(|hotkey| !registered.contains(hotkey))
            .collect();
        HOTKEYS_SUSPENDED.store(true, Ordering::Relaxed);
        register_hotkeys(&app, &[], &[])
    } else {
        HOTKEYS_SUSPENDED.store(false, Ordering::Relaxed);
        // As at startup: every one that is free; the status names the rest.
        let settings = store.lock().expect("settings lock").settings().clone();
        register_startup_hotkeys(&app, &settings);
        let _ = app.emit(HOTKEYS_EVENT, ());
        Ok(())
    }
}

/// Whether the recording of the last dictation that could not be recognized is kept; changes
/// arrive as `retry_available` events.
#[tauri::command]
pub fn retry_available(dictation: State<'_, Dictation>) -> bool {
    dictation.retry_available.load(Ordering::Relaxed)
}

/// Recognizes the kept recording again; the text goes to the history and the last result.
#[tauri::command]
pub fn retry_dictation(dictation: State<'_, Dictation>) {
    let _ = dictation.inbox.send(Message::RetryDictation);
}

#[tauri::command]
pub fn get_last_result(dictation: State<'_, Dictation>) -> Option<DictationResult> {
    dictation.last_result.lock().expect("last result lock").clone()
}

/// The programs dictated into since the app started, the latest first.
#[tauri::command]
pub fn recent_apps(dictation: State<'_, Dictation>) -> Vec<AppId> {
    dictation.recent_apps.lock().expect("recent apps lock").list()
}

#[derive(Serialize)]
pub struct LanguageInfo {
    code: &'static str,
    name: &'static str,
}

/// Languages the speech engine knows; the UI shows localized names.
#[tauri::command]
pub fn list_languages() -> Vec<LanguageInfo> {
    languages::all().iter().map(|(code, name)| LanguageInfo { code, name }).collect()
}

#[tauri::command]
pub fn list_input_devices() -> Result<Vec<InputDevice>, String> {
    capture::input_devices().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn list_models(store: State<'_, ModelStore>) -> Vec<ModelInfo> {
    catalog::models()
        .into_iter()
        .map(|spec| ModelInfo {
            state: store.state(&spec),
            download_size: spec.download_size(),
            languages: spec.languages(),
            takes_dictionary: spec.takes_dictionary(),
            takes_language: spec.takes_language(),
            spec,
        })
        .collect()
}

/// The cloud engines; the dictionary and the spoken language reach every one of them.
#[tauri::command]
pub fn list_cloud_engines() -> Vec<CloudEngineInfo> {
    cloud::ENGINES
        .iter()
        .map(|spec| CloudEngineInfo {
            id: spec.id,
            name: spec.name,
            service: spec.service.id(),
            translate: spec.translate,
            files: spec.word_times(),
            languages: spec.languages(),
            takes_dictionary: true,
            takes_language: true,
        })
        .collect()
}

/// Whether one of `connections` is the service's.
fn connected(connections: &[Connection], service: Service) -> bool {
    connections.iter().any(|connection| Service::of(connection) == Some(service))
}

/// Whether the dictation model may be chosen: none, a model of the catalog that is not being
/// deleted, or the cloud engine of a connected service.
fn choosable_model(
    model_id: Option<&str>,
    connections: &[Connection],
    deleting: impl Fn(&str) -> bool,
) -> bool {
    let Some(id) = model_id else { return true };
    !deleting(id)
        && (catalog::find(id).is_some()
            || cloud::find(id).is_some_and(|spec| connected(connections, spec.service)))
}

/// Why a model was not deleted.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum DeleteModelError {
    UnknownModel,
    Downloading,
    /// Already being deleted (pressed again while the files go).
    Deleting,
    /// Dictation could not move to another model (a hotkey of the new one is taken, say).
    Settings(SaveError),
    Io(String),
}

/// Frees the disk space of a downloaded or partly downloaded model. Deleting the dictation
/// model first moves dictation to another downloaded model, or to none.
/// Off the main thread and without holding the settings: deleting gigabytes and waiting for the
/// engine to let go of the files would freeze every window.
#[tauri::command]
pub async fn delete_model(id: String, app: AppHandle) -> Result<(), DeleteModelError> {
    tauri::async_runtime::spawn_blocking(move || delete_model_files(&id, &app))
        .await
        .unwrap_or_else(|error| Err(DeleteModelError::Io(error.to_string())))
}

fn delete_model_files(id: &str, app: &AppHandle) -> Result<(), DeleteModelError> {
    let spec = catalog::find(id).ok_or(DeleteModelError::UnknownModel)?;
    let model = id.to_string();
    // On the main thread nothing else changes the settings meanwhile.
    on_main_thread(app, move |app| {
        let downloads = app.state::<Downloads>();
        if !downloads.start_deleting(&model) {
            let running = downloads.running(&model);
            return Err(if running {
                DeleteModelError::Downloading
            } else {
                DeleteModelError::Deleting
            });
        }
        let current =
            app.state::<Mutex<SettingsStore>>().lock().expect("settings lock").settings().clone();
        if current.model_id.as_deref() == Some(model.as_str()) {
            // Another downloaded model that is not being deleted too.
            let models = app.state::<ModelStore>();
            let ready: Vec<String> = catalog::models()
                .into_iter()
                .filter(|other| models.ready(other).is_some() && !downloads.deleting(&other.id))
                .map(|other| other.id)
                .collect();
            let mut settings = current;
            settings.model_id = replacement_model(&model, &ready);
            if let Err(error) =
                save_settings(settings, app.clone(), app.state(), app.state(), app.state())
            {
                downloads.finish_deleting(&model);
                return Err(DeleteModelError::Settings(error));
            }
        }
        Ok(())
    })
    .unwrap_or(Err(DeleteModelError::Settings(closing())))?;
    // The engine of the former dictation model stops in the background and may hold its
    // files for a moment.
    let store = app.state::<ModelStore>();
    let deleted = delete_retrying(|| store.delete(&spec), 10, Duration::from_millis(200))
        .map_err(|error| DeleteModelError::Io(error.to_string()));
    app.state::<Downloads>().finish_deleting(id);
    deleted
}

/// The dictation model after `deleted` is deleted: the first other downloaded one, or none.
fn replacement_model(deleted: &str, ready: &[String]) -> Option<String> {
    ready.iter().find(|id| id.as_str() != deleted).cloned()
}

/// Runs `delete` up to `attempts` times with `pause` between failures; the last error stays.
fn delete_retrying(
    mut delete: impl FnMut() -> std::io::Result<()>,
    attempts: u32,
    pause: Duration,
) -> std::io::Result<()> {
    let mut left = attempts.max(1);
    loop {
        match delete() {
            Ok(()) => return Ok(()),
            Err(error) if left <= 1 => return Err(error),
            Err(_) => {
                left -= 1;
                thread::sleep(pause);
            }
        }
    }
}

/// What the user can open in the file manager: the downloaded models, the app's data (history,
/// meetings, models; on Windows and Linux the settings and keys are in a folder of their own), or
/// the history file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataFolder {
    Models,
    All,
    History,
}

/// Where the models and the app's data are kept.
#[derive(Debug, Serialize)]
pub struct DataFolders {
    models: String,
    all: String,
}

fn data_folder(app: &AppHandle, folder: DataFolder) -> Result<PathBuf, String> {
    match folder {
        DataFolder::Models => Ok(app.state::<ModelStore>().root().to_path_buf()),
        DataFolder::All => app.path().app_local_data_dir().map_err(|error| error.to_string()),
        DataFolder::History => Ok(app.state::<Arc<SharedHistory>>().path().to_path_buf()),
    }
}

#[tauri::command]
pub fn data_folders(app: AppHandle) -> Result<DataFolders, String> {
    let path = |folder| data_folder(&app, folder).map(|path| path.display().to_string());
    Ok(DataFolders { models: path(DataFolder::Models)?, all: path(DataFolder::All)? })
}

/// How the file manager shows a place: opens a folder, or opens the folder around an item
/// with the item selected.
#[derive(Debug, PartialEq, Eq)]
enum Show {
    Open(PathBuf),
    Reveal(PathBuf),
}

/// A folder opens; the history file (`path`) is selected in its folder once it exists.
fn show_target(folder: DataFolder, path: &Path, exists: bool) -> Show {
    match folder {
        DataFolder::History if exists => Show::Reveal(path.to_path_buf()),
        DataFolder::History => Show::Open(path.parent().unwrap_or(path).to_path_buf()),
        DataFolder::Models | DataFolder::All => Show::Open(path.to_path_buf()),
    }
}

/// Shows the place in the system file manager, creating its folder (for the history, the one
/// its file goes in) on a fresh install.
#[tauri::command]
pub fn open_data_folder(folder: DataFolder, app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let path = data_folder(&app, folder)?;
    // A fresh install has no folders yet: the history's is made too, for its file to be shown in.
    let made = if folder == DataFolder::History { path.parent() } else { Some(path.as_path()) };
    if let Some(dir) = made {
        fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    match show_target(folder, &path, path.exists()) {
        Show::Open(path) => app.opener().open_path(path.display().to_string(), None::<&str>),
        Show::Reveal(path) => app.opener().reveal_item_in_dir(path),
    }
    .map_err(|error| error.to_string())
}

/// Starts a download in the background; progress and the outcome arrive as `core-event`s.
#[tauri::command]
pub fn download_model(
    id: String,
    app: AppHandle,
    store: State<'_, ModelStore>,
    downloads: State<'_, Downloads>,
) -> Result<(), String> {
    let spec = catalog::find(&id).ok_or_else(|| format!("unknown model {id}"))?;
    let cancel = Arc::new(AtomicBool::new(false));
    if !downloads.start(&id, Arc::clone(&cancel)) {
        return Ok(());
    }
    let store = store.inner().clone();
    thread::spawn(move || {
        let emit = |event| {
            let _ = app.emit(CORE_EVENT, CoreEvent::Model(event));
        };
        let mut progress = throttled(|downloaded, total| {
            emit(ModelEvent::DownloadProgress { model: spec.id.clone(), downloaded, total });
        });
        let mut unpacking = || emit(ModelEvent::Unpacking { model: spec.id.clone() });
        let agent = runtimes::http::agent();
        let result = store.download(&agent, &spec, &cancel, &mut progress, &mut unpacking);
        app.state::<Downloads>().finish(&spec.id);
        match result {
            Ok(_) => emit(ModelEvent::DownloadFinished { model: spec.id.clone() }),
            Err(error) => emit(ModelEvent::DownloadFailed {
                model: spec.id.clone(),
                failure: error.failure(),
                detail: error.to_string(),
            }),
        }
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_model_download(id: String, downloads: State<'_, Downloads>) {
    downloads.cancel(&id);
}

/// Why a connection could not be added, removed or refreshed. Never carries the key.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum ConnectionError {
    InvalidName,
    /// Not HTTPS and not this computer, or not a URL.
    InvalidBaseUrl,
    MissingKey,
    NotFound,
    /// Names the mode that uses the connection.
    InUse(String),
    /// Dictation is recognized by this service: choose another engine first.
    UsedForRecognition,
    Unauthorized,
    RateLimited,
    QuotaExceeded,
    Network(String),
    Server(String),
    NoModels,
    BadResponse(String),
    /// The key could not be stored or read.
    Secrets(String),
    Settings(SaveError),
    /// ChatGPT sign-in: cancelled in ahem, declined or not finished in the browser.
    SignInCancelled,
    SignInDenied,
    SignInTimedOut,
    /// Signed in to ChatGPT, but plan usage was not allowed.
    PlanNotGranted,
    /// Another ChatGPT account than before: signing in once more connects it.
    OtherAccount,
    /// ChatGPT no longer knows ahem's registration, which is forgotten: sign in again.
    ClientForgotten,
    /// The ChatGPT account cannot use its plan in ahem (Plus or Pro needed).
    PlanNotEligible,
    /// Another ChatGPT sign-in failure; names the step.
    SignIn(String),
}

impl From<AuthError> for ConnectionError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::Cancelled => Self::SignInCancelled,
            AuthError::Denied => Self::SignInDenied,
            AuthError::TimedOut => Self::SignInTimedOut,
            AuthError::PlanNotGranted => Self::PlanNotGranted,
            AuthError::OtherAccount => Self::OtherAccount,
            AuthError::ClientForgotten => Self::ClientForgotten,
            AuthError::SignInAgain => Self::Unauthorized,
            AuthError::Network(detail) => Self::Network(detail),
            AuthError::Storage(detail) => Self::Secrets(detail),
            error => Self::SignIn(error.to_string()),
        }
    }
}

impl From<LlmError> for ConnectionError {
    fn from(error: LlmError) -> Self {
        match error {
            LlmError::MissingBaseUrl => Self::InvalidBaseUrl,
            LlmError::Unauthorized => Self::Unauthorized,
            LlmError::RateLimited => Self::RateLimited,
            LlmError::QuotaExceeded => Self::QuotaExceeded,
            LlmError::Network(detail) => Self::Network(detail),
            error @ LlmError::Server { .. } => Self::Server(error.to_string()),
            LlmError::Empty => Self::NoModels,
            LlmError::PlanNotEligible => Self::PlanNotEligible,
            LlmError::PlanLimit => Self::QuotaExceeded,
            error => Self::BadResponse(error.to_string()),
        }
    }
}

impl From<CloudError> for ConnectionError {
    fn from(error: CloudError) -> Self {
        match error {
            CloudError::Unauthorized => Self::Unauthorized,
            CloudError::RateLimited => Self::RateLimited,
            CloudError::QuotaExceeded => Self::QuotaExceeded,
            CloudError::Network(detail) => Self::Network(detail),
            CloudError::BadResponse(detail) => Self::BadResponse(detail),
            error => Self::Server(error.to_string()),
        }
    }
}

impl From<SaveError> for ConnectionError {
    fn from(error: SaveError) -> Self {
        Self::Settings(error)
    }
}

const MAX_CONNECTION_NAME_CHARS: usize = 60;

/// The provider's model list, fetched off the async runtime: it may take up to 30 seconds. A
/// speech-only service has no models: its key is checked on its own.
async fn fetch_models(
    provider: ProviderKind,
    base_url: Option<String>,
    key: String,
) -> Result<Vec<String>, ConnectionError> {
    tauri::async_runtime::spawn_blocking(move || {
        let agent = runtimes::http::agent();
        match llm::Provider::of(provider) {
            Some(provider) => llm::list_models(&agent, provider, base_url.as_deref(), &key)
                .map_err(ConnectionError::from),
            None => cloud::check_deepgram_key(&agent, &key)
                .map(|()| Vec::new())
                .map_err(ConnectionError::from),
        }
    })
    .await
    .map_err(|error| ConnectionError::BadResponse(error.to_string()))?
}

/// Saves `change` applied to the current settings and tells the controller, the tray and every
/// window, as `save_settings` does: the quick panel lists the engines of the connections. Only
/// on the main thread (`on_main_thread`).
fn save_connections(
    app: &AppHandle,
    store: &mut SettingsStore,
    change: impl FnOnce(&mut Settings),
) -> Result<Settings, ConnectionError> {
    let mut settings = store.settings().clone();
    change(&mut settings);
    let saved = store.update(settings)?.clone();
    announce_settings(app, &app.state::<Dictation>(), &saved);
    Ok(saved)
}

/// Runs a change of the connections on the main thread (`on_main_thread`); waits for it on a
/// thread for blocking work, not on one of the async runtime's.
async fn change_connections<T: Send + 'static>(
    app: &AppHandle,
    work: impl FnOnce(&AppHandle) -> Result<T, ConnectionError> + Send + 'static,
) -> Result<T, ConnectionError> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || on_main_thread(&app, work))
        .await
        .ok()
        .flatten()
        .unwrap_or(Err(ConnectionError::Settings(closing())))
}

/// Checks the key by listing the provider's models, then stores the key encrypted and the
/// connection with the model list in the settings.
#[tauri::command]
pub async fn add_connection(
    provider: ProviderKind,
    name: String,
    base_url: Option<String>,
    api_key: String,
    app: AppHandle,
) -> Result<Settings, ConnectionError> {
    let name = name.trim().to_owned();
    if name.is_empty() || name.chars().count() > MAX_CONNECTION_NAME_CHARS {
        return Err(ConnectionError::InvalidName);
    }
    let base_url = match provider {
        ProviderKind::OpenAiCompatible => Some(
            base_url
                .as_deref()
                .and_then(llm::checked_base_url)
                .ok_or(ConnectionError::InvalidBaseUrl)?,
        ),
        ProviderKind::Anthropic | ProviderKind::OpenAi | ProviderKind::Deepgram => None,
        // Connected by signing in, never with a key.
        ProviderKind::ChatGptAccount => return Err(ConnectionError::MissingKey),
    };
    let key = new_key(provider, &api_key)?;
    let models = fetch_models(provider, base_url.clone(), key.clone()).await?;

    let id = connection_id()?;
    let connection = Connection {
        id: id.clone(),
        name,
        provider,
        base_url,
        key_hint: (!key.is_empty()).then(|| secrets::hint(&key)),
        added_at_ms: now_ms(),
        models,
    };
    change_connections(&app, move |app| {
        let secrets = app.state::<SecretStore>();
        let store = app.state::<Mutex<SettingsStore>>();
        let mut store = store.lock().expect("settings lock");
        if !key.is_empty() {
            secrets.set(&id, &key).map_err(|error| ConnectionError::Secrets(error.to_string()))?;
        }
        let saved = save_connections(app, &mut store, |settings| {
            settings.connections.push(connection);
        });
        if saved.is_err() {
            let _ = secrets.delete(&id);
        }
        saved
    })
    .await
}

/// Replaces the key of a connection (an expired or revoked one) without removing it, so the
/// modes that use it go on: the new key is checked by listing the models, as when adding, and
/// a refused one leaves the old key in place. A ChatGPT account signs in again instead.
#[tauri::command]
pub async fn replace_connection_key(
    id: String,
    api_key: String,
    app: AppHandle,
    secrets: State<'_, SecretStore>,
) -> Result<Settings, ConnectionError> {
    let connection = find_connection(
        app.state::<Mutex<SettingsStore>>().lock().expect("settings lock").settings(),
        &id,
    )?
    .clone();
    let key = new_key(connection.provider, &api_key)?;
    let models =
        fetch_models(connection.provider, connection.base_url.clone(), key.clone()).await?;
    // The old key, to put back if the settings cannot be saved; read here, off the main thread:
    // the first read after an update may wait for the Keychain's prompt. Unknown when it
    // cannot be read: then nothing is put back rather than the key deleted.
    let vault = (*secrets).clone();
    let read = id.clone();
    let old =
        tauri::async_runtime::spawn_blocking(move || vault.get(&read).ok()).await.ok().flatten();
    change_connections(&app, move |app| {
        let secrets = app.state::<SecretStore>();
        let store = app.state::<Mutex<SettingsStore>>();
        let mut store = store.lock().expect("settings lock");
        // As it is now: removed meanwhile, or its models read again.
        let current = find_connection(store.settings(), &id)?.clone();
        let renewed = with_new_key(&current, &key, models, now_ms());
        let stored = if key.is_empty() { secrets.delete(&id) } else { secrets.set(&id, &key) };
        stored.map_err(|error| ConnectionError::Secrets(error.to_string()))?;
        let saved = save_connections(app, &mut store, |settings| {
            if let Some(slot) = settings.connections.iter_mut().find(|item| item.id == id) {
                *slot = renewed;
            }
        });
        // Not saved: the old key goes back with the old settings, when it is known.
        if saved.is_err() {
            let _ = match &old {
                Some(Some(old)) => secrets.set(&id, old),
                Some(None) => secrets.delete(&id),
                None => Ok(()),
            };
        }
        saved
    })
    .await
}

/// The new key of a connection of `provider`: trimmed; only an OpenAI-compatible server (often
/// one on this computer) may go on without one, and a ChatGPT account has none.
fn new_key(provider: ProviderKind, api_key: &str) -> Result<String, ConnectionError> {
    let key = api_key.trim().to_owned();
    match provider {
        ProviderKind::ChatGptAccount => Err(ConnectionError::MissingKey),
        ProviderKind::OpenAiCompatible => Ok(key),
        _ if key.is_empty() => Err(ConnectionError::MissingKey),
        _ => Ok(key),
    }
}

/// `connection` with a new key: the same id, so its modes go on; the key's hint, the models the
/// key lists, and when it was added.
fn with_new_key(connection: &Connection, key: &str, models: Vec<String>, now: i64) -> Connection {
    Connection {
        key_hint: (!key.is_empty()).then(|| secrets::hint(key)),
        added_at_ms: now,
        models,
        ..connection.clone()
    }
}

/// The ChatGPT sign-in running; a new one or `cancel_chatgpt_sign_in` stops it.
#[derive(Default)]
pub struct ChatGptSignIn(Mutex<Option<Arc<AtomicBool>>>);

impl ChatGptSignIn {
    /// A new sign-in; an earlier one still running is stopped.
    fn start(&self) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(earlier) = self.0.lock().expect("sign-in lock").replace(Arc::clone(&cancel)) {
            earlier.store(true, Ordering::SeqCst);
        }
        cancel
    }

    fn cancel(&self) {
        if let Some(cancel) = self.0.lock().expect("sign-in lock").take() {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Ends the sign-in of `cancel`: `true` if it was cancelled or a newer one took its place,
    /// even after the browser answered, so nothing of it is saved.
    fn finish(&self, cancel: &Arc<AtomicBool>) -> bool {
        let mut current = self.0.lock().expect("sign-in lock");
        if current.as_ref().is_some_and(|current| Arc::ptr_eq(current, cancel)) {
            *current = None;
        }
        cancel.load(Ordering::SeqCst)
    }
}

/// Connects ChatGPT through the user's account: the browser asks them to sign in and to let
/// ahem use their plan. Signing in again (an existing connection) keeps its client and must be
/// the same account. The models come from the plan.
#[tauri::command]
pub async fn connect_chatgpt(
    app: AppHandle,
    store: State<'_, Mutex<SettingsStore>>,
    secrets: State<'_, SecretStore>,
    sign_in: State<'_, ChatGptSignIn>,
) -> Result<Settings, ConnectionError> {
    use tauri_plugin_opener::OpenerExt;
    let cancel = sign_in.start();
    let existing = store
        .lock()
        .expect("settings lock")
        .settings()
        .connections
        .iter()
        .find(|connection| connection.provider == ProviderKind::ChatGptAccount)
        .cloned();
    let vault = (*secrets).clone();
    let previous = existing
        .as_ref()
        .and_then(|connection| Credentials::load(&vault, &connection.id).ok().flatten());
    let waiting = Arc::clone(&cancel);
    let browser = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let agent = runtimes::http::agent();
        let credentials = chatgpt::sign_in(&agent, &vault, previous.as_ref(), &waiting, |url| {
            browser.opener().open_url(url, None::<&str>).map_err(|error| error.to_string())
        })?;
        let models =
            llm::list_models(&agent, llm::Provider::ChatGptPlan, None, &credentials.access_token);
        Ok::<_, ConnectionError>((credentials, models))
    })
    .await
    .map_err(|error| ConnectionError::BadResponse(error.to_string()));
    let cancelled = sign_in.finish(&cancel);
    let (credentials, models) = match result {
        Ok(Ok(signed_in)) => signed_in,
        // A session OpenAI made for a sign-in that failed is ended by `chatgpt::sign_in`.
        Ok(Err(error)) | Err(error) => {
            return Err(if cancelled { AuthError::Cancelled.into() } else { error });
        }
    };
    // From here on, signed in at OpenAI: tokens that are not kept are revoked.
    let connected = existing.is_some();
    let models = match models {
        Ok(models) if !cancelled => models,
        outcome => {
            discard_sign_in(&secrets, credentials, connected);
            return Err(match outcome {
                Err(error) if !cancelled => error.into(),
                _ => AuthError::Cancelled.into(),
            });
        }
    };
    let id = match &existing {
        Some(connection) => connection.id.clone(),
        None => match connection_id() {
            Ok(id) => id,
            Err(error) => {
                discard_sign_in(&secrets, credentials, connected);
                return Err(error);
            }
        },
    };
    let connection = Connection {
        id: id.clone(),
        name: credentials.email.clone().unwrap_or_else(|| "ChatGPT".into()),
        provider: ProviderKind::ChatGptAccount,
        base_url: None,
        key_hint: None,
        added_at_ms: existing.as_ref().map_or_else(now_ms, |connection| connection.added_at_ms),
        models,
    };
    change_connections(&app, move |app| {
        let secrets = app.state::<SecretStore>();
        let store = app.state::<Mutex<SettingsStore>>();
        let mut store = store.lock().expect("settings lock");
        if let Err(error) = credentials.save(&secrets, &id) {
            discard_sign_in(&secrets, credentials, connected);
            return Err(error.into());
        }
        let saved = save_connections(app, &mut store, |settings| {
            settings.connections.retain(|other| other.id != id);
            settings.connections.push(connection);
        });
        // A connection signed in again keeps these tokens: they replaced the old ones.
        if saved.is_err() && !connected {
            let _ = secrets.delete(&id);
            discard_sign_in(&secrets, credentials, connected);
        }
        saved
    })
    .await
}

/// The tokens of a sign-in that is not kept: OpenAI ends their session in the background, and
/// after a first sign-in the next one lets the user choose the account again.
fn discard_sign_in(secrets: &SecretStore, credentials: Credentials, connected: bool) {
    if !connected && let Err(error) = chatgpt::Registration::signed_out(&credentials).save(secrets)
    {
        log_line!("ChatGPT sign-in not kept: {error}");
    }
    let agent = runtimes::http::agent();
    chatgpt::end_session(
        &agent,
        credentials.client_id,
        credentials.refresh_token,
        "sign-in not kept",
    );
}

/// Opens the ChatGPT settings where the user sets how much of the plan ahem may use.
#[tauri::command]
pub fn open_chatgpt_usage(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(chatgpt::MANAGE_USAGE_URL, None::<&str>)
        .map_err(|error| error.to_string())
}

/// The permissions dictation needs and whether the OS grants them now.
#[tauri::command]
pub fn get_permissions() -> crate::platform::Permissions {
    crate::platform::permissions()
}

/// Asks for a permission: the OS's own prompt where it shows one, else its settings page.
#[tauri::command]
pub fn request_permission(
    app: AppHandle,
    kind: crate::platform::PermissionKind,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    match crate::platform::request_permission(kind) {
        Some(settings) => {
            app.opener().open_url(settings, None::<&str>).map_err(|error| error.to_string())
        }
        None => Ok(()),
    }
}

/// Stops a ChatGPT sign-in that waits for the browser.
#[tauri::command]
pub fn cancel_chatgpt_sign_in(sign_in: State<'_, ChatGptSignIn>) {
    sign_in.cancel();
}

/// The settings without a removed connection.
#[derive(Serialize)]
pub struct Removed {
    settings: Settings,
    /// OpenAI did not confirm the end of a ChatGPT account's session: the user can disconnect
    /// ahem in the ChatGPT settings.
    sign_out_unconfirmed: bool,
}

/// Refuses while a mode uses the connection; then forgets it and its key. A ChatGPT account is
/// signed out: its refresh token is revoked where OpenAI allows it, again after a network or
/// server failure, and the user hears when that was not confirmed.
#[tauri::command]
pub async fn remove_connection(
    id: String,
    app: AppHandle,
    secrets: State<'_, SecretStore>,
) -> Result<Removed, ConnectionError> {
    let forgotten = id.clone();
    let (settings, account) =
        change_connections(&app, move |app| forget_connection(&forgotten, app)).await?;
    if !account {
        // The connection is gone either way: a key that stays sealed in the file is not an
        // error for the user, and the settings they get back are the saved ones.
        if let Err(error) = secrets.delete(&id) {
            log_line!("cannot delete the key of a removed connection: {:?}", error.kind());
        }
        return Ok(Removed { settings, sign_out_unconfirmed: false });
    }
    let vault = (*secrets).clone();
    let revocation = tauri::async_runtime::spawn_blocking(move || {
        match chatgpt::sign_out(&vault, &id) {
            Ok(Some(credentials)) => Some(chatgpt::revoke_retrying(
                &runtimes::http::agent(),
                &credentials.client_id,
                &credentials.refresh_token,
            )),
            Ok(None) => None,
            // Tokens that cannot be read cannot be revoked.
            Err(error) => Some(chatgpt::Revocation::Failed { error, again: false }),
        }
    })
    .await;
    // The outcome only, never a token.
    let confirmed = match revocation {
        Ok(Some(revocation)) => {
            log_line!("ChatGPT sign-out: {revocation}");
            revocation == chatgpt::Revocation::Revoked
        }
        // Nothing was signed in.
        Ok(None) => true,
        Err(error) => {
            log_line!("ChatGPT sign-out: {error}");
            false
        }
    };
    Ok(Removed { settings, sign_out_unconfirmed: !confirmed })
}

/// Connection `id` of `settings`.
fn find_connection<'a>(
    settings: &'a Settings,
    id: &str,
) -> Result<&'a Connection, ConnectionError> {
    settings
        .connections
        .iter()
        .find(|connection| connection.id == id)
        .ok_or(ConnectionError::NotFound)
}

/// The checks of `remove_connection` and the settings without the connection; `true` with them
/// for a ChatGPT account.
fn forget_connection(id: &str, app: &AppHandle) -> Result<(Settings, bool), ConnectionError> {
    let store = app.state::<Mutex<SettingsStore>>();
    let mut store = store.lock().expect("settings lock");
    let account = find_connection(store.settings(), id)?.provider == ProviderKind::ChatGptAccount;
    if let Some(error) = connection_needed(store.settings(), id) {
        return Err(error);
    }
    let saved = save_connections(app, &mut store, |settings| drop_connection(settings, id))?;
    Ok((saved, account))
}

/// Why connection `id` must stay: a mode uses it, or the chosen cloud engine has no other
/// connection of its service.
fn connection_needed(settings: &Settings, id: &str) -> Option<ConnectionError> {
    if let Some(mode) = modes::mode_using(&settings.modes, id) {
        return Some(ConnectionError::InUse(mode.name.clone()));
    }
    let others: Vec<Connection> =
        settings.connections.iter().filter(|connection| connection.id != id).cloned().collect();
    settings
        .model_id
        .as_deref()
        .and_then(cloud::find)
        .is_some_and(|spec| !connected(&others, spec.service))
        .then_some(ConnectionError::UsedForRecognition)
}

/// Takes connection `id` out of `settings`; "clean text with a model" of it goes back to
/// fillers, as it would work.
fn drop_connection(settings: &mut Settings, id: &str) {
    settings.connections.retain(|connection| connection.id != id);
    if matches!(&settings.clean_text, CleanText::Model { connection_id, .. } if *connection_id == id)
    {
        settings.clean_text = CleanText::Fillers;
    }
}

/// Fetches the model list again with the stored key.
#[tauri::command]
pub async fn refresh_connection_models(
    id: String,
    app: AppHandle,
    store: State<'_, Mutex<SettingsStore>>,
    secrets: State<'_, SecretStore>,
) -> Result<Settings, ConnectionError> {
    let connection = find_connection(store.lock().expect("settings lock").settings(), &id)?.clone();
    let key = if connection.provider == ProviderKind::ChatGptAccount {
        let vault = (*secrets).clone();
        let account = id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            chatgpt::access_token(&runtimes::http::agent(), &vault, &account)
        })
        .await
        .map_err(|error| ConnectionError::BadResponse(error.to_string()))??
    } else {
        secrets
            .get(&id)
            .map_err(|error| ConnectionError::Secrets(error.to_string()))?
            .unwrap_or_default()
    };
    let models = fetch_models(connection.provider, connection.base_url, key).await?;
    change_connections(&app, move |app| {
        let store = app.state::<Mutex<SettingsStore>>();
        let mut store = store.lock().expect("settings lock");
        find_connection(store.settings(), &id)?;
        save_connections(app, &mut store, |settings| {
            for connection in &mut settings.connections {
                if connection.id == id {
                    connection.models = models.clone();
                }
            }
        })
    })
    .await
}

/// Why a file transcription did not start; later problems arrive as `file` events.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum FileStartError {
    UnknownMode,
    UnknownFormat,
    /// Subtitles and JSON need word times, which processed text does not have.
    FormatNeedsTimes,
    /// A text file can only be processed by a language-model mode.
    TextNeedsLanguageModel,
    /// The chosen cloud engine gives no word times, which audio and video need.
    NoWordTimes,
    TextTooLong,
    Read(String),
    NoOutputFolder,
}

/// Text files larger than this are refused: far beyond what one model request can take.
const MAX_TEXT_BYTES: u64 = 1_000_000;

/// Transcribes an audio or video file (or processes a `.txt` file) with the mode and saves
/// the result to `output`. Progress and the outcome arrive as `file` events. `replacements`
/// applies the user's replacements to the recognized words.
// Tauri passes each argument and state separately.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn transcribe_file(
    path: String,
    output: String,
    format: String,
    mode_id: String,
    replacements: bool,
    speakers: Option<Labels>,
    store: State<'_, Mutex<SettingsStore>>,
    dictation: State<'_, Dictation>,
    meetings: State<'_, Meetings>,
) -> Result<(), FileStartError> {
    let path = PathBuf::from(path);
    let output = PathBuf::from(output);
    let settings = store.lock().expect("settings lock").settings().clone();
    let mode =
        settings.modes.iter().find(|mode| mode.id == mode_id).ok_or(FileStartError::UnknownMode)?;
    let text = path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("txt"));
    if text && !mode.processes() {
        return Err(FileStartError::TextNeedsLanguageModel);
    }
    if !text && !engines::transcribes_media(settings.model_id.as_deref()) {
        return Err(FileStartError::NoWordTimes);
    }
    let title =
        path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let format =
        ExportFormat::from_extension(&format, title).ok_or(FileStartError::UnknownFormat)?;
    let timed = !matches!(format, ExportFormat::Md { .. } | ExportFormat::Txt);
    if timed && (text || mode.processes()) {
        return Err(FileStartError::FormatNeedsTimes);
    }
    if !output.parent().is_some_and(Path::is_dir) {
        return Err(FileStartError::NoOutputFolder);
    }
    let source = if text {
        let size = fs::metadata(&path).map_err(|error| FileStartError::Read(error.to_string()))?;
        if size.len() > MAX_TEXT_BYTES {
            return Err(FileStartError::TextTooLong);
        }
        let contents =
            fs::read_to_string(&path).map_err(|error| FileStartError::Read(error.to_string()))?;
        FileSource::Text(contents)
    } else {
        FileSource::Media(path.clone())
    };
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    // Replacements fix what was recognized; a text file was not.
    let replacements = replacements && !text;
    // The voices are told apart in audio and video, once the speaker models are downloaded.
    let speakers = speakers.filter(|_| !text && meetings.speakers().ready());
    let request =
        FileRequest { source, name, output, format, mode_id, replacements, speakers, reply: None };
    let _ = dictation.inbox.send(Message::StartFile(Box::new(request)));
    Ok(())
}

#[tauri::command]
pub fn cancel_file(dictation: State<'_, Dictation>) {
    let _ = dictation.inbox.send(Message::CancelFile);
}

/// Writes the result of the last file that could not be written where the user chose now; the
/// orchestrator holds it until the next file. The error says why the file could not be written.
#[tauri::command]
pub async fn save_held_result(
    path: String,
    app: AppHandle,
    dictation: State<'_, Dictation>,
) -> Result<(), String> {
    let (reply, answer) = mpsc::channel();
    dictation
        .inbox
        .send(Message::TakeHeld { reply })
        .map_err(|_| "the app is quitting".to_string())?;
    // Written here, not on the orchestrator's thread: a slow drive holds up no hotkey.
    tauri::async_runtime::spawn_blocking(move || {
        let held = answer.recv().ok().flatten().ok_or("no result is held")?;
        let path = PathBuf::from(path);
        crate::storage::write_whole(&path, &held.text).map_err(|error| error.to_string())?;
        // Its entry in the list has a saved file now.
        if let Some(entry) = held.transcript
            && let Err(error) = app.state::<Meetings>().file_saved(entry, &path)
        {
            log_line!("a saved result's entry not updated: {error:?}");
        }
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Replacement files larger than this are refused: far beyond any list a person keeps.
const MAX_REPLACEMENTS_BYTES: u64 = 1_000_000;

/// Writes the saved replacements to `path` as JSON.
#[tauri::command]
pub fn export_replacements(
    path: String,
    store: State<'_, Mutex<SettingsStore>>,
) -> Result<(), String> {
    let text = replacements::to_file(&store.lock().expect("settings lock").settings().replacements);
    crate::storage::write_whole(Path::new(&path), &text).map_err(|error| error.to_string())
}

/// Reads the replacements of a file written by `export_replacements`. The interface adds them to
/// the list and saves it.
#[tauri::command]
pub fn import_replacements(path: String) -> Result<Vec<Replacement>, String> {
    let size = fs::metadata(&path).map_err(|error| error.to_string())?.len();
    if size > MAX_REPLACEMENTS_BYTES {
        return Err("the file is too large".into());
    }
    let text = fs::read_to_string(&path).map_err(|error| error.to_string())?;
    replacements::from_file(&text)
}

#[derive(Serialize)]
pub struct HistoryView {
    /// Shown in the settings so the user knows where the text is kept.
    path: String,
    /// Whether the file exists: there is something to show or delete.
    file_exists: bool,
    /// Newest first.
    entries: Vec<history::Entry>,
}

/// Runs work on the history on a thread for blocking work: a long history (thousands of
/// entries, `secure_delete` on clearing) would hold the main thread, and with it the hotkeys.
async fn on_history<T: Send + 'static>(
    history: &Arc<SharedHistory>,
    work: impl FnOnce(&SharedHistory) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let history = Arc::clone(history);
    tauri::async_runtime::spawn_blocking(move || work(&history))
        .await
        .unwrap_or_else(|error| Err(error.to_string()))
}

/// The entries, the newest first; at most `limit` of them (the quick panel shows a few).
#[tauri::command]
pub async fn get_history(
    limit: Option<usize>,
    history: State<'_, Arc<SharedHistory>>,
) -> Result<HistoryView, String> {
    on_history(&history, move |history| {
        Ok(HistoryView {
            path: history.path().display().to_string(),
            entries: history.list(limit).map_err(|error| error.to_string())?,
            file_exists: history.file_exists(),
        })
    })
    .await
}

#[tauri::command]
pub async fn delete_history_entry(
    id: i64,
    app: AppHandle,
    history: State<'_, Arc<SharedHistory>>,
) -> Result<(), String> {
    let result =
        on_history(&history, move |history| history.delete(id).map_err(|error| error.to_string()))
            .await;
    history_changed(&app);
    result
}

#[tauri::command]
pub async fn clear_history(
    app: AppHandle,
    history: State<'_, Arc<SharedHistory>>,
) -> Result<(), String> {
    let result =
        on_history(&history, |history| history.clear().map_err(|error| error.to_string())).await;
    history_changed(&app);
    result
}

/// Today, the last 7 days and everything: dictations, words and recording time, no text.
#[tauri::command]
pub fn get_stats(stats: State<'_, Arc<SharedStats>>) -> Result<stats::Summary, String> {
    stats.summary().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn reset_stats(app: AppHandle, stats: State<'_, Arc<SharedStats>>) -> Result<(), String> {
    let result = stats.reset().map_err(|error| error.to_string());
    let _ = app.emit(CORE_EVENT, CoreEvent::Dictation(DictationEvent::StatsChanged));
    result
}

/// Every window with a history list reads it again: the History page and the quick panel each
/// keep their own copy.
fn history_changed(app: &AppHandle) {
    let _ = app.emit(CORE_EVENT, CoreEvent::Dictation(DictationEvent::HistoryChanged));
}

/// The quick panel's page reports its height in CSS pixels after every layout change.
#[tauri::command]
pub fn fit_tray_panel(height: f64, app: AppHandle) {
    if let Some(panel) = crate::tray_panel::get(&app)
        && height.is_finite()
        && height > 0.0
    {
        panel.fit(height);
    }
}

/// Escape in the quick panel.
#[tauri::command]
pub fn hide_tray_panel(app: AppHandle) {
    if let Some(panel) = crate::tray_panel::get(&app) {
        panel.dismiss();
    }
}

/// "Open ahem" in the quick panel: the full window.
#[tauri::command]
pub fn open_main_window(app: AppHandle) {
    tray::show_main(&app);
}

/// "Quit" in the quick panel, like the tray menu.
#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}

/// Deletes the history file itself; the history stays on and starts a new file.
#[tauri::command]
pub async fn delete_history_file(
    app: AppHandle,
    history: State<'_, Arc<SharedHistory>>,
) -> Result<(), String> {
    let result =
        on_history(&history, |history| history.remove_file().map_err(|error| error.to_string()))
            .await;
    history_changed(&app);
    result
}

/// A new connection's id.
fn connection_id() -> Result<String, ConnectionError> {
    let id = random_hex().map_err(|error| ConnectionError::Secrets(error.to_string()))?;
    Ok(format!("connection-{id}"))
}

#[cfg(test)]
mod tests {
    use crate::domain::modes::{Connection, Mode, ModeAction, ProviderKind};
    use crate::domain::services::GROQ_BASE_URL;

    fn groq(id: &str) -> Connection {
        Connection {
            id: id.into(),
            name: "Groq".into(),
            provider: ProviderKind::OpenAiCompatible,
            base_url: Some(GROQ_BASE_URL.into()),
            key_hint: None,
            added_at_ms: 0,
            models: vec!["llama".into()],
        }
    }

    /// A connection a mode uses, or the only one of the chosen cloud engine's service, stays; a
    /// second one of that service may go.
    #[test]
    fn a_connection_in_use_stays() {
        use super::{ConnectionError, connection_needed};
        let mut settings =
            super::Settings { connections: vec![groq("g1")], ..super::Settings::default() };
        assert!(connection_needed(&settings, "g1").is_none());
        settings.modes.push(Mode {
            id: "polish".into(),
            name: "Polish".into(),
            action: ModeAction::Process {
                connection_id: "g1".into(),
                model: "llama".into(),
                instruction: "Polish it".into(),
                output_language: None,
                context: Default::default(),
            },
            hotkey: None,
            selection_hotkey: None,
        });
        assert!(matches!(
            connection_needed(&settings, "g1"),
            Some(ConnectionError::InUse(name)) if name == "Polish"
        ));
        settings.modes.clear();
        settings.model_id = Some("groq-whisper-large-v3".into());
        assert!(matches!(
            connection_needed(&settings, "g1"),
            Some(ConnectionError::UsedForRecognition)
        ));
        settings.connections.push(groq("g2"));
        assert!(connection_needed(&settings, "g1").is_none(), "another Groq connection stays");
    }

    /// Removed, a connection takes "clean text with a model" of it back to fillers; another's
    /// stays.
    #[test]
    fn a_removed_connection_takes_its_text_cleaning_along() {
        use super::{CleanText, drop_connection};
        let mut settings = super::Settings {
            connections: vec![groq("g1"), groq("g2")],
            clean_text: CleanText::Model { connection_id: "g1".into(), model: "llama".into() },
            ..super::Settings::default()
        };
        drop_connection(&mut settings, "g2");
        assert!(matches!(settings.clean_text, CleanText::Model { .. }));
        drop_connection(&mut settings, "g1");
        assert_eq!(settings.clean_text, CleanText::Fillers);
        assert!(settings.connections.is_empty());
    }

    /// A model being deleted, an unknown one and the cloud engine of a service not connected
    /// cannot be chosen.
    #[test]
    fn only_a_usable_model_is_chosen() {
        use super::choosable_model;
        let none = |_: &str| false;
        assert!(choosable_model(None, &[], none));
        assert!(choosable_model(Some("whisper-large-v3"), &[], none));
        assert!(!choosable_model(Some("whisper-large-v3"), &[], |_| true), "being deleted");
        assert!(!choosable_model(Some("no-such-model"), &[], none));
        assert!(!choosable_model(Some("groq-whisper-large-v3"), &[], none));
        assert!(choosable_model(Some("groq-whisper-large-v3"), &[groq("g1")], none));
    }

    #[test]
    fn a_model_is_either_downloaded_or_deleted() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let jobs = super::Downloads::default();
        assert!(jobs.start_deleting("m"));
        assert!(jobs.deleting("m"));
        assert!(!jobs.start("m", Arc::new(AtomicBool::new(false))), "no download while deleting");
        assert!(!jobs.start_deleting("m"), "deleted once at a time");
        jobs.finish_deleting("m");
        assert!(!jobs.deleting("m"));
        assert!(jobs.start("m", Arc::new(AtomicBool::new(false))));
        assert!(!jobs.start_deleting("m"), "no deleting while downloading");
        jobs.finish("m");
        assert!(jobs.start_deleting("m"));
    }

    #[test]
    fn a_hotkey_taken_before_is_left_out_and_a_new_taken_one_refused() {
        use super::register_with;
        use std::collections::HashSet;
        use tauri_plugin_global_shortcut::Shortcut;
        let key = |text: &str| text.parse::<Shortcut>().unwrap();
        let (main, app, new) = (key("Alt+Space"), key("Alt+KeyY"), key("Alt+KeyN"));
        // Another program holds the per-app hotkey and the new one.
        let taken = [app, new];
        let mut registered = Vec::new();
        let known = HashSet::from([main, app]);
        let mut register = |shortcut: Shortcut| {
            let free = !taken.contains(&shortcut);
            if free {
                registered.push(shortcut);
            }
            free
        };
        let mut unregister = |_: Shortcut| {};
        // Taken before: the main hotkey registers all the same.
        let left_out = register_with(&[main, app], &[], &known, &mut register, &mut unregister);
        assert_eq!(left_out, Ok(vec![app]));
        // Newly chosen and taken: refused.
        let refused = register_with(&[new], &[], &known, &mut register, &mut unregister);
        assert_eq!(refused, Err(new));
        assert_eq!(registered, [main]);
    }

    /// The translation hotkey of a model that does not translate is not registered, yet it is
    /// no newly chosen one: taken, it does not refuse a save of something else.
    #[test]
    fn known_hotkeys_include_the_translation_one_of_any_model() {
        use crate::domain::modes::{HotkeyAction, english_mode};
        use crate::storage::settings::Settings;
        let mut settings =
            Settings { model_id: Some("gigaam-v3-e2e-rnnt".into()), ..Settings::default() };
        settings.modes.push(english_mode());
        settings.modes.last_mut().unwrap().hotkey = Some("F9".into());
        let has_f9 = |set: &[(String, HotkeyAction)]| set.iter().any(|(hotkey, _)| hotkey == "F9");
        assert!(!has_f9(&super::hotkey_set(&settings)), "GigaAM does not translate");
        assert!(has_f9(&super::known_hotkeys(&settings)));
    }

    /// The login entry is written again only for a release build with "start at login" on, and
    /// not when the system says it was turned off there (Windows' Task Manager).
    #[test]
    fn the_login_entry_is_written_again_only_when_on_everywhere() {
        use super::refreshes_autostart;
        assert!(refreshes_autostart(true, false, || true));
        assert!(!refreshes_autostart(true, false, || false), "turned off in the system");
        assert!(!refreshes_autostart(true, true, || panic!("not asked")), "a development build");
        assert!(!refreshes_autostart(false, false, || panic!("not asked")), "off in ahem");
    }

    /// A new key goes into the same connection, so the modes that use it go on; only an own
    /// server may go without one, and a ChatGPT account signs in again instead.
    #[test]
    fn a_new_key_keeps_the_connection() {
        use super::{ConnectionError, new_key, with_new_key};
        let old = Connection {
            id: "anthropic-1".into(),
            name: "Anthropic".into(),
            provider: ProviderKind::Anthropic,
            base_url: None,
            key_hint: Some("…old1".into()),
            added_at_ms: 1,
            models: vec!["claude-old".into()],
        };
        let renewed = with_new_key(&old, "sk-ant-new-key-1234", vec!["claude-new".into()], 9);
        assert_eq!((renewed.id.as_str(), renewed.name.as_str()), ("anthropic-1", "Anthropic"));
        assert_eq!(renewed.provider, ProviderKind::Anthropic);
        assert_ne!(renewed.key_hint, old.key_hint);
        assert_eq!(
            (renewed.added_at_ms, renewed.models.clone()),
            (9, vec!["claude-new".to_string()])
        );
        assert_eq!(new_key(ProviderKind::Anthropic, "  sk-1 ").ok().as_deref(), Some("sk-1"));
        assert!(matches!(new_key(ProviderKind::Anthropic, " "), Err(ConnectionError::MissingKey)));
        assert_eq!(new_key(ProviderKind::OpenAiCompatible, "").ok().as_deref(), Some(""));
        assert!(matches!(
            new_key(ProviderKind::ChatGptAccount, "x"),
            Err(ConnectionError::MissingKey)
        ));
    }

    /// One hotkey for two actions is refused as a taken one, which the interface says in its
    /// language; settings the interface never sends name only what is wrong.
    #[test]
    fn a_hotkey_for_two_actions_is_refused_as_taken() {
        use crate::domain::modes::english_mode;
        use crate::storage::settings::{SaveError, Settings};
        let mut settings = Settings::default();
        settings.modes.push(english_mode());
        settings.modes.last_mut().unwrap().hotkey = Some(settings.hotkey.clone());
        assert_eq!(
            super::wanted_hotkeys(&settings),
            Err(SaveError::HotkeyTaken(settings.hotkey.clone()))
        );
        settings.modes.last_mut().unwrap().hotkey = None;
        settings.main_mode_id = "gone".into();
        assert_eq!(super::wanted_hotkeys(&settings), Err(SaveError::Invalid("modes".into())));
    }

    /// Settings that name no valid set register the main hotkey alone, and the status names it
    /// when it is not registered.
    #[test]
    fn an_invalid_set_of_hotkeys_falls_back_to_the_main_one() {
        use crate::domain::modes::HotkeyAction;
        use crate::storage::settings::Settings;
        let settings = Settings { main_mode_id: "gone".into(), ..Settings::default() };
        assert!(super::wanted_hotkeys(&settings).is_err());
        assert_eq!(super::hotkey_set(&settings), [(settings.hotkey.clone(), HotkeyAction::Main)]);
    }

    /// Every registered command has a permission (`build.rs`) and the main window may call them
    /// all; the other windows name only registered ones.
    #[test]
    fn windows_may_call_the_commands_they_use() {
        use std::collections::BTreeSet;
        use std::fs;
        use std::path::Path;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let read = |name: &str| fs::read_to_string(root.join(name)).unwrap();
        let between = |text: &str, from: &str, to: &str| {
            let start = text.find(from).unwrap() + from.len();
            text[start..start + text[start..].find(to).unwrap()].to_string()
        };
        let lib = read("src/lib.rs");
        let registered: BTreeSet<String> = between(&lib, "generate_handler![", "]")
            .split([',', ' ', '\n'])
            .filter_map(|item| item.rsplit_once("::").map(|(_, name)| name.to_string()))
            .collect();
        let manifest: BTreeSet<String> = between(&read("build.rs"), "&[\n", "];")
            .split(',')
            .map(|item| item.trim().trim_matches('"').to_string())
            .filter(|name| !name.is_empty())
            .collect();
        assert_eq!(manifest, registered, "build.rs lists the commands of lib.rs");
        let allowed = |file: &str| -> BTreeSet<String> {
            let capability: serde_json::Value =
                serde_json::from_str(&read(&format!("capabilities/{file}.json"))).unwrap();
            capability["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|permission| permission.as_str()?.strip_prefix("allow-"))
                .map(|name| name.replace('-', "_"))
                .collect()
        };
        assert_eq!(allowed("default"), registered);
        // The quick panel and the indicator may call what their pages call:
        // `src/capabilities.test.ts` follows the pages to their commands.
        for page in ["tray", "indicator"] {
            assert!(allowed(page).is_subset(&registered), "{page}");
        }
    }

    #[test]
    fn download_progress_goes_out_first_then_at_most_every_interval_and_at_the_end() {
        let mut seen = Vec::new();
        let mut progress = super::throttled(|done, total| seen.push((done, total)));
        progress(1, 10);
        progress(2, 10);
        progress(10, 10);
        drop(progress);
        assert_eq!(seen, [(1, 10), (10, 10)]);
    }

    #[test]
    fn deleting_the_dictation_model_moves_to_another_downloaded_one() {
        use super::replacement_model;
        let ready = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        assert_eq!(replacement_model("a", &ready(&["a", "b", "c"])), Some("b".into()));
        assert_eq!(replacement_model("b", &ready(&["a", "b"])), Some("a".into()));
        assert_eq!(replacement_model("a", &ready(&["a"])), None);
        assert_eq!(replacement_model("a", &ready(&[])), None);
    }

    #[test]
    fn deletion_is_retried_while_the_files_are_busy() {
        use super::delete_retrying;
        use std::io::{Error, ErrorKind};
        use std::time::Duration;
        let busy = || Error::new(ErrorKind::PermissionDenied, "busy");

        let mut calls = 0;
        let result = delete_retrying(
            || {
                calls += 1;
                if calls < 3 { Err(busy()) } else { Ok(()) }
            },
            5,
            Duration::ZERO,
        );
        assert!(result.is_ok());
        assert_eq!(calls, 3);

        let mut calls = 0;
        let result = delete_retrying(
            || {
                calls += 1;
                Err(busy())
            },
            4,
            Duration::ZERO,
        );
        assert_eq!(result.unwrap_err().kind(), ErrorKind::PermissionDenied);
        assert_eq!(calls, 4);
    }

    #[test]
    fn data_folders_open_and_the_history_is_selected() {
        use super::{DataFolder, Show, show_target};
        use std::path::Path;
        let models = Path::new("/data/com.useahem.ahem/models");
        let all = Path::new("/data/com.useahem.ahem");
        let history = Path::new("/data/com.useahem.ahem/history.sqlite3");
        assert_eq!(show_target(DataFolder::Models, models, true), Show::Open(models.into()));
        assert_eq!(show_target(DataFolder::All, all, true), Show::Open(all.into()));
        assert_eq!(show_target(DataFolder::History, history, true), Show::Reveal(history.into()));
        assert_eq!(show_target(DataFolder::History, history, false), Show::Open(all.into()));
    }

    #[test]
    fn system_locales_are_not_blank() {
        assert!(super::system_locales().iter().all(|locale| !locale.trim().is_empty()));
    }

    #[test]
    fn a_chatgpt_sign_in_cancelled_or_replaced_saves_nothing() {
        let sign_in = super::ChatGptSignIn::default();
        let first = sign_in.start();
        let second = sign_in.start();
        assert!(sign_in.finish(&first), "a newer sign-in took its place");
        assert!(!sign_in.finish(&second));
        // Cancelled after the browser answered, while the code is exchanged.
        let third = sign_in.start();
        sign_in.cancel();
        assert!(sign_in.finish(&third));
    }

    #[test]
    fn the_engine_state_follows_the_chosen_model() {
        use super::chosen_engine_available;
        let present = std::env::current_exe().unwrap();
        let missing = present.with_file_name("no-such-engine");
        let whisper_only = |id| chosen_engine_available(Some(id), &present, &missing);
        assert!(whisper_only("whisper-large-v3"));
        assert!(!whisper_only("qwen3-asr-0_6b"), "a llama.cpp model needs llama-server");
        assert!(chosen_engine_available(Some("gigaam-v3-e2e-rnnt"), &missing, &missing));
        assert!(chosen_engine_available(Some("qwen3-asr-0_6b"), &missing, &present));
        assert!(!chosen_engine_available(Some("whisper-medium"), &missing, &present));
        assert!(chosen_engine_available(Some("deepgram-nova-3"), &missing, &missing));
        assert!(chosen_engine_available(None, &missing, &missing));
    }
}

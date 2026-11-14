//! User settings: a JSON file with `schema_version`, upgraded by step-by-step migrations.
//!
//! A file that cannot be read, or was written by a newer version, is never overwritten:
//! the app runs with defaults and saving is disabled until the file is fixed.

use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::domain::app_rules::AppRule;
use crate::domain::clean_text::{self, CleanText};
use crate::domain::engine::UnloadPolicy;
use crate::domain::gesture::RecordingMode;
use crate::domain::modes::{self, Connection, HotkeyAction, Mode, ModeAction, ModesError};
use crate::domain::replacements::Replacement;
use crate::engines::languages;
use crate::hotkeys;

pub const SCHEMA_VERSION: u32 = 2;

/// Migration `i` upgrades a file from version `i + 1` to `i + 2`.
type Migration = fn(&mut Map<String, Value>);

const MIGRATIONS: &[Migration] = &[english_on_request];

/// Version 2: plain dictation runs on the main hotkey only, and translation to English is there
/// only when the user turned it on. Version 1 always had it, so it stays only if it has a hotkey
/// or is the main mode.
fn english_on_request(map: &mut Map<String, Value>) {
    let main = map.get("main_mode_id").and_then(Value::as_str).unwrap_or(modes::PLAIN).to_owned();
    let Some(Value::Array(list)) = map.get_mut("modes") else { return };
    list.retain(|mode| {
        mode["id"] != modes::ENGLISH || !mode["hotkey"].is_null() || main == modes::ENGLISH
    });
    for mode in list.iter_mut().filter(|mode| mode["id"] == modes::PLAIN) {
        if let Some(fields) = mode.as_object_mut() {
            fields.insert("hotkey".into(), Value::Null);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiLanguage {
    /// Follow the OS language.
    #[default]
    System,
    En,
    Ru,
}

/// The window's color scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the OS light or dark mode.
    #[default]
    System,
    Light,
    Dark,
}

/// Fields added later get their default when an older file lacks them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub ui_language: UiLanguage,
    /// Canonical form from `hotkeys::canonical`.
    pub hotkey: String,
    /// `Auto` for new settings; a file saved before the choice existed keeps start and stop.
    #[serde(default = "RecordingMode::saved_before_the_choice")]
    pub recording_mode: RecordingMode,
    /// `None` records from the system default microphone.
    pub microphone_id: Option<String>,
    /// The dictation model; `None` until one is chosen.
    pub model_id: Option<String>,
    pub unload_policy: UnloadPolicy,
    pub mute_while_recording: bool,
    /// The capsule at the bottom of the screen while dictating and for its notices; on unless
    /// the user turns it off (the tray icon and the window still tell the state).
    pub show_indicator: bool,
    /// `None` lets Whisper detect the language; otherwise a code from `engines::languages`.
    pub spoken_language: Option<String>,
    /// Terms, names and abbreviations separated by commas or new lines: a hint for the engine.
    pub dictionary: String,
    /// Applied to the recognized text of every dictation, and of files unless turned off there.
    pub replacements: Vec<Replacement>,
    /// How plain dictation cleans the recognized text.
    pub clean_text: CleanText,
    /// The words "clean text" removes, separated by commas or new lines.
    pub fillers: String,
    /// Language-model services; their keys are in the secret store.
    pub connections: Vec<Connection>,
    /// Built-in modes first, then the user's modes.
    pub modes: Vec<Mode>,
    /// The mode of the main hotkey.
    pub main_mode_id: String,
    /// The modes the "by application" hotkey takes in particular programs; only the user adds
    /// them.
    pub app_rules: Vec<AppRule>,
    /// Dictates with the mode of the program in focus (`app_rules`); `None` until the user sets
    /// one. Canonical form.
    pub app_hotkey: Option<String>,
    /// Finished dictations go to the local history only while this is on.
    pub history_enabled: bool,
    /// How many dictations the history keeps, 1 to [`HISTORY_LIMIT_MAX`]; `None` keeps all.
    pub history_limit: Option<u32>,
    /// Start with the system, hidden in the tray.
    pub launch_at_login: bool,
    /// The first-run steps were dismissed.
    pub onboarding_done: bool,
    pub theme: Theme,
    /// Transcribing a file tells its voices apart; off until the user turns it on.
    pub files_by_speaker: bool,
    /// New versions download in the background and install at a restart (or at once, when the
    /// user presses the button); off until the user turns it on.
    pub auto_update: bool,
    /// New versions as betas, before everyone gets them.
    pub beta_updates: bool,
}

/// The longest dictionary, in characters.
pub const MAX_DICTIONARY_CHARS: usize = 2000;
/// The longest wait before an idle model is unloaded: a day.
pub const MAX_IDLE_MINUTES: u32 = 1440;

/// The largest number of dictations the user can type for the history; more is "all".
pub const HISTORY_LIMIT_MAX: u32 = 10_000;

/// Whether the history can keep this many dictations: any number from 1 to
/// [`HISTORY_LIMIT_MAX`], or all of them.
pub fn history_limit_valid(limit: Option<u32>) -> bool {
    limit.is_none_or(|limit| (1..=HISTORY_LIMIT_MAX).contains(&limit))
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ui_language: UiLanguage::default(),
            hotkey: hotkeys::canonical(hotkeys::DEFAULT_HOTKEY).expect("default hotkey is valid"),
            recording_mode: RecordingMode::default(),
            microphone_id: None,
            model_id: None,
            unload_policy: UnloadPolicy::default(),
            // Muting hides a headset switching modes when its microphone opens.
            mute_while_recording: true,
            show_indicator: true,
            spoken_language: None,
            dictionary: String::new(),
            replacements: Vec::new(),
            clean_text: CleanText::default(),
            fillers: clean_text::DEFAULT_FILLERS.into(),
            connections: Vec::new(),
            modes: modes::builtin_modes(),
            main_mode_id: modes::PLAIN.into(),
            app_rules: Vec::new(),
            app_hotkey: None,
            history_enabled: false,
            history_limit: Some(10),
            launch_at_login: false,
            onboarding_done: false,
            theme: Theme::default(),
            files_by_speaker: false,
            auto_update: false,
            beta_updates: false,
        }
    }
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Invalid(String),
    NewerVersion { found: u32 },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "cannot read settings: {error}"),
            Self::Invalid(reason) => write!(f, "invalid settings file: {reason}"),
            Self::NewerVersion { found } => write!(
                f,
                "settings file has schema version {found}, this app supports up to {SCHEMA_VERSION}"
            ),
        }
    }
}

impl std::error::Error for LoadError {}

/// A missing file means default settings.
pub fn load(path: &Path) -> Result<Settings, LoadError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(error) => return Err(LoadError::Io(error)),
    };
    let value =
        serde_json::from_str(&text).map_err(|error| LoadError::Invalid(error.to_string()))?;
    let map = migrate(value, SCHEMA_VERSION, MIGRATIONS)?;
    let mut settings: Settings = serde_json::from_value(Value::Object(map))
        .map_err(|error| LoadError::Invalid(error.to_string()))?;
    repair(&mut settings);
    Ok(settings)
}

impl Settings {
    /// Every hotkey these settings name with its action, after checking the modes; the
    /// translation hotkey only when `translates`.
    pub fn hotkey_actions(
        &self,
        translates: bool,
    ) -> Result<Vec<(String, HotkeyAction)>, ModesError> {
        modes::hotkey_actions(
            &self.modes,
            &self.main_mode_id,
            &self.hotkey,
            self.app_hotkey.as_deref(),
            &self.connections,
            translates,
        )
    }

    /// What a save from the interface must hold: the hotkeys in their stored form (made so
    /// here), and every value in its range. Names the first field that is not.
    pub fn check(&mut self) -> Result<(), SaveError> {
        let canonical = |text: &str| {
            hotkeys::canonical(text).map_err(|error| SaveError::InvalidHotkey(error.0))
        };
        self.hotkey = canonical(&self.hotkey)?;
        if let Some(hotkey) = &mut self.app_hotkey {
            *hotkey = canonical(hotkey)?;
        }
        let invalid = |field: &str| Err(SaveError::Invalid(field.into()));
        for mode in &mut self.modes {
            for hotkey in [&mut mode.hotkey, &mut mode.selection_hotkey].into_iter().flatten() {
                *hotkey = canonical(hotkey)?;
            }
            if let ModeAction::Process { output_language: Some(code), .. } = &mode.action
                && !known_language(code)
            {
                return invalid("output_language");
            }
        }
        if let UnloadPolicy::AfterIdle { minutes } = self.unload_policy
            && !(1..=MAX_IDLE_MINUTES).contains(&minutes)
        {
            return invalid("unload_policy.minutes");
        }
        if self.spoken_language.as_deref().is_some_and(|code| !known_language(code)) {
            return invalid("spoken_language");
        }
        if self.dictionary.chars().count() > MAX_DICTIONARY_CHARS {
            return invalid("dictionary");
        }
        if !history_limit_valid(self.history_limit) {
            return invalid("history_limit");
        }
        Ok(())
    }
}

/// Brings values the app itself would never save (a file edited by hand, a model a later
/// version no longer has) back into their ranges, each on its own, so one bad value neither
/// misbehaves nor makes every later save fail as invalid. In memory only: the file changes
/// with the next save.
fn repair(settings: &mut Settings) {
    let canonical = |hotkey: &str| hotkeys::canonical(hotkey).ok();
    settings.hotkey = canonical(&settings.hotkey).unwrap_or(Settings::default().hotkey);
    settings.app_hotkey = settings.app_hotkey.as_deref().and_then(canonical);
    for mode in &mut settings.modes {
        mode.hotkey = mode.hotkey.as_deref().and_then(canonical);
        mode.selection_hotkey = mode.selection_hotkey.as_deref().and_then(canonical);
        if let ModeAction::Process { output_language, .. } = &mut mode.action
            && output_language.as_deref().is_some_and(|code| !known_language(code))
        {
            *output_language = None;
        }
    }
    if settings.spoken_language.as_deref().is_some_and(|code| !known_language(code)) {
        settings.spoken_language = None;
    }
    match settings.history_limit {
        // More than can be chosen: all of them, never fewer than the file asks for.
        Some(limit) if limit > HISTORY_LIMIT_MAX => settings.history_limit = None,
        limit if !history_limit_valid(limit) => {
            settings.history_limit = Settings::default().history_limit;
        }
        _ => {}
    }
    if let UnloadPolicy::AfterIdle { minutes } = &mut settings.unload_policy {
        *minutes = (*minutes).clamp(1, MAX_IDLE_MINUTES);
    }
    if settings.dictionary.chars().count() > MAX_DICTIONARY_CHARS {
        settings.dictionary = settings.dictionary.chars().take(MAX_DICTIONARY_CHARS).collect();
    }
    // A model neither the catalog nor a cloud service knows: none, as before the first choice.
    if settings.model_id.as_deref().is_some_and(|id| {
        super::catalog::find(id).is_none() && crate::engines::cloud::find(id).is_none()
    }) {
        settings.model_id = None;
    }
    // Modes no save would pass, so that every save is refused: each problem is mended in turn.
    for _ in 0..256 {
        let checked = settings.hotkey_actions(true);
        match checked {
            Ok(_) => break,
            Err(error) => mend_modes(settings, error),
        }
    }
}

/// Mends one problem of the modes: a missing built-in comes back, a changed one gets its action
/// back, a repeated mode or one that cannot work (no connection, model or instruction) goes, a
/// mode without a name is named by its id, and a hotkey used twice stays with its first action.
fn mend_modes(settings: &mut Settings, error: ModesError) {
    let builtin = |id: &str| {
        modes::builtin_modes().into_iter().chain([modes::english_mode()]).find(|mode| mode.id == id)
    };
    let modes = &mut settings.modes;
    match error {
        ModesError::DuplicateId(id) => {
            if let Some(last) = modes.iter().rposition(|mode| mode.id == id) {
                modes.remove(last);
            }
        }
        ModesError::MissingBuiltin(id) => {
            modes.insert(0, builtin(&id).unwrap_or_else(modes::plain_mode))
        }
        ModesError::BuiltinChanged(id) => {
            if let (Some(mode), Some(builtin)) =
                (modes.iter_mut().find(|mode| mode.id == id), builtin(&id))
            {
                mode.action = builtin.action;
            }
        }
        ModesError::UnknownMainMode(_) => settings.main_mode_id = modes::PLAIN.into(),
        ModesError::EmptyName(id) => {
            if let Some(mode) = modes.iter_mut().find(|mode| mode.id == id) {
                mode.name = id;
            }
        }
        ModesError::UnknownConnection(id)
        | ModesError::NoLanguageModels(id)
        | ModesError::EmptyInstruction(id)
        | ModesError::EmptyModel(id) => modes.retain(|mode| mode.id != id),
        ModesError::SelectionWithoutProcessing(id) => {
            if let Some(mode) = modes.iter_mut().find(|mode| mode.id == id) {
                mode.selection_hotkey = None;
            }
        }
        ModesError::HotkeyClash(hotkey) => {
            let mut taken = settings.hotkey == hotkey;
            let slots = std::iter::once(&mut settings.app_hotkey).chain(
                modes.iter_mut().flat_map(|mode| [&mut mode.hotkey, &mut mode.selection_hotkey]),
            );
            for slot in slots {
                if slot.as_deref() == Some(hotkey.as_str()) {
                    if taken {
                        *slot = None;
                    }
                    taken = true;
                }
            }
        }
    }
}

fn known_language(code: &str) -> bool {
    languages::code_for(code) == Some(code)
}

fn migrate(
    value: Value,
    current: u32,
    migrations: &[Migration],
) -> Result<Map<String, Value>, LoadError> {
    let Value::Object(mut map) = value else {
        return Err(LoadError::Invalid("not a JSON object".into()));
    };
    let version = map
        .remove("schema_version")
        .and_then(|version| version.as_u64())
        .and_then(|version| u32::try_from(version).ok())
        .filter(|version| *version >= 1)
        .ok_or_else(|| LoadError::Invalid("missing or bad schema_version".into()))?;
    if version > current {
        return Err(LoadError::NewerVersion { found: version });
    }
    let steps = migrations
        .get((version - 1) as usize..(current - 1) as usize)
        .ok_or_else(|| LoadError::Invalid(format!("no migration from version {version}")))?;
    for migration in steps {
        migration(&mut map);
    }
    Ok(map)
}

/// Writes through a temporary file so a crash never leaves a half-written file.
pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    #[derive(Serialize)]
    struct SettingsFile<'a> {
        schema_version: u32,
        #[serde(flatten)]
        settings: &'a Settings,
    }

    let file = SettingsFile { schema_version: SCHEMA_VERSION, settings };
    let mut text = serde_json::to_string_pretty(&file).map_err(io::Error::other)?;
    text.push('\n');
    super::write_json(path, &text)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum SaveError {
    /// The file could not be loaded at startup and is kept untouched.
    ReadOnly,
    Io(String),
    InvalidHotkey(String),
    /// Another program, the OS or another action of these settings uses the hotkey.
    HotkeyTaken(String),
    /// A value outside its allowed range; names the field.
    Invalid(String),
}

#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    settings: Settings,
    load_error: Option<String>,
}

impl SettingsStore {
    pub fn open(path: PathBuf) -> Self {
        let (settings, load_error) = match load(&path) {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::default(), Some(error.to_string())),
        };
        Self { path, settings, load_error }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Why the file was not loaded; saving is disabled while this is set.
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn update(&mut self, settings: Settings) -> Result<&Settings, SaveError> {
        if self.load_error.is_some() {
            return Err(SaveError::ReadOnly);
        }
        save(&self.path, &settings).map_err(|error| SaveError::Io(error.to_string()))?;
        self.settings = settings;
        Ok(&self.settings)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_save_holds_the_stored_hotkeys_and_values_in_range() {
        let mut settings = Settings { hotkey: "Ctrl+Alt+F9".into(), ..Settings::default() };
        assert_eq!(settings.check(), Ok(()));
        assert_eq!(settings.hotkey, hotkeys::canonical("Ctrl+Alt+F9").unwrap(), "stored form");
        let refused = |change: fn(&mut Settings)| {
            let mut settings = Settings::default();
            change(&mut settings);
            settings.check()
        };
        assert!(matches!(
            refused(|s| s.hotkey = "nothing".into()),
            Err(SaveError::InvalidHotkey(_))
        ));
        let invalid = |field: &str| Err(SaveError::Invalid(field.into()));
        assert_eq!(refused(|s| s.history_limit = Some(0)), invalid("history_limit"));
        assert_eq!(refused(|s| s.spoken_language = Some("xx".into())), invalid("spoken_language"));
        assert_eq!(
            refused(|s| s.unload_policy = UnloadPolicy::AfterIdle { minutes: 0 }),
            invalid("unload_policy.minutes")
        );
        assert_eq!(
            refused(|s| s.dictionary = "a".repeat(MAX_DICTIONARY_CHARS + 1)),
            invalid("dictionary")
        );
    }

    #[test]
    fn the_history_keeps_any_typed_number() {
        for limit in [None, Some(1), Some(3), Some(10), Some(16), Some(HISTORY_LIMIT_MAX)] {
            assert!(history_limit_valid(limit), "{limit:?}");
        }
        assert!(!history_limit_valid(Some(0)));
        assert!(!history_limit_valid(Some(HISTORY_LIMIT_MAX + 1)));
    }

    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::json;

    use super::*;

    /// A fresh directory inside the project's target folder, never the user's data.
    fn test_dir() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("test-artifacts").join(
            format!("settings-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)),
        );
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn ru() -> Settings {
        Settings { ui_language: UiLanguage::Ru, ..Settings::default() }
    }

    #[test]
    fn missing_file_gives_defaults() {
        let store = SettingsStore::open(test_dir().join("settings.json"));
        assert_eq!(store.settings(), &Settings::default());
        assert_eq!(store.load_error(), None);
    }

    #[test]
    fn saved_settings_load_back() {
        let path = test_dir().join("nested").join("settings.json");
        let mut store = SettingsStore::open(path.clone());
        store.update(ru()).expect("saved");
        assert_eq!(load(&path).expect("loaded"), ru());
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["schema_version"], json!(SCHEMA_VERSION));
        assert_eq!(written["ui_language"], json!("ru"));
        assert!(!path.with_extension("json.ahem-part").exists());
    }

    #[test]
    fn newer_file_is_kept_untouched() {
        let path = test_dir().join("settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let newer = r#"{ "schema_version": 99, "ui_language": "ru" }"#;
        fs::write(&path, newer).unwrap();
        assert!(matches!(load(&path), Err(LoadError::NewerVersion { found: 99 })));
        let mut store = SettingsStore::open(path.clone());
        assert_eq!(store.settings(), &Settings::default());
        assert_eq!(store.update(ru()), Err(SaveError::ReadOnly));
        assert_eq!(fs::read_to_string(&path).unwrap(), newer);
    }

    #[test]
    fn broken_file_is_kept_untouched() {
        let path = test_dir().join("settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();
        let mut store = SettingsStore::open(path.clone());
        assert!(store.load_error().is_some());
        assert_eq!(store.update(ru()), Err(SaveError::ReadOnly));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn schema_version_is_required() {
        assert!(matches!(
            migrate(json!({ "ui_language": "ru" }), 1, &[]),
            Err(LoadError::Invalid(_))
        ));
        assert!(matches!(
            migrate(json!({ "schema_version": 0 }), 1, &[]),
            Err(LoadError::Invalid(_))
        ));
        assert!(matches!(migrate(json!([1]), 1, &[]), Err(LoadError::Invalid(_))));
    }

    #[test]
    fn migrations_run_from_the_file_version() {
        fn rename_language(map: &mut Map<String, Value>) {
            if let Some(value) = map.remove("language") {
                map.insert("ui_language".into(), value);
            }
        }
        fn add_flag(map: &mut Map<String, Value>) {
            map.insert("flag".into(), json!(true));
        }
        let migrations: &[Migration] = &[rename_language, add_flag];

        let v1 = json!({ "schema_version": 1, "language": "ru" });
        let map = migrate(v1, 3, migrations).unwrap();
        assert_eq!(map.get("ui_language"), Some(&json!("ru")));
        assert_eq!(map.get("flag"), Some(&json!(true)));

        let v2 = json!({ "schema_version": 2, "ui_language": "en" });
        let map = migrate(v2, 3, migrations).unwrap();
        assert_eq!(map.get("ui_language"), Some(&json!("en")));
        assert_eq!(map.get("flag"), Some(&json!(true)));

        let v3 = json!({ "schema_version": 3, "ui_language": "en" });
        assert_eq!(migrate(v3, 3, migrations).unwrap().get("flag"), None);
    }

    #[test]
    fn default_json_matches_the_frontend_type() {
        assert_eq!(
            serde_json::to_value(Settings::default()).unwrap(),
            json!({
                "ui_language": "system",
                "hotkey": hotkeys::canonical(hotkeys::DEFAULT_HOTKEY).unwrap(),
                "recording_mode": "auto",
                "microphone_id": null,
                "model_id": null,
                "unload_policy": { "mode": "immediate" },
                "mute_while_recording": true,
                "show_indicator": true,
                "spoken_language": null,
                "dictionary": "",
                "replacements": [],
                "clean_text": { "level": "fillers" },
                "fillers": clean_text::DEFAULT_FILLERS,
                "connections": [],
                "modes": [
                    { "id": "plain", "name": "", "action": { "kind": "plain" }, "hotkey": null, "selection_hotkey": null }
                ],
                "main_mode_id": "plain",
                "app_rules": [],
                "app_hotkey": null,
                "history_enabled": false,
                "history_limit": 10,
                "launch_at_login": false,
                "onboarding_done": false,
                "theme": "system",
                "files_by_speaker": false,
                "auto_update": false,
                "beta_updates": false
            })
        );
        let idle = UnloadPolicy::AfterIdle { minutes: 20 };
        assert_eq!(
            serde_json::to_value(idle).unwrap(),
            json!({ "mode": "after_idle", "minutes": 20 })
        );
    }

    /// A fresh install: nothing updates or starts by itself, the system sound is muted while
    /// recording and the indicator shows.
    #[test]
    fn a_fresh_install_starts_with_these_switches() {
        let fresh: Settings = serde_json::from_value(json!({})).unwrap();
        assert!(!fresh.auto_update);
        assert!(!fresh.beta_updates);
        assert!(!fresh.launch_at_login);
        assert!(fresh.mute_while_recording);
        assert!(fresh.show_indicator);
    }

    #[test]
    fn theme_is_read_by_name() {
        let dark: Settings = serde_json::from_value(json!({ "theme": "dark" })).unwrap();
        assert_eq!(dark.theme, Theme::Dark);
        assert!(serde_json::from_value::<Settings>(json!({ "theme": "blue" })).is_err());
    }

    #[test]
    fn version_1_keeps_english_only_when_used_and_plain_loses_its_hotkey() {
        let mode = |id: &str, kind: &str, hotkey: Option<&str>| json!({ "id": id, "name": "", "action": { "kind": kind }, "hotkey": hotkey, "selection_hotkey": null });
        let file = |english_hotkey: Option<&str>, main: &str| {
            json!({
                "schema_version": 1,
                "modes": [mode("plain", "plain", Some("F8")), mode("english", "translate_to_english", english_hotkey)],
                "main_mode_id": main,
            })
        };
        let ids = |map: &Map<String, Value>| {
            map["modes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|mode| mode["id"].clone())
                .collect::<Vec<_>>()
        };

        let unused = migrate(file(None, "plain"), 2, MIGRATIONS).unwrap();
        assert_eq!(ids(&unused), vec![json!("plain")]);
        assert_eq!(unused["modes"][0]["hotkey"], Value::Null);

        let with_hotkey = migrate(file(Some("F9"), "plain"), 2, MIGRATIONS).unwrap();
        assert_eq!(ids(&with_hotkey), vec![json!("plain"), json!("english")]);
        assert_eq!(with_hotkey["modes"][1]["hotkey"], json!("F9"));

        let main = migrate(file(None, "english"), 2, MIGRATIONS).unwrap();
        assert_eq!(ids(&main), vec![json!("plain"), json!("english")]);
    }

    #[test]
    fn older_file_gets_defaults_for_new_fields() {
        let path = test_dir().join("settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{ "schema_version": 1, "ui_language": "ru" }"#).unwrap();
        // Saved before the recording mode could be chosen: it recorded with start and stop.
        assert_eq!(
            load(&path).unwrap(),
            Settings { recording_mode: RecordingMode::Toggle, ..ru() }
        );
        // Saved before the indicator could be turned off: it stays on.
        assert!(load(&path).unwrap().show_indicator);
    }

    #[test]
    fn modes_edited_by_hand_are_mended_so_saves_go_on() {
        let mut settings = Settings::default();
        let custom = Mode {
            id: "mine".into(),
            name: String::new(),
            action: ModeAction::Plain,
            hotkey: Some(settings.hotkey.clone()),
            selection_hotkey: Some("F7".into()),
        };
        let gone = Mode {
            id: "gone".into(),
            name: "Gone".into(),
            action: ModeAction::Process {
                connection_id: "nowhere".into(),
                model: "m".into(),
                instruction: "Do.".into(),
                output_language: None,
                context: Default::default(),
            },
            hotkey: None,
            selection_hotkey: None,
        };
        let mut english = modes::english_mode();
        english.action = ModeAction::Plain;
        settings.modes = vec![custom.clone(), custom.clone(), gone, english];
        settings.main_mode_id = "unknown".into();
        settings.history_limit = Some(50_000);
        repair(&mut settings);
        assert!(settings.hotkey_actions(true).is_ok(), "{:?}", settings.modes);
        let ids: Vec<&str> = settings.modes.iter().map(|mode| mode.id.as_str()).collect();
        assert_eq!(ids, ["plain", "mine", "english"]);
        assert_eq!(settings.main_mode_id, modes::PLAIN);
        let mine = &settings.modes[1];
        assert_eq!(mine.name, "mine");
        assert_eq!(mine.hotkey, None, "the main hotkey keeps its own");
        assert_eq!(mine.selection_hotkey, None, "no rewriting without a language model");
        assert_eq!(settings.modes[2].action, ModeAction::TranslateToEnglish);
        assert_eq!(settings.history_limit, None, "all of them: never fewer than asked");
    }

    #[test]
    fn a_file_edited_by_hand_is_brought_back_into_range() {
        let path = test_dir().join("settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let long = "я".repeat(MAX_DICTIONARY_CHARS + 50);
        let file = json!({
            "schema_version": SCHEMA_VERSION,
            // Not the stored form, but a hotkey all the same.
            "hotkey": "Ctrl+Alt+F9",
            "app_hotkey": "not a hotkey",
            "history_limit": 0,
            "unload_policy": { "mode": "after_idle", "minutes": 0 },
            "spoken_language": "klingon",
            "model_id": "a-model-of-an-older-version",
            "dictionary": long,
        });
        fs::write(&path, file.to_string()).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.hotkey, hotkeys::canonical("Ctrl+Alt+F9").unwrap());
        assert_eq!(loaded.app_hotkey, None);
        assert_eq!(loaded.history_limit, Settings::default().history_limit);
        assert_eq!(loaded.unload_policy, UnloadPolicy::AfterIdle { minutes: 1 });
        assert_eq!(loaded.spoken_language, None);
        assert_eq!(loaded.model_id, None);
        assert_eq!(loaded.dictionary.chars().count(), MAX_DICTIONARY_CHARS);
        // Good values stay as they are.
        let mut settings = ru();
        settings.history_limit = Some(16);
        let before = settings.clone();
        repair(&mut settings);
        assert_eq!(settings, before);
    }

    /// New settings record in auto mode; a saved choice stays as it is.
    #[test]
    fn auto_is_the_recording_mode_of_new_settings_only() {
        assert_eq!(Settings::default().recording_mode, RecordingMode::Auto);
        let path = test_dir().join("settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for (saved, mode) in [("hold", RecordingMode::Hold), ("toggle", RecordingMode::Toggle)] {
            let file = format!(r#"{{ "schema_version": 2, "recording_mode": "{saved}" }}"#);
            fs::write(&path, file).unwrap();
            assert_eq!(load(&path).unwrap().recording_mode, mode);
        }
    }
}

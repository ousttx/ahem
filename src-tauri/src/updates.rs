//! Updates: the check for a new version on GitHub Releases, its download and its installation.
//! The package is signed (minisign); the app installs only one that the public key in
//! `tauri.conf.json` accepts. The plugin does the downloading, the check and the replacing of
//! the program; this module decides when, tells the windows by events, and keeps the notes of
//! the version for the first start after the installation.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::commands::CORE_EVENT;
use crate::domain::events::{CoreEvent, UpdateEvent};
use crate::storage::settings::SettingsStore;

/// The file of the latest stable version; GitHub points `latest` at the newest release that is
/// not a pre-release.
const STABLE_MANIFEST: &str = "https://github.com/ousttx/ahem/releases/latest/download/latest.json";
/// The same file for those who chose beta versions: it names the newest version of either kind.
const BETA_MANIFEST: &str = "https://github.com/ousttx/ahem/releases/download/beta/latest.json";
/// Where the changes of a version are told in full.
const RELEASES_PAGE: &str = "https://github.com/ousttx/ahem/releases/tag/v";

/// After the start, the windows have loaded and listen to events.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(12 * 60 * 60);
/// A check or a download that gets no answer is given up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Progress is told this often at most.
const PROGRESS_EVERY: Duration = Duration::from_millis(150);

/// The update found and, with automatic updates, already downloaded.
struct Found {
    update: Update,
    bytes: Option<Vec<u8>>,
}

#[derive(Default)]
pub struct Updates {
    found: Mutex<Option<Found>>,
    installing: AtomicBool,
}

/// What survives a restart: the version that ran last, and the notes of the version being
/// installed, for the card that tells it was updated.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    ran: Option<String>,
    notes_for: Option<String>,
    notes: Option<String>,
}

impl Record {
    fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) {
        let written = serde_json::to_string(self)
            .map_err(|error| error.to_string())
            .and_then(|text| std::fs::write(path, text).map_err(|error| error.to_string()));
        if let Err(error) = written {
            log_line!("cannot keep the update record: {error}");
        }
    }
}

fn manifest(beta: bool) -> &'static str {
    if beta { BETA_MANIFEST } else { STABLE_MANIFEST }
}

fn release_page(version: &str) -> String {
    format!("{RELEASES_PAGE}{version}")
}

/// A fix of a serious defect says so in the manifest: `"urgent": true`.
fn is_urgent(manifest: &Value) -> bool {
    manifest.get("urgent").and_then(Value::as_bool).unwrap_or(false)
}

/// The app runs another version than the last run did: it has just been updated.
fn was_updated(last_run: Option<&str>, running: &str) -> bool {
    last_run.is_some_and(|last| last != running)
}

fn emit(app: &AppHandle, event: UpdateEvent) {
    let _ = app.emit(CORE_EVENT, CoreEvent::Update(event));
}

/// Starts the checks: the first soon after the start, then twice a day. Not in a development
/// build, which has no release to look for.
pub fn start(app: &AppHandle, data_dir: &Path) {
    let record_path = data_dir.join("update.json");
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK_AFTER);
        tell_if_updated(&app, &record_path);
        if cfg!(debug_assertions) {
            return;
        }
        loop {
            tauri::async_runtime::block_on(check(&app));
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

/// The first start after an installation: "updated to", with the notes the manifest had.
fn tell_if_updated(app: &AppHandle, record_path: &Path) {
    let running = app.package_info().version.to_string();
    let mut record = Record::load(record_path);
    if was_updated(record.ran.as_deref(), &running) {
        let notes = (record.notes_for.as_deref() == Some(running.as_str()))
            .then(|| record.notes.take())
            .flatten()
            .filter(|notes| !notes.trim().is_empty());
        let page = Some(release_page(&running));
        emit(app, UpdateEvent::Installed { version: running.clone(), notes, page });
    }
    if record.ran.as_deref() != Some(running.as_str()) || record.notes_for.is_some() {
        record.ran = Some(running);
        record.notes_for = None;
        record.notes = None;
        record.save(record_path);
    }
}

fn updater(app: &AppHandle, beta: bool) -> Result<tauri_plugin_updater::Updater, String> {
    let url = manifest(beta).parse().map_err(|error| format!("{error}"))?;
    let handle = app.clone();
    app.updater_builder()
        .endpoints(vec![url])
        .map_err(|error| error.to_string())?
        .timeout(REQUEST_TIMEOUT)
        // The program is replaced while it runs: what it holds is let go first (the engines'
        // processes, a meeting being recorded). The plugin calls this before it exits.
        .on_before_exit(move || {
            crate::shut_down(&handle);
            handle.cleanup_before_exit();
        })
        .build()
        .map_err(|error| error.to_string())
}

async fn check(app: &AppHandle) {
    let updates: State<'_, Updates> = app.state();
    if updates.installing.load(Ordering::SeqCst) {
        return;
    }
    let (beta, automatic) = {
        let store = app.state::<Mutex<SettingsStore>>();
        let store = store.lock().expect("settings lock");
        (store.settings().beta_updates, store.settings().auto_update)
    };
    let update = match updater(app, beta) {
        Ok(updater) => updater.check().await.map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    let update = match update {
        Ok(Some(update)) => update,
        Ok(None) => {
            *updates.found.lock().expect("update lock") = None;
            return;
        }
        Err(error) => {
            log_line!("the check for a new version failed: {error}");
            return;
        }
    };
    let version = update.version.clone();
    let urgent = is_urgent(&update.raw_json);
    let kept = {
        let mut found = updates.found.lock().expect("update lock");
        // The same version again: what was downloaded stays.
        let bytes = found.take().filter(|f| f.update.version == version).and_then(|f| f.bytes);
        let ready = bytes.is_some();
        *found = Some(Found { update: update.clone(), bytes });
        ready
    };
    if kept {
        emit(app, UpdateEvent::Available { version, urgent, ready: true });
        return;
    }
    if !automatic {
        emit(app, UpdateEvent::Available { version, urgent, ready: false });
        return;
    }
    // Automatic: downloaded in the background, told once one press installs it.
    match update.download(|_, _| {}, || {}).await {
        Ok(bytes) => {
            if let Some(found) = updates.found.lock().expect("update lock").as_mut()
                && found.update.version == version
            {
                found.bytes = Some(bytes);
            }
            emit(app, UpdateEvent::Available { version, urgent, ready: true });
        }
        Err(error) => {
            log_line!("the new version did not download: {error}");
            emit(app, UpdateEvent::Available { version, urgent, ready: false });
        }
    }
}

/// "Update now": downloads the new version if that is not done, installs it and restarts.
#[tauri::command]
pub async fn install_update(app: AppHandle, updates: State<'_, Updates>) -> Result<(), String> {
    if updates.installing.swap(true, Ordering::SeqCst) {
        return Err("an update is already being installed".into());
    }
    let result = install(&app, &updates).await;
    updates.installing.store(false, Ordering::SeqCst);
    result
}

async fn install(app: &AppHandle, updates: &Updates) -> Result<(), String> {
    let (update, kept) = {
        let found = updates.found.lock().expect("update lock");
        let found = found.as_ref().ok_or("there is no new version")?;
        (found.update.clone(), found.bytes.clone())
    };
    let version = update.version.clone();
    let failed = |detail: String| {
        emit(app, UpdateEvent::Failed { version: version.clone(), detail: detail.clone() });
        detail
    };
    let bytes = match kept {
        Some(bytes) => bytes,
        None => {
            emit(app, UpdateEvent::Progress { version: version.clone(), done: 0, total: None });
            let mut done = 0u64;
            let mut told = Instant::now();
            update
                .download(
                    |chunk, total| {
                        done += chunk as u64;
                        if told.elapsed() >= PROGRESS_EVERY {
                            told = Instant::now();
                            emit(
                                app,
                                UpdateEvent::Progress { version: version.clone(), done, total },
                            );
                        }
                    },
                    || {},
                )
                .await
                .map_err(|error| failed(error.to_string()))?
        }
    };
    // The whole of it: what is left is the restart.
    emit(app, UpdateEvent::Progress { version: version.clone(), done: 1, total: Some(1) });
    if let Ok(dir) = app.path().app_local_data_dir() {
        let path = dir.join("update.json");
        let mut record = Record::load(&path);
        record.notes_for = Some(version.clone());
        record.notes = update.body.clone();
        record.save(&path);
    }
    // Windows: the installer starts and the app exits inside this call.
    update.install(&bytes).map_err(|error| failed(error.to_string()))?;
    crate::shut_down(app);
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_channel_picks_the_file() {
        assert_eq!(
            manifest(false),
            "https://github.com/ousttx/ahem/releases/latest/download/latest.json"
        );
        assert_eq!(
            manifest(true),
            "https://github.com/ousttx/ahem/releases/download/beta/latest.json"
        );
        assert!(manifest(false).starts_with("https://") && manifest(true).starts_with("https://"));
    }

    #[test]
    fn the_page_of_a_version_is_its_release() {
        assert_eq!(release_page("0.2.0"), "https://github.com/ousttx/ahem/releases/tag/v0.2.0");
    }

    #[test]
    fn only_an_explicit_true_is_urgent() {
        assert!(is_urgent(&json!({"version": "0.2.1", "urgent": true})));
        assert!(!is_urgent(&json!({"version": "0.2.1", "urgent": false})));
        assert!(!is_urgent(&json!({"version": "0.2.1", "urgent": "yes"})));
        assert!(!is_urgent(&json!({"version": "0.2.1"})));
    }

    #[test]
    fn another_version_than_the_last_run_means_updated() {
        assert!(was_updated(Some("0.1.0"), "0.2.0"));
        assert!(!was_updated(Some("0.2.0"), "0.2.0"));
        // The first start ever is not an update.
        assert!(!was_updated(None, "0.1.0"));
    }

    #[test]
    fn the_record_survives_and_a_damaged_one_is_forgotten() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/test-artifacts/update-record-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("update.json");
        assert_eq!(Record::load(&path), Record::default());
        let record = Record {
            ran: Some("0.1.0".into()),
            notes_for: Some("0.2.0".into()),
            notes: Some("- A faster start".into()),
        };
        record.save(&path);
        assert_eq!(Record::load(&path), record);
        std::fs::write(&path, "{damaged").unwrap();
        assert_eq!(Record::load(&path), Record::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The key in the configuration is a minisign public key, the only one that may sign what
    /// the app installs, and the endpoints are fixed in code, so the configuration names none.
    #[test]
    fn the_configuration_holds_the_signing_key() {
        let config: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let key = config["plugins"]["updater"]["pubkey"].as_str().expect("a public key");
        let url_safe = key.trim_end_matches('=').replace('+', "-").replace('/', "_");
        let text =
            String::from_utf8(crate::util::from_base64url(&url_safe).expect("base64")).unwrap();
        assert!(text.starts_with("untrusted comment: minisign public key"), "{text}");
        assert!(config["plugins"]["updater"].get("endpoints").is_none());
        // What the plugin reads at the start: a configuration it rejects would stop the app.
        let read: tauri_plugin_updater::Config =
            serde_json::from_value(config["plugins"]["updater"].clone())
                .expect("a valid configuration");
        assert_eq!(read.pubkey, key);
    }
}

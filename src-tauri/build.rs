/// The app's commands, as `lib.rs` registers them. Each gets an `allow-` permission, and a window
/// may call only those its capability names (`capabilities/*.json`): the quick panel and the
/// indicator only what they use.
const COMMANDS: &[&str] = &[
    "system_locales",
    "get_settings",
    "get_status",
    "save_settings",
    "set_hotkey_suspended",
    "get_last_result",
    "retry_available",
    "retry_dictation",
    "connect_chatgpt",
    "cancel_chatgpt_sign_in",
    "open_chatgpt_usage",
    "get_permissions",
    "request_permission",
    "recent_apps",
    "list_input_devices",
    "list_languages",
    "list_models",
    "download_model",
    "cancel_model_download",
    "delete_model",
    "data_folders",
    "open_data_folder",
    "list_cloud_engines",
    "add_connection",
    "remove_connection",
    "refresh_connection_models",
    "get_history",
    "delete_history_entry",
    "clear_history",
    "get_stats",
    "reset_stats",
    "delete_history_file",
    "fit_tray_panel",
    "hide_tray_panel",
    "open_main_window",
    "quit_app",
    "transcribe_file",
    "cancel_file",
    "save_held_result",
    "replace_connection_key",
    "export_replacements",
    "import_replacements",
    "get_meetings",
    "get_meeting",
    "start_meeting",
    "stop_meeting",
    "discard_meeting",
    "transcribe_meeting",
    "cancel_meeting_transcription",
    "delete_meeting",
    "rename_meeting",
    "export_meeting",
    "get_speaker_models",
    "download_speaker_models",
    "delete_speaker_models",
    "install_update",
];

fn main() {
    // macOS: the computer's sound of meetings needs Core Audio taps from macOS 14.2.
    // Core Audio is weakly linked, so the app still starts on older systems; there the tap is
    // not opened (`platform::open_system_audio` checks the version first).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,CoreAudio");
    }
    let manifest = tauri_build::AppManifest::new().commands(COMMANDS);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("failed to run tauri-build");
}

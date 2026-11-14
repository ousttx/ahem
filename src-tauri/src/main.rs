// A release build on Windows starts without a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The app started as its own sherpa-onnx worker: no window, no tray.
    if ahem_lib::engines::sherpa_process::run_worker_if_asked() {
        return;
    }
    // The guard of a local server: ends it when the app ends.
    if ahem_lib::engines::local_server::run_guard_if_asked() {
        return;
    }
    ahem_lib::run()
}

//! OS-specific code lives here and only here. Other modules call these functions; on an OS
//! without an implementation they report "unsupported", so the app builds everywhere.

use std::fmt;
use std::io;
use std::path::Path;
use std::process::Command;

mod keys;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_secrets;
#[cfg(target_os = "macos")]
mod macos_selection;
#[cfg(target_os = "macos")]
mod macos_tap;
#[cfg(not(any(windows, target_os = "macos")))]
mod unsupported;
#[cfg(windows)]
mod win32;
#[cfg(windows)]
mod win32_secrets;
#[cfg(windows)]
mod win32_selection;

#[cfg(target_os = "macos")]
use macos as os;
#[cfg(not(any(windows, target_os = "macos")))]
use unsupported as os;
#[cfg(windows)]
use win32 as os;

#[cfg(target_os = "macos")]
pub use macos_secrets::{protect, unprotect};
#[cfg(target_os = "macos")]
pub use macos_selection::{rewrite_selection, selected_text};
pub use os::{
    DefaultOutput, app_identity, clipboard_text, focus_target, insert_text, modifiers_down,
    permissions, request_permission, shortcut_modifiers_down, taskbar_is_light,
};
#[cfg(not(any(windows, target_os = "macos")))]
pub use unsupported::{protect, rewrite_selection, selected_text, unprotect};
#[cfg(windows)]
pub use win32_secrets::{protect, unprotect};
#[cfg(windows)]
pub use win32_selection::{rewrite_selection, selected_text};

/// Where a meeting records the computer's sound from: a cpal device recorded as
/// input (on Windows the default output, by loopback), and what must live as long as its stream
/// (the macOS tap).
pub struct SystemAudio {
    pub device: cpal::Device,
    _keep: Box<dyn std::any::Any>,
}

/// The main hotkey of new settings. On macOS Control+Option+Space is the system's "next input
/// source", so the default there is Option+Space.
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+Space";
#[cfg(target_os = "macos")]
pub const DEFAULT_HOTKEY: &str = "Alt+Space";

/// Whether this system can record the computer's sound (macOS 14.2+, Windows).
#[cfg(target_os = "macos")]
pub fn system_audio_supported() -> bool {
    macos_tap::taps_available()
}

#[cfg(not(target_os = "macos"))]
pub fn system_audio_supported() -> bool {
    cfg!(windows)
}

/// Opens the computer's sound for recording: on macOS a tap of every process, whatever device
/// plays it; on Windows WASAPI loopback of the default output device.
#[cfg(target_os = "macos")]
pub use macos_tap::open_system_audio;

#[cfg(windows)]
pub fn open_system_audio() -> Result<SystemAudio, String> {
    use cpal::traits::HostTrait;
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| String::from("no output device"))?;
    Ok(SystemAudio { device, _keep: Box::new(()) })
}

/// cpal records no output on ALSA.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn open_system_audio() -> Result<SystemAudio, String> {
    Err("recording the computer's sound is not supported on this system".into())
}

/// macOS places windows and finds monitors in points: the mouse comes in points times the main
/// screen's scale, and a place in physical pixels is read in the scale of the screen the window
/// is on now, not the one it goes to. Elsewhere, in physical pixels.
#[cfg(target_os = "macos")]
pub const PLACES_IN_POINTS: bool = true;
/// See the macOS one.
#[cfg(not(target_os = "macos"))]
pub const PLACES_IN_POINTS: bool = false;

/// How much larger pages are drawn. On Windows (and Linux) pages are drawn 1.2 times larger than
/// their CSS sizes. macOS draws its own interface at those CSS sizes, so there the pages are not
/// zoomed and the main window is 1.2 times smaller (`tauri.macos.conf.json`).
#[cfg(not(target_os = "macos"))]
pub const UI_ZOOM: f64 = 1.2;
#[cfg(target_os = "macos")]
pub const UI_ZOOM: f64 = 1.0;

/// WebView2 arguments of every window (other OSes ignore them): wry's defaults plus grayscale
/// text antialiasing, because ClearType's colored fringes look grainy, mostly on the dark theme.
/// Every window must get exactly these: the windows share one browser environment. The main
/// window gets them from `tauri.conf.json`.
pub const BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-lcd-text";

/// The running app was opened again from the system (macOS: Finder, Spotlight, Launchpad, the
/// Dock), which starts no second process: it shows its window, also when only its menu bar
/// icon is left.
pub fn reopened(event: &tauri::RunEvent) -> bool {
    #[cfg(target_os = "macos")]
    {
        matches!(event, tauri::RunEvent::Reopen { .. })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = event;
        false
    }
}

/// Sends this process's standard error to `file` from now on (`log_line!`, panics): a release
/// build has none to read (no console on Windows, `/dev/null` when macOS starts it).
pub fn redirect_stderr(file: std::fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn dup2(old: i32, new: i32) -> i32;
        }
        // SAFETY: both descriptors are open; standard error gets its own copy of the file's,
        // so dropping `file` closes only that one.
        if unsafe { dup2(file.as_raw_fd(), 2) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};
        // The handle stays open for the rest of the run: std writes to the one the system
        // gives for standard error at each write.
        let handle = HANDLE(file.into_raw_handle());
        // SAFETY: the handle is a file open for writing, never closed.
        unsafe { SetStdHandle(STD_ERROR_HANDLE, handle) }
            .map_err(|error| io::Error::other(error.to_string()))
    }
}

/// Free space for new files in `dir`, in bytes. On macOS as Finder counts it, with what the
/// system frees on demand (local Time Machine snapshots, iCloud files): `statvfs` leaves that
/// out and refused downloads that fit.
pub fn available_space(dir: &Path) -> io::Result<u64> {
    #[cfg(target_os = "macos")]
    if let Some(bytes) = macos::important_capacity(dir) {
        return Ok(bytes);
    }
    fs4::available_space(dir)
}

/// Where the quick panel stands at its tray icon: its left edge this many points left of the
/// icon's, or `None` for centered on the icon. macOS: as the menus of the menu bar, 5 pt left of
/// their item and 1 pt below the bar (measured 2026-11-12 on a status item menu).
#[cfg(target_os = "macos")]
pub const TRAY_PANEL_LEFT_OF_ICON: Option<f64> = Some(5.0);
#[cfg(not(target_os = "macos"))]
pub const TRAY_PANEL_LEFT_OF_ICON: Option<f64> = None;

/// The transparent margin around the quick panel in its window, in CSS pixels (`tray.css`):
/// 16 keeps it off a taskbar and the screen edges; on macOS 1, for its hairline, so it hangs
/// from the menu bar 1 pt below it.
#[cfg(target_os = "macos")]
pub const TRAY_PANEL_MARGIN: f64 = 1.0;
#[cfg(not(target_os = "macos"))]
pub const TRAY_PANEL_MARGIN: f64 = 16.0;

/// Gap between the indicator and the bottom of the work area, in logical pixels. The macOS work
/// area already ends above the Dock, so there the indicator sits just over it.
#[cfg(not(target_os = "macos"))]
pub const INDICATOR_MARGIN: f64 = 48.0;
#[cfg(target_os = "macos")]
pub const INDICATOR_MARGIN: f64 = 10.0;

/// How the tray icon is drawn (`tray_status`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrayIconSpec {
    /// Height in logical pixels; the image is this times the monitor's scale, at least
    /// `min_scale`.
    pub height: f64,
    pub min_scale: f64,
    /// Windows and Linux show a square icon; the macOS menu bar takes the glyph's own width.
    pub square: bool,
    /// macOS paints a template in the menu bar's own colors, like the system icons.
    pub template: bool,
}

/// The menu bar shows an image 18 points high (`tray-icon`); drawn at 2× for Retina.
#[cfg(target_os = "macos")]
pub const TRAY_ICON: TrayIconSpec =
    TrayIconSpec { height: 18.0, min_scale: 2.0, square: false, template: true };
/// The notification area shows 16 × 16 pixels at 100 %.
#[cfg(windows)]
pub const TRAY_ICON: TrayIconSpec =
    TrayIconSpec { height: 16.0, min_scale: 1.0, square: true, template: false };
/// Status icons of the top bar; the image is scaled to the panel.
#[cfg(not(any(windows, target_os = "macos")))]
pub const TRAY_ICON: TrayIconSpec =
    TrayIconSpec { height: 24.0, min_scale: 2.0, square: true, template: false };

/// Whether the OS lets the app use something it needs, as the OS reports it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Granted,
    Missing,
    /// This OS does not ask for it.
    NotNeeded,
}

/// The permissions dictation needs: recording and typing into other programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Permissions {
    pub microphone: Permission,
    /// macOS Accessibility: typing into other programs and reading their selected text.
    pub accessibility: Permission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    Microphone,
    Accessibility,
}

/// Windows keeps three microphone switches as "Allow" or "Deny": for the device, for apps and
/// for desktop apps. Any "Deny" blocks ahem; a switch never touched has no value and allows.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_microphone(switches: [Option<&str>; 3]) -> Permission {
    if switches.iter().flatten().any(|value| value.eq_ignore_ascii_case("deny")) {
        Permission::Missing
    } else {
        Permission::Granted
    }
}

/// Why the selected text could not be rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionError {
    NoFocus,
    FocusChanged,
    /// Password fields are never read.
    Password,
    /// The field does not expose its text (UI Automation text pattern).
    NotSupported,
    NoSelection,
    SeveralRanges,
    ReadOnly,
    /// The field or the selection changed while the new text was prepared.
    Changed,
    /// The rewrite step (a language model) failed; carries its message.
    Processing(String),
    Insert(InsertError),
    /// The OS has not allowed this app to read other programs (macOS Accessibility).
    NoPermission,
    Unsupported,
    Failed(String),
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFocus => write!(f, "no focused field"),
            Self::FocusChanged => write!(f, "focus moved to another window"),
            Self::Password => write!(f, "password fields are not read"),
            Self::NotSupported => write!(f, "this field does not expose its text"),
            Self::NoSelection => write!(f, "no text is selected"),
            Self::SeveralRanges => write!(f, "select one continuous piece of text"),
            Self::ReadOnly => write!(f, "the field is read-only"),
            Self::Changed => write!(f, "the selection changed"),
            Self::Processing(detail) => write!(f, "{detail}"),
            Self::Insert(error) => write!(f, "{error}"),
            Self::NoPermission => write!(f, "the Accessibility permission is not granted"),
            Self::Unsupported => write!(f, "selected text is not supported on this OS yet"),
            Self::Failed(detail) => write!(f, "accessibility error: {detail}"),
        }
    }
}

impl std::error::Error for SelectionError {}

/// The foreground window and its program. `window` is 0 on macOS when it is not known: no
/// Accessibility permission, a program without a focused window, or one slow to answer. A
/// window of ahem itself is always known: its quick panel, or another of its windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusTarget {
    pub window: isize,
    pub process_id: u32,
}

impl FocusTarget {
    /// The same program and window; a window that could not be read is not taken for another.
    pub fn same_place(&self, other: &FocusTarget) -> bool {
        self.process_id == other.process_id
            && (self.window == other.window || self.window == 0 || other.window == 0)
    }
}

/// How long a program may take to put the copied selection on the clipboard.
#[cfg(any(windows, target_os = "macos"))]
const COPY_WAIT: std::time::Duration = std::time::Duration::from_millis(500);
/// How long after `COPY_WAIT` a late copy (a virtual machine, a remote desktop) still gets the
/// clipboard put back.
#[cfg(any(windows, target_os = "macos"))]
const LATE_COPY: std::time::Duration = std::time::Duration::from_secs(2);
/// Counts the copies of the selection: the late-copy watcher of an earlier one stops when a
/// newer one starts, so it never puts the clipboard back over the newer copy.
#[cfg(any(windows, target_os = "macos"))]
static COPIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A rewritten selection with the whitespace the user selected around the words kept.
#[cfg(any(windows, target_os = "macos"))]
fn with_selected_edges(selected: &str, replacement: &str) -> String {
    let leading = &selected[..selected.len() - selected.trim_start().len()];
    let trailing = &selected[selected.trim_end().len()..];
    format!("{leading}{}{trailing}", replacement.trim())
}

/// The selection as the copy shortcut puts it on the clipboard, and then the clipboard as it was.
/// `changes` counts the clipboard's changes; `read` gives its text, `None` without text or when a
/// password manager marked it private; `copy` presses the shortcut; `restore` puts the earlier
/// clipboard back. Nothing copied in time: a watcher still puts the clipboard back if a late
/// copy comes, unless a newer copy has started.
#[cfg(any(windows, target_os = "macos"))]
fn wait_for_copy(
    changes: fn() -> u64,
    read: fn() -> Option<String>,
    copy: impl FnOnce() -> bool,
    restore: impl FnOnce() + Send + 'static,
) -> Option<String> {
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::{Duration, Instant};

    let generation = COPIES.fetch_add(1, Ordering::Relaxed) + 1;
    let before = changes();
    let pressed = copy();
    let deadline = Instant::now() + COPY_WAIT;
    let mut text = None;
    while pressed && Instant::now() < deadline {
        if changes() != before {
            text = read();
            if text.is_some() {
                break;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    if changes() != before {
        restore();
    } else {
        // Nothing copied yet, also after a press that went only partly through: a late copy
        // still gets the clipboard put back, unless a newer copy has started.
        thread::spawn(move || {
            let deadline = Instant::now() + LATE_COPY;
            while Instant::now() < deadline && COPIES.load(Ordering::Relaxed) == generation {
                if changes() != before {
                    return restore();
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
    }
    text.filter(|text| !text.trim().is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertError {
    NoTarget,
    /// Focus moved away from the expected target.
    FocusChanged,
    /// Focus moved away while the text was typed: its first `typed` bytes went in.
    Interrupted {
        typed: usize,
    },
    /// The target refuses synthetic input, e.g. it runs as administrator.
    Blocked,
    /// The OS has not allowed this app to type into other programs (macOS Accessibility).
    NoPermission,
    Unsupported,
    Failed(String),
}

impl fmt::Display for InsertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTarget => write!(f, "no focused window"),
            Self::FocusChanged => write!(f, "focus moved to another window"),
            Self::Interrupted { .. } => write!(f, "focus moved to another window while typing"),
            Self::Blocked => write!(f, "the window does not accept typed input"),
            Self::NoPermission => write!(f, "the Accessibility permission is not granted"),
            Self::Unsupported => write!(f, "text insertion is not implemented on this OS"),
            Self::Failed(detail) => write!(f, "text insertion failed: {detail}"),
        }
    }
}

impl std::error::Error for InsertError {}

/// Builds one of the small windows (the capsule, the quick panel). macOS: it acts as a panel
/// that does not activate ahem, as the menus of the menu bar: it shows on every space and over
/// another program in full screen, also while ahem is in the Dock, and takes the keyboard
/// without bringing ahem forward. Called on the main thread. Windows and Linux show a topmost
/// window over everything as it is.
pub fn build_floating(
    build: impl FnOnce() -> tauri::Result<tauri::WebviewWindow>,
) -> tauri::Result<tauri::WebviewWindow> {
    #[cfg(target_os = "macos")]
    return macos::make_floating(build, |made| {
        made.as_ref().ok().and_then(|window| window.ns_window().ok())
    });
    #[cfg(not(target_os = "macos"))]
    build()
}

/// Gives the keyboard to the quick panel just shown. macOS: shown, a window of `build_floating`
/// is the key window already, and ahem stays where it is; activating ahem would take the user
/// from another program in full screen to ahem's window. Any other window, and the window on
/// Windows and Linux, takes the focus.
pub fn focus_panel(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    if window.ns_window().is_ok_and(macos::is_floating) {
        return;
    }
    let _ = window.set_focus();
}

/// The quick panel took or lost the keyboard. macOS: it takes keys without bringing ahem
/// forward, so while it has them insertion and the copy of a selection take ahem for the
/// program in focus (a focus change: the text is kept, never typed into the panel). Windows
/// and Linux bring ahem forward with the panel, which the focus checks see as it is.
pub fn set_panel_keys(has: bool) {
    #[cfg(target_os = "macos")]
    macos::set_panel_keys(has);
    #[cfg(not(target_os = "macos"))]
    let _ = has;
}

/// The program in front, to give the focus back to once a window of ours that took it is put
/// away. macOS only: there a hidden window that brought ahem forward leaves ahem in front, with
/// no window to type into (the quick panel does not, unless it could not be made non-activating);
/// Windows activates the next window by itself.
pub fn frontmost_program() -> Option<u32> {
    #[cfg(target_os = "macos")]
    return macos::frontmost_process();
    #[cfg(not(target_os = "macos"))]
    None
}

/// Brings the program `frontmost_program` named forward again (not ahem itself).
pub fn give_focus_back(pid: u32) {
    #[cfg(target_os = "macos")]
    macos::activate_process(pid);
    #[cfg(not(target_os = "macos"))]
    let _ = pid;
}

/// macOS: ahem is in the Dock only while its window is open; closed, it lives in the menu bar.
/// Windows and Linux leave the taskbar to the window itself.
pub fn set_in_dock(app: &tauri::AppHandle, visible: bool) {
    // The activation policy, not `set_dock_visibility`: its `TransformProcessType` hides the
    // app but does not bring it back to the Dock and Cmd+Tab on current macOS.
    #[cfg(target_os = "macos")]
    {
        let policy = if visible {
            tauri::ActivationPolicy::Regular
        } else {
            tauri::ActivationPolicy::Accessory
        };
        if let Err(error) = app.set_activation_policy(policy) {
            log_line!("cannot change the Dock icon: {error}");
        }
        if visible {
            macos::restore_dock_icon();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}

/// Hands memory freed by a long job (a file's or a meeting's decoded audio) back to the system
/// now. macOS keeps freed pages with the process unless its allocator is told; the setting that
/// does it at once (`LSEnvironment` in `Info.plist`) does not reach an app started at login,
/// which launchd starts without LaunchServices. Elsewhere there is nothing to do.
pub fn release_memory() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        // SAFETY: no zone means every zone, and a goal of 0 as much as can be freed.
        unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
    }
}

/// Starts a console program without a visible window.
pub fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn a_rewritten_selection_keeps_its_edges() {
        use super::with_selected_edges;
        assert_eq!(with_selected_edges(" two words\n", "  Two words. "), " Two words.\n");
        assert_eq!(with_selected_edges("plain", "Plain"), "Plain");
    }

    /// Handing memory back is safe at any time, also twice.
    #[test]
    fn memory_can_be_handed_back_at_any_time() {
        super::release_memory();
        super::release_memory();
    }

    /// Only the system's "opened again" shows the window; other events of the app do not.
    #[test]
    fn only_a_reopen_shows_the_window() {
        assert!(!super::reopened(&tauri::RunEvent::Ready));
        assert!(!super::reopened(&tauri::RunEvent::Exit));
    }

    /// The allocator setting that hands freed memory back to the system at once. Without it
    /// macOS keeps the pages of an unloaded speech model with the process, and the idle app looks
    /// a few hundred megabytes larger (measured 2026-11-09: no slower transcription with it).
    /// macOS reads it only when a process starts: `Info.plist` sets it (`LSEnvironment`), which
    /// an app started at login does not get, so long jobs also end with `release_memory`.
    /// Restarting the process with it instead (`exec`) lost the menu bar icon of an app opened
    /// from Finder.
    const SPACE_EFFICIENT_MALLOC: (&str, &str) = ("MallocSpaceEfficient", "1");

    #[test]
    fn an_unknown_window_is_not_another_one() {
        use super::FocusTarget;
        let here = FocusTarget { window: 7, process_id: 3 };
        assert!(here.same_place(&FocusTarget { window: 0, process_id: 3 }));
        assert!(!here.same_place(&FocusTarget { window: 8, process_id: 3 }));
        assert!(!here.same_place(&FocusTarget { window: 7, process_id: 4 }));
    }

    #[cfg(any(windows, target_os = "macos"))]
    mod copying {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::{Arc, Mutex};
        use std::thread;
        use std::time::Duration;

        use super::super::wait_for_copy;

        // One clipboard for these tests, which run one at a time on it.
        static LOCK: Mutex<()> = Mutex::new(());
        static CHANGES: AtomicU64 = AtomicU64::new(0);
        static TEXT: Mutex<Option<String>> = Mutex::new(None);

        fn changes() -> u64 {
            CHANGES.load(Ordering::SeqCst)
        }
        fn read() -> Option<String> {
            TEXT.lock().unwrap().clone()
        }
        fn copy_later(text: Option<&str>, after: Duration) {
            let text = text.map(String::from);
            thread::spawn(move || {
                thread::sleep(after);
                *TEXT.lock().unwrap() = text;
                CHANGES.fetch_add(1, Ordering::SeqCst);
            });
        }

        #[test]
        fn the_selection_comes_and_the_clipboard_goes_back() {
            let _one = LOCK.lock().unwrap();
            let restored = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&restored);
            copy_later(Some("selected"), Duration::from_millis(20));
            let text =
                wait_for_copy(changes, read, || true, move || flag.store(true, Ordering::SeqCst));
            assert_eq!(text.as_deref(), Some("selected"));
            assert!(restored.load(Ordering::SeqCst));
        }

        #[test]
        fn a_private_copy_is_put_back_and_not_given() {
            let _one = LOCK.lock().unwrap();
            let restored = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&restored);
            // `read` gives nothing for a copy a password manager marked private.
            copy_later(None, Duration::from_millis(20));
            let text =
                wait_for_copy(changes, read, || true, move || flag.store(true, Ordering::SeqCst));
            assert_eq!(text, None);
            assert!(restored.load(Ordering::SeqCst));
        }

        #[test]
        fn a_late_copy_is_put_back_unless_a_newer_copy_started() {
            let _one = LOCK.lock().unwrap();
            let late = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&late);
            // Nothing in time: the watcher waits on.
            let text =
                wait_for_copy(changes, read, || true, move || flag.store(true, Ordering::SeqCst));
            assert_eq!(text, None);
            copy_later(Some("late"), Duration::from_millis(100));
            thread::sleep(Duration::from_millis(300));
            assert!(late.load(Ordering::SeqCst), "the late copy was put back");
            // Again, but a newer copy starts before the late one comes: the old watcher stops.
            let old = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&old);
            wait_for_copy(changes, read, || true, move || flag.store(true, Ordering::SeqCst));
            copy_later(Some("newer"), Duration::from_millis(50));
            let newer = wait_for_copy(changes, read, || true, || {});
            assert_eq!(newer.as_deref(), Some("newer"));
            thread::sleep(Duration::from_millis(100));
            assert!(!old.load(Ordering::SeqCst), "the old watcher put the clipboard back");
        }
    }

    const CONFIG: &str = include_str!("../../tauri.conf.json");
    /// Replaces the whole window list of `CONFIG` on macOS.
    const MACOS_CONFIG: &str = include_str!("../../tauri.macos.conf.json");

    fn main_window(config: &str) -> serde_json::Value {
        let config: serde_json::Value = serde_json::from_str(config).unwrap();
        let windows = config["app"]["windows"].as_array().unwrap();
        windows.iter().find(|window| window["label"] == "main").unwrap().clone()
    }

    /// Pages get the same CSS size on every OS: the zoomed main window is the macOS one times
    /// `UI_ZOOM`.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn zoomed_main_window_is_the_macos_one_times_the_zoom() {
        let sizes = |window: serde_json::Value| {
            ["width", "height", "minWidth", "minHeight"].map(|key| window[key].as_f64().unwrap())
        };
        let zoomed = sizes(main_window(CONFIG));
        let macos = sizes(main_window(MACOS_CONFIG));
        assert_eq!(zoomed, macos.map(|size| (size * super::UI_ZOOM).round()));
    }

    /// Tauri builds the windows of the config before `setup`, but the main window's page calls
    /// commands as soon as it loads: `setup` builds it once their state is managed.
    #[test]
    fn setup_builds_the_main_window() {
        for config in [CONFIG, MACOS_CONFIG] {
            assert_eq!(main_window(config)["create"], false);
        }
    }

    #[test]
    fn any_windows_microphone_switch_turned_off_blocks_recording() {
        use super::{Permission, windows_microphone};
        assert_eq!(windows_microphone([None, None, None]), Permission::Granted);
        assert_eq!(
            windows_microphone([Some("Allow"), Some("Allow"), Some("Allow")]),
            Permission::Granted
        );
        assert_eq!(windows_microphone([Some("Deny"), Some("Allow"), None]), Permission::Missing);
        assert_eq!(windows_microphone([Some("Allow"), Some("Deny"), None]), Permission::Missing);
        assert_eq!(windows_microphone([None, None, Some("Deny")]), Permission::Missing);
    }

    /// The interface reads these names (`src/onboarding.ts`).
    #[test]
    fn permissions_reach_the_interface_by_name() {
        use super::{Permission, PermissionKind, Permissions};
        let permissions =
            Permissions { microphone: Permission::Granted, accessibility: Permission::NotNeeded };
        assert_eq!(
            serde_json::to_value(permissions).unwrap(),
            serde_json::json!({ "microphone": "granted", "accessibility": "not_needed" })
        );
        assert_eq!(serde_json::to_value(Permission::Missing).unwrap(), "missing");
        let kind: PermissionKind = serde_json::from_str("\"accessibility\"").unwrap();
        assert_eq!(kind, PermissionKind::Accessibility);
    }

    /// Without a signing identity the bundle keeps only the linker's signature, under an
    /// identifier other than the bundle id, and the Accessibility permission does not match it.
    /// `-` signs the whole bundle ad hoc as the bundle id.
    #[test]
    fn macos_bundle_is_signed_as_a_whole() {
        let config: serde_json::Value = serde_json::from_str(MACOS_CONFIG).unwrap();
        assert_eq!(config["bundle"]["macOS"]["signingIdentity"], "-");
    }

    /// The signature turns on the hardened runtime: without the audio-input entitlement macOS
    /// never asks for the microphone, and recording stays silent.
    #[test]
    fn macos_bundle_may_ask_for_the_microphone() {
        let config: serde_json::Value = serde_json::from_str(MACOS_CONFIG).unwrap();
        assert_eq!(config["bundle"]["macOS"]["entitlements"], "Entitlements.plist");
        let entitlements = include_str!("../../Entitlements.plist");
        assert!(
            entitlements.contains("<key>com.apple.security.device.audio-input</key>\n\t<true/>")
        );
    }

    /// The app gets the allocator setting from `Info.plist` when macOS starts it.
    #[test]
    fn info_plist_sets_the_allocator_setting() {
        let info = include_str!("../../Info.plist");
        let (name, value) = SPACE_EFFICIENT_MALLOC;
        let expected = format!(
            "<key>LSEnvironment</key>\n\t<dict>\n\t\t<key>{name}</key>\n\t\t<string>{value}</string>"
        );
        assert!(info.contains(&expected), "{info}");
    }

    /// The setting gives freed memory back: a child of this test binary frees 200 MB.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "a helper run by the_allocator_setting_returns_freed_memory"]
    fn allocates_and_frees() {
        let blocks: Vec<Vec<u8>> = (0..200_000).map(|i| vec![i as u8; 1_000]).collect();
        drop(blocks);
        println!("footprint {}", footprint());
    }

    #[cfg(target_os = "macos")]
    fn footprint() -> u64 {
        unsafe extern "C" {
            fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
        }
        // `rusage_info_v0`: a 16-byte UUID, then u64 fields; `ri_phys_footprint` is the 8th.
        let mut info = [0u64; 12];
        // SAFETY: the buffer is larger than `rusage_info_v0` (96 bytes).
        let status = unsafe { proc_pid_rusage(std::process::id() as i32, 0, info.as_mut_ptr()) };
        assert_eq!(status, 0);
        info[2 + 7]
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_allocator_setting_returns_freed_memory() {
        let run = |setting: Option<&str>| {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args([
                "--ignored",
                "--exact",
                "platform::tests::allocates_and_frees",
                "--nocapture",
            ]);
            child.env_remove(SPACE_EFFICIENT_MALLOC.0);
            if let Some(value) = setting {
                child.env(SPACE_EFFICIENT_MALLOC.0, value);
            }
            let output = String::from_utf8(child.output().unwrap().stdout).unwrap();
            let line = output.lines().find_map(|line| line.strip_prefix("footprint ")).unwrap();
            line.trim().parse::<u64>().unwrap()
        };
        let kept = run(None);
        let returned = run(Some(SPACE_EFFICIENT_MALLOC.1));
        assert!(
            returned + 50_000_000 < kept,
            "{} MB with the setting, {} MB without",
            returned / 1_000_000,
            kept / 1_000_000
        );
    }

    /// macOS refuses to record the computer's sound for a meeting without this description.
    #[test]
    fn macos_bundle_may_ask_for_the_computers_sound() {
        let info = include_str!("../../Info.plist");
        assert!(info.contains("<key>NSMicrophoneUsageDescription</key>"));
        assert!(info.contains("<key>NSAudioCaptureUsageDescription</key>"));
    }

    /// The main window opens at its most compact size: it can grow, not shrink.
    #[test]
    fn main_window_opens_at_its_minimum_size() {
        for config in [CONFIG, MACOS_CONFIG] {
            let window = main_window(config);
            assert_eq!(window["minWidth"], window["width"]);
            assert_eq!(window["minHeight"], window["height"]);
        }
    }

    /// Downloads check free space with this before they start.
    #[test]
    fn free_space_of_a_folder_is_known() {
        assert!(super::available_space(&std::env::temp_dir()).unwrap() > 0);
        assert!(super::available_space(std::path::Path::new("/no/such/folder")).is_err());
    }

    /// On macOS the free space takes in what the system frees on demand, as Finder counts it;
    /// `statvfs` leaves that out. The two are read a moment apart: allow for files written
    /// meanwhile.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_counts_what_the_system_frees_on_demand() {
        let dir = std::env::temp_dir();
        let important = super::macos::important_capacity(&dir).expect("the volume says");
        let statvfs = fs4::available_space(&dir).unwrap();
        assert!(important + (1 << 30) >= statvfs, "{important} < {statvfs}");
        assert!(super::available_space(&dir).unwrap().abs_diff(important) < 1 << 30);
    }

    /// The main window shares the browser environment with the windows built in code, so it
    /// needs their arguments.
    #[test]
    fn main_window_gets_the_browser_args_of_the_other_windows() {
        assert_eq!(main_window(CONFIG)["additionalBrowserArgs"], super::BROWSER_ARGS);
        assert!(super::BROWSER_ARGS.contains("--disable-lcd-text"));
    }

    /// Windows and Linux draw the main window's title bar in the app's style (`TitleBar`);
    /// macOS keeps the system window buttons over the page: a transparent bar without the title,
    /// the buttons in the corner above the brand.
    #[test]
    fn only_macos_keeps_the_system_window_buttons_over_the_page() {
        assert_eq!(main_window(CONFIG)["decorations"], false);
        let macos = main_window(MACOS_CONFIG);
        assert_ne!(macos["decorations"], false);
        assert_eq!(macos["titleBarStyle"], "Overlay");
        assert_eq!(macos["hiddenTitle"], true);
        assert!(macos["trafficLightPosition"]["x"].is_number());
        assert!(macos["trafficLightPosition"]["y"].is_number());
    }
}

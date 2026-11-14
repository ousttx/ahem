//! macOS backend: text insertion with `CGEvent`, the focused application and window, modifier
//! state, output muting with Core Audio. Typing into other programs and reading their windows
//! need the Accessibility permission (System Settings → Privacy & Security → Accessibility).

use std::ffi::{CStr, c_void};
use std::io;
use std::mem::size_of;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, Once, OnceLock};
use std::thread;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, ProtocolObject, Sel};
use objc2::{AllocAnyThread, ClassType, MainThreadMarker, class, msg_send};
use objc2_app_kit::{
    NSApplication, NSImage, NSPasteboard, NSPasteboardItem, NSPasteboardTypeString,
    NSPasteboardWriting, NSRunningApplication, NSWindow, NSWindowCollectionBehavior, NSWorkspace,
};
use objc2_application_services::{
    AXError, AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElement,
    kAXTrustedCheckOptionPrompt,
};
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectHasProperty, AudioObjectID,
    AudioObjectIsPropertySettable, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
    kAudioDevicePropertyMute, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject, kAudioObjectUnknown,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFHash, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGKeyCode,
};
use objc2_foundation::{
    NSArray, NSBundle, NSData, NSNumber, NSString, NSURL,
    NSURLVolumeAvailableCapacityForImportantUsageKey,
};

use super::keys::{Key, key_plan, typed_len};
use super::{FocusTarget, InsertError, Permission, PermissionKind, Permissions};
use crate::audio::mute::SystemOutput;
use crate::domain::app_rules::AppId;

/// `CGEventKeyboardSetUnicodeString` takes at most 20 UTF-16 units per event.
const UNITS_PER_EVENT: usize = 20;
/// A short pause between events keeps slow targets from dropping input on long texts.
const EVENT_PAUSE: Duration = Duration::from_millis(2);
/// `kVK_Return`.
const RETURN_KEY: CGKeyCode = 36;
/// `kVK_ANSI_C`.
const C_KEY: CGKeyCode = 8;
/// Seconds an application may take to answer one Accessibility request.
const ANSWER_TIMEOUT: f32 = 2.0;
/// Clipboard types by which password managers and similar programs mark what others must not
/// read or record (nspasteboard.org).
const PRIVATE_TYPES: [&str; 2] =
    ["org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType"];

/// What is typed: a piece of text in one event, or the Return key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stroke {
    Text(Vec<u16>),
    Return,
}

/// Groups the key plan into events of at most `UNITS_PER_EVENT` units; a surrogate pair is
/// never split between two events.
fn strokes(keys: &[Key]) -> Vec<Stroke> {
    let mut strokes = Vec::new();
    let mut text = Vec::new();
    for key in keys {
        match *key {
            Key::Unicode(unit) => {
                let low_surrogate = (0xDC00..=0xDFFF).contains(&unit);
                let needs = if (0xD800..=0xDBFF).contains(&unit) { 2 } else { 1 };
                if !low_surrogate && text.len() + needs > UNITS_PER_EVENT {
                    strokes.push(Stroke::Text(std::mem::take(&mut text)));
                }
                text.push(unit);
            }
            Key::Enter => {
                if !text.is_empty() {
                    strokes.push(Stroke::Text(std::mem::take(&mut text)));
                }
                strokes.push(Stroke::Return);
            }
        }
    }
    if !text.is_empty() {
        strokes.push(Stroke::Text(text));
    }
    strokes
}

/// Whether this app may type into and read other programs (the Accessibility permission).
/// Checked live, so a permission granted while the app runs works at once. The synthetic-event
/// check (`CGPreflightPostEventAccess`) is not used: macOS caches its first answer for the life
/// of the process, so a refusal before the grant would last until a restart.
pub fn accessibility_allowed() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Asks macOS once per run to show its own prompt that leads to the Accessibility settings; it
/// also puts ahem in the list there. Returns whether it asked now.
pub fn request_accessibility() -> bool {
    static ASKED: AtomicBool = AtomicBool::new(false);
    if ASKED.swap(true, Ordering::SeqCst) {
        return false;
    }
    let key: &CFString = unsafe { kAXTrustedCheckOptionPrompt };
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[CFBoolean::new(true)]);
    let options: &CFDictionary = (*options).as_ref();
    unsafe { AXIsProcessTrustedWithOptions(Some(options)) };
    true
}

const ACCESSIBILITY_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
const MICROPHONE_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeAudio: &'static NSString;
}

/// `AVAuthorizationStatus`: whether this app may record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MicrophoneStatus {
    NotDetermined,
    Refused,
    Authorized,
}

/// What the system answered when the app itself asked for the microphone. A process that read
/// the status before that answer goes on hearing "not determined" from AVFoundation until it
/// restarts, so its own answer is kept and counts.
const ANSWER_NONE: u8 = 0;
const ANSWER_GRANTED: u8 = 1;
const ANSWER_REFUSED: u8 = 2;
static MICROPHONE_ANSWER: AtomicU8 = AtomicU8::new(ANSWER_NONE);

/// The status from `AVAuthorizationStatus` (0 not determined, 3 authorized, else refused) and
/// the answer to this app's own request.
fn microphone_status_from(status: isize, answer: u8) -> MicrophoneStatus {
    match (status, answer) {
        (3, _) | (_, ANSWER_GRANTED) => MicrophoneStatus::Authorized,
        (0, ANSWER_REFUSED) => MicrophoneStatus::Refused,
        (0, _) => MicrophoneStatus::NotDetermined,
        _ => MicrophoneStatus::Refused,
    }
}

fn microphone_status() -> MicrophoneStatus {
    // SAFETY: a class method of AVFoundation with its own media type constant.
    let status: isize = unsafe {
        msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: AVMediaTypeAudio]
    };
    microphone_status_from(status, MICROPHONE_ANSWER.load(Ordering::SeqCst))
}

/// Asks for the microphone: the system's prompt on the first time, else its stored answer at
/// once. The answer is kept in `MICROPHONE_ANSWER`.
fn ask_for_microphone() {
    let answered = block2::RcBlock::new(|granted: Bool| {
        let answer = if granted.as_bool() { ANSWER_GRANTED } else { ANSWER_REFUSED };
        MICROPHONE_ANSWER.store(answer, Ordering::SeqCst);
    });
    // SAFETY: a class method of AVFoundation taking the media type and a block that gets a BOOL;
    // the system copies the block, so it may be dropped after the call.
    unsafe {
        let _: () = msg_send![
            class!(AVCaptureDevice),
            requestAccessForMediaType: AVMediaTypeAudio,
            completionHandler: &*answered
        ];
    }
}

pub fn permissions() -> Permissions {
    let granted = |yes: bool| if yes { Permission::Granted } else { Permission::Missing };
    Permissions {
        microphone: granted(microphone_status() == MicrophoneStatus::Authorized),
        accessibility: granted(accessibility_allowed()),
    }
}

/// macOS shows its own prompt the first time; after a refusal only the settings page helps.
pub fn request_permission(kind: PermissionKind) -> Option<&'static str> {
    match kind {
        PermissionKind::Microphone => {
            if microphone_status() != MicrophoneStatus::NotDetermined {
                return Some(MICROPHONE_SETTINGS);
            }
            ask_for_microphone();
            None
        }
        PermissionKind::Accessibility => {
            if request_accessibility() {
                None
            } else {
                Some(ACCESSIBILITY_SETTINGS)
            }
        }
    }
}

pub(super) fn attribute(element: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let name = CFString::from_str(name);
    let mut value: *const CFType = std::ptr::null();
    let error = unsafe { element.copy_attribute_value(&name, NonNull::from(&mut value)) };
    if error != AXError::Success {
        return None;
    }
    NonNull::new(value.cast_mut()).map(|value| unsafe { CFRetained::from_raw(value) })
}

pub(super) fn element_attribute(
    element: &AXUIElement,
    name: &str,
) -> Option<CFRetained<AXUIElement>> {
    attribute(element, name)?.downcast::<AXUIElement>().ok()
}

pub(super) fn string_attribute(element: &AXUIElement, name: &str) -> Option<String> {
    attribute(element, name)?.downcast::<CFString>().ok().map(|text| text.to_string())
}

/// The Accessibility element of an application. Every Accessibility request of this process
/// waits at most `ANSWER_TIMEOUT`, so a hung program cannot hold the hotkeys.
pub(super) fn application(process_id: u32) -> CFRetained<AXUIElement> {
    static TIMEOUT: Once = Once::new();
    // Set on the system-wide element, the timeout holds for every element of the process.
    TIMEOUT.call_once(|| {
        let _ = unsafe { AXUIElement::new_system_wide().set_messaging_timeout(ANSWER_TIMEOUT) };
    });
    unsafe { AXUIElement::new_application(process_id as _) }
}

/// A stable number for an Accessibility element: equal elements have equal hashes.
fn identity(element: &AXUIElement) -> isize {
    CFHash(Some(element.as_ref())) as isize
}

pub fn focus_target() -> Option<FocusTarget> {
    let process_id = keyboard_process()?;
    if process_id == std::process::id() {
        let window = own_window(PANEL_KEYS.load(Ordering::SeqCst));
        return Some(FocusTarget { window, process_id });
    }
    // Without the Accessibility permission only the application is known; insertion then
    // reports the missing permission.
    let window = accessibility_allowed()
        .then(|| element_attribute(&application(process_id), "AXFocusedWindow"))
        .flatten()
        .map_or(0, |w| identity(&w));
    Some(FocusTarget { window, process_id })
}

/// The window of ahem's quick panel while it has the keys, as `focus_target` names it.
const PANEL_WINDOW: isize = isize::MIN;
/// Any other window of ahem: the main one, or a dialog of it.
const OWN_WINDOW: isize = isize::MIN + 1;

/// A window of ahem itself is never an unknown one (0, which `same_place` takes for any window
/// of the program): the panel is `PANEL_WINDOW`, every other `OWN_WINDOW`. So a text meant for
/// the main window never goes into the panel opened meanwhile, nor the reverse. Its own
/// windows are not told apart further: asking Accessibility about them waits for ahem's own
/// main thread, and only the panel takes keys beside the main window and its dialogs.
fn own_window(panel_keys: bool) -> isize {
    if panel_keys { PANEL_WINDOW } else { OWN_WINDOW }
}

/// The keys no longer go where `target` is: another program, or ahem's panel instead of its
/// other windows (or the reverse). Asks no program over Accessibility.
pub(super) fn keys_left(target: &FocusTarget) -> bool {
    left(PANEL_KEYS.load(Ordering::SeqCst), std::process::id(), frontmost_process, target)
}

/// `keys_left`, with whether the panel has the keys, ahem's process and the program in front
/// given.
fn left(
    panel_keys: bool,
    own: u32,
    front: impl FnOnce() -> Option<u32>,
    target: &FocusTarget,
) -> bool {
    keys_go_to(panel_keys, own, front) != Some(target.process_id)
        || target.process_id == own && panel_keys != (target.window == PANEL_WINDOW)
}

/// The application's bundle identifier and name; without a bundle, the name stands for both.
pub fn app_identity(target: &FocusTarget) -> Option<AppId> {
    if target.process_id == std::process::id() {
        return None;
    }
    let pid = i32::try_from(target.process_id).ok()?;
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    let name = app.localizedName().map(|name| name.to_string()).unwrap_or_default();
    let id = app.bundleIdentifier().map(|id| id.to_string()).unwrap_or_else(|| name.clone());
    (!id.is_empty()).then(|| AppId { name: if name.is_empty() { id.clone() } else { name }, id })
}

/// The clipboard's text; `None` when it holds none (an image, files) or a password manager
/// marked it private.
pub fn clipboard_text() -> Option<String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    if private_clipboard(&pasteboard) {
        return None;
    }
    let text = pasteboard.stringForType(unsafe { NSPasteboardTypeString })?;
    // A password manager may mark its copy a moment after writing the text (KeePassXC does it
    // in a call of its own); reading waits for the writer, so the mark is there by now.
    if private_clipboard(&pasteboard) {
        return None;
    }
    Some(text.to_string())
}

/// Counts every change of the clipboard.
pub(super) fn clipboard_changes() -> u64 {
    NSPasteboard::generalPasteboard().changeCount() as u64
}

/// Whether a password manager or a similar program marked the clipboard as not to be read.
pub(super) fn private_clipboard(pasteboard: &NSPasteboard) -> bool {
    pasteboard
        .types()
        .is_some_and(|types| types.iter().any(|kind| PRIVATE_TYPES.contains(&&*kind.to_string())))
}

/// Everything on the clipboard, item by item, each type with its data. Plain bytes, so a
/// thread of its own can put it back.
pub(super) struct SavedClipboard(Vec<Vec<(String, Vec<u8>)>>);

pub(super) fn save_clipboard(pasteboard: &NSPasteboard) -> SavedClipboard {
    let items = pasteboard.pasteboardItems().map(|items| items.to_vec()).unwrap_or_default();
    SavedClipboard(
        items
            .iter()
            .map(|item| {
                let types = item.types().to_vec();
                types
                    .into_iter()
                    .filter_map(|kind| Some((kind.to_string(), item.dataForType(&kind)?.to_vec())))
                    .collect()
            })
            .collect(),
    )
}

/// Puts the saved items back; an empty clipboard stays empty.
pub(super) fn restore_clipboard(pasteboard: &NSPasteboard, saved: SavedClipboard) {
    pasteboard.clearContents();
    if saved.0.is_empty() {
        return;
    }
    let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = saved
        .0
        .into_iter()
        .map(|types| {
            let item = NSPasteboardItem::new();
            for (kind, data) in &types {
                item.setData_forType(&NSData::with_bytes(data), &NSString::from_str(kind));
            }
            ProtocolObject::from_retained(item)
        })
        .collect();
    pasteboard.writeObjects(&NSArray::from_retained_slice(&items));
}

/// Presses ⌘C in the program in focus. The event carries only Command, so the modifiers of a
/// hotkey the user still holds do not change it.
pub(super) fn press_copy() -> bool {
    if !accessibility_allowed() {
        return false;
    }
    for down in [true, false] {
        let Some(event) = CGEvent::new_keyboard_event(None, C_KEY, down) else { return false };
        CGEvent::set_flags(Some(&event), CGEventFlags::MaskCommand);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    true
}

fn flags_down(mask: CGEventFlags) -> bool {
    CGEventSource::flags_state(CGEventSourceStateID::CombinedSessionState).intersects(mask)
}

pub fn modifiers_down() -> bool {
    flags_down(
        CGEventFlags::MaskShift
            | CGEventFlags::MaskControl
            | CGEventFlags::MaskAlternate
            | CGEventFlags::MaskCommand,
    )
}

/// Control, Option or Command: with one of them down typed letters act as shortcuts. A held
/// Shift does not change the Unicode text `insert_text` sends.
pub fn shortcut_modifiers_down() -> bool {
    flags_down(CGEventFlags::MaskControl | CGEventFlags::MaskAlternate | CGEventFlags::MaskCommand)
}

/// Run without a bundle (`tools/dev.sh`), the app shows a generic "exec" icon in the Dock once
/// it comes back there: Tauri sets the icon only at the start. A bundled app takes its icon
/// from the bundle. Main thread only.
pub fn restore_dock_icon() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if NSBundle::mainBundle().bundlePath().to_string().ends_with(".app") {
        return;
    }
    let data = NSData::with_bytes(include_bytes!("../../icons/icon.png"));
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        // SAFETY: on the main thread (`mtm`), with a valid image.
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }
}

/// Not used: the menu bar paints the tray template in its own colors.
pub fn taskbar_is_light() -> bool {
    false
}

pub fn insert_text(text: &str, expected: Option<&FocusTarget>) -> Result<(), InsertError> {
    // Without the permission macOS drops synthetic key events silently.
    if !accessibility_allowed() {
        request_accessibility();
        return Err(InsertError::NoPermission);
    }
    let target = focus_target().ok_or(InsertError::NoTarget)?;
    // Another field of the same window is still the same place.
    if expected.is_some_and(|expected| !expected.same_place(&target)) {
        return Err(InsertError::FocusChanged);
    }
    // Keys of the plan posted so far.
    let mut sent = 0;
    for (index, stroke) in strokes(&key_plan(text)).iter().enumerate() {
        if index > 0 {
            thread::sleep(EVENT_PAUSE);
            // The user may switch applications, or open the quick panel, while a long text is
            // typed. No program is asked: Accessibility calls go to the target and may hang.
            if keys_left(&target) {
                return Err(InsertError::Interrupted { typed: typed_len(text, sent) });
            }
        }
        post(stroke)?;
        sent += match stroke {
            Stroke::Text(units) => units.len(),
            Stroke::Return => 1,
        };
    }
    Ok(())
}

/// The quick panel has the keyboard (`set_panel_keys`).
static PANEL_KEYS: AtomicBool = AtomicBool::new(false);

/// The quick panel took or lost the keyboard. It takes keys without bringing ahem forward
/// (`make_floating`), so while it has them the program in front is not where keys go.
pub fn set_panel_keys(has: bool) {
    PANEL_KEYS.store(has, Ordering::SeqCst);
}

/// The program typed keys go to: ahem itself while its quick panel has the keyboard, the
/// program in front otherwise. `focus_target` names it; insertion and the copy of a selection
/// check `keys_left`, so a text is never typed into the panel.
fn keyboard_process() -> Option<u32> {
    keys_go_to(PANEL_KEYS.load(Ordering::SeqCst), std::process::id(), frontmost_process)
}

fn keys_go_to(panel_keys: bool, own: u32, front: impl FnOnce() -> Option<u32>) -> Option<u32> {
    if panel_keys { Some(own) } else { front() }
}

pub(super) fn frontmost_process() -> Option<u32> {
    let frontmost = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    u32::try_from(frontmost.processIdentifier()).ok()
}

/// Brings another program forward again; ahem itself is left as it is.
pub(super) fn activate_process(pid: u32) {
    if pid == std::process::id() {
        return;
    }
    let Ok(pid) = i32::try_from(pid) else { return };
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        app.activateWithOptions(objc2_app_kit::NSApplicationActivationOptions::empty());
    }
}

/// Modifiers the user still holds must not turn the text into shortcuts. A line break is
/// Shift+Return: a new line in chats, where Return sends the message.
fn stroke_flags(stroke: &Stroke) -> CGEventFlags {
    if *stroke == Stroke::Return { CGEventFlags::MaskShift } else { CGEventFlags(0) }
}

fn post(stroke: &Stroke) -> Result<(), InsertError> {
    for down in [true, false] {
        let key = if *stroke == Stroke::Return { RETURN_KEY } else { 0 };
        let event = CGEvent::new_keyboard_event(None, key, down)
            .ok_or_else(|| InsertError::Failed("cannot create a key event".into()))?;
        CGEvent::set_flags(Some(&event), stroke_flags(stroke));
        if let Stroke::Text(units) = stroke {
            unsafe {
                CGEvent::keyboard_set_unicode_string(Some(&event), units.len() as _, units.as_ptr())
            };
        }
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    Ok(())
}

/// The output devices by their UID, which stays the same across restarts and reconnections;
/// muting starts at the default one.
pub struct DefaultOutput;

/// `kAudioDevicePropertyDeviceUID` ('uid '): a device's persistent id, a `CFString`.
const DEVICE_UID: u32 = u32::from_be_bytes(*b"uid ");
/// `kAudioHardwarePropertyTranslateUIDToDevice` ('uidd', macOS 11): the device of a UID.
const TRANSLATE_UID: u32 = u32::from_be_bytes(*b"uidd");

const fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn status(code: i32, what: &str) -> io::Result<()> {
    if code == 0 { Ok(()) } else { Err(io::Error::other(format!("{what}: OSStatus {code}"))) }
}

impl DefaultOutput {
    /// The default output device, or `None` when there is none.
    fn device() -> io::Result<Option<AudioObjectID>> {
        let mut device: AudioObjectID = kAudioObjectUnknown;
        let mut size = size_of::<AudioObjectID>() as u32;
        let mut address =
            address(kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyScopeGlobal);
        let code = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&mut address),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut device).cast::<c_void>(),
            )
        };
        status(code, "default output device")?;
        Ok((device != kAudioObjectUnknown).then_some(device))
    }

    fn uid(device: AudioObjectID) -> io::Result<String> {
        let mut address = address(DEVICE_UID, kAudioObjectPropertyScopeGlobal);
        let mut uid: *const CFString = std::ptr::null();
        let mut size = size_of::<*const CFString>() as u32;
        let code = unsafe {
            AudioObjectGetPropertyData(
                device,
                NonNull::from(&mut address),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut uid).cast::<c_void>(),
            )
        };
        status(code, "device UID")?;
        // The string is ours to release.
        let uid = NonNull::new(uid.cast_mut()).map(|uid| unsafe { CFRetained::from_raw(uid) });
        uid.map(|uid| uid.to_string()).ok_or_else(|| io::Error::other("device UID: none"))
    }

    /// The device with this UID; `None` when it is gone.
    fn device_of(uid: &str) -> io::Result<Option<AudioObjectID>> {
        let uid = CFString::from_str(uid);
        let qualifier: *const CFString = &*uid;
        let mut address = address(TRANSLATE_UID, kAudioObjectPropertyScopeGlobal);
        let mut device: AudioObjectID = kAudioObjectUnknown;
        let mut size = size_of::<AudioObjectID>() as u32;
        let code = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&mut address),
                size_of::<*const CFString>() as u32,
                (&raw const qualifier).cast::<c_void>(),
                NonNull::from(&mut size),
                NonNull::from(&mut device).cast::<c_void>(),
            )
        };
        status(code, "device of a UID")?;
        Ok((device != kAudioObjectUnknown).then_some(device))
    }

    /// The device and its mute property, or `None` when the device cannot be muted (many
    /// HDMI and some USB outputs) or is gone.
    fn mutable(uid: &str) -> io::Result<Option<(AudioObjectID, AudioObjectPropertyAddress)>> {
        let Some(device) = Self::device_of(uid)? else {
            return Ok(None);
        };
        let mut mute = address(kAudioDevicePropertyMute, kAudioObjectPropertyScopeOutput);
        if !unsafe { AudioObjectHasProperty(device, NonNull::from(&mut mute)) } {
            return Ok(None);
        }
        let mut settable = 0u8;
        let code = unsafe {
            AudioObjectIsPropertySettable(
                device,
                NonNull::from(&mut mute),
                NonNull::from(&mut settable),
            )
        };
        status(code, "mute settable")?;
        Ok((settable != 0).then_some((device, mute)))
    }
}

impl SystemOutput for DefaultOutput {
    fn default_device(&self) -> io::Result<Option<String>> {
        Self::device()?.map(Self::uid).transpose()
    }

    fn muted(&self, device: &str) -> io::Result<bool> {
        // A device that cannot be muted counts as "muted", so nothing gets touched.
        let Some((device, mut mute)) = Self::mutable(device)? else {
            return Ok(true);
        };
        let mut value = 0u32;
        let mut size = size_of::<u32>() as u32;
        let code = unsafe {
            AudioObjectGetPropertyData(
                device,
                NonNull::from(&mut mute),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast::<c_void>(),
            )
        };
        status(code, "read mute")?;
        Ok(value != 0)
    }

    fn set_muted(&self, device: &str, muted: bool) -> io::Result<()> {
        let Some((device, mut mute)) = Self::mutable(device)? else {
            return Ok(());
        };
        let value = u32::from(muted);
        let code = unsafe {
            AudioObjectSetPropertyData(
                device,
                NonNull::from(&mut mute),
                0,
                std::ptr::null(),
                size_of::<u32>() as u32,
                NonNull::from(&value).cast::<c_void>(),
            )
        };
        status(code, "set mute")
    }
}

/// The space for new important files on the volume of `path`
/// (`NSURLVolumeAvailableCapacityForImportantUsageKey`): the free space and what the system frees
/// on demand, such as local Time Machine snapshots and iCloud files, as Finder shows it. `None`
/// when the system does not say, or says 0 (some volumes leave the key unset).
pub fn important_capacity(path: &Path) -> Option<u64> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str()?));
    // SAFETY: a constant of Foundation, set before any code of ours runs.
    let key = unsafe { NSURLVolumeAvailableCapacityForImportantUsageKey };
    let values = url.resourceValuesForKeys_error(&NSArray::from_slice(&[key])).ok()?;
    let number = values.objectForKey(key)?.downcast::<NSNumber>().ok()?;
    u64::try_from(number.longLongValue()).ok().filter(|&bytes| bytes > 0)
}

/// AppKit asks a window whether it is a panel that does not activate its program.
const NONACTIVATING: &CStr = c"_isNonactivatingPanel";
/// The class `tao` makes every window of.
const TAO_WINDOW: &CStr = c"TaoWindow";

/// The floating windows made so far (their `NSWindow` addresses): the capsule and the quick panel.
static FLOATING: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// A floating window is being made (`make_floating`): AppKit asks at its creation.
static MAKING: AtomicBool = AtomicBool::new(false);
/// NSWindow's own `NONACTIVATING`, once ours answers in its place; the error otherwise.
static OWN_NONACTIVATING: OnceLock<Result<Imp, String>> = OnceLock::new();

/// Makes a window (`make`) that acts as a panel that does not activate ahem, as the menus of
/// the menu bar: over another program in full screen too, while ahem is in the Dock, and it
/// takes the keyboard without bringing ahem forward, which would take the user from the
/// full-screen program to ahem's window. Everything else about the window stays.
///
/// AppKit decides both by `NONACTIVATING`, answered yes by an `NSPanel` with the style
/// `NonactivatingPanel` only (tried on macOS 27, 2026-11-12). `tao` makes an `NSWindow`, and its
/// class cannot be swapped for a panel's: AppKit and WebKit observe the window by key-value
/// observing, which is tied to the class, and removing an observer afterwards throws (at the
/// latest when the window is freed, at quit). AppKit asks once when the window is made and keeps
/// the answer for full screen; adding a method to `tao`'s class after that changes nothing, and
/// the capsule is the first window `tao` makes. So NSWindow's `NONACTIVATING` answers yes for
/// `tao`'s windows while one is made here and for the windows made so, and NSWindow's own answer
/// for every other window. Called on the main thread.
pub fn make_floating<T>(
    make: impl FnOnce() -> T,
    ns_window: impl FnOnce(&T) -> Option<*mut c_void>,
) -> T {
    if MainThreadMarker::new().is_none() {
        return make();
    }
    // Without the answer the window is an ordinary one, still on every space.
    let answered = answer_nonactivating()
        .inspect_err(|reason| log_line!("the window will act as an ordinary window: {reason}"))
        .is_ok();
    MAKING.store(answered, Ordering::SeqCst);
    let made = make();
    MAKING.store(false, Ordering::SeqCst);
    if let Some(window) = ns_window(&made).filter(|window| !window.is_null()) {
        if answered {
            FLOATING.lock().expect("floating windows lock").push(window.addr());
        }
        // SAFETY: the NSWindow of a live Tauri window, used on the main thread.
        let window = unsafe { &*window.cast::<NSWindow>() };
        let behavior = window.collectionBehavior()
            | NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary;
        window.setCollectionBehavior(behavior);
        // "Hide ahem" (⌘H) hides the program's windows: not these, which work for other programs.
        window.setCanHide(false);
    }
    made
}

/// The window is one of `make_floating`: shown, it is the key window already, without ahem
/// coming forward.
pub fn is_floating(ns_window: *mut c_void) -> bool {
    FLOATING.lock().is_ok_and(|floating| floating.contains(&ns_window.addr()))
}

/// Puts our `NONACTIVATING` in place of NSWindow's, once; the error stays.
fn answer_nonactivating() -> Result<(), String> {
    OWN_NONACTIVATING
        .get_or_init(|| {
            let method = NSWindow::class()
                .instance_method(Sel::register(NONACTIVATING))
                .ok_or("AppKit does not ask windows whether they are non-activating panels")?;
            let ours: unsafe extern "C-unwind" fn(&AnyObject, Sel) -> Bool = is_nonactivating_panel;
            // SAFETY: `ours` takes and returns what NSWindow's method does (the window and the
            // selector; a `BOOL`, `appkit_asks_windows_whether_they_are_panels`). It calls the
            // method it replaces for every other window, so they answer as before.
            let own = unsafe {
                objc2::ffi::method_setImplementation(
                    method,
                    std::mem::transmute::<unsafe extern "C-unwind" fn(&AnyObject, Sel) -> Bool, Imp>(
                        ours,
                    ),
                )
            };
            own.ok_or_else(|| "NSWindow's own answer is missing".to_owned())
        })
        .as_ref()
        .map(|_| ())
        .map_err(Clone::clone)
}

/// NSWindow's `NONACTIVATING`, in place of AppKit's own.
unsafe extern "C-unwind" fn is_nonactivating_panel(window: &AnyObject, selector: Sel) -> Bool {
    let address = std::ptr::from_ref(window).addr();
    let floating = FLOATING.lock().is_ok_and(|floating| floating.contains(&address));
    let making = MAKING.load(Ordering::SeqCst) && is_tao_window(window);
    if answers_yes(making, floating) {
        return Bool::YES;
    }
    match OWN_NONACTIVATING.get() {
        Some(Ok(own)) => {
            // SAFETY: NSWindow's own method, with the arguments it was asked with.
            let own = unsafe {
                std::mem::transmute::<Imp, unsafe extern "C-unwind" fn(&AnyObject, Sel) -> Bool>(
                    *own,
                )
            };
            // SAFETY: as above.
            unsafe { own(window, selector) }
        }
        _ => Bool::NO,
    }
}

/// A window `tao` makes: not one AppKit or WebKit makes for itself meanwhile.
fn is_tao_window(window: &AnyObject) -> bool {
    AnyClass::get(TAO_WINDOW).is_some_and(|tao| {
        // SAFETY: `isKindOfClass:` of NSObject, with a class.
        let kind: bool = unsafe { msg_send![window, isKindOfClass: tao] };
        kind
    })
}

/// Whether a window answers as a non-activating panel: one of `tao`'s being made by
/// `make_floating`, or one made so.
fn answers_yes(making_tao_window: bool, floating: bool) -> bool {
    making_tao_window || floating
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process that read the microphone status before its own request was answered keeps
    /// hearing "not determined": its own answer counts, a refusal in the system's list too.
    #[test]
    fn the_microphone_answer_of_this_app_beats_a_stale_status() {
        use MicrophoneStatus::*;
        assert_eq!(microphone_status_from(0, ANSWER_NONE), NotDetermined);
        assert_eq!(microphone_status_from(0, ANSWER_GRANTED), Authorized);
        assert_eq!(microphone_status_from(0, ANSWER_REFUSED), Refused);
        assert_eq!(microphone_status_from(3, ANSWER_NONE), Authorized);
        assert_eq!(microphone_status_from(3, ANSWER_REFUSED), Authorized);
        assert_eq!(microphone_status_from(2, ANSWER_NONE), Refused);
        assert_eq!(microphone_status_from(1, ANSWER_NONE), Refused);
    }

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// AppKit still asks a window whether it is a non-activating panel, with no arguments and a
    /// BOOL back, as `is_nonactivating_panel` answers. Fails on a macOS that no longer does: the
    /// capsule and the quick panel would act as ordinary windows on every space (behind
    /// full-screen programs while ahem is in the Dock; the panel taking the keyboard only with
    /// ahem activated).
    #[test]
    fn appkit_asks_windows_whether_they_are_panels() {
        let method = NSWindow::class()
            .instance_method(Sel::register(NONACTIVATING))
            .expect("AppKit asks windows whether they are non-activating panels");
        // SAFETY: a method of a class, which lives as long as the program.
        let types = unsafe { CStr::from_ptr(objc2::ffi::method_getTypeEncoding(method)) };
        let types = types.to_str().expect("an encoding is ASCII");
        // Returns a BOOL, takes self and _cmd (offsets left out).
        let kinds: String = types.chars().filter(|c| !c.is_ascii_digit()).collect();
        assert!(["B@:", "c@:"].contains(&kinds.as_str()), "{types}");
    }

    /// Ahem's panel and its other windows are different places, and neither is the unknown
    /// window (0).
    #[test]
    fn the_panel_is_not_another_window_of_ahem() {
        let panel = FocusTarget { window: own_window(true), process_id: 1 };
        let main = FocusTarget { window: own_window(false), process_id: 1 };
        assert_ne!(main.window, 0, "never an unknown window");
        assert!(!panel.same_place(&main));
        assert!(main.same_place(&main));
        assert!(panel.same_place(&panel));
    }

    /// The keys left: to another program, or between ahem's panel and its other windows.
    #[test]
    fn keys_left_another_program_and_between_the_panel_and_the_main_window() {
        let ahem = 7;
        let other = FocusTarget { window: 5, process_id: 42 };
        let panel = FocusTarget { window: own_window(true), process_id: ahem };
        let main = FocusTarget { window: own_window(false), process_id: ahem };
        // Another program's window, and the panel opened over it.
        assert!(!left(false, ahem, || Some(42), &other));
        assert!(left(true, ahem, || Some(42), &other));
        // The panel, and then the panel closed.
        assert!(!left(true, ahem, || Some(42), &panel));
        assert!(left(false, ahem, || Some(42), &panel));
        // The main window, then the panel opened over it, or another program in front.
        assert!(!left(false, ahem, || Some(ahem), &main));
        assert!(left(true, ahem, || Some(ahem), &main));
        assert!(left(false, ahem, || Some(42), &main));
    }

    /// While the quick panel has the keyboard, keys go to ahem, not to the program in front; the
    /// program in front is not even asked.
    #[test]
    fn keys_go_to_the_panel_while_it_has_them() {
        assert_eq!(keys_go_to(true, 7, || panic!("not asked")), Some(7));
        assert_eq!(keys_go_to(false, 7, || Some(42)), Some(42));
        assert_eq!(keys_go_to(false, 7, || None), None);
    }

    /// A `tao` window being made as a floating one, and the floating windows made, answer as
    /// non-activating panels; every other window answers as AppKit would.
    #[test]
    fn only_floating_windows_answer_as_panels() {
        assert!(answers_yes(true, false), "being made");
        assert!(answers_yes(false, true), "made");
        assert!(!answers_yes(false, false), "any other window");
    }

    /// AVFoundation is linked and answers: macOS asks for both permissions, whatever their state.
    #[test]
    fn reads_both_permissions_from_the_os() {
        let permissions = permissions();
        assert_ne!(permissions.microphone, Permission::NotNeeded);
        assert_ne!(permissions.accessibility, Permission::NotNeeded);
    }

    #[test]
    fn a_line_break_is_shift_return_and_text_has_no_modifiers() {
        assert_eq!(stroke_flags(&Stroke::Return), CGEventFlags::MaskShift);
        assert_eq!(stroke_flags(&Stroke::Text(units("a"))), CGEventFlags(0));
    }

    #[test]
    fn short_text_is_one_event() {
        assert_eq!(strokes(&key_plan("Привет")), vec![Stroke::Text(units("Привет"))]);
    }

    #[test]
    fn long_text_is_split_into_events_of_at_most_twenty_units() {
        let text = "a".repeat(45);
        let strokes = strokes(&key_plan(&text));
        let lengths: Vec<usize> = strokes
            .iter()
            .map(|stroke| match stroke {
                Stroke::Text(units) => units.len(),
                Stroke::Return => 0,
            })
            .collect();
        assert_eq!(lengths, vec![20, 20, 5]);
    }

    #[test]
    fn surrogate_pairs_stay_in_one_event() {
        // 19 letters and an emoji: the pair would end at unit 21, so it moves to the next event.
        let text = format!("{}😀", "a".repeat(19));
        assert_eq!(
            strokes(&key_plan(&text)),
            vec![Stroke::Text(units(&"a".repeat(19))), Stroke::Text(units("😀"))]
        );
        for stroke in strokes(&key_plan(&"😀".repeat(30))) {
            let Stroke::Text(units) = stroke else { panic!("no line breaks here") };
            assert!(units.len() <= UNITS_PER_EVENT);
            assert!(!(0xDC00..=0xDFFF).contains(&units[0]), "a pair was split");
        }
    }

    #[test]
    fn line_breaks_are_the_return_key() {
        assert_eq!(
            strokes(&key_plan("a\nb")),
            vec![Stroke::Text(units("a")), Stroke::Return, Stroke::Text(units("b"))]
        );
        assert_eq!(strokes(&key_plan("\n\n")), vec![Stroke::Return, Stroke::Return]);
    }

    #[test]
    fn the_frontmost_application_is_known() {
        // Some application is always frontmost in a logged-in session.
        assert!(focus_target().is_some_and(|target| target.process_id > 0));
    }

    /// A Core Audio property of a plain type; `None` when the object does not answer.
    fn property<T: Default + Copy>(object: AudioObjectID, selector: u32, scope: u32) -> Option<T> {
        let mut address = address(selector, scope);
        let mut value = T::default();
        let mut size = size_of::<T>() as u32;
        let code = unsafe {
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&mut address),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast::<c_void>(),
            )
        };
        (code == 0).then_some(value)
    }

    fn four_cc(code: u32) -> String {
        code.to_be_bytes().iter().map(|&byte| byte as char).collect()
    }

    /// What Core Audio and cpal say about the default devices, as one line.
    fn devices_line() -> String {
        use objc2_core_audio::{
            kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioDevicePropertyNominalSampleRate,
            kAudioDevicePropertyTransportType, kAudioHardwarePropertyDefaultInputDevice,
        };
        let system = kAudioObjectSystemObject as AudioObjectID;
        let global = kAudioObjectPropertyScopeGlobal;
        let describe = |selector| {
            let Some(device) = property::<AudioObjectID>(system, selector, global) else {
                return "none".to_string();
            };
            let transport = property::<u32>(device, kAudioDevicePropertyTransportType, global)
                .map_or("?".into(), four_cc);
            let rate = property::<f64>(device, kAudioDevicePropertyNominalSampleRate, global)
                .map_or("?".into(), |rate| format!("{rate:.0}"));
            let running =
                property::<u32>(device, kAudioDevicePropertyDeviceIsRunningSomewhere, global)
                    .map_or("?".into(), |running| running.to_string());
            format!("#{device} {transport} {rate} Hz running={running}")
        };
        let cpal = crate::audio::output::current()
            .map_or("none".into(), |format| format!("{} Hz {} ch", format.rate, format.channels));
        format!(
            "output {} | input {} | cpal output {cpal}",
            describe(kAudioHardwarePropertyDefaultOutputDevice),
            describe(kAudioHardwarePropertyDefaultInputDevice)
        )
    }

    /// What a wireless headset does when its microphone opens and closes: the default devices
    /// every 10 ms, and the pauses of a silent output stream, which are the gaps in the sound.
    /// Run with the headset as both output and input and music playing.
    #[test]
    #[ignore = "opens the microphone: cargo test -- --ignored headset_timeline --nocapture"]
    fn headset_timeline() {
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        const OPEN_AT: Duration = Duration::from_millis(1500);
        const CLOSE_AT: Duration = Duration::from_millis(5500);
        const END_AT: Duration = Duration::from_millis(9500);
        const GAP_MS: u128 = 40;

        let start = Instant::now();
        let callbacks = Arc::new(Mutex::new(Vec::<u128>::new()));
        let device = cpal::default_host().default_output_device().expect("an output device");
        let config = device.default_output_config().expect("its format").config();
        let sink = Arc::clone(&callbacks);
        let stream = device
            .build_output_stream(
                config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    data.fill(0.0);
                    sink.lock().unwrap().push(start.elapsed().as_millis());
                },
                move |error| {
                    println!("{:>6} ms  output stream error: {error}", start.elapsed().as_millis())
                },
                None,
            )
            .expect("a silent output stream");
        stream.play().expect("the stream plays");

        let mut last = String::new();
        let mut recorder = None;
        let mut opened = false;
        while start.elapsed() < END_AT {
            let now = start.elapsed();
            if !opened && now >= OPEN_AT {
                opened = true;
                println!("{:>6} ms  MICROPHONE OPENS", now.as_millis());
                recorder = Some(crate::audio::capture::Recorder::start(None).expect("microphone"));
            }
            if now >= CLOSE_AT
                && let Some(recorder) = recorder.take()
            {
                println!("{:>6} ms  MICROPHONE CLOSES", now.as_millis());
                let _ = recorder.stop();
            }
            let line = devices_line();
            if line != last {
                println!("{:>6} ms  {line}", start.elapsed().as_millis());
                last = line;
            }
            thread::sleep(Duration::from_millis(10));
        }
        drop(stream);

        let times = callbacks.lock().unwrap();
        println!("--- gaps in the output longer than {GAP_MS} ms ---");
        for pair in times.windows(2) {
            if pair[1] - pair[0] > GAP_MS {
                println!("{:>6} ms  no sound for {} ms", pair[0], pair[1] - pair[0]);
            }
        }
        if let Some(&end) = times.last()
            && END_AT.as_millis().saturating_sub(end) > GAP_MS
        {
            println!("{end:>6} ms  no sound until the end");
        }
    }

    #[test]
    #[ignore = "uses the real clipboard: cargo test -- --ignored clipboard_comes_back"]
    fn clipboard_comes_back_whole() {
        let pasteboard = NSPasteboard::generalPasteboard();
        let original = save_clipboard(&pasteboard);
        let kind = unsafe { NSPasteboardTypeString };
        pasteboard.clearContents();
        let first = objc2_foundation::NSString::from_str("first");
        assert!(pasteboard.setString_forType(&first, kind));
        let saved = save_clipboard(&pasteboard);
        pasteboard.clearContents();
        let second = objc2_foundation::NSString::from_str("second");
        assert!(pasteboard.setString_forType(&second, kind));
        restore_clipboard(&pasteboard, saved);
        let back = pasteboard.stringForType(kind).map(|text| text.to_string());
        restore_clipboard(&pasteboard, original);
        assert_eq!(back.as_deref(), Some("first"));
    }

    /// Toggles mute on the default output device and restores it.
    #[test]
    #[ignore = "briefly mutes the real speakers: cargo test -- --ignored toggles_mute"]
    fn toggles_mute_on_the_default_output() {
        let output = DefaultOutput;
        let device = output.default_device().unwrap().expect("an output device");
        let before = output.muted(&device).unwrap();
        output.set_muted(&device, !before).unwrap();
        assert_eq!(output.muted(&device).unwrap(), !before);
        output.set_muted(&device, before).unwrap();
        assert_eq!(output.muted(&device).unwrap(), before);
    }

    #[test]
    fn an_output_device_that_is_gone_is_left_alone() {
        let output = DefaultOutput;
        assert!(output.muted("no-such-device-uid").unwrap());
        output.set_muted("no-such-device-uid", false).unwrap();
    }
}

//! Windows backend: text insertion with `SendInput`, the focused window, output muting.

use std::io;
use std::mem::size_of;
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, GetLastError, GlobalFree, HANDLE, HGLOBAL, HWND,
};
use windows::Win32::Graphics::Gdi::{
    DeleteEnhMetaFile, GetEnhMetaFileBits, HENHMETAFILE, SetEnhMetaFileBits,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY, TokenIntegrityLevel,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_CONTROL,
    VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RETURN, VK_RMENU,
    VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
};
use windows::core::{PCWSTR, PWSTR, w};

use super::keys::{Key, key_plan, typed_len};
use super::{FocusTarget, InsertError, Permission, PermissionKind, Permissions};
use crate::audio::mute::SystemOutput;
use crate::domain::app_rules::AppId;

/// Keys per `SendInput` call; a pause between batches keeps the target's input queue from
/// overflowing on long texts.
const BATCH: usize = 200;
const BATCH_PAUSE: Duration = Duration::from_millis(5);

/// Whether the taskbar is light ("Choose your default Windows mode" in the colors settings);
/// dark when the setting cannot be read, as by default.
pub fn taskbar_is_light() -> bool {
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    };
    status.is_ok() && value != 0
}

/// The microphone page of the privacy settings.
const MICROPHONE_SETTINGS: &str = "ms-settings:privacy-microphone";

/// One microphone switch of the privacy settings: "Allow", "Deny" or never set.
fn microphone_switch(root: HKEY, key: PCWSTR) -> Option<String> {
    let mut buffer = [0u16; 64];
    let mut size = size_of_val(&buffer) as u32;
    let status = unsafe {
        RegGetValueW(
            root,
            key,
            w!("Value"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

/// Windows has no Accessibility permission; the microphone may be turned off for the device,
/// for apps or for desktop apps.
pub fn permissions() -> Permissions {
    let apps = w!(
        r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone"
    );
    let desktop = w!(
        r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone\NonPackaged"
    );
    let switches = [
        microphone_switch(HKEY_LOCAL_MACHINE, apps),
        microphone_switch(HKEY_CURRENT_USER, apps),
        microphone_switch(HKEY_CURRENT_USER, desktop),
    ];
    Permissions {
        microphone: super::windows_microphone(switches.each_ref().map(|value| value.as_deref())),
        accessibility: Permission::NotNeeded,
    }
}

/// Windows shows no prompt for desktop apps: the switches are on the settings page.
pub fn request_permission(kind: PermissionKind) -> Option<&'static str> {
    match kind {
        PermissionKind::Microphone => Some(MICROPHONE_SETTINGS),
        PermissionKind::Accessibility => None,
    }
}

pub fn focus_target() -> Option<FocusTarget> {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_invalid() {
            return None;
        }
        let mut process_id = 0;
        GetWindowThreadProcessId(window, Some(&mut process_id));
        Some(FocusTarget { window: window.0 as isize, process_id })
    }
}

/// The program file's name, such as `Telegram.exe`, and that name without `.exe`. The name
/// is the same wherever and in whatever version the program is installed.
pub fn app_identity(target: &FocusTarget) -> Option<AppId> {
    if target.process_id == std::process::id() {
        return None;
    }
    let path = unsafe {
        let process =
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, target.process_id).ok()?;
        let mut buffer = vec![0u16; 1024];
        let mut size = buffer.len() as u32;
        let read = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(process);
        read.ok()?;
        String::from_utf16_lossy(&buffer[..size as usize])
    };
    let file = path.rsplit(['\\', '/']).next().unwrap_or_default().to_string();
    let name = match file.len().checked_sub(4) {
        Some(stem) if file.is_char_boundary(stem) && file[stem..].eq_ignore_ascii_case(".exe") => {
            file[..stem].to_string()
        }
        _ => file.clone(),
    };
    (!file.is_empty()).then_some(AppId { id: file, name })
}

/// `CF_UNICODETEXT`: the clipboard's text in UTF-16, ending with a zero.
const UNICODE_TEXT: u32 = 13;
/// `CF_BITMAP`, `CF_METAFILEPICT`, `CF_PALETTE`: handles Windows makes again from the formats
/// that are kept (`CF_DIB`, `CF_DIBV5`, `CF_ENHMETAFILE`).
const REMADE_FORMATS: [u32; 3] = [2, 3, 9];
/// `CF_ENHMETAFILE`: a picture handle, kept as its bytes.
const ENHANCED_METAFILE: u32 = 14;
/// `CF_OWNERDISPLAY`, `CF_DSPBITMAP`, `CF_DSPMETAFILEPICT`, `CF_DSPENHMETAFILE`: handles that
/// cannot be copied. So are the private formats (0x200–0x2FF) and GDI objects (0x300–0x3FF).
const HANDLE_FORMATS: [u32; 4] = [0x80, 0x82, 0x83, 0x8E];
/// OLE's own bookkeeping: programs read the clipboard's other formats when it is gone, as they
/// do after any program that does not use OLE.
const OLE_FORMATS: [&str; 2] = ["DataObject", "Ole Private Data"];
/// `'C'`.
const C_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0x43);
/// Window classes of consoles, where Ctrl+C stops the running command.
const CONSOLE_CLASSES: [&str; 4] =
    ["ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS", "mintty", "VirtualConsoleClass"];

/// Whether a password manager or a similar program marked the clipboard as not to be read: the
/// format Windows documents for clipboard monitors, the older one KeePass also sets, and the
/// clipboard history's "not this one" (any value but an explicit 1, which allows it: a value
/// that cannot be read counts as private).
pub(super) fn private_clipboard() -> bool {
    let format = |name: PCWSTR| unsafe { RegisterClipboardFormatW(name) };
    let present =
        |format: u32| format != 0 && unsafe { IsClipboardFormatAvailable(format) }.is_ok();
    if present(format(w!("ExcludeClipboardContentFromMonitorProcessing")))
        || present(format(w!("Clipboard Viewer Ignore")))
    {
        return true;
    }
    let history = format(w!("CanIncludeInClipboardHistory"));
    present(history) && clipboard_dword(history) != Some(1)
}

/// The value of a DWORD clipboard format.
fn clipboard_dword(format: u32) -> Option<u32> {
    if !open_clipboard() {
        return None;
    }
    let value = unsafe {
        (|| {
            let memory = HGLOBAL(GetClipboardData(format).ok()?.0);
            if GlobalSize(memory) < size_of::<u32>() {
                return None;
            }
            let data = GlobalLock(memory) as *const u32;
            if data.is_null() {
                return None;
            }
            let value = data.read_unaligned();
            let _ = GlobalUnlock(memory);
            Some(value)
        })()
    };
    let _ = unsafe { CloseClipboard() };
    value
}

/// Counts every change of the clipboard.
pub(super) fn clipboard_changes() -> u64 {
    u64::from(unsafe { GetClipboardSequenceNumber() })
}

/// The clipboard's formats with their data, in its order.
pub(super) struct SavedClipboard(Vec<(u32, Saved)>);

enum Saved {
    /// Memory formats: text, HTML, pictures as DIB, files, programs' own formats.
    Memory(Vec<u8>),
    /// `CF_ENHMETAFILE` as its bytes.
    Metafile(Vec<u8>),
}

/// How a clipboard format is saved.
#[derive(Debug, PartialEq, Eq)]
enum Keep {
    Memory,
    Metafile,
    /// Made again by Windows from a kept format, or OLE's bookkeeping.
    Skip,
    /// A handle that cannot be copied: such a clipboard is left alone.
    Impossible,
}

fn keep(format: u32, name: &str) -> Keep {
    match format {
        _ if REMADE_FORMATS.contains(&format) || OLE_FORMATS.contains(&name) => Keep::Skip,
        ENHANCED_METAFILE => Keep::Metafile,
        0x200..=0x3FF => Keep::Impossible,
        _ if HANDLE_FORMATS.contains(&format) => Keep::Impossible,
        _ => Keep::Memory,
    }
}

/// Opens the clipboard, waiting a little while another program has it open: the program that
/// has just copied, or a clipboard manager reading the change.
fn open_clipboard() -> bool {
    for _ in 0..20 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn format_name(format: u32) -> String {
    let mut name = [0u16; 128];
    let length = unsafe { GetClipboardFormatNameW(format, &mut name) };
    String::from_utf16_lossy(&name[..length.max(0) as usize])
}

/// Every format on the clipboard with its data; `None` when one of them could not be put back
/// or the clipboard stays busy.
pub(super) fn save_clipboard() -> Option<SavedClipboard> {
    if !open_clipboard() {
        return None;
    }
    let saved = (|| {
        let mut formats = Vec::new();
        let mut format = 0;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                return Some(SavedClipboard(formats));
            }
            let data = match keep(format, &format_name(format)) {
                Keep::Skip => continue,
                Keep::Impossible => return None,
                Keep::Memory => Saved::Memory(memory_bytes(format)?),
                Keep::Metafile => Saved::Metafile(metafile_bytes(format)?),
            };
            formats.push((format, data));
        }
    })();
    let _ = unsafe { CloseClipboard() };
    saved
}

fn memory_bytes(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let memory = HGLOBAL(GetClipboardData(format).ok()?.0);
        let size = GlobalSize(memory);
        if size == 0 {
            return Some(Vec::new());
        }
        let data = GlobalLock(memory) as *const u8;
        if data.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(data, size).to_vec();
        let _ = GlobalUnlock(memory);
        Some(bytes)
    }
}

fn metafile_bytes(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let metafile = HENHMETAFILE(GetClipboardData(format).ok()?.0);
        let size = GetEnhMetaFileBits(metafile, None);
        if size == 0 {
            return None;
        }
        let mut bytes = vec![0u8; size as usize];
        (GetEnhMetaFileBits(metafile, Some(&mut bytes)) == size).then_some(bytes)
    }
}

/// Puts back what `save_clipboard` kept, in the same order.
pub(super) fn restore_clipboard(saved: &SavedClipboard) {
    if !open_clipboard() {
        return;
    }
    unsafe {
        let _ = EmptyClipboard();
        for (format, data) in &saved.0 {
            match data {
                Saved::Memory(bytes) => {
                    // A zero-sized block would be a discarded one.
                    let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) else {
                        continue;
                    };
                    let target = GlobalLock(memory) as *mut u8;
                    if target.is_null() {
                        let _ = GlobalFree(Some(memory));
                        continue;
                    }
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
                    let _ = GlobalUnlock(memory);
                    // The clipboard owns the data once it takes it.
                    if SetClipboardData(*format, Some(HANDLE(memory.0))).is_err() {
                        let _ = GlobalFree(Some(memory));
                    }
                }
                Saved::Metafile(bytes) => {
                    let metafile = SetEnhMetaFileBits(bytes);
                    if !metafile.is_invalid()
                        && SetClipboardData(*format, Some(HANDLE(metafile.0))).is_err()
                    {
                        let _ = DeleteEnhMetaFile(Some(metafile));
                    }
                }
            }
        }
        let _ = CloseClipboard();
    }
}

pub(super) fn is_console(window: isize) -> bool {
    let mut name = [0u16; 64];
    let length = unsafe { GetClassNameW(HWND(window as *mut _), &mut name) };
    let name = String::from_utf16_lossy(&name[..length.max(0) as usize]);
    CONSOLE_CLASSES.contains(&name.as_str())
}

/// Keys that turn a typed key into another shortcut, left and right apart.
const MODIFIERS: [VIRTUAL_KEY; 8] =
    [VK_LSHIFT, VK_RSHIFT, VK_LCONTROL, VK_RCONTROL, VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN];
/// Without the extended flag Windows takes the right Ctrl and Alt for the left ones.
const EXTENDED_KEYS: [VIRTUAL_KEY; 4] = [VK_RCONTROL, VK_RMENU, VK_LWIN, VK_RWIN];
/// An unassigned key (`vkE8`), pressed before the held modifiers go up and after they are back
/// down: a modifier let go with no key pressed since it went down would open the window menu
/// (Alt) or Start (Win), or switch the keyboard layout (Alt+Shift, Ctrl+Shift).
const MASK_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

/// Presses Ctrl+C in the program in focus at once, also while the user still holds the hotkey:
/// Windows adds held keys to typed ones, so held modifiers go up for the copy and back down
/// after it. One `SendInput` call, so the user's own key presses cannot come in between.
pub(super) fn press_copy() -> bool {
    let held: Vec<VIRTUAL_KEY> = MODIFIERS.into_iter().filter(|key| any_down(&[*key])).collect();
    let inputs = copy_inputs(&held);
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    sent as usize == inputs.len()
}

/// Ctrl+C with the `held` modifiers up around it; a held Ctrl stays down and serves the copy.
fn copy_inputs(held: &[VIRTUAL_KEY]) -> Vec<INPUT> {
    let (down, up) = (false, true);
    let is_ctrl = |key: &VIRTUAL_KEY| [VK_LCONTROL, VK_RCONTROL].contains(key);
    let ctrl_held = held.iter().any(is_ctrl);
    let lifted: Vec<VIRTUAL_KEY> = held.iter().copied().filter(|key| !is_ctrl(key)).collect();
    let mask = [(MASK_KEY, down), (MASK_KEY, up)];
    let mut keys = Vec::new();
    if !lifted.is_empty() {
        keys.extend(mask);
    }
    keys.extend(lifted.iter().map(|key| (*key, up)));
    if !ctrl_held {
        keys.push((VK_CONTROL, down));
    }
    keys.extend([(C_KEY, down), (C_KEY, up)]);
    if !ctrl_held {
        keys.push((VK_CONTROL, up));
    }
    keys.extend(lifted.iter().map(|key| (*key, down)));
    if !lifted.is_empty() {
        keys.extend(mask);
    }
    keys.into_iter().map(|(key, up)| key_event(key, up)).collect()
}

fn key_event(key: VIRTUAL_KEY, up: bool) -> INPUT {
    let mut flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    if EXTENDED_KEYS.contains(&key) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: key, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

/// The clipboard's text; `None` when it holds none, another program has it open, or a password
/// manager marked it private.
pub fn clipboard_text() -> Option<String> {
    if private_clipboard() {
        return None;
    }
    unsafe {
        OpenClipboard(None).ok()?;
        let text = (|| {
            let data = GetClipboardData(UNICODE_TEXT).ok()?;
            let memory = HGLOBAL(data.0);
            // The text ends at its terminating zero, or at the end of the block when a program
            // left the zero out.
            let capacity = GlobalSize(memory) / 2;
            let units = GlobalLock(memory) as *const u16;
            if units.is_null() {
                return None;
            }
            let units = std::slice::from_raw_parts(units, capacity);
            let length = units.iter().position(|unit| *unit == 0).unwrap_or(capacity);
            let text = String::from_utf16_lossy(&units[..length]);
            let _ = GlobalUnlock(memory);
            Some(text)
        })();
        let _ = CloseClipboard();
        // A password manager may mark its copy after writing the text: checked once more.
        if private_clipboard() {
            return None;
        }
        text
    }
}

pub fn modifiers_down() -> bool {
    any_down(&[VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN])
}

/// Ctrl, Alt or Win: with one of them down typed letters act as shortcuts. A held Shift does
/// not change the Unicode characters `insert_text` sends.
pub fn shortcut_modifiers_down() -> bool {
    any_down(&[VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN])
}

fn any_down(keys: &[VIRTUAL_KEY]) -> bool {
    keys.iter().any(|key| unsafe { GetAsyncKeyState(i32::from(key.0)) } as u16 & 0x8000 != 0)
}

pub fn insert_text(text: &str, expected: Option<&FocusTarget>) -> Result<(), InsertError> {
    let target = focus_target().ok_or(InsertError::NoTarget)?;
    // Another field of the same window is still the same place.
    let moved = |other: &FocusTarget| !other.same_place(&target);
    if expected.is_some_and(moved) {
        return Err(InsertError::FocusChanged);
    }
    // Windows drops typed input to processes with a higher integrity level (UIPI, e.g. a
    // window run as administrator) and still reports success, so check before typing.
    if !accepts_our_input(target.process_id) {
        return Err(InsertError::Blocked);
    }
    let keys = key_plan(text);
    for (index, batch) in keys.chunks(BATCH).enumerate() {
        if index > 0 {
            thread::sleep(BATCH_PAUSE);
            // The user may switch windows while a long text is typed.
            if focus_target().is_none_or(|now| moved(&now)) {
                return Err(InsertError::Interrupted { typed: typed_len(text, index * BATCH) });
            }
        }
        let inputs: Vec<INPUT> = batch.iter().flat_map(|key| key_inputs(*key)).collect();
        let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            let error = unsafe { GetLastError() };
            // UIPI drops input to windows running with higher privileges.
            return Err(if error == ERROR_ACCESS_DENIED {
                InsertError::Blocked
            } else {
                InsertError::Failed(format!(
                    "SendInput sent {sent} of {} events ({error:?})",
                    inputs.len()
                ))
            });
        }
    }
    Ok(())
}

/// Medium is the level of ordinary programs; ours if Windows does not tell us.
const MEDIUM_INTEGRITY: u32 = 0x2000;

/// False when the process runs at a higher integrity level than ours, or when Windows does
/// not even let us read its level — which is what happens for elevated processes.
fn accepts_our_input(process_id: u32) -> bool {
    let ours = unsafe { integrity_level(GetCurrentProcess()) }.unwrap_or(MEDIUM_INTEGRITY);
    let theirs = unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) {
            Ok(process) => {
                let level = integrity_level(process);
                let _ = CloseHandle(process);
                level
            }
            Err(_) => None,
        }
    };
    theirs.is_some_and(|theirs| theirs <= ours)
}

/// The mandatory integrity level of a process (0x2000 medium, 0x3000 high, …).
unsafe fn integrity_level(process: HANDLE) -> Option<u32> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
        let mut size = 0;
        let _ = GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut size);
        // u64 storage keeps the label's pointer field aligned.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let read = GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        );
        let _ = CloseHandle(token);
        read.ok()?;
        let label = &*(buffer.as_ptr() as *const TOKEN_MANDATORY_LABEL);
        let count = *GetSidSubAuthorityCount(label.Label.Sid);
        Some(*GetSidSubAuthority(label.Label.Sid, u32::from(count.checked_sub(1)?)))
    }
}

/// A key's down and up events; a line break is Shift+Enter, a new line in chats, where Enter
/// sends the message.
fn key_inputs(key: Key) -> Vec<INPUT> {
    let input = |vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    };
    let none = KEYBD_EVENT_FLAGS(0);
    match key {
        Key::Unicode(unit) => vec![
            input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE),
            input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
        ],
        Key::Enter => vec![
            input(VK_SHIFT, 0, none),
            input(VK_RETURN, 0, none),
            input(VK_RETURN, 0, KEYEVENTF_KEYUP),
            input(VK_SHIFT, 0, KEYEVENTF_KEYUP),
        ],
    }
}

/// The playback devices by their endpoint id; muting starts at the default one.
pub struct DefaultOutput;

/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`: no such device.
const NOT_FOUND: u32 = 0x8007_0490;

impl DefaultOutput {
    fn with_devices<T>(
        &self,
        action: impl FnOnce(&IMMDeviceEnumerator) -> windows::core::Result<T>,
    ) -> io::Result<T> {
        unsafe {
            // Another COM mode on this thread is fine: the calls below work in either.
            let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let result = (|| {
                let enumerator: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                action(&enumerator)
            })();
            if initialized {
                CoUninitialize();
            }
            result.map_err(|error| io::Error::other(error.to_string()))
        }
    }

    /// `None` when the device is gone: unknown, unplugged or disabled.
    fn with_volume<T>(
        &self,
        device: &str,
        action: impl FnOnce(&IAudioEndpointVolume) -> windows::core::Result<T>,
    ) -> io::Result<Option<T>> {
        let id: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
        self.with_devices(|enumerator| unsafe {
            let device = match enumerator.GetDevice(PCWSTR(id.as_ptr())) {
                Ok(device) => device,
                Err(error) if error.code().0 as u32 == NOT_FOUND => return Ok(None),
                Err(error) => return Err(error),
            };
            if device.GetState()? != DEVICE_STATE_ACTIVE {
                return Ok(None);
            }
            let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
            action(&volume).map(Some)
        })
    }
}

impl SystemOutput for DefaultOutput {
    fn default_device(&self) -> io::Result<Option<String>> {
        self.with_devices(|enumerator| unsafe {
            let device = match enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                Ok(device) => device,
                // No playback device: nothing to mute.
                Err(error) if error.code().0 as u32 == NOT_FOUND => return Ok(None),
                Err(error) => return Err(error),
            };
            let id = device.GetId()?;
            let text = String::from_utf16_lossy(id.as_wide());
            CoTaskMemFree(Some(id.0 as *const std::ffi::c_void));
            Ok(Some(text))
        })
    }

    fn muted(&self, device: &str) -> io::Result<bool> {
        // A device that is gone counts as "muted" so nothing gets touched.
        Ok(self
            .with_volume(device, |volume| unsafe { volume.GetMute() }.map(|muted| muted.as_bool()))?
            .unwrap_or(true))
    }

    fn set_muted(&self, device: &str, muted: bool) -> io::Result<()> {
        self.with_volume(device, |volume| unsafe { volume.SetMute(muted, std::ptr::null()) })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_become_down_and_up_events() {
        let unicode = key_inputs(Key::Unicode(0x0416));
        unsafe {
            assert_eq!(unicode.len(), 2);
            assert_eq!(unicode[0].Anonymous.ki.wScan, 0x0416);
            assert_eq!(unicode[0].Anonymous.ki.dwFlags, KEYEVENTF_UNICODE);
            assert_eq!(unicode[1].Anonymous.ki.dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
        }
        let enter = key_inputs(Key::Enter);
        let keys: Vec<_> = enter
            .iter()
            .map(|input| unsafe { (input.Anonymous.ki.wVk, input.Anonymous.ki.dwFlags) })
            .collect();
        assert_eq!(
            keys,
            [
                (VK_SHIFT, KEYBD_EVENT_FLAGS(0)),
                (VK_RETURN, KEYBD_EVENT_FLAGS(0)),
                (VK_RETURN, KEYEVENTF_KEYUP),
                (VK_SHIFT, KEYEVENTF_KEYUP),
            ],
            "a line break is Shift+Enter"
        );
    }

    /// Each event as (key, released, extended).
    fn copy_events(held: &[VIRTUAL_KEY]) -> Vec<(VIRTUAL_KEY, bool, bool)> {
        copy_inputs(held)
            .iter()
            .map(|input| {
                let ki = unsafe { input.Anonymous.ki };
                let flag = |flag: KEYBD_EVENT_FLAGS| ki.dwFlags.0 & flag.0 != 0;
                (ki.wVk, flag(KEYEVENTF_KEYUP), flag(KEYEVENTF_EXTENDEDKEY))
            })
            .collect()
    }

    const DOWN: bool = false;
    const UP: bool = true;
    const MASK: [(VIRTUAL_KEY, bool, bool); 2] = [(MASK_KEY, DOWN, false), (MASK_KEY, UP, false)];

    #[test]
    fn clipboard_formats_are_kept_left_to_windows_or_stop_the_copy() {
        for (format, name) in [(13, ""), (15, ""), (8, ""), (17, ""), (0xC0F0, "HTML Format")] {
            assert_eq!(keep(format, name), Keep::Memory, "{format:#x} {name}");
        }
        assert_eq!(keep(14, ""), Keep::Metafile);
        for (format, name) in [(2, ""), (3, ""), (9, ""), (0xC009, "DataObject")] {
            assert_eq!(keep(format, name), Keep::Skip, "{format:#x} {name}");
        }
        assert_eq!(keep(0xC013, "Ole Private Data"), Keep::Skip);
        for format in [0x80, 0x82, 0x83, 0x8E, 0x200, 0x2FF, 0x300, 0x3FF] {
            assert_eq!(keep(format, ""), Keep::Impossible, "{format:#x}");
        }
    }

    #[test]
    fn copy_is_plain_ctrl_c_when_no_modifier_is_held() {
        assert_eq!(
            copy_events(&[]),
            [
                (VK_CONTROL, DOWN, false),
                (C_KEY, DOWN, false),
                (C_KEY, UP, false),
                (VK_CONTROL, UP, false),
            ]
        );
    }

    #[test]
    fn held_modifiers_go_up_for_the_copy_and_back_down() {
        let middle = [
            (VK_LSHIFT, UP, false),
            (VK_CONTROL, DOWN, false),
            (C_KEY, DOWN, false),
            (C_KEY, UP, false),
            (VK_CONTROL, UP, false),
            (VK_LSHIFT, DOWN, false),
        ];
        assert_eq!(copy_events(&[VK_LSHIFT]), [&MASK[..], &middle, &MASK].concat());
    }

    #[test]
    fn a_held_ctrl_serves_the_copy_and_right_keys_stay_extended() {
        let middle = [
            (VK_RMENU, UP, true),
            (C_KEY, DOWN, false),
            (C_KEY, UP, false),
            (VK_RMENU, DOWN, true),
        ];
        assert_eq!(copy_events(&[VK_RCONTROL, VK_RMENU]), [&MASK[..], &middle, &MASK].concat());
    }

    #[test]
    fn our_own_process_accepts_our_input() {
        assert!(accepts_our_input(std::process::id()));
    }

    #[test]
    fn unknown_processes_are_treated_as_blocked() {
        assert!(!accepts_our_input(u32::MAX - 3));
    }

    #[test]
    fn system_process_is_blocked() {
        // PID 4 is always the Windows "System" process, above any user program.
        assert!(!accepts_our_input(4));
    }

    /// Toggles mute on the default playback device and restores it.
    #[test]
    #[ignore = "briefly mutes the real speakers: cargo test -- --ignored toggles_mute"]
    fn toggles_mute_on_the_default_output() {
        let output = DefaultOutput;
        let device = output.default_device().unwrap().expect("a playback device");
        let before = output.muted(&device).unwrap();
        output.set_muted(&device, !before).unwrap();
        assert_eq!(output.muted(&device).unwrap(), !before);
        output.set_muted(&device, before).unwrap();
        assert_eq!(output.muted(&device).unwrap(), before);
    }

    #[test]
    fn a_playback_device_that_is_gone_is_left_alone() {
        let output = DefaultOutput;
        let gone = "{0.0.0.00000000}.{00000000-0000-0000-0000-000000000000}";
        assert!(output.muted(gone).unwrap());
        output.set_muted(gone, false).unwrap();
    }
}

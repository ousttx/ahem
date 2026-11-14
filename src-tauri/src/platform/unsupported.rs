//! Backend for systems other than Windows and macOS (Linux): no insertion, selection, secret
//! storage or permissions; the app runs, but dictation cannot type into other programs.

use std::io;

use super::{FocusTarget, InsertError, Permission, PermissionKind, Permissions, SelectionError};
use crate::audio::mute::SystemOutput;
use crate::domain::app_rules::AppId;

pub fn protect(_data: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "secret storage is not implemented on this OS"))
}

pub fn unprotect(_data: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "secret storage is not implemented on this OS"))
}

pub fn rewrite_selection(
    _target: &FocusTarget,
    _rewrite: &mut dyn FnMut(&str) -> Result<String, SelectionError>,
) -> Result<(), SelectionError> {
    Err(SelectionError::Unsupported)
}

pub fn focus_target() -> Option<FocusTarget> {
    None
}

pub fn selected_text(_target: &FocusTarget) -> Option<String> {
    None
}

pub fn clipboard_text() -> Option<String> {
    None
}

pub fn app_identity(_target: &FocusTarget) -> Option<AppId> {
    None
}

pub fn modifiers_down() -> bool {
    false
}

pub fn shortcut_modifiers_down() -> bool {
    false
}

/// The GNOME top bar is dark.
pub fn taskbar_is_light() -> bool {
    false
}

pub fn insert_text(_text: &str, _expected: Option<&FocusTarget>) -> Result<(), InsertError> {
    Err(InsertError::Unsupported)
}

pub struct DefaultOutput;

impl SystemOutput for DefaultOutput {
    fn default_device(&self) -> io::Result<Option<String>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "muting is not implemented on this OS"))
    }

    fn muted(&self, _device: &str) -> io::Result<bool> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "muting is not implemented on this OS"))
    }

    fn set_muted(&self, _device: &str, _muted: bool) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "muting is not implemented on this OS"))
    }
}

/// Linux asks no permission for recording or typing.
pub fn permissions() -> Permissions {
    Permissions { microphone: Permission::NotNeeded, accessibility: Permission::NotNeeded }
}

pub fn request_permission(_kind: PermissionKind) -> Option<&'static str> {
    None
}

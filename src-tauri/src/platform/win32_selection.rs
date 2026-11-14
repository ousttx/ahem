//! Reading and replacing the selected text with UI Automation:
//! no password fields, exactly one contiguous editable range, and before replacing the
//! element, the range and the text must still be the same as when they were read.

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomation2, IUIAutomationElement, IUIAutomationTextPattern,
    IUIAutomationTextRange, UIA_IsReadOnlyAttributeId, UIA_TextPatternId,
};
use windows::core::Interface;

use super::win32::{
    clipboard_changes, clipboard_text, focus_target, insert_text, is_console, press_copy,
    private_clipboard, restore_clipboard, save_clipboard,
};
use super::{FocusTarget, SelectionError};

struct Selection {
    element: IUIAutomationElement,
    range: IUIAutomationTextRange,
    text: String,
}

/// Runs the whole read → `rewrite` → replace cycle on the calling thread (UI Automation
/// objects stay on one thread). `rewrite` gets the selected text and returns its
/// replacement; it may take long (a language model call).
pub fn rewrite_selection(
    target: &FocusTarget,
    rewrite: &mut dyn FnMut(&str) -> Result<String, SelectionError>,
) -> Result<(), SelectionError> {
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let result = (|| {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(failed)?;
            // Never move focus, and do not wait forever for a hung application.
            if let Ok(bounded) = automation.cast::<IUIAutomation2>() {
                let _ = bounded.SetAutoSetFocus(false);
                let _ = bounded.SetConnectionTimeout(1500);
                let _ = bounded.SetTransactionTimeout(2000);
            }
            let before = read(&automation, target)?;
            let replacement = rewrite(&before.text)?;
            let after = read(&automation, target)?;
            let same_element =
                automation.CompareElements(&before.element, &after.element).map_err(failed)?;
            let same_range = before.range.Compare(&after.range).map_err(failed)?;
            if !same_element.as_bool() || !same_range.as_bool() || before.text != after.text {
                return Err(SelectionError::Changed);
            }
            insert_text(&super::with_selected_edges(&before.text, &replacement), Some(target))
                .map_err(SelectionError::Insert)
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

/// The selected text only, for a mode's context: also from read-only fields, all ranges
/// joined, never from a password field. Programs whose selection UI Automation does not see
/// (Telegram, many web pages) get it copied with Ctrl+C instead.
pub fn selected_text(target: &FocusTarget) -> Option<String> {
    match automation_selection(target) {
        Found::Text(text) => Some(text),
        Found::Forbidden => None,
        Found::Unseen => copied_selection(target),
    }
}

/// What UI Automation found.
enum Found {
    Text(String),
    /// A password field, or the focus moved: neither read nor copied.
    Forbidden,
    /// No selection it can see: the program may still have one.
    Unseen,
}

/// The selection as Ctrl+C copies it; the clipboard gets every earlier format back. Not in
/// consoles, where Ctrl+C stops the running command, and not while the clipboard holds a
/// handle that could not be put back (a program's private format) or what a password manager
/// marked private.
fn copied_selection(target: &FocusTarget) -> Option<String> {
    if is_console(target.window) || private_clipboard() {
        return None;
    }
    // Saving can take seconds (programs render their formats), so focus is checked after it.
    let saved = save_clipboard()?;
    if !focus_target().is_some_and(|now| now.same_place(target)) {
        return None;
    }
    super::wait_for_copy(clipboard_changes, clipboard_text, press_copy, move || {
        restore_clipboard(&saved)
    })
}

fn automation_selection(target: &FocusTarget) -> Found {
    // One more than the context keeps, so a cut is still marked.
    let limit = i32::try_from(crate::domain::context::CONTEXT_LIMIT + 1).unwrap_or(i32::MAX);
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let selection = (|| {
            if !focus_target().is_some_and(|now| now.same_place(target)) {
                return Found::Forbidden;
            }
            let automation: windows::core::Result<IUIAutomation> =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
            let Ok(automation) = automation else { return Found::Unseen };
            if let Ok(bounded) = automation.cast::<IUIAutomation2>() {
                let _ = bounded.SetAutoSetFocus(false);
                let _ = bounded.SetConnectionTimeout(1500);
                let _ = bounded.SetTransactionTimeout(2000);
            }
            let Ok(element) = automation.GetFocusedElement() else { return Found::Unseen };
            // No answer counts as a password: never read, never copied.
            let not_password =
                element.CurrentIsPassword().is_ok_and(|password| !password.as_bool());
            if !not_password {
                return Found::Forbidden;
            }
            let Ok(pattern) =
                element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            else {
                return Found::Unseen;
            };
            let Ok(ranges) = pattern.GetSelection() else { return Found::Unseen };
            let mut parts = Vec::new();
            for index in 0..ranges.Length().unwrap_or(0) {
                if let Ok(text) = ranges.GetElement(index).and_then(|range| range.GetText(limit)) {
                    parts.push(text.to_string());
                }
            }
            let text = parts.join("\n");
            if text.trim().is_empty() { Found::Unseen } else { Found::Text(text) }
        })();
        if initialized {
            CoUninitialize();
        }
        selection
    }
}

unsafe fn read(
    automation: &IUIAutomation,
    target: &FocusTarget,
) -> Result<Selection, SelectionError> {
    unsafe {
        if !focus_target().is_some_and(|now| now.same_place(target)) {
            return Err(SelectionError::FocusChanged);
        }
        let element = automation.GetFocusedElement().map_err(|_| SelectionError::NoFocus)?;
        if element.CurrentIsPassword().map_err(failed)?.as_bool() {
            return Err(SelectionError::Password);
        }
        let pattern: IUIAutomationTextPattern = element
            .GetCurrentPatternAs(UIA_TextPatternId)
            .map_err(|_| SelectionError::NotSupported)?;
        let ranges = pattern.GetSelection().map_err(|_| SelectionError::NotSupported)?;
        match ranges.Length().map_err(failed)? {
            0 => return Err(SelectionError::NoSelection),
            1 => {}
            _ => return Err(SelectionError::SeveralRanges),
        }
        let range = ranges.GetElement(0).map_err(failed)?.Clone().map_err(failed)?;
        // Only a definite "not read-only" allows replacing.
        let read_only = range.GetAttributeValue(UIA_IsReadOnlyAttributeId).map_err(failed)?;
        if bool::try_from(&read_only).unwrap_or(true) {
            return Err(SelectionError::ReadOnly);
        }
        let text = range.GetText(-1).map_err(failed)?.to_string();
        if text.trim().is_empty() {
            return Err(SelectionError::NoSelection);
        }
        Ok(Selection { element, range, text })
    }
}

fn failed(error: windows::core::Error) -> SelectionError {
    SelectionError::Failed(error.message())
}

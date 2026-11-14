//! Reading and replacing the selected text with the Accessibility API, by the same rules as
//! `win32_selection.rs`: no password fields, exactly one selected range in an editable field,
//! and before replacing, the field, the range and the text must still be the same as when
//! they were read. The replacement is typed over the selection.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_app_kit::NSPasteboard;
use objc2_application_services::{AXError, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{CFArray, CFHash, CFRange, CFRetained, CFString};

use super::macos::{
    accessibility_allowed, application, attribute, clipboard_changes, clipboard_text,
    element_attribute, focus_target, insert_text, keys_left, press_copy, private_clipboard,
    request_accessibility, restore_clipboard, save_clipboard, string_attribute,
};
use super::{FocusTarget, SelectionError};

/// What the focused field told us, before any rule is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    password: bool,
    /// `None` when the field does not expose its selected text.
    text: Option<String>,
    ranges: usize,
    editable: bool,
}

/// The rules, separate from the Accessibility calls so they can be tested.
fn check(snapshot: Snapshot) -> Result<String, SelectionError> {
    if snapshot.password {
        return Err(SelectionError::Password);
    }
    let text = snapshot.text.ok_or(SelectionError::NotSupported)?;
    if snapshot.ranges > 1 {
        return Err(SelectionError::SeveralRanges);
    }
    if text.trim().is_empty() {
        return Err(SelectionError::NoSelection);
    }
    // Only a definite "editable" allows replacing.
    if !snapshot.editable {
        return Err(SelectionError::ReadOnly);
    }
    Ok(text)
}

struct Selection {
    element: isize,
    range: (isize, isize),
    text: String,
}

/// Runs the whole read → `rewrite` → replace cycle. `rewrite` gets the selected text and
/// returns its replacement; it may take long (a language model call).
pub fn rewrite_selection(
    target: &FocusTarget,
    rewrite: &mut dyn FnMut(&str) -> Result<String, SelectionError>,
) -> Result<(), SelectionError> {
    if !accessibility_allowed() {
        request_accessibility();
        return Err(SelectionError::NoPermission);
    }
    let before = read(target)?;
    let replacement = rewrite(&before.text)?;
    let after = read(target)?;
    if before.element != after.element || before.range != after.range || before.text != after.text {
        return Err(SelectionError::Changed);
    }
    insert_text(&super::with_selected_edges(&before.text, &replacement), Some(target))
        .map_err(SelectionError::Insert)
}

/// The selected text only, for a mode's context: also from read-only fields and several
/// ranges, never from a password field. `None` without the permission (no prompt), when focus
/// moved or nothing is selected.
///
/// Programs that do not tell Accessibility their selection (Telegram, most web pages) get it
/// copied with ⌘C instead, and the clipboard is put back as it was.
pub fn selected_text(target: &FocusTarget) -> Option<String> {
    if !accessibility_allowed() || !focus_target().is_some_and(|now| now.same_place(target)) {
        return None;
    }
    let element = element_attribute(&application(target.process_id), "AXFocusedUIElement");
    if let Some(element) = &element {
        let role = string_attribute(element, "AXRole");
        let subrole = string_attribute(element, "AXSubrole");
        if [role.as_deref(), subrole.as_deref()].contains(&Some("AXSecureTextField")) {
            return None;
        }
        let text = string_attribute(element, "AXSelectedText");
        if text.as_deref().is_some_and(|text| !text.trim().is_empty()) {
            return text;
        }
    }
    copied_selection(target)
}

/// The selection as ⌘C copies it; the clipboard gets its earlier items back. `None` when the
/// program copied nothing (no selection) in time. A clipboard a password manager marked private
/// is left alone.
fn copied_selection(target: &FocusTarget) -> Option<String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    if private_clipboard(&pasteboard) {
        return None;
    }
    // Saving makes programs hand over their delayed data and can take a while: ⌘C goes only to
    // the program that still takes the keys.
    let saved = save_clipboard(&pasteboard);
    if keys_left(target) {
        return None;
    }
    // What ⌘C copies is read like any clipboard: a password manager's copy (KeePassXC copies
    // the entry's password) is not taken.
    super::wait_for_copy(clipboard_changes, clipboard_text, press_copy, move || {
        restore_clipboard(&NSPasteboard::generalPasteboard(), saved)
    })
}

fn read(target: &FocusTarget) -> Result<Selection, SelectionError> {
    if !focus_target().is_some_and(|now| now.same_place(target)) {
        return Err(SelectionError::FocusChanged);
    }
    let element = element_attribute(&application(target.process_id), "AXFocusedUIElement")
        .ok_or(SelectionError::NoFocus)?;
    let role = string_attribute(&element, "AXRole");
    let subrole = string_attribute(&element, "AXSubrole");
    let snapshot = Snapshot {
        password: [role.as_deref(), subrole.as_deref()].contains(&Some("AXSecureTextField")),
        text: string_attribute(&element, "AXSelectedText"),
        ranges: attribute(&element, "AXSelectedTextRanges")
            .and_then(|ranges| ranges.downcast::<CFArray>().ok())
            .map_or(1, |ranges| ranges.count().max(0) as usize),
        editable: settable(&element, "AXSelectedText") || settable(&element, "AXValue"),
    };
    let text = check(snapshot)?;
    let range = selected_range(&element).ok_or(SelectionError::NotSupported)?;
    Ok(Selection { element: CFHash(Some(element.as_ref())) as isize, range, text })
}

fn settable(element: &AXUIElement, name: &str) -> bool {
    let name = CFString::from_str(name);
    let mut settable = 0u8;
    let error = unsafe { element.is_attribute_settable(&name, NonNull::from(&mut settable)) };
    error == AXError::Success && settable != 0
}

fn selected_range(element: &AXUIElement) -> Option<(isize, isize)> {
    let value: CFRetained<AXValue> =
        attribute(element, "AXSelectedTextRange")?.downcast::<AXValue>().ok()?;
    let mut range = CFRange { location: 0, length: 0 };
    let read =
        unsafe { value.value(AXValueType::CFRange, NonNull::from(&mut range).cast::<c_void>()) };
    read.then_some((range.location, range.length))
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use objc2_app_kit::NSPasteboardTypeString;

    use super::*;

    /// A walk through what a mode's context gets from the program in front: switch to it and
    /// select some text within the delay. Prints each step; the clipboard is checked to come back.
    /// `AHEM_DELAY` sets the seconds to wait (8 by default).
    #[test]
    #[ignore = "reads the real selection: cargo test -- --ignored diagnose_selection --nocapture"]
    fn diagnose_selection() {
        let seconds = std::env::var("AHEM_DELAY").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
        println!("Switch to the program and select text: reading in {seconds} s…");
        thread::sleep(Duration::from_secs(seconds));
        let preview = |text: &str| {
            let short: String = text.chars().take(60).collect();
            format!("{} chars: {short:?}", text.chars().count())
        };
        println!("1. Accessibility allowed: {}", accessibility_allowed());
        let Some(target) = focus_target() else { return println!("2. No program in front.") };
        let app = crate::platform::app_identity(&target);
        println!("2. In front: {app:?} (window {})", target.window);
        let application = application(target.process_id);
        match element_attribute(&application, "AXFocusedUIElement") {
            None => println!("3. Focused element: none"),
            Some(element) => {
                let role = string_attribute(&element, "AXRole");
                let subrole = string_attribute(&element, "AXSubrole");
                println!("3. Focused element: role {role:?}, subrole {subrole:?}");
                match string_attribute(&element, "AXSelectedText") {
                    Some(text) => println!("4. Accessibility selection: {}", preview(&text)),
                    None => println!("4. Accessibility selection: none"),
                }
            }
        }
        let pasteboard = NSPasteboard::generalPasteboard();
        let kind = unsafe { NSPasteboardTypeString };
        let before = pasteboard.stringForType(kind).map(|text| text.to_string());
        let count = pasteboard.changeCount();
        let copied = copied_selection(&target);
        match &copied {
            Some(text) => println!("5. Copied with Cmd+C: {}", preview(text)),
            None => println!("5. Copied with Cmd+C: nothing"),
        }
        let after = pasteboard.stringForType(kind).map(|text| text.to_string());
        println!(
            "6. Clipboard back as before: {} (changes: {})",
            before == after,
            pasteboard.changeCount() - count
        );
        match selected_text(&target) {
            Some(text) => println!("7. What the mode gets: {}", preview(&text)),
            None => println!("7. What the mode gets: nothing"),
        }
    }

    fn field(text: &str) -> Snapshot {
        Snapshot { password: false, text: Some(text.into()), ranges: 1, editable: true }
    }

    #[test]
    fn one_editable_selection_is_read() {
        assert_eq!(check(field(" some words ")), Ok(" some words ".to_string()));
    }

    #[test]
    fn password_fields_are_never_read() {
        assert_eq!(
            check(Snapshot { password: true, ..field("secret") }),
            Err(SelectionError::Password)
        );
    }

    #[test]
    fn fields_without_selected_text_are_not_supported() {
        assert_eq!(check(Snapshot { text: None, ..field("") }), Err(SelectionError::NotSupported));
    }

    #[test]
    fn empty_or_blank_selection_is_no_selection() {
        assert_eq!(check(field("")), Err(SelectionError::NoSelection));
        assert_eq!(check(field("  \n")), Err(SelectionError::NoSelection));
    }

    #[test]
    fn several_ranges_are_refused() {
        assert_eq!(check(Snapshot { ranges: 2, ..field("a") }), Err(SelectionError::SeveralRanges));
    }

    #[test]
    fn read_only_text_is_refused() {
        assert_eq!(
            check(Snapshot { editable: false, ..field("a") }),
            Err(SelectionError::ReadOnly)
        );
    }
}

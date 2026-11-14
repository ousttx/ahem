//! Global hotkeys: parsing and the stored form. Registration with press and release events
//! goes through the Tauri `global-shortcut` plugin (`commands::register_hotkeys`).

use std::fmt;

use tauri_plugin_global_shortcut::{Modifiers, Shortcut};

pub use crate::platform::DEFAULT_HOTKEY;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidHotkey(pub String);

impl fmt::Display for InvalidHotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid hotkey: {}", self.0)
    }
}

impl std::error::Error for InvalidHotkey {}

/// A hotkey, refused when it is a key that types or moves the cursor with no modifier: taken
/// for the whole system, it would be gone from typing in every program (Tab, Enter, a letter).
pub fn parse(text: &str) -> Result<Shortcut, InvalidHotkey> {
    let shortcut = text.parse::<Shortcut>().map_err(|error| InvalidHotkey(error.to_string()))?;
    if shortcut.mods.is_empty() && types(&shortcut.key.to_string()) {
        return Err(InvalidHotkey(format!("{} needs a modifier", shortcut.key)));
    }
    Ok(shortcut)
}

/// Whether a key (by its `KeyboardEvent.code` name) types text or moves through it.
fn types(code: &str) -> bool {
    const KEYS: [&str; 24] = [
        "Space",
        "Enter",
        "Tab",
        "Backspace",
        "Delete",
        "Escape",
        "ArrowUp",
        "ArrowDown",
        "ArrowLeft",
        "ArrowRight",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "Minus",
        "Equal",
        "BracketLeft",
        "BracketRight",
        "Backslash",
        "Semicolon",
        "Quote",
        "Backquote",
        "Comma",
        "Period",
    ];
    ["Key", "Digit", "Numpad"].iter().any(|prefix| code.starts_with(prefix))
        || code == "Slash"
        || KEYS.contains(&code)
}

/// The form kept in settings: the parser's own spelling, so equal hotkeys compare equal.
pub fn canonical(text: &str) -> Result<String, InvalidHotkey> {
    Ok(parse(text)?.into_string())
}

/// Whether holding the hotkey keeps Ctrl, Alt or Win down, so text typed meanwhile would act
/// as shortcuts. Shift alone is safe; an unparsable hotkey counts as unsafe.
pub fn holds_shortcut_modifier(text: &str) -> bool {
    let shortcut = Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER | Modifiers::META;
    parse(text).map_or(true, |hotkey| hotkey.mods.intersects(shortcut))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_current_app_hotkeys_parse() {
        for text in [DEFAULT_HOTKEY, "Shift+Z", "Shift+X", "Ctrl+Shift+F9"] {
            assert!(parse(text).is_ok(), "{text}");
        }
    }

    #[test]
    fn a_typing_key_alone_is_no_hotkey() {
        for alone in ["Tab", "Enter", "Space", "Backspace", "KeyA", "Digit1", "Escape", "ArrowUp"] {
            assert!(parse(alone).is_err(), "{alone}");
        }
        // The extra keys of ISO and JIS keyboards (§ or < beside the left Shift, ろ, ¥) are no
        // hotkey keys at all.
        for key in ["IntlBackslash", "IntlRo", "IntlYen"] {
            assert!(parse(&format!("Alt+{key}")).is_err(), "{key}");
        }
        for fine in ["F9", "Shift+KeyZ", "Control+Space", "Alt+Enter", "CapsLock"] {
            assert!(parse(fine).is_ok(), "{fine}");
        }
    }

    #[test]
    fn canonical_form_is_stable() {
        let once = canonical("ctrl+alt+space").unwrap();
        assert_eq!(canonical(&once).unwrap(), once);
        assert_eq!(once, canonical("Ctrl+Alt+Space").unwrap());
        let default = canonical(DEFAULT_HOTKEY).unwrap();
        assert_eq!(canonical(&default).unwrap(), default);
    }

    #[test]
    fn browser_key_codes_parse() {
        // The interface builds hotkeys from `KeyboardEvent.code`.
        for text in ["Control+Alt+KeyZ", "Shift+Digit1", "Control+F9", "Alt+Backquote"] {
            assert!(parse(text).is_ok(), "{text}");
        }
        assert!(canonical(DEFAULT_HOTKEY).unwrap().ends_with("alt+Space"));
        assert_eq!(canonical("Shift+Z").unwrap(), "shift+KeyZ");
    }

    #[test]
    fn only_ctrl_alt_and_win_make_typing_unsafe() {
        for safe in ["shift+KeyZ", "F9", "Shift+F9", "CapsLock"] {
            assert!(!holds_shortcut_modifier(safe), "{safe}");
        }
        for unsafe_ in ["control+alt+Space", "Alt+Backquote", "Super+KeyD", "Ctrl+Shift+F9", "?"] {
            assert!(holds_shortcut_modifier(unsafe_), "{unsafe_}");
        }
    }

    #[test]
    fn invalid_hotkeys_are_rejected() {
        for text in ["", "Ctrl+", "Banana+Q", "Ctrl+Alt"] {
            assert!(parse(text).is_err(), "{text:?} accepted");
        }
    }
}

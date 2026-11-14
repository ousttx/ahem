//! Context for a language-model mode: what is around the dictation, so the model can answer an
//! email or go on with a thought. Each source is off until the user ticks it in the mode; the
//! sources are read at the hotkey press. The context goes to the model only: never to the
//! history, the last transcription or a log.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The most characters taken from one source; the rest is cut off.
pub const CONTEXT_LIMIT: usize = 12_000;

/// What a mode lets the model see besides the dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextSources {
    /// The text selected in the program in focus; never a password field.
    pub selection: bool,
    /// The text in the clipboard; read only with this on.
    pub clipboard: bool,
    /// The name of the program in focus.
    pub app: bool,
}

impl ContextSources {
    /// Whether something has to be read from the OS: the program's name is known anyway.
    pub fn reads(&self) -> bool {
        self.selection || self.clipboard
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Context {
    pub app: Option<String>,
    pub selection: Option<String>,
    pub clipboard: Option<String>,
}

/// No context: what a mode without ticked sources sends.
pub static NO_CONTEXT: Context = Context { app: None, selection: None, clipboard: None };

impl Context {
    pub fn is_empty(&self) -> bool {
        self.app.is_none() && self.selection.is_none() && self.clipboard.is_none()
    }
}

/// Never shows the texts, so they cannot reach a log.
impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let chars = |text: &Option<String>| text.as_ref().map(|text| text.chars().count());
        f.debug_struct("Context")
            .field("app", &self.app.is_some())
            .field("selection_chars", &chars(&self.selection))
            .field("clipboard_chars", &chars(&self.clipboard))
            .finish()
    }
}

/// A source's text ready for the model: without the spaces around, `None` when empty, and at
/// most `CONTEXT_LIMIT` characters with a mark where it was cut.
pub fn clip(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    match text.char_indices().nth(CONTEXT_LIMIT) {
        None => Some(text.to_string()),
        Some((end, _)) => Some(format!("{} […]", text[..end].trim_end())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_trims_drops_empty_and_cuts_long_text() {
        assert_eq!(clip("  hello \n"), Some("hello".into()));
        assert_eq!(clip(" \n\t"), None);
        let exact = "я".repeat(CONTEXT_LIMIT);
        assert_eq!(clip(&exact), Some(exact.clone()));
        let long = clip(&format!("{exact}лишнее")).unwrap();
        assert_eq!(long, format!("{exact} […]"));
    }

    #[test]
    fn sources_are_off_by_default_and_read_from_old_files() {
        let none = ContextSources::default();
        assert!(!none.selection && !none.clipboard && !none.app);
        let sources: ContextSources = serde_json::from_str(r#"{"clipboard":true}"#).unwrap();
        assert!(sources.reads() && !sources.selection && !sources.app);
        let app_only = ContextSources { app: true, ..ContextSources::default() };
        assert!(!app_only.reads());
    }

    #[test]
    fn debug_never_shows_the_texts() {
        let context = Context {
            app: Some("Mail".into()),
            selection: Some("secret letter".into()),
            clipboard: None,
        };
        let shown = format!("{context:?}");
        assert!(!shown.contains("secret") && !shown.contains("Mail"));
        assert!(shown.contains("selection_chars: Some(13)"));
    }
}

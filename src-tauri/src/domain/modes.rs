//! Dictation modes: plain dictation (always there), Whisper's translation to English (there only
//! when the user turns it on; the interface shows it as a hotkey of plain dictation) and the
//! user's language-model modes. A mode may have its own dictation hotkey; language-model modes
//! may also have a hotkey that rewrites the selected text. The main hotkey dictates in
//! `main_mode_id`.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::context::ContextSources;

pub const PLAIN: &str = "plain";
pub const ENGLISH: &str = "english";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    /// Chat Completions at a custom base URL: DeepSeek, Groq, OpenRouter, Ollama, LM Studio…
    OpenAiCompatible,
    /// Speech recognition only: no mode can use it.
    Deepgram,
    /// ChatGPT through the user's account: language models on their Plus or Pro plan, no key
    /// (`engines::chatgpt`).
    #[serde(rename = "chatgpt_account")]
    ChatGptAccount,
}

/// A service for language models, speech recognition or both. Its key lives in the secret
/// store under `id`, never here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub provider: ProviderKind,
    pub base_url: Option<String>,
    /// `••••abcd`, or `None` for a service without a key (a local server).
    pub key_hint: Option<String>,
    pub added_at_ms: i64,
    /// From the provider's model list when the connection was added or refreshed; empty for a
    /// speech-only service.
    pub models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModeAction {
    /// Insert the transcript as it is.
    Plain,
    /// Whisper translates the speech to English while recognizing it.
    TranslateToEnglish,
    /// A language model rewrites the transcript by the instruction.
    Process {
        connection_id: String,
        model: String,
        instruction: String,
        /// `None` keeps the language of the speech.
        output_language: Option<String>,
        /// What the model sees besides the dictation; nothing by default.
        #[serde(default)]
        context: ContextSources,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mode {
    pub id: String,
    /// Empty for the built-in modes: the interface shows their translated names.
    pub name: String,
    pub action: ModeAction,
    /// Dictate with this mode.
    pub hotkey: Option<String>,
    /// Rewrite the selected text with this mode (language-model modes only).
    pub selection_hotkey: Option<String>,
}

impl Mode {
    pub fn processes(&self) -> bool {
        matches!(self.action, ModeAction::Process { .. })
    }
}

fn builtin(id: &str, action: ModeAction) -> Mode {
    Mode { id: id.into(), name: String::new(), action, hotkey: None, selection_hotkey: None }
}

/// Plain dictation, which every settings file has.
pub fn plain_mode() -> Mode {
    builtin(PLAIN, ModeAction::Plain)
}

/// The modes every settings file has.
pub fn builtin_modes() -> Vec<Mode> {
    vec![plain_mode()]
}

/// Translation to English, added when the user turns it on.
pub fn english_mode() -> Mode {
    builtin(ENGLISH, ModeAction::TranslateToEnglish)
}

/// A mode that uses the connection; such a connection cannot be removed.
pub fn mode_using<'a>(modes: &'a [Mode], connection_id: &str) -> Option<&'a Mode> {
    modes.iter().find(|mode| {
        matches!(&mode.action, ModeAction::Process { connection_id: id, .. } if id == connection_id)
    })
}

/// What a registered hotkey does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    /// The main hotkey: always the main mode.
    Main,
    /// The "by application" hotkey: the mode the user chose for the program in focus, else the
    /// main mode.
    ByApp,
    Dictate {
        mode_id: String,
    },
    Rewrite {
        mode_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModesError {
    DuplicateId(String),
    MissingBuiltin(String),
    BuiltinChanged(String),
    UnknownMainMode(String),
    EmptyName(String),
    UnknownConnection(String),
    /// The mode's service recognizes speech only.
    NoLanguageModels(String),
    EmptyInstruction(String),
    EmptyModel(String),
    SelectionWithoutProcessing(String),
    /// Two actions share one hotkey.
    HotkeyClash(String),
}

impl fmt::Display for ModesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "duplicate mode {id}"),
            Self::MissingBuiltin(id) => write!(f, "built-in mode {id} is missing"),
            Self::BuiltinChanged(id) => write!(f, "built-in mode {id} cannot change its action"),
            Self::UnknownMainMode(id) => write!(f, "main mode {id} does not exist"),
            Self::EmptyName(id) => write!(f, "mode {id} has no name"),
            Self::UnknownConnection(id) => write!(f, "mode {id} uses a missing connection"),
            Self::NoLanguageModels(id) => {
                write!(f, "mode {id} uses a service without language models")
            }
            Self::EmptyInstruction(id) => write!(f, "mode {id} has no instruction"),
            Self::EmptyModel(id) => write!(f, "mode {id} has no model"),
            Self::SelectionWithoutProcessing(id) => {
                write!(f, "mode {id} cannot rewrite selected text without a language model")
            }
            Self::HotkeyClash(hotkey) => write!(f, "hotkey {hotkey} is used twice"),
        }
    }
}

impl std::error::Error for ModesError {}

/// Checks the modes and returns every hotkey to register with its action; `main_hotkey`
/// dictates in `main_mode_id`. `translates`: whether the dictation model translates to English;
/// without it the translation hotkey is left out, so its combination stays free (it is still
/// checked for clashes). Hotkeys must already be in canonical form.
pub fn hotkey_actions(
    modes: &[Mode],
    main_mode_id: &str,
    main_hotkey: &str,
    app_hotkey: Option<&str>,
    connections: &[Connection],
    translates: bool,
) -> Result<Vec<(String, HotkeyAction)>, ModesError> {
    let mut ids = HashSet::new();
    for mode in modes {
        if !ids.insert(mode.id.as_str()) {
            return Err(ModesError::DuplicateId(mode.id.clone()));
        }
    }
    for (builtin, required) in [(plain_mode(), true), (english_mode(), false)] {
        match modes.iter().find(|mode| mode.id == builtin.id) {
            None if required => return Err(ModesError::MissingBuiltin(builtin.id)),
            Some(mode) if mode.action != builtin.action => {
                return Err(ModesError::BuiltinChanged(builtin.id));
            }
            _ => {}
        }
    }
    if !ids.contains(main_mode_id) {
        return Err(ModesError::UnknownMainMode(main_mode_id.into()));
    }
    let mut actions = vec![(main_hotkey.to_string(), HotkeyAction::Main)];
    actions.extend(app_hotkey.map(|hotkey| (hotkey.to_string(), HotkeyAction::ByApp)));
    for mode in modes {
        let builtin = mode.id == PLAIN || mode.id == ENGLISH;
        if !builtin && mode.name.trim().is_empty() {
            return Err(ModesError::EmptyName(mode.id.clone()));
        }
        if let ModeAction::Process { connection_id, model, instruction, .. } = &mode.action {
            match connections.iter().find(|connection| connection.id == *connection_id) {
                None => return Err(ModesError::UnknownConnection(mode.id.clone())),
                Some(connection) if connection.provider == ProviderKind::Deepgram => {
                    return Err(ModesError::NoLanguageModels(mode.id.clone()));
                }
                Some(_) => {}
            }
            if instruction.trim().is_empty() {
                return Err(ModesError::EmptyInstruction(mode.id.clone()));
            }
            if model.trim().is_empty() {
                return Err(ModesError::EmptyModel(mode.id.clone()));
            }
        } else if mode.selection_hotkey.is_some() {
            return Err(ModesError::SelectionWithoutProcessing(mode.id.clone()));
        }
        if let Some(hotkey) = &mode.hotkey {
            actions.push((hotkey.clone(), HotkeyAction::Dictate { mode_id: mode.id.clone() }));
        }
        if let Some(hotkey) = &mode.selection_hotkey {
            actions.push((hotkey.clone(), HotkeyAction::Rewrite { mode_id: mode.id.clone() }));
        }
    }
    let mut seen = HashSet::new();
    for (hotkey, _) in &actions {
        if !seen.insert(hotkey.as_str()) {
            return Err(ModesError::HotkeyClash(hotkey.clone()));
        }
    }
    if !translates {
        let english =
            modes.iter().find(|mode| mode.id == ENGLISH).and_then(|mode| mode.hotkey.clone());
        actions.retain(|(hotkey, _)| Some(hotkey) != english.as_ref());
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        Connection {
            id: "c1".into(),
            name: "Claude".into(),
            provider: ProviderKind::Anthropic,
            base_url: None,
            key_hint: Some("••••abcd".into()),
            added_at_ms: 0,
            models: vec!["claude".into()],
        }
    }

    fn email() -> Mode {
        Mode {
            id: "email".into(),
            name: "Email".into(),
            action: ModeAction::Process {
                connection_id: "c1".into(),
                model: "claude".into(),
                instruction: "Write it as a polite email".into(),
                output_language: None,
                context: ContextSources::default(),
            },
            hotkey: Some("control+alt+KeyE".into()),
            selection_hotkey: Some("control+alt+KeyR".into()),
        }
    }

    fn modes() -> Vec<Mode> {
        let mut modes = builtin_modes();
        modes.push(english_mode());
        modes.push(email());
        modes
    }

    #[test]
    fn hotkeys_map_to_actions() {
        let actions =
            hotkey_actions(&modes(), "email", "control+alt+Space", None, &[connection()], true)
                .unwrap();
        assert_eq!(
            actions,
            vec![
                ("control+alt+Space".into(), HotkeyAction::Main),
                ("control+alt+KeyE".into(), HotkeyAction::Dictate { mode_id: "email".into() }),
                ("control+alt+KeyR".into(), HotkeyAction::Rewrite { mode_id: "email".into() }),
            ]
        );
    }

    #[test]
    fn the_by_app_hotkey_has_its_own_action_and_combination() {
        let actions =
            hotkey_actions(&builtin_modes(), PLAIN, "F9", Some("F10"), &[], true).unwrap();
        assert_eq!(
            actions,
            vec![("F9".into(), HotkeyAction::Main), ("F10".into(), HotkeyAction::ByApp)]
        );
        assert_eq!(
            hotkey_actions(&builtin_modes(), PLAIN, "F9", Some("F9"), &[], true),
            Err(ModesError::HotkeyClash("F9".into()))
        );
    }

    #[test]
    fn built_in_modes_must_stay() {
        let only_email = vec![email()];
        assert_eq!(
            hotkey_actions(&only_email, "email", "F9", None, &[connection()], true),
            Err(ModesError::MissingBuiltin(PLAIN.into()))
        );
        let mut changed = modes();
        changed[1].action = ModeAction::Plain;
        assert_eq!(
            hotkey_actions(&changed, PLAIN, "F9", None, &[connection()], true),
            Err(ModesError::BuiltinChanged(ENGLISH.into()))
        );
    }

    #[test]
    fn translation_to_english_is_optional() {
        let mut english = english_mode();
        english.hotkey = Some("control+alt+KeyT".into());
        let mut with_english = builtin_modes();
        with_english.push(english);
        assert_eq!(
            hotkey_actions(&with_english, PLAIN, "F9", None, &[], true).unwrap(),
            vec![
                ("F9".into(), HotkeyAction::Main),
                ("control+alt+KeyT".into(), HotkeyAction::Dictate { mode_id: ENGLISH.into() }),
            ]
        );
        assert_eq!(
            hotkey_actions(&builtin_modes(), PLAIN, "F9", None, &[], true).unwrap(),
            vec![("F9".into(), HotkeyAction::Main)]
        );
    }

    #[test]
    fn translation_hotkey_is_off_while_the_model_does_not_translate() {
        let mut english = english_mode();
        english.hotkey = Some("control+alt+KeyT".into());
        let mut modes = builtin_modes();
        modes.push(english);
        assert_eq!(
            hotkey_actions(&modes, PLAIN, "F9", None, &[], false).unwrap(),
            vec![("F9".into(), HotkeyAction::Main)]
        );
        // Its combination still cannot be taken twice.
        modes[1].hotkey = Some("F9".into());
        assert_eq!(
            hotkey_actions(&modes, PLAIN, "F9", None, &[], false),
            Err(ModesError::HotkeyClash("F9".into()))
        );
    }

    #[test]
    fn broken_modes_are_rejected() {
        let check =
            |modes: &[Mode]| hotkey_actions(modes, PLAIN, "F9", None, &[connection()], true);
        let mut clash = modes();
        clash[2].selection_hotkey = Some("control+alt+KeyE".into());
        assert_eq!(check(&clash), Err(ModesError::HotkeyClash("control+alt+KeyE".into())));
        let mut main_clash = modes();
        main_clash[2].hotkey = Some("F9".into());
        assert_eq!(check(&main_clash), Err(ModesError::HotkeyClash("F9".into())));
        let mut plain_selection = modes();
        plain_selection[0].selection_hotkey = Some("F10".into());
        assert_eq!(
            check(&plain_selection),
            Err(ModesError::SelectionWithoutProcessing(PLAIN.into()))
        );
        let mut no_connection = modes();
        if let ModeAction::Process { connection_id, .. } = &mut no_connection[2].action {
            *connection_id = "gone".into();
        }
        assert_eq!(check(&no_connection), Err(ModesError::UnknownConnection("email".into())));
        let speech_only = Connection { provider: ProviderKind::Deepgram, ..connection() };
        assert_eq!(
            hotkey_actions(&modes(), PLAIN, "F9", None, &[speech_only], true),
            Err(ModesError::NoLanguageModels("email".into()))
        );
        let mut nameless = modes();
        nameless[2].name = " ".into();
        assert_eq!(check(&nameless), Err(ModesError::EmptyName("email".into())));
        assert_eq!(
            hotkey_actions(&modes(), "nope", "F9", None, &[connection()], true),
            Err(ModesError::UnknownMainMode("nope".into()))
        );
        let mut twice = modes();
        twice.push(email());
        assert_eq!(check(&twice), Err(ModesError::DuplicateId("email".into())));
    }

    #[test]
    fn finds_the_mode_that_uses_a_connection() {
        assert_eq!(mode_using(&modes(), "c1").map(|mode| mode.id.as_str()), Some("email"));
        assert_eq!(mode_using(&modes(), "c2"), None);
        assert_eq!(mode_using(&builtin_modes(), "c1"), None);
    }
}

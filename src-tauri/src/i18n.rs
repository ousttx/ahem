//! Strings for what the OS draws (the tray menu), taken from the web interface's translation
//! files so there is one source of text.

use serde_json::Value;

use crate::storage::settings::UiLanguage;

const EN: &str = include_str!("../../src/i18n/locales/en.json");
const RU: &str = include_str!("../../src/i18n/locales/ru.json");

/// Mirrors `pickLanguage` in `src/i18n/index.ts`: the first OS locale with a supported primary
/// language, English otherwise.
pub fn language(choice: UiLanguage, locales: &[String]) -> &'static str {
    match choice {
        UiLanguage::En => "en",
        UiLanguage::Ru => "ru",
        UiLanguage::System => locales
            .iter()
            .filter_map(|locale| {
                let primary = locale.split(['-', '_', '.']).next()?.to_ascii_lowercase();
                ["en", "ru"].into_iter().find(|supported| *supported == primary)
            })
            .next()
            .unwrap_or("en"),
    }
}

pub struct Strings {
    texts: Value,
    english: Value,
}

impl Strings {
    pub fn new(language: &str) -> Self {
        let parse = |text: &str| -> Value {
            serde_json::from_str(text).expect("translation files are JSON")
        };
        let english = parse(EN);
        let texts = if language == "ru" { parse(RU) } else { english.clone() };
        Self { texts, english }
    }

    /// `key` is dotted, as in the web interface; a missing key falls back to English.
    pub fn get(&self, key: &str) -> String {
        let find = |root: &Value| {
            key.split('.').try_fold(root, |value, part| value.get(part))?.as_str().map(String::from)
        };
        find(&self.texts).or_else(|| find(&self.english)).unwrap_or_else(|| key.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_follows_the_choice_or_the_os() {
        let locales = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(language(UiLanguage::Ru, &locales(&["en-US"])), "ru");
        assert_eq!(language(UiLanguage::System, &locales(&["de-DE", "ru_RU.UTF-8"])), "ru");
        assert_eq!(language(UiLanguage::System, &locales(&["EN-gb"])), "en");
        assert_eq!(language(UiLanguage::System, &locales(&["zh-CN"])), "en");
    }

    #[test]
    fn strings_come_from_the_translation_files() {
        assert_eq!(Strings::new("ru").get("tray.quit"), "Выход");
        assert_eq!(Strings::new("en").get("tray.quit"), "Quit");
        assert_eq!(Strings::new("en").get("modes.builtin.plain"), "Plain dictation");
        assert_eq!(Strings::new("en").get("no.such.key"), "no.such.key");
    }
}

//! Whisper's languages: the code and the English name that `whisper-server` reports.
//! Copied from `g_lang` in whisper.cpp 1.9.5 (`src/whisper.cpp`, MIT).

const LANGUAGES: &[(&str, &str)] = &[
    ("en", "english"),
    ("zh", "chinese"),
    ("de", "german"),
    ("es", "spanish"),
    ("ru", "russian"),
    ("ko", "korean"),
    ("fr", "french"),
    ("ja", "japanese"),
    ("pt", "portuguese"),
    ("tr", "turkish"),
    ("pl", "polish"),
    ("ca", "catalan"),
    ("nl", "dutch"),
    ("ar", "arabic"),
    ("sv", "swedish"),
    ("it", "italian"),
    ("id", "indonesian"),
    ("hi", "hindi"),
    ("fi", "finnish"),
    ("vi", "vietnamese"),
    ("he", "hebrew"),
    ("uk", "ukrainian"),
    ("el", "greek"),
    ("ms", "malay"),
    ("cs", "czech"),
    ("ro", "romanian"),
    ("da", "danish"),
    ("hu", "hungarian"),
    ("ta", "tamil"),
    ("no", "norwegian"),
    ("th", "thai"),
    ("ur", "urdu"),
    ("hr", "croatian"),
    ("bg", "bulgarian"),
    ("lt", "lithuanian"),
    ("la", "latin"),
    ("mi", "maori"),
    ("ml", "malayalam"),
    ("cy", "welsh"),
    ("sk", "slovak"),
    ("te", "telugu"),
    ("fa", "persian"),
    ("lv", "latvian"),
    ("bn", "bengali"),
    ("sr", "serbian"),
    ("az", "azerbaijani"),
    ("sl", "slovenian"),
    ("kn", "kannada"),
    ("et", "estonian"),
    ("mk", "macedonian"),
    ("br", "breton"),
    ("eu", "basque"),
    ("is", "icelandic"),
    ("hy", "armenian"),
    ("ne", "nepali"),
    ("mn", "mongolian"),
    ("bs", "bosnian"),
    ("kk", "kazakh"),
    ("sq", "albanian"),
    ("sw", "swahili"),
    ("gl", "galician"),
    ("mr", "marathi"),
    ("pa", "punjabi"),
    ("si", "sinhala"),
    ("km", "khmer"),
    ("sn", "shona"),
    ("yo", "yoruba"),
    ("so", "somali"),
    ("af", "afrikaans"),
    ("oc", "occitan"),
    ("ka", "georgian"),
    ("be", "belarusian"),
    ("tg", "tajik"),
    ("sd", "sindhi"),
    ("gu", "gujarati"),
    ("am", "amharic"),
    ("yi", "yiddish"),
    ("lo", "lao"),
    ("uz", "uzbek"),
    ("fo", "faroese"),
    ("ht", "haitian creole"),
    ("ps", "pashto"),
    ("tk", "turkmen"),
    ("nn", "nynorsk"),
    ("mt", "maltese"),
    ("sa", "sanskrit"),
    ("lb", "luxembourgish"),
    ("my", "myanmar"),
    ("bo", "tibetan"),
    ("tl", "tagalog"),
    ("mg", "malagasy"),
    ("as", "assamese"),
    ("tt", "tatar"),
    ("haw", "hawaiian"),
    ("ln", "lingala"),
    ("ha", "hausa"),
    ("ba", "bashkir"),
    ("jw", "javanese"),
    ("su", "sundanese"),
    ("yue", "cantonese"),
];

/// `(code, English name)` of every language Whisper knows.
pub fn all() -> &'static [(&'static str, &'static str)] {
    LANGUAGES
}

/// Names other engines give a language of the table: Qwen3-ASR says Filipino for Tagalog.
const OTHER_NAMES: [(&str, &str); 1] = [("tl", "filipino")];

/// `"english"` → `"en"`; a value that already is a code is returned as is.
pub fn code_for(name: &str) -> Option<&'static str> {
    let name = name.trim().to_lowercase();
    LANGUAGES
        .iter()
        .chain(&OTHER_NAMES)
        .find(|(code, full)| *full == name || *code == name)
        .map(|(code, _)| *code)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn names_become_codes() {
        assert_eq!(code_for("english"), Some("en"));
        assert_eq!(code_for("Russian"), Some("ru"));
        assert_eq!(code_for("haitian creole"), Some("ht"));
        assert_eq!(code_for("yue"), Some("yue"));
        assert_eq!(code_for("Filipino"), Some("tl"));
        assert_eq!(code_for("klingon"), None);
    }

    #[test]
    fn table_has_the_hundred_whisper_languages() {
        assert_eq!(LANGUAGES.len(), 100);
        let codes: HashSet<_> = LANGUAGES.iter().map(|(code, _)| code).collect();
        let names: HashSet<_> = LANGUAGES.iter().map(|(_, name)| name).collect();
        assert_eq!(codes.len(), 100);
        assert_eq!(names.len(), 100);
    }
}

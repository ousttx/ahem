//! Clean text for plain dictation: filler sounds removed without a model, or the whole text
//! cleaned up by a language model with a built-in instruction.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use super::replacements::{Replacement, Replacer};

/// How plain dictation cleans the recognized text.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum CleanText {
    /// The text as it is recognized.
    Off,
    /// Filler sounds removed, without a model.
    #[default]
    Fillers,
    /// Filler sounds removed, then a language model cleans up the rest.
    Model { connection_id: String, model: String },
}

/// What the language model is asked to do; the system prompt already keeps the meaning and
/// forbids adding anything. The text stays in one paragraph unless a list or paragraphs are
/// dictated: a dictation is mostly a message or a sentence, where breaks were not meant.
pub const INSTRUCTION: &str = r#"Clean up this dictated text so it reads as the speaker meant to write it.

Remove:
- filler sounds, and filler words that carry no meaning in the sentence (uh, um, э-э, ну, типа, like, you know); keep such words where they carry meaning;
- false starts, stutters and accidental repetitions; keep repetitions made for emphasis;
- what the speaker took back when correcting themselves ("at two, no, at three", "I mean", "scratch that", "нет, точнее", "вернее", "то есть"): keep only the final version. Such words stay when they do not correct anything.

Fix punctuation, capitalization and sentence breaks. Make only the smallest grammar fixes that the removals require.

Keep everything else as spoken: the words and their order, the language or mix of languages, names, numbers, terms, tone and formality. Do not summarize, shorten, rephrase, translate or add anything.

Keep the text in one paragraph, unless the speaker clearly dictates separate paragraphs or a list ("first… second…", "the list is…"): then put each item on its own line, starting with "- " or the number the speaker used. A sentence that names several things is not a list."#;

/// The words removed until the user changes the list (`Settings::fillers`): sounds that are
/// never words in any language the engines write with these letters.
pub const DEFAULT_FILLERS: &str =
    "uh, uhh, uhm, umm, erm, hmm, hm, э, ээ, эээ, э-э, э-э-э, эм, эмм, мм, ммм, м-м";
/// English fillers that are words elsewhere ("um" in German and Portuguese, "er" in German):
/// removed from English speech only, unless the user emptied the list.
const ENGLISH_FILLERS: &str = "um, er";

/// The fillers as replacements that remove them. `words` is the user's list, separated by
/// commas or new lines; the English-only ones join it when the speech is English.
pub fn filler_rules(words: &str, english: bool) -> Vec<Replacement> {
    let removal = |from: &str| Replacement { from: from.into(), to: String::new() };
    let mut rules = vec![removal(words)];
    if english && !words.trim().is_empty() {
        rules.push(removal(ENGLISH_FILLERS));
    }
    rules
}

/// The text without the fillers, with the commas and pauses written around them, and a
/// capital letter kept at the start of a sentence.
pub fn remove_fillers(text: &str, words: &str, english: bool) -> String {
    let rules = filler_rules(words, english);
    let replacer = Replacer::new(&rules);
    let mut text = text.to_string();
    // A sound joined to a word by a hyphen is part of it: "мм-да".
    let joined = |text: &str, range: &Range<usize>| {
        text[..range.start].ends_with('-') || text[range.end..].starts_with('-')
    };
    while let Some(found) =
        replacer.find(&text).into_iter().find(|found| !joined(&text, &found.range))
    {
        text = cut(&text, found.range);
    }
    text
}

fn cut(text: &str, range: Range<usize>) -> String {
    const SPACES: [char; 2] = [' ', '\t'];
    let before = &text[..range.start];
    let after = &text[range.end..];
    let trimmed = before.trim_end_matches(SPACES);
    let comma_before = trimmed.ends_with(',');
    let pause_after = [",", "…", "..."].into_iter().find(|mark| after.starts_with(mark));
    let sentence_start = trimmed.is_empty() || trimmed.ends_with(['.', '!', '?', '…', '\n']);
    let capital = text[range].chars().next().is_some_and(char::is_uppercase);
    let without_comma = || &trimmed[..trimmed.len() - 1];
    let (head, mut tail) = match (comma_before, pause_after) {
        // "я, э-э, думаю" → "я думаю"
        (true, Some(mark)) => (without_comma(), &after[mark.len()..]),
        // "Э-э, я думаю" → "я думаю"; "и э-э, вот" → "и вот"
        (false, Some(mark)) => (before, after[mark.len()..].trim_start_matches(SPACES)),
        // "думаю, э-э." → "думаю."
        (true, None) => (without_comma(), after),
        // "привет э-э мир" → "привет мир"
        (false, None) => (trimmed, after),
    };
    if sentence_start {
        // "Хорошо. Э-э. Дальше" → "Хорошо. Дальше"
        tail = tail.strip_prefix(['.', '!', '?']).unwrap_or(tail);
    }
    if head.trim_end_matches(SPACES).is_empty() || head.ends_with('\n') {
        tail = tail.trim_start_matches(SPACES);
    }
    let mut out = head.to_string();
    let mut rest = tail.chars();
    match rest.next() {
        Some(first) if sentence_start && capital => out.extend(first.to_uppercase()),
        Some(first) => out.push(first),
        None => {}
    }
    out.extend(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(text: &str) -> String {
        remove_fillers(text, DEFAULT_FILLERS, false)
    }

    #[test]
    fn fillers_go_with_the_commas_around_them() {
        assert_eq!(clean("Э-э, я думаю, что да."), "Я думаю, что да.");
        assert_eq!(clean("я, э-э, думаю"), "я думаю");
        assert_eq!(clean("я, э-э думаю"), "я думаю");
        assert_eq!(clean("и э-э, вот"), "и вот");
        assert_eq!(clean("Ну что, эм?"), "Ну что?");
        assert_eq!(clean("привет ммм мир"), "привет мир");
        assert_eq!(clean("Хорошо. Э-э. Дальше"), "Хорошо. Дальше");
        assert_eq!(clean("Привет. Эм, как дела?"), "Привет. Как дела?");
        assert_eq!(clean("раз\nэ-э два"), "раз\nдва");
        assert_eq!(clean("Uh, hmm... so we start"), "So we start");
        assert_eq!(clean("Э-э."), "");
    }

    #[test]
    fn words_and_parts_of_words_stay() {
        assert_eq!(clean("ну типа like так"), "ну типа like так");
        assert_eq!(
            clean("эмоции, хмель, humble, umbrella, эмм-да"),
            "эмоции, хмель, humble, umbrella, эмм-да"
        );
        assert_eq!(clean("мама мыла раму"), "мама мыла раму");
    }

    #[test]
    fn um_and_er_go_only_from_english_speech() {
        assert_eq!(
            remove_fillers("Wir treffen uns um acht, er kommt", DEFAULT_FILLERS, false),
            "Wir treffen uns um acht, er kommt"
        );
        assert_eq!(remove_fillers("Um, I think, er, yes", DEFAULT_FILLERS, true), "I think yes");
    }

    #[test]
    fn the_user_list_is_removed_as_whole_words() {
        let words = "ну, типа\nкак бы";
        assert_eq!(
            remove_fillers("Ну, я типа нужно как бы сделать", words, false),
            "Я нужно сделать"
        );
        assert_eq!(remove_fillers("Э-э, ну", words, false), "Э-э", "only the user's list");
        assert_eq!(
            remove_fillers("Um, э-э, hi", "", true),
            "Um, э-э, hi",
            "an empty list removes nothing"
        );
    }

    #[test]
    fn the_interface_restores_the_same_basic_set() {
        let frontend = include_str!("../../../src/settings.ts");
        assert!(frontend.contains(&format!("DEFAULT_FILLERS = \"{DEFAULT_FILLERS}\"")));
        assert!(frontend.contains(&format!("CLEAN_INSTRUCTION = `{INSTRUCTION}`")));
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(clean(""), "");
        assert_eq!(clean("Всё чисто."), "Всё чисто.");
    }

    #[test]
    fn the_setting_is_read_by_level() {
        let model = CleanText::Model { connection_id: "groq-1".into(), model: "llama".into() };
        let json = serde_json::to_value(&model).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "level": "model", "connection_id": "groq-1", "model": "llama" })
        );
        assert_eq!(serde_json::from_value::<CleanText>(json).unwrap(), model);
        assert_eq!(
            serde_json::to_value(CleanText::default()).unwrap(),
            serde_json::json!({ "level": "fillers" })
        );
    }
}

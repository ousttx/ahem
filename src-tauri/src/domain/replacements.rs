//! The user's replacements: a phrase as the engine writes it → the text to put instead. They
//! run on the recognized text, so they work with every engine, also the ones that ignore the
//! dictionary. One entry may list several ways the engine writes the same thing, separated by
//! commas or new lines. A phrase matches in any case and only as whole words; a space in it
//! stands for any run of spaces in the text.

use std::ops::Range;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replacement {
    /// What the engine writes, such as `эй ай`; variants are separated by commas or new lines,
    /// such as `эй ай, ей яй`.
    pub from: String,
    /// What goes in instead: a correction or a whole snippet over several lines. Empty removes
    /// the phrase.
    pub to: String,
}

/// One phrase found in a text: its bytes there and what replaces them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<'a> {
    pub range: Range<usize>,
    pub to: &'a str,
}

/// The replacements ready for matching.
pub struct Replacer<'a> {
    /// Phrases with single spaces, longest first; among equal lengths the user's order.
    rules: Vec<(Vec<char>, &'a str)>,
}

enum Fit {
    /// The whole phrase, up to this byte.
    Whole(usize),
    /// The text ends inside the phrase.
    Partial,
    No,
}

impl<'a> Replacer<'a> {
    pub fn new(replacements: &'a [Replacement]) -> Self {
        let mut rules: Vec<(Vec<char>, &str)> = replacements
            .iter()
            .flat_map(|item| variants(&item.from).map(|phrase| (phrase, item.to.as_str())))
            .collect();
        rules.sort_by_key(|(phrase, _)| std::cmp::Reverse(phrase.len()));
        Self { rules }
    }

    pub fn apply(&self, text: &str) -> String {
        render(text, &self.find(text))
    }

    /// The phrases in `text`, left to right; where several start at one place, the longest.
    pub fn find(&self, text: &str) -> Vec<Found<'a>> {
        let mut found = Vec::new();
        let mut before = None;
        let mut position = 0;
        while let Some(next) = text[position..].chars().next() {
            match self.whole_at(text, position, before) {
                Some(item) => {
                    before = text[item.range.clone()].chars().next_back();
                    position = item.range.end;
                    found.push(item);
                }
                None => {
                    before = Some(next);
                    position += next.len_utf8();
                }
            }
        }
        found
    }

    /// The part of a growing text, such as a streaming model's text so far, that can be typed
    /// now: replaced, and cut before the first place where a phrase may still be completing.
    /// Its end waits for the next words, so what is typed never needs to change: the spaces,
    /// commas and dots at the end wait too, as a removed phrase after them takes them along.
    pub fn settled(&self, text: &str) -> String {
        let held = match self.pending(text) {
            Some(cut) => &text[..cut],
            None => text,
        };
        // Commas and dots at the end wait too: a removed phrase after them may take them along.
        let held = held.trim_end().trim_end_matches([',', '.', '…']).trim_end();
        self.apply(held).trim_end().to_string()
    }

    fn whole_at(&self, text: &str, position: usize, before: Option<char>) -> Option<Found<'a>> {
        self.rules.iter().find_map(|(phrase, to)| {
            if !starts_word(phrase, before) {
                return None;
            }
            match fit(text, position, phrase) {
                Fit::Whole(end) if ends_word(phrase, text[end..].chars().next()) => {
                    Some(Found { range: position..end, to })
                }
                _ => None,
            }
        })
    }

    /// Where the first phrase starts that the text ends inside of, or that ends with the text:
    /// its last word may still grow.
    fn pending(&self, text: &str) -> Option<usize> {
        let mut before = None;
        for (position, next) in text.char_indices() {
            let open = self.rules.iter().any(|(phrase, _)| {
                starts_word(phrase, before)
                    && match fit(text, position, phrase) {
                        Fit::Partial => true,
                        Fit::Whole(end) => end == text.len(),
                        Fit::No => false,
                    }
            });
            if open {
                return Some(position);
            }
            before = Some(next);
        }
        None
    }
}

/// The text with the found phrases replaced. A removed phrase (an empty replacement: a filler
/// sound, a word the user drops) takes the spaces and commas written around it along, as
/// `clean_text::remove_fillers` does: "Hello, uh, world." → "Hello world.", "Э-э, я думаю." →
/// "Я думаю."; a sound joined to a word by a hyphen ("мм-да") stays.
pub fn render(text: &str, found: &[Found]) -> String {
    render_after("", false, text, found).0
}

/// [`render`] of `text` that follows `before` (a file's earlier words, as rendered), which is
/// read — where a sentence starts — and never changed; `capital`: a removed phrase before it
/// began a sentence and left its capital to give. Also says whether the word after `text`
/// should take such a capital.
pub fn render_after(before: &str, capital: bool, text: &str, found: &[Found]) -> (String, bool) {
    let mut render = Render {
        before,
        out: String::with_capacity(text.len()),
        replaced: 0,
        tail: Tail { capital, ..Tail::default() },
    };
    let mut position = 0;
    for item in found {
        let (start, end) = (item.range.start, item.range.end);
        if item.to.is_empty() && (text[..start].ends_with('-') || text[end..].starts_with('-')) {
            continue;
        }
        render.piece(&text[position..start]);
        if item.to.is_empty() {
            render.remove(&text[start..end], &text[end..]);
        } else {
            render.replace(item.to);
        }
        position = end;
    }
    render.piece(&text[position..]);
    (render.out, render.tail.capital)
}

const SPACES: [char; 2] = [' ', '\t'];
const PAUSES: [&str; 3] = [",", "…", "..."];

struct Render<'a> {
    /// The text before, read only.
    before: &'a str,
    out: String,
    /// Where the last replacement's text ends in `out`: a comma before that is its own, not
    /// one written before a removed phrase.
    replaced: usize,
    tail: Tail,
}

/// What a removed phrase asks of the text after it, in this order.
#[derive(Default)]
struct Tail {
    /// The comma or pause written after it, in bytes.
    pause: usize,
    /// Spaces left over after the pause.
    trim_pause: bool,
    /// It stood alone as a sentence: the full stop after it goes.
    sentence: bool,
    /// Nothing is before it on its line: the spaces after it go, until something is written.
    trim_line: bool,
    /// It began a sentence with a capital: the next word takes it, once something is written.
    capital: bool,
}

impl Render<'_> {
    /// Text of the source between phrases.
    fn piece(&mut self, piece: &str) {
        let tail = &mut self.tail;
        let mut piece = piece.get(std::mem::take(&mut tail.pause)..).unwrap_or_default();
        if std::mem::take(&mut tail.trim_pause) {
            piece = piece.trim_start_matches(SPACES);
        }
        if std::mem::take(&mut tail.sentence) {
            piece = piece.strip_prefix(['.', '!', '?']).unwrap_or(piece);
        }
        self.write(piece);
    }

    /// A phrase's replacement: a removed phrase before it asks it for nothing but its line's
    /// spaces and the capital.
    fn replace(&mut self, to: &str) {
        self.tail.pause = 0;
        self.tail.trim_pause = false;
        self.tail.sentence = false;
        self.write(to);
        self.replaced = self.out.len();
    }

    fn write(&mut self, piece: &str) {
        let tail = &mut self.tail;
        let piece = if tail.trim_line { piece.trim_start_matches(SPACES) } else { piece };
        if piece.is_empty() {
            return;
        }
        // Something is written: the rest of the line keeps its spaces.
        tail.trim_line = false;
        let text = piece.trim_start_matches(SPACES);
        self.out.push_str(&piece[..piece.len() - text.len()]);
        let mut chars = text.chars();
        let Some(first) = chars.next() else { return };
        if std::mem::take(&mut tail.capital) {
            self.out.extend(first.to_uppercase());
        } else {
            self.out.push(first);
        }
        self.out.push_str(chars.as_str());
    }

    /// Takes a removed `phrase` out, with the spaces and the comma before it as the text
    /// `after` it calls for; notes what the text after it should lose.
    fn remove(&mut self, phrase: &str, after: &str) {
        let out = &mut self.out;
        let trimmed = out.trim_end_matches(SPACES).len();
        let comma_before = trimmed > self.replaced && out[..trimmed].ends_with(',');
        let pause = PAUSES.into_iter().find(|mark| after.starts_with(mark));
        // Where it stands: after what was written, else after the text before.
        let context =
            if trimmed > 0 { &out[..trimmed] } else { self.before.trim_end_matches(SPACES) };
        let sentence = context.is_empty() || context.ends_with(['.', '!', '?', '…', '\n']);
        let line = context.is_empty() || context.ends_with('\n');
        match (comma_before, pause) {
            // "я, э-э, думаю" → "я думаю"; "думаю, э-э." → "думаю."
            (true, _) => out.truncate(trimmed - 1),
            // "и э-э, вот" → "и вот": the spaces before it stay, the ones after the pause go.
            (false, Some(_)) => {}
            // "привет э-э мир" → "привет мир"
            (false, None) => out.truncate(trimmed),
        }
        let line_start =
            if out.trim_end_matches(SPACES).is_empty() { line } else { out.ends_with('\n') };
        // A capital not yet given (several removed in a row) is kept.
        let capital = sentence
            && (phrase.chars().next().is_some_and(char::is_uppercase) || self.tail.capital);
        self.tail = Tail {
            pause: pause.map_or(0, str::len),
            trim_pause: !comma_before && pause.is_some(),
            sentence,
            trim_line: line_start,
            capital,
        };
    }
}

/// The file the replacements are exported to and imported from.
#[derive(Serialize, Deserialize)]
struct ReplacementsFile {
    replacements: Vec<Replacement>,
}

pub fn to_file(replacements: &[Replacement]) -> String {
    let file = ReplacementsFile { replacements: replacements.to_vec() };
    let mut text = serde_json::to_string_pretty(&file).expect("replacements serialize");
    text.push('\n');
    text
}

/// The replacements of an exported file; entries without a phrase are left out.
pub fn from_file(text: &str) -> Result<Vec<Replacement>, String> {
    let file: ReplacementsFile = serde_json::from_str(text).map_err(|error| error.to_string())?;
    Ok(file.replacements.into_iter().filter(|item| variants(&item.from).next().is_some()).collect())
}

/// The phrases of one entry: its variants with single spaces, empty ones left out.
fn variants(from: &str) -> impl Iterator<Item = Vec<char>> + '_ {
    from.split([',', '\n']).map(phrase).filter(|phrase| !phrase.is_empty())
}

fn phrase(variant: &str) -> Vec<char> {
    variant.split_whitespace().collect::<Vec<_>>().join(" ").chars().collect()
}

/// A phrase that begins with a letter or digit does not start inside a word.
fn starts_word(phrase: &[char], before: Option<char>) -> bool {
    !phrase[0].is_alphanumeric() || !before.is_some_and(char::is_alphanumeric)
}

fn ends_word(phrase: &[char], after: Option<char>) -> bool {
    !phrase[phrase.len() - 1].is_alphanumeric() || !after.is_some_and(char::is_alphanumeric)
}

fn fit(text: &str, start: usize, phrase: &[char]) -> Fit {
    let mut chars = text[start..].char_indices().peekable();
    for &wanted in phrase {
        let Some(&(_, next)) = chars.peek() else { return Fit::Partial };
        if wanted == ' ' {
            if !next.is_whitespace() {
                return Fit::No;
            }
            while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        } else if next == wanted || next.to_lowercase().eq(wanted.to_lowercase()) {
            chars.next();
        } else {
            return Fit::No;
        }
    }
    Fit::Whole(chars.peek().map_or(text.len(), |&(offset, _)| start + offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(pairs: &[(&str, &str)]) -> Vec<Replacement> {
        pairs
            .iter()
            .map(|(from, to)| Replacement { from: (*from).into(), to: (*to).into() })
            .collect()
    }

    fn apply(pairs: &[(&str, &str)], text: &str) -> String {
        Replacer::new(&list(pairs)).apply(text)
    }

    #[test]
    fn a_phrase_is_replaced_anywhere_in_any_case() {
        let ai = [("эй ай", "AI")];
        assert_eq!(apply(&ai, "эй ай"), "AI");
        assert_eq!(apply(&ai, "Эй ай поможет."), "AI поможет.");
        assert_eq!(apply(&ai, "Спросим эй  ай."), "Спросим AI.");
        assert_eq!(apply(&ai, "Это ЭЙ АЙ, а это эй ай!"), "Это AI, а это AI!");
        assert_eq!(apply(&[("Kubernetes", "K8s")], "kubernetes and KUBERNETES"), "K8s and K8s");
    }

    #[test]
    fn only_whole_words_match() {
        let ai = [("ай", "AI")];
        assert_eq!(apply(&ai, "майка и айва"), "майка и айва");
        assert_eq!(apply(&ai, "ай-ай"), "AI-AI");
        assert_eq!(
            apply(&[("C++", "C plus plus")], "C++ and C++11"),
            "C plus plus and C plus plus11"
        );
    }

    #[test]
    fn spaces_inside_a_phrase_match_any_run_of_spaces() {
        let sign = [("моя  подпись ", "С уважением,\nИван")];
        assert_eq!(apply(&sign, "Спасибо. Моя\tподпись"), "Спасибо. С уважением,\nИван");
        assert_eq!(apply(&sign, "мояподпись"), "мояподпись");
    }

    #[test]
    fn the_longest_phrase_wins_and_inserted_text_is_not_replaced_again() {
        let pairs = [("ай", "AI"), ("эй ай модель", "LLM"), ("AI", "ай")];
        assert_eq!(apply(&pairs, "эй ай модель и ай"), "LLM и AI");
        assert_eq!(apply(&[("a", "b"), ("b", "a")], "a b"), "b a");
    }

    #[test]
    fn one_entry_lists_variants_separated_by_commas_or_new_lines() {
        let ai = [("эй ай, ей яй,\nэйай, ,", "AI")];
        assert_eq!(apply(&ai, "Эй ай, ей яй и эйай."), "AI, AI и AI.");
        assert_eq!(apply(&ai, "эй, ай"), "эй, ай", "a comma separates variants");
    }

    #[test]
    fn the_first_of_equal_phrases_wins() {
        assert_eq!(apply(&[("гугл", "Google"), ("Гугл", "Гугл")], "гугл"), "Google");
    }

    #[test]
    fn empty_phrases_are_ignored() {
        assert_eq!(apply(&[("", "x"), ("  ", "y")], "a b"), "a b");
        assert_eq!(apply(&[], "a  b"), "a  b");
    }

    #[test]
    fn an_empty_replacement_removes_the_phrase_without_double_spaces() {
        let filler = [("э-э", "")];
        assert_eq!(apply(&filler, "привет э-э мир"), "привет мир");
        assert_eq!(apply(&filler, "привет э-э, мир"), "привет мир");
        assert_eq!(apply(&filler, "Э-э мир"), "Мир");
        assert_eq!(apply(&filler, "э-э э-э мир"), "мир");
        assert_eq!(apply(&filler, "привет э-э"), "привет");
        assert_eq!(apply(&filler, "раз\nэ-э два"), "раз\nдва");
    }

    #[test]
    fn found_phrases_carry_their_bytes() {
        let pairs = list(&[("эй ай", "AI")]);
        let found = Replacer::new(&pairs).find("и эй ай");
        assert_eq!(found, vec![Found { range: 3..12, to: "AI" }]);
    }

    #[test]
    fn a_growing_text_holds_back_a_phrase_still_being_said() {
        let pairs = list(&[("эй ай", "AI"), ("кубернетис", "Kubernetes")]);
        let replacer = Replacer::new(&pairs);
        assert_eq!(replacer.settled("Привет эй"), "Привет");
        assert_eq!(replacer.settled("Привет э"), "Привет");
        assert_eq!(replacer.settled("Привет эй ай"), "Привет", "the word may still grow");
        assert_eq!(replacer.settled("Привет эй ай,"), "Привет AI", "the comma waits too");
        assert_eq!(replacer.settled("Привет эй ай и"), "Привет AI и");
        assert_eq!(replacer.settled("Привет эй там"), "Привет эй там");
        assert_eq!(replacer.settled("эй"), "");
        assert_eq!(replacer.settled("Про кубер"), "Про");
        assert_eq!(replacer.settled("Про куб и"), "Про куб и");
        assert_eq!(replacer.settled("майка"), "майка");
    }

    #[test]
    fn a_settled_text_only_grows() {
        let pairs = list(&[("эй ай", "AI"), ("эй ай модель", "LLM"), ("э-э", "")]);
        let replacer = Replacer::new(&pairs);
        let final_text = "Ну э-э, эй ай модель и эй ай отвечают.";
        let mut typed = String::new();
        for end in (0..=final_text.len()).filter(|end| final_text.is_char_boundary(*end)) {
            let settled = replacer.settled(final_text[..end].trim_end());
            assert!(settled.starts_with(&typed), "{typed:?} → {settled:?}");
            typed = settled;
        }
        // The end of the dictation puts in what waited: the full stop.
        assert_eq!(replacer.apply(final_text), "Ну LLM и AI отвечают.");
        assert_eq!(typed, "Ну LLM и AI отвечают");
    }

    /// A replacement right after a removed phrase keeps its spaces and takes the capital; a
    /// capital not yet given passes over several removed phrases; a comma of a replacement is
    /// its own.
    #[test]
    fn removed_phrases_next_to_replacements_and_each_other() {
        let rules = [("uh, э-э, мм, короче", ""), ("kubernetes", "Kubernetes"), ("эй ай", "AI")];
        let pairs = list(&rules);
        let replacer = Replacer::new(&pairs);
        assert_eq!(replacer.apply("Uh, kubernetes is down."), "Kubernetes is down.");
        assert_eq!(replacer.apply("Короче, эй ай сломался"), "AI сломался");
        assert_eq!(replacer.apply("э-э эй ай отвечает"), "AI отвечает");
        assert_eq!(replacer.apply("Э-э, мм, я думаю."), "Я думаю.");
        assert_eq!(replacer.apply("Хорошо. Э-э, мм, дальше"), "Хорошо. Дальше");
        assert_eq!(replacer.apply("Хорошо. Э-э дальше"), "Хорошо. Дальше");
        let sign = list(&[("с уважением", "С уважением,"), ("э-э", "")]);
        assert_eq!(Replacer::new(&sign).apply("с уважением э-э Иван"), "С уважением, Иван");
    }

    /// What is typed while speaking never has to change, whatever the words, fillers,
    /// replacements and punctuation: each settled text continues the one before, and the end
    /// continues the last.
    #[test]
    fn a_settled_text_never_has_to_change() {
        let pairs = list(&[
            ("uh, э-э, мм, короче", ""),
            ("эй ай", "AI"),
            ("с уважением", "С уважением,"),
            ("kubernetes", "Kubernetes"),
        ]);
        let replacer = Replacer::new(&pairs);
        let words = [
            "Hello",
            "uh",
            "Uh",
            "э-э",
            "Э-э",
            "мм",
            "world",
            "я",
            "думаю",
            "эй",
            "ай",
            "с",
            "уважением",
            "kubernetes",
            "мм-да",
            "So",
            "короче",
            "Короче",
        ];
        let marks = ["", "", "", ",", ".", "...", "…", "!", "?", "\n"];
        let mut seed: u64 = 7;
        let mut next = |n: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % n
        };
        for _ in 0..3000 {
            let mut said = String::new();
            for _ in 0..1 + next(7) {
                if !said.is_empty() && !said.ends_with('\n') {
                    said.push(' ');
                }
                said.push_str(words[next(words.len())]);
                said.push_str(marks[next(marks.len())]);
            }
            let mut typed = String::new();
            for end in (0..=said.len()).filter(|end| said.is_char_boundary(*end)) {
                let settled = replacer.settled(said[..end].trim_end());
                assert!(settled.starts_with(&typed), "{said:?}: {typed:?} → {settled:?}");
                typed = settled;
            }
            let all = replacer.apply(&said);
            assert!(all.starts_with(&typed), "{said:?}: {typed:?} then {all:?}");
        }
    }

    /// Fillers removed from a growing text take their commas and pauses along and leave a
    /// capital, as the fillers of a whole text do, and what was typed is never taken back.
    #[test]
    fn removed_fillers_take_their_commas_also_in_a_growing_text() {
        let pairs = list(&[("э-э, uh, мм", "")]);
        let replacer = Replacer::new(&pairs);
        for (said, clean) in [
            ("Hello, uh, world.", "Hello world."),
            ("Э-э, я думаю.", "Я думаю."),
            ("Ну, э-э, я думаю.", "Ну я думаю."),
            ("Я думаю, э-э.", "Я думаю."),
            ("Хорошо. Э-э. Дальше", "Хорошо. Дальше"),
            ("мм-да, это так", "мм-да, это так"),
            ("и э-э, вот", "и вот"),
        ] {
            assert_eq!(replacer.apply(said), clean, "{said}");
            assert_eq!(
                crate::domain::clean_text::remove_fillers(said, "э-э, uh, мм", false),
                clean,
                "as fillers: {said}"
            );
            let mut typed = String::new();
            for end in (0..=said.len()).filter(|end| said.is_char_boundary(*end)) {
                let settled = replacer.settled(said[..end].trim_end());
                assert!(settled.starts_with(&typed), "{said}: {typed:?} → {settled:?}");
                typed = settled;
            }
            assert!(clean.starts_with(&typed), "{said}: {typed:?} then {clean:?}");
        }
    }

    #[test]
    fn the_file_keeps_the_list() {
        let pairs = list(&[("эй ай", "AI"), ("моя подпись", "С уважением,\nИван")]);
        assert_eq!(from_file(&to_file(&pairs)).unwrap(), pairs);
        let text =
            r#"{ "replacements": [ { "from": " , ", "to": "x" }, { "from": "a", "to": "b" } ] }"#;
        assert_eq!(from_file(text).unwrap(), list(&[("a", "b")]));
        assert!(from_file("[]").is_err());
        assert!(from_file("{ not json").is_err());
    }
}

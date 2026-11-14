//! Timed words from whisper-server's `verbose_json`, merged across windows and grouped into
//! subtitle cues and paragraphs.
//!
//! With `split_on_word=true` and `max_len=1` whisper.cpp cuts every segment at each word
//! start, so a segment is one word (in scripts without spaces — Chinese, Japanese — a whole
//! phrase): `start`/`end` in seconds from the request start (the end is
//! where the next word starts), and its `words` array holds the word's tokens with their own,
//! voice-based times. The word ends where its last token ends, so pauses stay visible.

use std::fmt;
use std::ops::Range;

use serde::{Deserialize, Serialize};

use super::chunks::{CONTEXT_SECONDS, Window};
use crate::domain::replacements::{Replacer, render_after};
use crate::engines::hallucinations;

/// Fewest words two windows must agree on to join there instead of by midpoints.
const SEAM_RUN: usize = 2;
/// How far apart two windows may place the same word, in seconds. Near the edges of a request
/// whisper's times drift: 0.6 s was seen on real speech.
const SEAM_DRIFT: f64 = 1.5;
/// A new subtitle cue after a pause longer than this, in seconds.
const CUE_PAUSE: f64 = 1.0;
const CUE_LONGEST: f64 = 7.0;
/// A new paragraph after a pause this long.
const PARAGRAPH_PAUSE: f64 = 2.0;
/// A paragraph this long ends at the next sentence end.
const PARAGRAPH_TARGET: f64 = 30.0;
/// A paragraph never grows past this; it is cut between cues.
const PARAGRAPH_LONGEST: f64 = 60.0;

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Word {
    /// Seconds from the file start.
    pub start: f64,
    pub end: f64,
    /// As the engine wrote it: a leading space starts a new word; languages written without
    /// spaces have none. Join words with [`join_text`].
    pub text: String,
}

impl Word {
    pub fn midpoint(&self) -> f64 {
        (self.start + self.end) / 2.0
    }
}

/// A run of words: a subtitle cue or a paragraph.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Not the engine's `verbose_json`.
    BadJson(String),
    /// Segments without times: the request did not ask for them (`WORD_TIMESTAMP_FIELDS`).
    NoTimestamps,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadJson(detail) => write!(f, "unexpected engine response: {detail}"),
            Self::NoTimestamps => write!(f, "the engine response has no word times"),
        }
    }
}

impl std::error::Error for ParseError {}

/// All words of one engine reply, context included, in file time; [`merge`] joins windows.
pub fn parse_words(verbose_json: &str, window: &Window) -> Result<Vec<Word>, ParseError> {
    #[derive(Deserialize)]
    struct Reply {
        segments: Vec<RawSegment>,
    }
    #[derive(Deserialize)]
    struct RawSegment {
        text: String,
        start: Option<f64>,
        end: Option<f64>,
        #[serde(default)]
        words: Vec<RawToken>,
    }
    #[derive(Deserialize)]
    struct RawToken {
        end: Option<f64>,
    }

    let reply: Reply = serde_json::from_str(verbose_json)
        .map_err(|error| ParseError::BadJson(error.to_string()))?;
    let offset = window.offset();
    let mut words = Vec::with_capacity(reply.segments.len());
    for segment in reply.segments {
        let (Some(start), Some(end)) = (segment.start, segment.end) else {
            return Err(ParseError::NoTimestamps);
        };
        if segment.text.trim().is_empty() {
            continue;
        }
        let end = end.max(start);
        let spoken_end = segment
            .words
            .iter()
            .rev()
            .find_map(|token| token.end)
            .filter(|token_end| *token_end > start)
            .map_or(end, |token_end| token_end.min(end));
        words.push(Word { start: offset + start, end: offset + spoken_end, text: segment.text });
    }
    Ok(words)
}

/// The words of consecutive windows (in order) as one transcript, each word once.
///
/// Both windows at a seam heard the 2 s on each side of it, but time the same words
/// differently, so midpoints alone can keep a word twice or lose it. The longest run of at least
/// [`SEAM_RUN`] equal words (ignoring case and punctuation) in that zone marks one place in both
/// transcripts; the cut falls inside it, at the seam by the earlier window's times. Without such
/// a run the windows' own parts ([`Window::owns`]) decide. Merging the windows received so far
/// gives the text up to the last one's own end; its last 2 s may change when the next window is
/// merged.
pub fn merge(parts: impl IntoIterator<Item = (Window, Vec<Word>)>) -> Vec<Word> {
    let mut merged = Vec::new();
    let mut current: Option<(Window, Vec<Word>)> = None;
    for (window, words) in parts {
        let words = match current.take() {
            None => words.into_iter().filter(|word| word.midpoint() >= window.keep_from).collect(),
            Some((previous, before)) => {
                let (keep, skip) = seam(&before, &words, previous.keep_to);
                merged.extend(before.into_iter().take(keep));
                words.into_iter().skip(skip).collect()
            }
        };
        current = Some((window, words));
    }
    // Its start was settled already (by the seam, or as the first window): only its end is
    // checked, for a window that is not the last yet.
    if let Some((window, words)) = current {
        merged.extend(
            words.into_iter().filter(|word| window.last || word.midpoint() < window.keep_to),
        );
    }
    merged
}

/// How many words of the earlier window to keep and of the later one to skip at a seam.
fn seam(before: &[Word], after: &[Word], seam: f64) -> (usize, usize) {
    let context = CONTEXT_SECONDS as f64;
    let zone_start = before.partition_point(|word| word.midpoint() < seam - context);
    let earlier = &before[zone_start..];
    let later = &after[..after.partition_point(|word| word.midpoint() < seam + context)];
    let (earlier_keys, later_keys) = (keys(earlier), keys(later));
    // Longest common run of words: (length, end in `earlier`, end in `later`).
    let mut best = (0, 0, 0);
    let mut runs = vec![0; later.len() + 1];
    for i in 1..=earlier.len() {
        for j in (1..=later.len()).rev() {
            let same = !earlier_keys[i - 1].is_empty()
                && earlier_keys[i - 1] == later_keys[j - 1]
                && (earlier[i - 1].midpoint() - later[j - 1].midpoint()).abs() <= SEAM_DRIFT;
            runs[j] = if same { runs[j - 1] + 1 } else { 0 };
            if runs[j] > best.0 {
                best = (runs[j], i, j);
            }
        }
    }
    let (length, earlier_end, later_end) = best;
    if length < SEAM_RUN {
        let keep = before.partition_point(|word| word.midpoint() < seam);
        return (keep, after.partition_point(|word| word.midpoint() < seam));
    }
    let (earlier_start, later_start) = (earlier_end - length, later_end - length);
    let inside = earlier[earlier_start..earlier_end]
        .iter()
        .take_while(|word| word.midpoint() < seam)
        .count();
    (zone_start + earlier_start + inside, later_start + inside)
}

/// Words compared at a seam: letters and digits only, lower case.
fn keys(words: &[Word]) -> Vec<String> {
    let key = |word: &Word| {
        word.text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
    };
    words.iter().map(key).collect()
}

/// Text of consecutive words with single spaces.
pub fn join_text(words: &[Word]) -> String {
    let joined: String = words.iter().map(|word| word.text.as_str()).collect();
    joined.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A window's words without the credits Whisper invents on silence: when its own part says only
/// such a line, that part goes. The words it shares with its neighbours stay for the seams.
pub fn without_credits(mut words: Vec<Word>, window: &Window) -> Vec<Word> {
    let own = |word: &Word| window.owns(word.midpoint());
    let said: String =
        words.iter().filter(|word| own(word)).map(|word| word.text.as_str()).collect();
    if hallucinations::is_hallucination(&said) {
        words.retain(|word| !own(word));
    }
    words
}

/// `text` with its first letter capital, its leading spaces kept.
fn capitalized(text: &str) -> String {
    let rest = text.trim_start();
    let mut chars = rest.chars();
    let Some(first) = chars.next() else { return text.to_string() };
    format!("{}{}{}", &text[..text.len() - rest.len()], first.to_uppercase(), chars.as_str())
}

/// The words with the user's replacements. The words a phrase spans become one word with their
/// times; a word left with no text is dropped.
pub fn replace_words(words: Vec<Word>, replacer: &Replacer) -> Vec<Word> {
    let joined: String = words.iter().map(|word| word.text.as_str()).collect();
    let found = replacer.find(&joined);
    if found.is_empty() {
        return words;
    }
    let mut ends = Vec::with_capacity(words.len());
    let mut offset = 0;
    for word in &words {
        offset += word.text.len();
        ends.push(offset);
    }
    let word_at = |byte: usize| ends.partition_point(|end| *end <= byte);
    let start_of = |index: usize| if index == 0 { 0 } else { ends[index - 1] };

    let mut result: Vec<Word> = Vec::with_capacity(words.len());
    let (mut copied, mut next) = (0, 0);
    // A removed word that began a sentence gives its capital to the word after it.
    let mut capital = false;
    // The text so far as it comes out: where a sentence starts for the next phrase.
    let mut rendered = String::new();
    let copy =
        |result: &mut Vec<Word>, rendered: &mut String, words: &[Word], capital: &mut bool| {
            for word in words {
                let mut word = word.clone();
                if std::mem::take(capital) {
                    word.text = capitalized(&word.text);
                }
                rendered.push_str(&word.text);
                result.push(word);
            }
        };
    while next < found.len() {
        let first = word_at(found[next].range.start);
        let mut last = word_at(found[next].range.end - 1);
        let mut group = vec![found[next].clone()];
        next += 1;
        // Phrases that share a word go into that one word.
        while next < found.len() && found[next].range.start < ends[last] {
            last = word_at(found[next].range.end - 1);
            group.push(found[next].clone());
            next += 1;
        }
        let span = start_of(first)..ends[last];
        for item in &mut group {
            item.range = item.range.start - span.start..item.range.end - span.start;
        }
        copy(&mut result, &mut rendered, &words[copied..first], &mut capital);
        // The words before tell where a sentence starts; they are not changed.
        let (text, gives_capital) = render_after(&rendered, capital, &joined[span], &group);
        if !text.trim().is_empty() {
            rendered.push_str(&text);
            result.push(Word { start: words[first].start, end: words[last].end, text });
        }
        capital = gives_capital;
        copied = last + 1;
    }
    copy(&mut result, &mut rendered, &words[copied..], &mut capital);
    result
}

/// Subtitle cues: a new cue after a pause over 1 s, after a sentence end, or where the cue
/// would pass 7 s (a single longer word stays one cue). A cue ends no later than the next starts.
pub fn segments(words: &[Word]) -> Vec<Segment> {
    let mut cues: Vec<Segment> =
        cue_ranges(words).into_iter().map(|range| segment(words, range)).collect();
    for i in 1..cues.len() {
        let next = cues[i].start;
        let cue = &mut cues[i - 1];
        cue.end = cue.end.min(next).max(cue.start);
    }
    cues
}

/// Paragraphs for plain text, made of whole cues: a new paragraph after a pause of 2 s, at the
/// first sentence end once a paragraph is 30 s long, or where it would pass 60 s.
pub fn paragraphs(words: &[Word]) -> Vec<Segment> {
    let mut groups: Vec<Range<usize>> = Vec::new();
    for cue in cue_ranges(words) {
        if let Some(group) = groups.last_mut() {
            let (first, previous) = (&words[group.start], &words[group.end - 1]);
            let split = words[cue.start].start - previous.end >= PARAGRAPH_PAUSE
                || (previous.end - first.start >= PARAGRAPH_TARGET
                    && ends_sentence(&previous.text))
                || words[cue.end - 1].end - first.start > PARAGRAPH_LONGEST;
            if !split {
                group.end = cue.end;
                continue;
            }
        }
        groups.push(cue);
    }
    groups.into_iter().map(|range| segment(words, range)).collect()
}

fn cue_ranges(words: &[Word]) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (i, word) in words.iter().enumerate() {
        if let Some(range) = ranges.last_mut() {
            let previous = &words[i - 1];
            let split = word.start - previous.end > CUE_PAUSE
                || ends_sentence(&previous.text)
                || word.end - words[range.start].start > CUE_LONGEST;
            if !split {
                range.end = i + 1;
                continue;
            }
        }
        ranges.push(i..i + 1);
    }
    ranges
}

fn segment(words: &[Word], range: Range<usize>) -> Segment {
    let words = &words[range];
    Segment { start: words[0].start, end: words[words.len() - 1].end, text: join_text(words) }
}

fn ends_sentence(text: &str) -> bool {
    let text = text.trim_end().trim_end_matches(['"', '\'', '»', '”', '’', ')', ']']);
    text.ends_with(['.', '!', '?', '…', '。', '！', '？', '؟', '।'])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Excerpt of a real reply from whisper-server 1.9.5 (large-v3-turbo) for
    /// `runtimes/test-audio/en-long.wav` with [`super::super::WORD_TIMESTAMP_FIELDS`]. The last
    /// segment is made up: without token times it has no `words`.
    const REPLY: &str = r#"{"task":"transcribe","language":"english","duration":31.799999237060547,
"text":"\n Voice\n dictation,\n models,\n speed.\n again\n","segments":[
{"id":0,"text":"","start":0.0,"end":0.11,"temperature":0.0,"avg_logprob":0.0,
 "no_speech_prob":8.424359300429618e-13},
{"id":1,"text":" Voice","start":0.11,"end":0.34,"tokens":[15229],
 "words":[{"word":" Voice","start":0.11,"end":0.34,"t_dtw":-1,"probability":0.8879488110542297}],
 "temperature":0.0,"avg_logprob":-0.11884117126464844,"no_speech_prob":0.0},
{"id":84,"text":" dictation,","start":28.69,"end":29.580000000000002,"tokens":[12569,399,11],
 "words":[{"word":" dict","start":28.69,"end":28.95,"t_dtw":-1,"probability":0.9990200996398926},
  {"word":"ation","start":29.28,"end":29.28,"t_dtw":-1,"probability":0.9999580383300781},
  {"word":",","start":29.34,"end":29.47,"t_dtw":-1,"probability":0.9995042085647583}],
 "temperature":0.0,"avg_logprob":-0.0005060831899754703,"no_speech_prob":0.0},
{"id":85,"text":" models,","start":29.580000000000002,"end":30.13,"tokens":[5245,11],
 "words":[{"word":" models","start":29.580000000000002,"end":29.96,"t_dtw":-1,
  "probability":0.9884523749351501},
  {"word":",","start":29.96,"end":30.05,"t_dtw":-1,"probability":0.9906899929046631}],
 "temperature":0.0,"avg_logprob":-0.01048421859741211,"no_speech_prob":0.0},
{"id":87,"text":" speed.","start":30.32,"end":30.92,"tokens":[3073,13],
 "words":[{"word":" speed","start":30.32,"end":30.37,"t_dtw":-1,"probability":0.9998989105224609},
  {"word":".","start":30.810000000000002,"end":30.92,"t_dtw":-1,"probability":0.9996586441993713}],
 "temperature":0.0,"avg_logprob":-0.00014750163245480508,"no_speech_prob":0.0},
{"id":88,"text":" again","start":30.92,"end":31.5,"temperature":0.0,"avg_logprob":-0.2,
 "no_speech_prob":0.0}]}"#;

    fn window(start_seconds: f64, keep_from: f64, keep_to: f64, last: bool) -> Window {
        let start = (start_seconds * 16_000.0) as usize;
        Window { index: 0, start, end: start + 32 * 16_000, keep_from, keep_to, last }
    }

    fn word(start: f64, end: f64, text: &str) -> Word {
        Word { start, end, text: text.into() }
    }

    fn texts(words: &[Word]) -> Vec<&str> {
        words.iter().map(|word| word.text.as_str()).collect()
    }

    fn assert_times(words: &[Word], expected: &[(f64, f64)]) {
        assert_eq!(words.len(), expected.len());
        for (word, (start, end)) in words.iter().zip(expected) {
            assert!((word.start - start).abs() < 1e-9, "{word:?} start");
            assert!((word.end - end).abs() < 1e-9, "{word:?} end");
        }
    }

    #[test]
    fn words_get_file_times_and_spoken_ends() {
        let words = parse_words(REPLY, &window(25.1, 27.1, 52.0, false)).unwrap();
        assert_eq!(texts(&words), [" Voice", " dictation,", " models,", " speed.", " again"]);
        assert_times(
            &words,
            &[
                (25.21, 25.44),
                // The comma token ends 0.11 s before the next word starts.
                (53.79, 54.57),
                (54.68, 55.15),
                (55.42, 56.02),
                // No tokens: the segment end.
                (56.02, 56.6),
            ],
        );
        assert_eq!(join_text(&words), "Voice dictation, models, speed. again");
    }

    /// whisper-server splits scripts without spaces only before a spaced token: a segment is a
    /// whole phrase and keeps its own end.
    #[test]
    fn a_phrase_of_a_script_without_spaces_keeps_its_end() {
        let reply = r#"{"segments": [
            {"text": "今日は会議があります", "start": 1.0, "end": 7.0, "words": [{"end": 7.0}]}
        ]}"#;
        let words = parse_words(reply, &window(0.0, 0.0, 30.0, false)).unwrap();
        assert_times(&words, &[(1.0, 7.0)]);
    }

    #[test]
    fn replies_without_times_or_segments_are_errors() {
        let first = window(0.0, 0.0, 30.0, false);
        let untimed = r#"{"segments":[{"id":0,"text":" Hi","tokens":[1]}],"text":" Hi"}"#;
        assert_eq!(parse_words(untimed, &first), Err(ParseError::NoTimestamps));
        assert!(matches!(parse_words("not json", &first), Err(ParseError::BadJson(_))));
        assert!(matches!(parse_words(r#"{"text":"Hi"}"#, &first), Err(ParseError::BadJson(_))));
        let silent = r#"{"segments":[],"text":""}"#;
        assert_eq!(parse_words(silent, &first), Ok(Vec::new()));
    }

    #[test]
    fn overlapping_windows_keep_each_word_once() {
        let first = window(0.0, 0.0, 27.1, false);
        let second = window(25.1, 27.1, 52.0, true);
        let from_first = vec![
            word(26.5, 26.9, " one"),
            // Midpoint 27.15: the second window's.
            word(27.0, 27.3, " two"),
            word(28.0, 28.4, " three"),
        ];
        let from_second = vec![
            // Context heard again: the first window's.
            word(25.3, 26.0, " zero"),
            word(26.52, 26.88, " one"),
            word(27.02, 27.28, " two"),
            word(28.0, 28.4, " three"),
            // Past the end of the audio, kept by the last window.
            word(52.5, 53.0, " four"),
        ];
        let merged = merge([(first, from_first), (second, from_second)]);
        assert_eq!(texts(&merged), [" one", " two", " three", " four"]);
        assert_eq!(merged[0].start, 26.5);
        assert_eq!(merged[1].start, 27.02);
    }

    fn timed(words: &[(f64, f64, &str)]) -> Vec<Word> {
        words.iter().map(|(start, end, text)| word(*start, *end, text)).collect()
    }

    /// Real replies for two windows of the same speech: the second places "so it keeps" 0.6 s
    /// later, so by midpoints both windows would keep it ("so it keeps so it keeps going").
    #[test]
    fn seams_join_where_both_windows_agree() {
        let first = window(0.0, 0.0, 25.1, false);
        let second = window(23.1, 25.1, 50.6, true);
        let from_first = timed(&[
            (22.78, 23.13, " works"),
            (23.18, 23.28, " on"),
            (23.28, 23.35, " a"),
            (23.35, 23.49, " longer"),
            (24.42, 24.64, " recording,"),
            (24.66, 24.79, " so"),
            (24.79, 24.92, " it"),
            (24.92, 25.21, " keeps"),
            (25.24, 25.44, " going"),
            (25.78, 25.89, " for"),
            (25.89, 26.11, " about"),
            (26.11, 26.38, " half"),
            (26.38, 26.44, " a"),
            (26.44, 27.10, " minute."),
        ]);
        let from_second = timed(&[
            (23.10, 23.50, " works"),
            (23.50, 23.65, " on"),
            (23.66, 23.74, " a"),
            (23.74, 24.20, " longer"),
            (24.21, 24.96, " recording,"),
            (25.25, 25.35, " so"),
            (25.35, 25.55, " it"),
            (25.65, 25.84, " keeps"),
            (25.84, 26.16, " going"),
            (26.16, 26.34, " for"),
            (26.34, 26.62, " about"),
            (26.63, 26.86, " half"),
            (26.86, 26.87, " a"),
            (26.94, 27.28, " minute"),
            (27.31, 27.43, " with"),
        ]);
        let merged = merge([(first, from_first), (second, from_second)]);
        let text = "works on a longer recording, so it keeps going for about half a minute with";
        assert_eq!(join_text(&merged), text);
        // The first window's times up to the seam, the second's after it.
        assert_eq!(merged[7].start, 24.92);
        assert_eq!(merged[8].start, 25.84);
    }

    /// Words the seam gives the later window stay, also when their midpoints in its times fall
    /// before the seam: the windows heard a word before the seam differently, or the later one
    /// times them earlier.
    #[test]
    fn the_last_window_keeps_every_word_the_seam_gives_it() {
        let first = window(0.0, 0.0, 27.1, false);
        let second = window(25.1, 27.1, 52.0, true);
        let heard = |third: &'static str| {
            timed(&[
                (25.6, 25.9, " a"),
                (26.0, 26.3, " b"),
                (26.5, 26.9, third),
                (27.3, 27.6, " d"),
            ])
        };
        let merged = merge([(first, heard(" c")), (second, heard(" c2"))]);
        assert_eq!(join_text(&merged), "a b c2 d", "one of the two words, none lost");
        let earlier = timed(&[(26.6, 26.9, " a"), (27.0, 27.4, " b"), (27.6, 27.9, " c")]);
        let later = timed(&[(26.3, 26.6, " a"), (26.7, 27.1, " b"), (27.3, 27.6, " c")]);
        let merged = merge([(first, earlier), (second, later)]);
        assert_eq!(join_text(&merged), "a b c");
    }

    #[test]
    fn seams_without_shared_words_use_midpoints() {
        let first = window(0.0, 0.0, 27.1, false);
        let second = window(25.1, 27.1, 52.0, true);
        let from_first = timed(&[(26.0, 26.4, " alpha"), (27.0, 27.4, " beta")]);
        let from_second = timed(&[(26.1, 26.3, " gamma"), (27.05, 27.35, " delta")]);
        let merged = merge([(first, from_first.clone()), (second, from_second)]);
        assert_eq!(texts(&merged), [" alpha", " delta"]);
        // One window that is not the last keeps only its own part.
        assert_eq!(texts(&merge([(first, from_first)])), [" alpha"]);
    }

    #[test]
    fn cues_split_at_pauses_and_sentence_ends() {
        let words = [
            word(0.0, 0.5, " Hello"),
            word(0.6, 1.0, " world."),
            word(1.2, 1.5, " Next"),
            word(1.6, 2.0, " part"),
            word(3.5, 3.9, " after"),
            word(4.0, 4.4, " pause"),
        ];
        let cues = segments(&words);
        let expected =
            [(0.0, 1.0, "Hello world."), (1.2, 2.0, "Next part"), (3.5, 4.4, "after pause")];
        assert_eq!(cues.len(), expected.len());
        for (cue, (start, end, text)) in cues.iter().zip(expected) {
            assert_eq!((cue.start, cue.end, cue.text.as_str()), (start, end, text));
        }
    }

    #[test]
    fn cues_last_at_most_seven_seconds() {
        let words: Vec<Word> =
            (0..30).map(|i| word(i as f64 * 0.6, i as f64 * 0.6 + 0.5, " la")).collect();
        let cues = segments(&words);
        assert!(cues.iter().all(|cue| cue.end - cue.start <= 7.0 + 1e-9));
        assert_eq!(cues[0].text.split(' ').count(), 11);
        assert_eq!(cues.iter().map(|cue| cue.text.split(' ').count()).sum::<usize>(), 30);
        let long = segments(&[word(0.0, 9.0, "很长的句子没有空格"), word(9.0, 9.5, "好")]);
        assert_eq!(long.len(), 2, "a long word is a cue of its own");
        assert_eq!(long[0].text, "很长的句子没有空格");
    }

    #[test]
    fn cues_do_not_overlap() {
        let words = [word(0.0, 2.0, " Done."), word(1.9, 2.3, " Next")];
        let cues = segments(&words);
        assert_eq!(cues[0].end, 1.9);
        assert_eq!(cues[1].start, 1.9);
    }

    #[test]
    fn paragraphs_break_at_long_pauses_and_after_half_a_minute() {
        let mut words = vec![word(0.0, 0.5, " First."), word(3.0, 3.5, " Second")];
        // Continuous speech from 4 s to 44 s, a sentence every 5 s.
        for i in 0..80 {
            let start = 4.0 + i as f64 * 0.5;
            let text = if i % 10 == 9 { " end." } else { " word" };
            words.push(word(start, start + 0.4, text));
        }
        let paragraphs = paragraphs(&words);
        let texts: Vec<&str> = paragraphs.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts[0], "First.");
        assert!(texts[1].starts_with("Second word"));
        // Second paragraph: from 3 s to the first sentence end 30 s later (word 59).
        assert_eq!(paragraphs[1].end, 4.0 + 59.0 * 0.5 + 0.4);
        assert_eq!(paragraphs.len(), 3);

        let endless: Vec<Word> =
            (0..200).map(|i| word(i as f64 * 0.5, i as f64 * 0.5 + 0.4, " word")).collect();
        let paragraphs = super::paragraphs(&endless);
        assert!(paragraphs.iter().all(|p| p.end - p.start <= 60.0));
        assert_eq!(paragraphs.len(), 2);
    }

    #[test]
    fn replaced_words_take_the_times_of_the_words_they_span() {
        use crate::domain::replacements::Replacement;
        let pairs = |list: &[(&str, &str)]| -> Vec<Replacement> {
            list.iter()
                .map(|(from, to)| Replacement { from: (*from).into(), to: (*to).into() })
                .collect()
        };
        let words = vec![
            word(0.0, 0.4, " Спросим"),
            word(0.5, 0.7, " эй"),
            word(0.7, 1.0, " ай,"),
            word(1.2, 1.4, " э-э"),
            word(1.5, 1.9, " майка."),
        ];
        let rules = pairs(&[("эй ай", "AI"), ("э-э", ""), ("ай", "AI")]);
        let replaced = replace_words(words.clone(), &Replacer::new(&rules));
        assert_eq!(
            replaced,
            vec![word(0.0, 0.4, " Спросим"), word(0.5, 1.0, " AI,"), word(1.5, 1.9, " майка.")]
        );
        assert_eq!(join_text(&replaced), "Спросим AI, майка.");

        // A removed word takes the pause written after it along, as the fillers of a dictation.
        let comma = vec![word(0.0, 0.3, " Ну"), word(0.4, 0.6, " э-э,"), word(0.7, 0.9, " да")];
        let replaced = replace_words(comma, &Replacer::new(&rules));
        assert_eq!(replaced, vec![word(0.0, 0.3, " Ну"), word(0.7, 0.9, " да")]);
        assert_eq!(join_text(&replaced), "Ну да");
        // The words before tell where a sentence starts: a full stop after a removed word in the
        // middle of one stays, and one that began a sentence gives its capital to the next word.
        let short = pairs(&[("короче", "")]);
        let middle = vec![
            word(0.0, 0.2, " Я"),
            word(0.3, 0.6, " думаю"),
            word(0.7, 0.9, " короче."),
            word(1.0, 1.3, " Дальше"),
        ];
        assert_eq!(join_text(&replace_words(middle, &Replacer::new(&short))), "Я думаю. Дальше");
        let first =
            vec![word(0.0, 0.3, " Короче,"), word(0.4, 0.6, " мы"), word(0.7, 0.9, " идём.")];
        assert_eq!(join_text(&replace_words(first, &Replacer::new(&short))), "Мы идём.");
        // Two removed in a row: the capital passes over both, as in a dictation.
        let both = pairs(&[("короче, э-э", "")]);
        let row = vec![
            word(0.0, 0.3, " Короче,"),
            word(0.4, 0.5, " э-э,"),
            word(0.6, 0.8, " мы"),
            word(0.9, 1.1, " идём."),
        ];
        assert_eq!(join_text(&replace_words(row, &Replacer::new(&both))), "Мы идём.");
        assert_eq!(replace_words(words.clone(), &Replacer::new(&[])), words);
    }

    #[test]
    fn credits_go_from_a_windows_own_part_only() {
        let window = window(25.0, 27.0, 40.0, true);
        // " world." is heard again from the window before: it stays for the seam.
        let credits = vec![
            word(25.5, 26.5, " world."),
            word(30.0, 30.5, " Продолжение"),
            word(30.5, 31.0, " следует..."),
        ];
        assert_eq!(texts(&without_credits(credits, &window)), [" world."]);
        // Said with other words of the window's own, the line stays.
        let speech = vec![
            word(29.0, 29.5, " Да."),
            word(30.0, 30.5, " Продолжение"),
            word(30.5, 31.0, " следует..."),
        ];
        assert_eq!(without_credits(speech.clone(), &window), speech);
    }
}

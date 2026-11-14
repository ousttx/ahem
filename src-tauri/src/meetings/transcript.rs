//! A meeting's transcript: the words of both tracks as turns by speaker.
//!
//! Each track is first grouped into turns of its own (one speaker, pauses under
//! [`TURN_PAUSE`]); the turns of both tracks are then ordered by start, so talking over each
//! other does not chop either side into single words.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::files::export::{escape_vtt, timestamp};
use crate::files::{ExportFormat, Segment, Word, join_text, segments};

/// A pause this long ends a turn even if the same person goes on.
pub const TURN_PAUSE: f64 = 1.5;
/// A pause this long ends a sentence even without a full stop: engines and stretches of
/// speech without punctuation.
const SENTENCE_PAUSE: f64 = 0.6;
/// A piece of a sentence shorter than this, cut off where the segments change voice, stays with
/// the rest of the sentence: the segments' edges are not that exact.
const SHORTEST_PIECE: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "number", rename_all = "snake_case")]
pub enum Speaker {
    /// The microphone track.
    Me,
    /// The computer's sound when it is not split by speaker: no diarization models, or one
    /// voice only.
    Others,
    /// A voice of the computer's sound, numbered from 1 in order of first appearance.
    Person(u32),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub speaker: Speaker,
    pub start: f64,
    pub end: f64,
    pub words: Vec<Word>,
}

impl Turn {
    pub fn text(&self) -> String {
        join_text(&self.words)
    }
}

/// A stretch of the computer's sound and the diarization model's speaker number for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpeakerSegment {
    pub start: f64,
    pub end: f64,
    pub speaker: i32,
}

/// What the interface calls the speakers; `person` has `{n}` for the number.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Labels {
    pub me: String,
    pub others: String,
    pub person: String,
}

impl Labels {
    pub fn name(&self, speaker: Speaker) -> String {
        match speaker {
            Speaker::Me => self.me.clone(),
            Speaker::Others => self.others.clone(),
            Speaker::Person(number) => self.person.replace("{n}", &number.to_string()),
        }
    }
}

/// The speaker of each word of the computer's sound: [`voice_at`] the middle of where it is
/// spoken. Numbers follow the order in which voices first speak; a single voice, or no
/// segments, is [`Speaker::Others`].
pub fn others_speakers(words: Vec<Word>, segments: &[SpeakerSegment]) -> Vec<(Speaker, Word)> {
    if segments.is_empty() {
        return words.into_iter().map(|word| (Speaker::Others, word)).collect();
    }
    let raw: Vec<i32> = words.iter().map(|word| voice_at(segments, word.midpoint())).collect();
    let mut order: Vec<i32> = Vec::new();
    for speaker in &raw {
        if !order.contains(speaker) {
            order.push(*speaker);
        }
    }
    words
        .into_iter()
        .zip(raw)
        .map(|(word, speaker)| {
            let speaker = if order.len() < 2 {
                Speaker::Others
            } else {
                let position = order.iter().position(|s| *s == speaker).unwrap_or(0);
                Speaker::Person(position as u32 + 1)
            };
            (speaker, word)
        })
        .collect()
}

/// The voice speaking at `at`: of the segments that cover it the shortest, since a short turn
/// inside a long segment of another voice is that turn; else the nearest segment's voice.
pub(crate) fn voice_at(segments: &[SpeakerSegment], at: f64) -> i32 {
    let distance = |segment: &SpeakerSegment| {
        if at < segment.start {
            segment.start - at
        } else if at > segment.end {
            at - segment.end
        } else {
            0.0
        }
    };
    segments
        .iter()
        .map(|segment| (distance(segment), segment.end - segment.start, segment.speaker))
        .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)))
        .map_or(0, |(_, _, speaker)| speaker)
}

/// A sentence of the computer's sound, in seconds: `audio` is where it is spoken; `covers` is
/// the stretch of its own words, from halfway after the previous sentence's last word to
/// halfway before the next one's first, so that [`others_speakers`] puts each word in its
/// sentence even where the engine let the sentences overlap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Sentence {
    pub audio: (f64, f64),
    pub covers: (f64, f64),
}

/// The words of the computer's sound as sentences, in the order of the words: a sentence ends
/// after a word that ends one ([`SENTENCE_ENDS`]) and before a pause of [`SENTENCE_PAUSE`]; it
/// is also cut where [`voice_at`] its words changes, into pieces of [`SHORTEST_PIECE`] or
/// longer (speech without punctuation runs over turns). Each then gets the voice it sounds like.
pub(crate) fn sentences(words: &[Word], segments: &[SpeakerSegment]) -> Vec<Sentence> {
    let lasts = |from: usize, to: usize| words[to].end - words[from].start;
    let voices: Vec<i32> = words.iter().map(|word| voice_at(segments, word.midpoint())).collect();
    let voice = |index: usize| voices[index];
    // The first and last word of each piece.
    let mut pieces: Vec<(usize, usize)> = Vec::new();
    for (first, last) in by_full_stops_and_pauses(words) {
        let in_sentence = pieces.len();
        let mut from = first;
        for index in first..=last {
            if index < last && voice(index + 1) == voice(index) {
                continue;
            }
            // A short piece joins the one before it in the same sentence.
            let earlier = pieces.len() > in_sentence;
            match pieces.last_mut() {
                Some(piece)
                    if earlier
                        && (lasts(piece.0, piece.1) < SHORTEST_PIECE
                            || lasts(from, index) < SHORTEST_PIECE) =>
                {
                    piece.1 = index;
                }
                _ => pieces.push((from, index)),
            }
            from = index + 1;
        }
    }
    let middle = |index: usize| words[index].midpoint();
    pieces
        .iter()
        .enumerate()
        .map(|(i, &(from, to))| {
            let audio = (words[from].start, words[to].end.max(words[from].start));
            let low = match i.checked_sub(1) {
                Some(before) => (middle(pieces[before].1) + middle(from)) / 2.0,
                None => audio.0.min(middle(from)),
            };
            let high = match pieces.get(i + 1) {
                Some(after) => (middle(to) + middle(after.0)) / 2.0,
                None => audio.1.max(middle(to)),
            };
            Sentence { audio, covers: (low, high.max(low)) }
        })
        .collect()
}

/// What ends a sentence, in the scripts the speech models write.
const SENTENCE_ENDS: [char; 9] = ['.', '?', '!', '…', '。', '？', '！', '؟', '।'];
/// Closing quotes and brackets that may follow the end of a sentence.
const CLOSERS: [char; 8] = ['"', '»', ')', '”', '’', '」', '』', '）'];

/// The first and last word of each run between full stops and pauses.
fn by_full_stops_and_pauses(words: &[Word]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut from = 0;
    for (index, word) in words.iter().enumerate() {
        let closes = word.text.trim_end().trim_end_matches(CLOSERS).ends_with(SENTENCE_ENDS);
        let pause = words.get(index + 1).is_none_or(|next| next.start - word.end >= SENTENCE_PAUSE);
        if closes || pause {
            runs.push((from, index));
            from = index + 1;
        }
    }
    runs
}

/// Both tracks as turns in order of their start.
pub fn turns(me: Vec<Word>, others: Vec<(Speaker, Word)>) -> Vec<Turn> {
    let mut all = group(me.into_iter().map(|word| (Speaker::Me, word)));
    all.extend(group(others.into_iter()));
    // Stable: at the same start the microphone's turn comes first.
    all.sort_by(|a, b| a.start.total_cmp(&b.start));
    all
}

fn group(words: impl Iterator<Item = (Speaker, Word)>) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for (speaker, word) in words {
        let end = word.end;
        match turns.last_mut() {
            Some(turn) if turn.speaker == speaker && word.start - turn.end < TURN_PAUSE => {
                turn.end = turn.end.max(end);
                turn.words.push(word);
            }
            _ => turns.push(Turn { speaker, start: word.start, end, words: vec![word] }),
        }
    }
    turns
}

/// A file's transcript as a file: by speaker when its voices were told apart, else
/// as the Files view saves a file without speakers — paragraphs, or cues with word times.
pub fn export_file(turns: &[Turn], format: &ExportFormat, labels: &Labels) -> String {
    if turns.iter().any(|turn| matches!(turn.speaker, Speaker::Person(_))) {
        return export(turns, format, labels);
    }
    let words: Vec<Word> = turns.iter().flat_map(|turn| turn.words.iter().cloned()).collect();
    crate::files::export(&words, format)
}

/// The subtitle cues of all turns with their speaker's name, in order of start: the turns of
/// the two tracks overlap.
fn cues(turns: &[Turn], labels: &Labels) -> Vec<(String, Segment)> {
    let mut cues: Vec<(String, Segment)> = turns
        .iter()
        .flat_map(|turn| {
            let name = labels.name(turn.speaker);
            segments(&turn.words).into_iter().map(move |cue| (name.clone(), cue))
        })
        .collect();
    cues.sort_by(|a, b| a.1.start.total_cmp(&b.1.start));
    cues
}

/// The transcript as a file. Text and Markdown put the time and the speaker before each turn;
/// subtitles split long turns into cues with the speaker on each.
pub fn export(turns: &[Turn], format: &ExportFormat, labels: &Labels) -> String {
    let clock = |seconds: f64| timestamp(seconds, '.')[..8].to_string();
    let mut out = String::new();
    match format {
        ExportFormat::Txt => {
            for turn in turns {
                let name = labels.name(turn.speaker);
                let _ = write!(out, "[{}] {name}: {}\n\n", clock(turn.start), turn.text());
            }
        }
        ExportFormat::Md { title } => {
            let _ = write!(out, "# {title}\n\n");
            for turn in turns {
                let name = labels.name(turn.speaker);
                let _ = write!(out, "**{name}** · {}\n{}\n\n", clock(turn.start), turn.text());
            }
        }
        ExportFormat::Srt => {
            for (number, (name, cue)) in cues(turns, labels).into_iter().enumerate() {
                let (start, end) = (timestamp(cue.start, ','), timestamp(cue.end, ','));
                let _ = write!(out, "{}\n{start} --> {end}\n{name}: {}\n\n", number + 1, cue.text);
            }
        }
        ExportFormat::Vtt => {
            out.push_str("WEBVTT\n\n");
            for (name, cue) in cues(turns, labels) {
                let (start, end) = (timestamp(cue.start, '.'), timestamp(cue.end, '.'));
                let (name, text) = (escape_vtt(&name), escape_vtt(&cue.text));
                let _ = write!(out, "{start} --> {end}\n<v {name}>{text}\n\n");
            }
        }
        ExportFormat::Json => {
            #[derive(Serialize)]
            struct Out<'a> {
                speaker: String,
                start: f64,
                end: f64,
                text: String,
                words: &'a [Word],
            }
            let list: Vec<Out> = turns
                .iter()
                .map(|turn| Out {
                    speaker: labels.name(turn.speaker),
                    start: turn.start,
                    end: turn.end,
                    text: turn.text(),
                    words: &turn.words,
                })
                .collect();
            out = serde_json::to_string_pretty(&serde_json::json!({ "turns": list }))
                .unwrap_or_default();
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(start: f64, text: &str) -> Word {
        Word { start, end: start + 0.4, text: format!(" {text}") }
    }

    fn segment(start: f64, end: f64, speaker: i32) -> SpeakerSegment {
        SpeakerSegment { start, end, speaker }
    }

    fn labels() -> Labels {
        Labels { me: "Me".into(), others: "Others".into(), person: "Speaker {n}".into() }
    }

    #[test]
    fn voices_are_numbered_by_first_appearance() {
        let words = vec![word(1.0, "hi"), word(5.0, "hello"), word(9.0, "bye")];
        // The model calls the first voice 3 and the second 0.
        let segments = [segment(0.0, 4.0, 3), segment(4.5, 6.0, 0), segment(8.0, 10.0, 3)];
        let speakers: Vec<Speaker> =
            others_speakers(words, &segments).into_iter().map(|(speaker, _)| speaker).collect();
        assert_eq!(speakers, [Speaker::Person(1), Speaker::Person(2), Speaker::Person(1)]);
    }

    #[test]
    fn a_word_between_segments_takes_the_nearest() {
        let words = vec![word(4.1, "between")];
        let segments = [segment(0.0, 4.0, 1), segment(6.0, 8.0, 2), segment(9.0, 9.5, 1)];
        // One voice in the words: not numbered.
        assert_eq!(others_speakers(words, &segments)[0].0, Speaker::Others);
        let words = vec![word(1.0, "a"), word(5.4, "b")];
        let speakers: Vec<Speaker> =
            others_speakers(words, &segments).into_iter().map(|(speaker, _)| speaker).collect();
        assert_eq!(speakers, [Speaker::Person(1), Speaker::Person(2)]);
    }

    #[test]
    fn a_short_turn_inside_a_long_segment_keeps_its_voice() {
        // An interview: the guest's segment runs over the host's question.
        let segments = [segment(32.7, 46.4, 0), segment(35.3, 37.3, 1)];
        assert_eq!(voice_at(&segments, 36.0), 1);
        assert_eq!(voice_at(&segments, 40.0), 0);
        assert_eq!(voice_at(&segments, 50.0), 0);
    }

    #[test]
    fn sentences_end_at_full_stops_pauses_and_voice_changes() {
        let words = vec![
            word(0.0, "Why"),
            word(0.4, "rarely?"),
            // No full stop, but a pause.
            word(1.0, "people"),
            word(1.4, "come"),
            word(2.5, "and"),
            word(2.9, "we"),
            word(3.3, "talk"),
            // The next voice starts without a pause or a full stop.
            word(4.1, "really"),
            word(4.5, "always"),
            word(4.9, "yes"),
            // The segments change voice inside "mostly kind.": too short a piece to cut off.
            word(7.0, "mostly"),
            word(7.4, "kind."),
        ];
        let segments = [segment(0.0, 4.0, 0), segment(4.0, 7.3, 1), segment(7.3, 9.0, 0)];
        let tenths = |x: f64| (x * 10.0).round() / 10.0;
        let spans: Vec<(f64, f64)> = sentences(&words, &segments)
            .into_iter()
            .map(|sentence| (tenths(sentence.audio.0), tenths(sentence.audio.1)))
            .collect();
        assert_eq!(spans, [(0.0, 0.8), (1.0, 1.8), (2.5, 3.7), (4.1, 5.3), (7.0, 7.8)]);
        // Without segments only full stops and pauses count.
        assert_eq!(sentences(&words, &[]).len(), 4);
        assert!(sentences(&[], &segments).is_empty());
    }

    #[test]
    fn sentences_end_in_other_scripts_too() {
        let words =
            vec![word(0.0, "你好。"), word(0.4, "谢谢！"), word(0.8, "Да.»"), word(1.2, "Нет")];
        assert_eq!(sentences(&words, &[]).len(), 4);
    }

    #[test]
    fn each_word_stays_in_its_sentence() {
        // The engine let "Yes." run past the start of the next sentence.
        let words = vec![
            Word { start: 0.0, end: 0.9, text: " Yes.".into() },
            Word { start: 0.5, end: 0.7, text: " And".into() },
            Word { start: 0.7, end: 1.7, text: " then.".into() },
        ];
        let found = sentences(&words, &[]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].audio, (0.0, 0.9));
        assert_eq!(found[0].covers.1, found[1].covers.0);
        // As segments, the sentences give each word its own: "And" lies in both sentences'
        // audio, and the shorter first one would take it.
        let segments: Vec<SpeakerSegment> = found
            .iter()
            .enumerate()
            .map(|(i, s)| segment(s.covers.0, s.covers.1, i as i32))
            .collect();
        let speakers: Vec<Speaker> =
            others_speakers(words, &segments).into_iter().map(|(speaker, _)| speaker).collect();
        assert_eq!(speakers, [Speaker::Person(1), Speaker::Person(2), Speaker::Person(2)]);
    }

    #[test]
    fn without_segments_the_computer_is_one_speaker() {
        let found = others_speakers(vec![word(0.0, "a"), word(9.0, "b")], &[]);
        assert!(found.iter().all(|(speaker, _)| *speaker == Speaker::Others));
    }

    #[test]
    fn turns_follow_each_track_and_order_by_start() {
        let me = vec![word(0.0, "so"), word(0.5, "what"), word(1.0, "now"), word(6.0, "ok")];
        // The other side talks over the end of the first turn.
        let others =
            vec![(Speaker::Person(1), word(0.8, "wait")), (Speaker::Person(1), word(1.2, "no"))];
        let turns = turns(me, others);
        let summary: Vec<(Speaker, String)> =
            turns.iter().map(|turn| (turn.speaker, turn.text())).collect();
        assert_eq!(
            summary,
            [
                (Speaker::Me, "so what now".to_string()),
                (Speaker::Person(1), "wait no".to_string()),
                (Speaker::Me, "ok".to_string()),
            ]
        );
        assert_eq!(turns[0].end, 1.4);
    }

    #[test]
    fn a_long_pause_starts_a_new_turn() {
        let turns = turns(vec![word(0.0, "one"), word(0.4 + TURN_PAUSE, "two")], Vec::new());
        assert_eq!(turns.len(), 2);
    }

    #[test]
    fn text_names_the_speaker_and_the_time() {
        let turns = turns(vec![word(61.0, "hello")], vec![(Speaker::Person(2), word(65.0, "hi"))]);
        assert_eq!(
            export(&turns, &ExportFormat::Txt, &labels()),
            "[00:01:01] Me: hello\n\n[00:01:05] Speaker 2: hi\n\n"
        );
        let md = export(&turns, &ExportFormat::Md { title: "Call".into() }, &labels());
        assert!(md.starts_with("# Call\n\n**Me** · 00:01:01\nhello\n\n"), "{md}");
    }

    /// Turns of the two tracks overlap; subtitles go by their start (WebVTT requires it).
    #[test]
    fn subtitles_of_overlapping_turns_go_in_order_of_start() {
        // The others talk for 20 s in one turn of several cues; "yes" comes in the middle.
        let others: Vec<(Speaker, Word)> =
            (0..40).map(|half| (Speaker::Others, word(f64::from(half) * 0.5, "talk"))).collect();
        let turns = turns(vec![word(10.0, "yes")], others);
        let vtt = export(&turns, &ExportFormat::Vtt, &labels());
        let times: Vec<&str> = vtt.lines().filter(|line| line.contains(" --> ")).collect();
        assert!(times.len() > 2, "{vtt}");
        let mut sorted = times.clone();
        sorted.sort();
        assert_eq!(times, sorted, "{vtt}");
        assert!(vtt.contains("<v Me>yes"));
    }

    #[test]
    fn subtitles_carry_the_speaker() {
        let turns = turns(Vec::new(), vec![(Speaker::Others, word(1.0, "a<b"))]);
        let srt = export(&turns, &ExportFormat::Srt, &labels());
        assert_eq!(srt, "1\n00:00:01,000 --> 00:00:01,400\nOthers: a<b\n\n");
        let vtt = export(&turns, &ExportFormat::Vtt, &labels());
        assert_eq!(vtt, "WEBVTT\n\n00:00:01.000 --> 00:00:01.400\n<v Others>a&lt;b\n\n");
        let json = export(&turns, &ExportFormat::Json, &labels());
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["turns"][0]["speaker"], "Others");
        assert_eq!(parsed["turns"][0]["text"], "a<b");
    }

    #[test]
    fn a_file_without_voices_is_saved_as_the_files_view_saves_it() {
        let words = vec![word(1.0, "Hello"), word(1.4, "world."), word(30.0, "Bye.")];
        let unnamed = turns(Vec::new(), others_speakers(words.clone(), &[]));
        for format in [ExportFormat::Txt, ExportFormat::Srt, ExportFormat::Json] {
            assert_eq!(
                export_file(&unnamed, &format, &labels()),
                crate::files::export(&words, &format)
            );
        }
        let named = turns(
            Vec::new(),
            vec![(Speaker::Person(1), word(1.0, "Hello")), (Speaker::Person(2), word(4.0, "Hi"))],
        );
        assert_eq!(
            export_file(&named, &ExportFormat::Txt, &labels()),
            "[00:00:01] Speaker 1: Hello\n\n[00:00:04] Speaker 2: Hi\n\n"
        );
    }

    #[test]
    fn turns_survive_storage() {
        let turns = turns(vec![word(0.0, "a")], vec![(Speaker::Person(3), word(2.0, "b"))]);
        let stored = serde_json::to_string(&turns).unwrap();
        assert!(stored.contains(r#"{"kind":"person","number":3}"#), "{stored}");
        assert_eq!(serde_json::from_str::<Vec<Turn>>(&stored).unwrap(), turns);
    }
}

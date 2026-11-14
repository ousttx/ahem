//! Transcript files: plain text, Markdown, SRT, WebVTT and JSON. File naming is the UI's job.

use std::fmt::Write as _;

use serde::Serialize;

use super::words::{Word, join_text, paragraphs, segments};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportFormat {
    /// Paragraphs separated by blank lines.
    Txt,
    /// `# <title>` and the paragraphs.
    Md { title: String },
    /// Subtitle cues (`00:00:01,000`).
    Srt,
    /// Subtitle cues (`00:00:01.000`).
    Vtt,
    /// The full text, cues and words with times in seconds.
    Json,
}

impl ExportFormat {
    /// The format of an extension the interface offers; `title` heads a Markdown file.
    pub fn from_extension(extension: &str, title: String) -> Option<Self> {
        match extension {
            "md" => Some(Self::Md { title }),
            "txt" => Some(Self::Txt),
            "srt" => Some(Self::Srt),
            "vtt" => Some(Self::Vtt),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Txt => "txt",
            Self::Md { .. } => "md",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
            Self::Json => "json",
        }
    }
}

/// The language model's text of a file as the chosen format: Markdown with its title, or plain
/// text; none for subtitles and JSON, which need word times the model's text has not.
pub fn processed_export(text: &str, format: &ExportFormat) -> Option<String> {
    match format {
        ExportFormat::Md { title } => Some(format!("# {title}\n\n{}\n", text.trim())),
        ExportFormat::Txt => Some(format!("{}\n", text.trim())),
        _ => None,
    }
}

/// The merged words of a file as the chosen format.
pub fn export(words: &[Word], format: &ExportFormat) -> String {
    match format {
        ExportFormat::Txt => {
            let text = paragraph_text(words);
            if text.is_empty() { text } else { text + "\n" }
        }
        ExportFormat::Md { title } => {
            let text = paragraph_text(words);
            if text.is_empty() { format!("# {title}\n") } else { format!("# {title}\n\n{text}\n") }
        }
        ExportFormat::Srt => {
            let mut srt = String::new();
            for (number, cue) in segments(words).iter().enumerate() {
                let (start, end) = (timestamp(cue.start, ','), timestamp(cue.end, ','));
                let _ = write!(srt, "{}\n{start} --> {end}\n{}\n\n", number + 1, cue.text);
            }
            srt
        }
        ExportFormat::Vtt => {
            let mut vtt = String::from("WEBVTT\n\n");
            for cue in segments(words) {
                let (start, end) = (timestamp(cue.start, '.'), timestamp(cue.end, '.'));
                let _ = write!(vtt, "{start} --> {end}\n{}\n\n", escape_vtt(&cue.text));
            }
            vtt
        }
        ExportFormat::Json => json(words),
    }
}

fn paragraph_text(words: &[Word]) -> String {
    let paragraphs = paragraphs(words);
    paragraphs.iter().map(|paragraph| paragraph.text.as_str()).collect::<Vec<_>>().join("\n\n")
}

/// `HH:MM:SS` and milliseconds after `separator`; hours grow past 99 if needed.
pub(crate) fn timestamp(seconds: f64, separator: char) -> String {
    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
    let (hours, minutes, secs) = (millis / 3_600_000, millis / 60_000 % 60, millis / 1000 % 60);
    format!("{hours:02}:{minutes:02}:{secs:02}{separator}{:03}", millis % 1000)
}

/// Cue text is markup in WebVTT.
pub(crate) fn escape_vtt(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn json(words: &[Word]) -> String {
    #[derive(Serialize)]
    struct Timed<'a> {
        start: f64,
        end: f64,
        text: &'a str,
    }
    #[derive(Serialize)]
    struct Transcript<'a> {
        text: String,
        segments: Vec<Timed<'a>>,
        words: Vec<Timed<'a>>,
    }
    let millis = |seconds: f64| (seconds * 1000.0).round() / 1000.0;
    let cues = segments(words);
    let transcript = Transcript {
        text: join_text(words),
        segments: cues
            .iter()
            .map(|cue| Timed { start: millis(cue.start), end: millis(cue.end), text: &cue.text })
            .collect(),
        words: words
            .iter()
            .map(|word| Timed {
                start: millis(word.start),
                end: millis(word.end),
                text: word.text.trim(),
            })
            .collect(),
    };
    serde_json::to_string_pretty(&transcript).expect("plain structs serialize") + "\n"
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn words() -> Vec<Word> {
        let word = |start, end, text: &str| Word { start, end, text: text.into() };
        vec![
            word(0.0, 0.4, " Hello"),
            word(0.5, 1.0, " world."),
            word(1.2, 1.6, " <Second>"),
            word(1.7, 2.25, " & more"),
            word(5.0, 5.5, " Later."),
        ]
    }

    /// A model's text goes into Markdown and plain text as it is; subtitles and JSON need the
    /// transcript's word times.
    #[test]
    fn a_processed_text_goes_into_text_formats_only() {
        let md = ExportFormat::Md { title: "Talk".into() };
        assert_eq!(processed_export(" Summary. ", &md).as_deref(), Some("# Talk\n\nSummary.\n"));
        assert_eq!(processed_export("Summary.", &ExportFormat::Txt).as_deref(), Some("Summary.\n"));
        assert_eq!(processed_export("Summary.", &ExportFormat::Srt), None);
        assert_eq!(processed_export("Summary.", &ExportFormat::Json), None);
    }
    #[test]
    fn timestamps_use_the_format_separator() {
        assert_eq!(timestamp(0.0, ','), "00:00:00,000");
        assert_eq!(timestamp(3_661.5, ','), "01:01:01,500");
        assert_eq!(timestamp(3_661.5, '.'), "01:01:01.500");
        assert_eq!(timestamp(59.9996, ','), "00:01:00,000");
        assert_eq!(timestamp(-0.2, '.'), "00:00:00.000");
        assert_eq!(timestamp(108_000.25, '.'), "30:00:00.250");
    }

    #[test]
    fn plain_text_is_paragraphs() {
        let text = export(&words(), &ExportFormat::Txt);
        assert_eq!(text, "Hello world. <Second> & more\n\nLater.\n");
    }

    #[test]
    fn markdown_has_the_title_and_paragraphs() {
        let format = ExportFormat::Md { title: "talk.mp3".into() };
        assert_eq!(
            export(&words(), &format),
            "# talk.mp3\n\nHello world. <Second> & more\n\nLater.\n"
        );
        assert_eq!(export(&[], &format), "# talk.mp3\n");
        assert_eq!(format.extension(), "md");
    }

    #[test]
    fn srt_numbers_cues_with_comma_milliseconds() {
        let expected = "1\n00:00:00,000 --> 00:00:01,000\nHello world.\n\n\
                        2\n00:00:01,200 --> 00:00:02,250\n<Second> & more\n\n\
                        3\n00:00:05,000 --> 00:00:05,500\nLater.\n\n";
        assert_eq!(export(&words(), &ExportFormat::Srt), expected);
    }

    #[test]
    fn vtt_has_a_header_and_escaped_cues() {
        let expected = "WEBVTT\n\n\
                        00:00:00.000 --> 00:00:01.000\nHello world.\n\n\
                        00:00:01.200 --> 00:00:02.250\n&lt;Second&gt; &amp; more\n\n\
                        00:00:05.000 --> 00:00:05.500\nLater.\n\n";
        assert_eq!(export(&words(), &ExportFormat::Vtt), expected);
        assert_eq!(export(&[], &ExportFormat::Vtt), "WEBVTT\n\n");
    }

    #[test]
    fn json_has_text_cues_and_words() {
        let mut words = words();
        words[0].end = 0.400_000_4;
        words[1].start = 0.123_456;
        let json: Value = serde_json::from_str(&export(&words, &ExportFormat::Json)).unwrap();
        assert_eq!(json["text"], "Hello world. <Second> & more Later.");
        let segments = json["segments"].as_array().unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[1]["start"], 1.2);
        assert_eq!(segments[1]["end"], 2.25);
        assert_eq!(segments[1]["text"], "<Second> & more");
        let words = json["words"].as_array().unwrap();
        assert_eq!(words.len(), 5);
        assert_eq!(words[0]["end"], 0.4);
        assert_eq!(words[1]["start"], 0.123);
        assert_eq!(words[3]["text"], "& more");
    }

    #[test]
    fn empty_transcripts_export_cleanly() {
        assert_eq!(export(&[], &ExportFormat::Txt), "");
        assert_eq!(export(&[], &ExportFormat::Srt), "");
        let json: Value = serde_json::from_str(&export(&[], &ExportFormat::Json)).unwrap();
        assert_eq!(json["words"].as_array().unwrap().len(), 0);
    }
}

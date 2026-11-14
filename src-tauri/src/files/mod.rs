//! Transcription of audio and video files with any engine.
//!
//! 1. [`decode()`] (or [`FileDecoder`] block by block): the first audio track as 16 kHz mono.
//! 2. [`plan`] (or [`Planner`] over a stream): requests of up to 32 s with 2 s of context.
//! 3. For each [`Window`]: the engine transcribes the window's audio with word times and replies
//!    in the shape of whisper-server's `verbose_json` (whisper-server itself with
//!    [`WORD_TIMESTAMP_FIELDS`]; sherpa-onnx, llama-server and the cloud services convert to it),
//!    then [`parse_words`] reads the reply.
//! 4. [`merge`] keeps each word once: windows are joined where they share a run of words, else
//!    by the words' midpoints; [`replace_words`] applies the user's
//!    replacements; [`segments`] and [`paragraphs`] group the words.
//! 5. [`export()`] writes TXT, Markdown, SRT, WebVTT or JSON.

pub mod chunks;
pub mod decode;
pub mod export;
pub mod words;

pub use chunks::{Planner, Window, plan};
pub use decode::{DecodeError, Decoded, FileDecoder, decode};
pub use export::{ExportFormat, export};
pub use words::{
    ParseError, Segment, Word, join_text, merge, paragraphs, parse_words, replace_words, segments,
    without_credits,
};

/// Multipart fields for `/inference` besides `language`, `translate` and `file`. The app starts
/// whisper-server with `--no-timestamps`, so timestamps must be asked for; `split_on_word` with
/// `max_len=1` makes every segment one word with its own times (see [`words`]); in scripts
/// without spaces (Chinese, Japanese) a segment is a whole phrase.
pub const WORD_TIMESTAMP_FIELDS: [(&str, &str); 6] = [
    ("response_format", "verbose_json"),
    ("no_timestamps", "false"),
    ("token_timestamps", "true"),
    ("split_on_word", "true"),
    ("max_len", "1"),
    // Not needed for the words: skips a second encoder pass per request that only computes
    // the language probabilities in `verbose_json`.
    ("no_language_probabilities", "true"),
];

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::audio::wav;

    fn test_audio(name: &str) -> PathBuf {
        let dir = std::env::var_os("AHEM_TEST_AUDIO_DIR").map(PathBuf::from).unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtimes/test-audio")
        });
        dir.join(name)
    }

    /// `/inference` with the word fields over plain HTTP; the app uses `EngineClient` instead.
    fn transcribe(url: &str, audio: &[f32]) -> String {
        let rest = url.strip_prefix("http://").expect("an http:// URL");
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let boundary = "ahem-files-test";
        let mut body = Vec::new();
        let fields = [("language", "auto"), ("translate", "false")];
        for (name, value) in fields.iter().chain(WORD_TIMESTAMP_FIELDS.iter()) {
            write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n")
                .unwrap();
            write!(body, "{value}\r\n").unwrap();
        }
        write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; ").unwrap();
        write!(body, "filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n").unwrap();
        body.extend(wav::encode(audio, 16_000));
        write!(body, "\r\n--{boundary}--\r\n").unwrap();

        let mut stream = TcpStream::connect(host).unwrap();
        let path = if path.is_empty() { String::new() } else { format!("/{path}") };
        write!(stream, "POST {path}/inference HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n")
            .unwrap();
        write!(stream, "Content-Type: multipart/form-data; boundary={boundary}\r\n").unwrap();
        write!(stream, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        stream.write_all(&body).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        let (head, body) = reply.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        body.to_string()
    }

    /// The whole pipeline on 67 s of synthesized speech (three windows) against a running
    /// whisper-server: every boundary word must come out exactly once.
    #[test]
    #[ignore = "needs a running whisper-server: set AHEM_ENGINE_URL=http://127.0.0.1:<port>/<request path>, then cargo test -- --ignored through_the_engine --nocapture"]
    fn long_file_through_the_engine() {
        let url = std::env::var("AHEM_ENGINE_URL").expect("AHEM_ENGINE_URL");
        let speech = decode(&test_audio("en-long.wav")).unwrap().samples;
        let fox = decode(&test_audio("en-short.wav")).unwrap().samples;
        let audio = [&speech[..], &fox[..], &speech[..]].concat();
        let windows = plan(&audio);
        assert_eq!(windows.len(), 3);
        let parts = windows.iter().map(|window| {
            let reply = transcribe(&url, &window.audio(&audio));
            (*window, parse_words(&reply, window).unwrap())
        });
        let words = merge(parts.collect::<Vec<_>>());
        for window in &windows {
            println!("window {}: {:.2}–{:.2} s", window.index, window.keep_from, window.keep_to);
        }
        print!("{}", export(&words, &ExportFormat::Srt));
        let text = join_text(&words).to_lowercase();
        for word in ["voice", "hotkey", "cursor", "dictation", "speed."] {
            assert_eq!(text.matches(word).count(), 2, "{word} in {text}");
        }
        assert_eq!(text.matches("fox").count(), 1, "{text}");
        // The recordings never repeat a phrase right away; a seam that keeps words twice does.
        let plain: Vec<&str> = text.split(' ').collect();
        for n in 1..=4 {
            for i in 0..plain.len().saturating_sub(2 * n - 1) {
                assert_ne!(plain[i..i + n], plain[i + n..i + 2 * n], "repeated at word {i}");
            }
        }
    }
}

//! sherpa-onnx: Parakeet, Nemotron and GigaAM run on the processor through the
//! official `sherpa-onnx` crate. This code runs in the speech worker process
//! (`sherpa_process.rs`); unloading the model ends that process, which is what gives
//! its memory back to the system.
//!
//! Replies take the shapes of [`super::whisper_server`]: a [`Transcript`] for dictation and, for
//! file windows, JSON laid out like whisper-server's `verbose_json` with one segment per word,
//! so the file pipeline reads both engines alike.
//!
//! A streaming model (Nemotron) can also take a dictation while it is recorded: a
//! [`LiveStream`] is fed the microphone's samples at the device rate (sherpa-onnx resamples
//! them itself, without seams between pieces) and returns the text so far. Its text only
//! grows — checked on English, Spanish and Chinese recordings fed in 0.2 s pieces.

use std::path::Path;

use serde_json::json;
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig, OnlineRecognizer,
    OnlineRecognizerConfig, OnlineStream, OnlineTransducerModelConfig,
};

use super::whisper_server::{TranscribeOptions, Transcript};
use crate::audio::resample::{self, TARGET_RATE};
use crate::files::chunks::{self, Window};
use crate::files::words::{self, Word};
use crate::storage::catalog::{Recognizer, SherpaSpec};

/// Silence before the audio: the streaming model loses the first word without it (the same
/// 0.3 s the sherpa-onnx command line adds for Nemotron).
const LEAD_SECONDS: f32 = 0.3;
/// Silence after the audio, so the last chunk is decoded.
const TAIL_SECONDS: f32 = 1.0;
/// How long the last token sounds when the model gives only start times; the others last until
/// the next one starts, at most `LONGEST_TOKEN`.
const TOKEN_SECONDS: f64 = 0.1;
/// Without durations (GigaAM, streaming models) a token would last until the next one starts,
/// across any pause ("yes" at 3.5 s ending at 13.2 s): it ends this long after its start at most.
const LONGEST_TOKEN: f64 = 1.0;

pub enum SherpaEngine {
    Offline(OfflineRecognizer),
    Online(OnlineRecognizer),
}

/// A dictation fed to the streaming model while it is recorded. Must be dropped before the
/// engine that made it.
pub struct LiveStream {
    stream: OnlineStream,
    rate: i32,
    /// The model's text so far, as last reported.
    text: String,
}

/// Recognized text and its tokens with start times in seconds from the audio start.
#[derive(Debug, Clone, PartialEq, Default)]
struct Recognized {
    text: String,
    tokens: Vec<String>,
    starts: Vec<f32>,
    /// Token lengths when the model reports them (Parakeet's TDT does).
    durations: Option<Vec<f32>>,
}

impl SherpaEngine {
    pub fn load(spec: &SherpaSpec, dir: &Path, threads: usize) -> Result<Self, String> {
        let file = |name: &str| Some(dir.join(name).to_string_lossy().into_owned());
        let threads = i32::try_from(threads).unwrap_or(1);
        let engine = match spec.recognizer {
            Recognizer::OfflineTransducer => {
                let mut config = OfflineRecognizerConfig::default();
                config.model_config.transducer = OfflineTransducerModelConfig {
                    encoder: file(&spec.encoder),
                    decoder: file(&spec.decoder),
                    joiner: file(&spec.joiner),
                };
                config.model_config.tokens = file(&spec.tokens);
                config.model_config.model_type = spec.model_type.clone();
                config.model_config.num_threads = threads;
                OfflineRecognizer::create(&config).map(Self::Offline)
            }
            Recognizer::OnlineTransducer => {
                let mut config = OnlineRecognizerConfig::default();
                config.model_config.transducer = OnlineTransducerModelConfig {
                    encoder: file(&spec.encoder),
                    decoder: file(&spec.decoder),
                    joiner: file(&spec.joiner),
                };
                config.model_config.tokens = file(&spec.tokens);
                config.model_config.model_type = spec.model_type.clone();
                config.model_config.num_threads = threads;
                // One recording is one utterance: no splitting at pauses.
                config.enable_endpoint = false;
                OnlineRecognizer::create(&config).map(Self::Online)
            }
        };
        engine.ok_or_else(|| format!("sherpa-onnx cannot load the model in {}", dir.display()))
    }

    /// A dictation. One longer than a file window goes in windows joined like a file, for an
    /// offline model: in one pass it gets worse towards the end (measured on 71 s with GigaAM)
    /// and its attention grows with the square of the length.
    pub fn transcribe(
        &self,
        samples: &[f32],
        rate: u32,
        options: &TranscribeOptions,
    ) -> Result<Transcript, String> {
        if matches!(self, Self::Offline(_)) {
            let audio = resample::to_16k(samples, rate);
            let windows = chunks::plan(&audio);
            if windows.len() > 1 {
                let mut parts = Vec::with_capacity(windows.len());
                for window in windows {
                    let recognized = self.recognize(&window.audio(&audio), TARGET_RATE, options)?;
                    parts.push((window, recognized));
                }
                return Ok(Transcript { text: tidy(&joined(parts)), language: None });
            }
        }
        let recognized = self.recognize(samples, rate, options)?;
        Ok(Transcript { text: tidy(&recognized.text), language: None })
    }

    /// Starts a live stream at the microphone's rate; `None` for a model that does not stream.
    pub fn start_live(&self, language: Option<&str>, rate: u32) -> Option<LiveStream> {
        let Self::Online(recognizer) = self else { return None };
        let stream = recognizer.create_stream();
        if let Some(language) = language {
            stream.set_option("language", language);
        }
        let rate = i32::try_from(rate).ok()?;
        stream.accept_waveform(rate, &silence_at(LEAD_SECONDS, rate));
        Some(LiveStream { stream, rate, text: String::new() })
    }

    /// Feeds more of the recording and decodes what is ready; the whole text so far when it
    /// changed.
    pub fn feed_live(&self, live: &mut LiveStream, samples: &[f32]) -> Option<String> {
        let Self::Online(recognizer) = self else { return None };
        live.stream.accept_waveform(live.rate, samples);
        while recognizer.is_ready(&live.stream) {
            recognizer.decode(&live.stream);
        }
        let text = recognizer.get_result(&live.stream).map(|result| result.text)?;
        if text == live.text {
            return None;
        }
        live.text.clone_from(&text);
        Some(text)
    }

    /// Ends the stream: silence after the speech, the last decode and the final transcript.
    pub fn finish_live(&self, live: LiveStream) -> Result<Transcript, String> {
        let Self::Online(recognizer) = self else {
            return Err("the model does not stream".into());
        };
        live.stream.accept_waveform(live.rate, &silence_at(TAIL_SECONDS, live.rate));
        live.stream.input_finished();
        while recognizer.is_ready(&live.stream) {
            recognizer.decode(&live.stream);
        }
        let result = recognizer.get_result(&live.stream).ok_or("sherpa-onnx returned no result")?;
        Ok(Transcript { text: tidy(&result.text), language: None })
    }

    /// A file window's words with their times, as whisper-server's `verbose_json`.
    pub fn transcribe_words(
        &self,
        samples: &[f32],
        rate: u32,
        options: &TranscribeOptions,
    ) -> Result<String, String> {
        words_json(&self.recognize(samples, rate, options)?)
    }

    fn recognize(
        &self,
        samples: &[f32],
        rate: u32,
        options: &TranscribeOptions,
    ) -> Result<Recognized, String> {
        let audio = resample::to_16k(samples, rate);
        let rate = TARGET_RATE as i32;
        let no_result = || "sherpa-onnx returned no result".to_string();
        match self {
            // No hint: Parakeet detects the language itself, GigaAM knows only Russian.
            Self::Offline(recognizer) => {
                let stream = recognizer.create_stream();
                stream.accept_waveform(rate, &audio);
                recognizer.decode(&stream);
                let result = stream.get_result().ok_or_else(no_result)?;
                Ok(Recognized {
                    text: result.text,
                    tokens: result.tokens,
                    starts: result.timestamps.unwrap_or_default(),
                    durations: result.durations,
                })
            }
            Self::Online(recognizer) => {
                let stream = recognizer.create_stream();
                if let Some(language) = &options.language {
                    stream.set_option("language", language);
                }
                stream.accept_waveform(rate, &silence(LEAD_SECONDS));
                stream.accept_waveform(rate, &audio);
                stream.accept_waveform(rate, &silence(TAIL_SECONDS));
                stream.input_finished();
                while recognizer.is_ready(&stream) {
                    recognizer.decode(&stream);
                }
                let result = recognizer.get_result(&stream).ok_or_else(no_result)?;
                // Times count from the stream start, which has the lead silence in front.
                let base = result.start_time.unwrap_or(0.0) - LEAD_SECONDS;
                let starts = result.timestamps.unwrap_or_default();
                Ok(Recognized {
                    text: result.text,
                    tokens: result.tokens,
                    starts: starts.into_iter().map(|start| (start + base).max(0.0)).collect(),
                    durations: None,
                })
            }
        }
    }
}

fn silence(seconds: f32) -> Vec<f32> {
    silence_at(seconds, TARGET_RATE as i32)
}

fn silence_at(seconds: f32, rate: i32) -> Vec<f32> {
    vec![0.0; (seconds * rate as f32) as usize]
}

/// Single spaces: the streaming model puts two after a sentence end.
pub fn tidy(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, PartialEq)]
struct TimedWord {
    text: String,
    start: f64,
    /// Where the next word starts.
    end: f64,
    /// Where this word's sound ends.
    spoken_end: f64,
}

/// One segment per word, like whisper-server with `max_len=1`: `start` and `end` in seconds,
/// and one token whose `end` is where the word's sound ends (see `files::words::parse_words`).
fn words_json(recognized: &Recognized) -> Result<String, String> {
    if recognized.starts.len() < recognized.tokens.len() {
        return Err("the model gives no word times".into());
    }
    let segments: Vec<_> = timed_words(recognized)
        .into_iter()
        .map(|word| {
            json!({
                "text": word.text,
                "start": word.start,
                "end": word.end,
                "words": [{ "end": word.spoken_end }],
            })
        })
        .collect();
    Ok(json!({ "segments": segments }).to_string())
}

/// The text of a dictation recognized in windows: each window's words in dictation time, joined
/// as a file's are (`files::words::merge`).
fn joined(parts: Vec<(Window, Recognized)>) -> String {
    let parts = parts.into_iter().map(|(window, recognized)| {
        let offset = window.offset();
        let words = timed_words(&recognized)
            .into_iter()
            // As a file's words (`parse_words`): a word ends where its sound ends.
            .map(|word| Word {
                start: offset + word.start,
                end: offset + word.spoken_end,
                text: word.text,
            })
            .collect();
        (window, words)
    });
    words::join_text(&words::merge(parts))
}

/// Tokens joined into words: a token starting with a space starts a word, and so does every
/// token of a script written without spaces (Chinese, Japanese, Korean).
fn timed_words(recognized: &Recognized) -> Vec<TimedWord> {
    let starts: Vec<f64> = recognized.starts.iter().map(|start| f64::from(*start)).collect();
    let token_end = |i: usize| {
        let given = recognized.durations.as_ref().and_then(|durations| durations.get(i));
        match given {
            Some(duration) if *duration > 0.0 => starts[i] + f64::from(*duration),
            _ => starts
                .get(i + 1)
                .map_or(starts[i] + TOKEN_SECONDS, |next| next.min(starts[i] + LONGEST_TOKEN)),
        }
    };
    let mut words: Vec<TimedWord> = Vec::new();
    for (i, token) in recognized.tokens.iter().enumerate() {
        let begins = token.starts_with(char::is_whitespace)
            || token.chars().next().is_some_and(written_without_spaces);
        match words.last_mut() {
            Some(word) if !begins => {
                word.text.push_str(token);
                word.spoken_end = word.spoken_end.max(token_end(i));
            }
            last => {
                if let Some(previous) = last {
                    previous.end = starts[i].max(previous.start);
                }
                words.push(TimedWord {
                    text: token.clone(),
                    start: starts[i],
                    end: starts[i],
                    spoken_end: token_end(i),
                });
            }
        }
    }
    if let Some(last) = words.last_mut() {
        last.end = last.spoken_end.max(last.start);
    }
    words
}

pub(crate) fn written_without_spaces(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30FF}' // Hiragana, Katakana
        | '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' // CJK ideographs
        | '\u{AC00}'..='\u{D7AF}' // Hangul syllables
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::files::chunks::Window;
    use crate::files::words::{join_text, parse_words};

    fn recognized(tokens: &[&str], starts: &[f32], durations: Option<&[f32]>) -> Recognized {
        Recognized {
            text: tokens.concat(),
            tokens: tokens.iter().map(|token| token.to_string()).collect(),
            starts: starts.to_vec(),
            durations: durations.map(<[f32]>::to_vec),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// A long dictation in windows: each window hears the words in its stretch (also the ones
    /// in its overlap with the next); joined, every word is there once, in order.
    #[test]
    fn a_long_dictation_in_windows_keeps_every_word_once() {
        let seconds = 75;
        let audio = vec![0.0; TARGET_RATE as usize * seconds];
        let windows = chunks::plan(&audio);
        assert!(windows.len() > 1, "long enough for windows");
        let spoken: Vec<(f64, String)> =
            (0..seconds / 3).map(|n| (n as f64 * 3.0 + 0.5, format!(" w{n}"))).collect();
        let parts = windows
            .iter()
            .map(|window| {
                let (from, to) = (window.offset(), window.end as f64 / f64::from(TARGET_RATE));
                let heard: Vec<&(f64, String)> =
                    spoken.iter().filter(|(at, _)| *at >= from && *at + 0.4 < to).collect();
                let r = recognized(
                    &heard.iter().map(|(_, word)| word.as_str()).collect::<Vec<_>>(),
                    &heard.iter().map(|(at, _)| (*at - from) as f32).collect::<Vec<_>>(),
                    Some(&vec![0.4; heard.len()]),
                );
                (*window, r)
            })
            .collect();
        let expected: Vec<String> =
            spoken.iter().map(|(_, word)| word.trim().to_string()).collect();
        assert_eq!(joined(parts), expected.join(" "));
    }

    #[test]
    fn tokens_become_words_with_spoken_ends() {
        // Parakeet's tokens for "River bank, done." with durations.
        let r = recognized(
            &[" Ri", "ver", " ba", "nk", ",", " done", "."],
            &[0.08, 0.24, 0.48, 0.72, 0.8, 1.6, 1.9],
            Some(&[0.16, 0.08, 0.24, 0.08, 0.08, 0.24, 0.08]),
        );
        let words = timed_words(&r);
        let texts: Vec<&str> = words.iter().map(|word| word.text.as_str()).collect();
        assert_eq!(texts, [" River", " bank,", " done."]);
        assert!(close(words[0].start, 0.08) && close(words[0].end, 0.48));
        assert!(close(words[0].spoken_end, 0.32));
        // The comma ends the second word's sound; a pause follows.
        assert!(close(words[1].spoken_end, 0.88) && close(words[1].end, 1.6));
        assert!(close(words[2].end, 1.98), "the last word ends with its sound");
    }

    #[test]
    fn without_durations_a_token_lasts_until_the_next() {
        let r = recognized(&[" Hal", "lo", " wor", "ld"], &[0.5, 0.7, 1.0, 1.2], None);
        let words = timed_words(&r);
        assert!(close(words[0].spoken_end, 1.0));
        assert!(close(words[1].spoken_end, 1.3));
        // Not across a pause: "yes" at 3.5 s does not last until the next word at 13.2 s.
        let r = recognized(&[" yes", " right"], &[3.5, 13.2], None);
        let words = timed_words(&r);
        assert!(close(words[0].spoken_end, 4.5));
    }

    #[test]
    fn scripts_without_spaces_split_per_token() {
        let r = recognized(&["国", "が", "何"], &[0.1, 0.3, 0.5], None);
        assert_eq!(timed_words(&r).len(), 3);
    }

    /// The JSON goes through the same parser as whisper-server's reply.
    #[test]
    fn file_pipeline_reads_the_words() {
        let r = recognized(&[" Hello", " world", "."], &[0.2, 0.9, 1.3], Some(&[0.4, 0.3, 0.1]));
        let json = words_json(&r).unwrap();
        let window = Window {
            index: 0,
            start: 32_000,
            end: 32_000 + 16_000 * 30,
            keep_from: 0.0,
            keep_to: 30.0,
            last: true,
        };
        let words = parse_words(&json, &window).unwrap();
        assert_eq!(join_text(&words), "Hello world.");
        // File time: the window starts 2 s into the file.
        assert!(close(words[0].start, 2.2) && close(words[0].end, 2.6));
        assert!(close(words[1].start, 2.9) && close(words[1].end, 3.4));
    }

    /// GigaAM gives no durations: the parsed words end where the speaker stops, so the pause
    /// after a word stays a pause for paragraphs, subtitles, turns and the echo.
    #[test]
    fn file_words_without_durations_do_not_span_pauses() {
        let r = recognized(&[" yes", " right"], &[3.5, 13.2], None);
        let json = words_json(&r).unwrap();
        let window = Window {
            index: 0,
            start: 0,
            end: 16_000 * 30,
            keep_from: 0.0,
            keep_to: 30.0,
            last: true,
        };
        let words = parse_words(&json, &window).unwrap();
        assert!(close(words[0].start, 3.5) && close(words[0].end, 4.5), "{words:?}");
    }

    #[test]
    fn missing_times_are_an_error() {
        let r = recognized(&[" Hi"], &[], None);
        assert!(words_json(&r).is_err());
        assert_eq!(words_json(&Recognized::default()).unwrap(), r#"{"segments":[]}"#);
    }

    /// Real models from the development cache, put there by
    /// `AHEM_MODEL_ID=<id> cargo test -- --ignored downloads_turbo`.
    #[test]
    #[ignore = "needs Parakeet v3 and Nemotron 3.5 in runtimes/models: cargo test -- --ignored sherpa_end_to_end"]
    fn sherpa_end_to_end() {
        let runtimes = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("runtimes");
        let wave =
            sherpa_onnx::Wave::read(&runtimes.join("test-audio/en-short.wav").to_string_lossy())
                .expect("test audio");
        let rate = wave.sample_rate() as u32;
        for id in ["parakeet-tdt-0_6b-v3", "nemotron-3_5-asr-streaming-0_6b"] {
            let spec = crate::storage::catalog::find(id).unwrap();
            let dir: PathBuf = runtimes.join("models").join(id);
            let engine = SherpaEngine::load(spec.sherpa.as_ref().unwrap(), &dir, 6).unwrap();
            let options = TranscribeOptions { language: Some("en".into()), ..Default::default() };
            let transcript = engine.transcribe(wave.samples(), rate, &options).unwrap();
            println!("{id}: {}", transcript.text);
            assert!(transcript.text.to_lowercase().contains("quick brown fox"), "{id}");
            let json = engine.transcribe_words(wave.samples(), rate, &options).unwrap();
            assert!(json.contains("brown"), "{id}: {json}");
        }
    }

    /// GigaAM v3 on Sber's test recordings (`example.wav` and `long_example.wav` from the GigaAM
    /// CDN, saved as `ru-short.wav` and `ru-long.wav`). The long one (71 s) goes as a dictation
    /// (in windows, past the 25 s that GigaAM's own `transcribe` accepts) and as file windows.
    #[test]
    #[ignore = "needs both GigaAM v3 models in runtimes/models: cargo test -- --ignored gigaam_end_to_end"]
    fn gigaam_end_to_end() {
        use crate::files::chunks::plan;
        use crate::files::words::merge;

        let runtimes = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("runtimes");
        let read = |name: &str| {
            let path = runtimes.join("test-audio").join(name);
            sherpa_onnx::Wave::read(&path.to_string_lossy()).expect("test audio")
        };
        let (short, long) = (read("ru-short.wav"), read("ru-long.wav"));
        assert_eq!((short.sample_rate(), long.sample_rate()), (16_000, 16_000));
        // Lowercase words only, so both models compare alike.
        let plain = |text: &str| {
            let letters: String = text
                .to_lowercase()
                .replace('ё', "е")
                .chars()
                .filter(|c| c.is_alphanumeric() || c.is_whitespace())
                .collect();
            tidy(&letters)
        };
        let whole_poem = |id: &str, how: &str, text: &str| {
            let text = plain(text);
            for line in ["вечерня отошла давно", "трепещет луч лампады", "я разрешу тебя грехов"]
            {
                assert!(text.contains(line), "{id}, {how}: no {line:?} in {text}");
            }
        };
        let options = TranscribeOptions::default();
        for id in ["gigaam-v3-e2e-rnnt", "gigaam-v3-rnnt"] {
            let spec = crate::storage::catalog::find(id).unwrap();
            let dir = runtimes.join("models").join(id);
            let engine = SherpaEngine::load(spec.sherpa.as_ref().unwrap(), &dir, 6).unwrap();
            let started = std::time::Instant::now();
            let text = engine.transcribe(short.samples(), TARGET_RATE, &options).unwrap().text;
            println!("{id} short, {:.2} s: {text}", started.elapsed().as_secs_f64());
            assert!(plain(&text).contains("у лукоморья дуб зеленый"), "{id}: {text}");
            // E2E writes punctuation and capitals; the plain model writes neither.
            let formatted = text.contains(',') && text.starts_with(char::is_uppercase);
            assert_eq!(formatted, id.contains("e2e"), "{id}: {text}");

            let started = std::time::Instant::now();
            let text = engine.transcribe(long.samples(), TARGET_RATE, &options).unwrap().text;
            println!(
                "{id} long as a dictation in windows, {:.2} s: {text}",
                started.elapsed().as_secs_f64()
            );
            whole_poem(id, "a dictation", &text);

            let audio = long.samples();
            let parts = plan(audio).into_iter().map(|window| {
                let json =
                    engine.transcribe_words(&window.audio(audio), TARGET_RATE, &options).unwrap();
                let words = parse_words(&json, &window).unwrap();
                (window, words)
            });
            let text = join_text(&merge(parts.collect::<Vec<_>>()));
            println!("{id} long as file windows: {text}");
            whole_poem(id, "file windows", &text);
        }
    }

    /// The streaming model fed like a dictation: 0.2 s pieces, text that only grows.
    #[test]
    #[ignore = "needs Nemotron 3.5 in runtimes/models: cargo test -- --ignored live_stream_end_to_end"]
    fn live_stream_end_to_end() {
        let runtimes = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("runtimes");
        let wave =
            sherpa_onnx::Wave::read(&runtimes.join("test-audio/en-long.wav").to_string_lossy())
                .expect("test audio");
        let rate = wave.sample_rate() as u32;
        let id = "nemotron-3_5-asr-streaming-0_6b";
        let spec = crate::storage::catalog::find(id).unwrap();
        let dir = runtimes.join("models").join(id);
        let engine = SherpaEngine::load(spec.sherpa.as_ref().unwrap(), &dir, 6).unwrap();
        let mut live = engine.start_live(Some("en"), rate).expect("a streaming model");
        let mut previous = String::new();
        for piece in wave.samples().chunks(rate as usize / 5) {
            if let Some(text) = engine.feed_live(&mut live, piece) {
                let text = tidy(&text);
                assert!(text.starts_with(&previous), "{previous:?} became {text:?}");
                previous = text;
            }
        }
        assert!(previous.contains("hotkey"), "text arrives while speaking: {previous}");
        let transcript = engine.finish_live(live).unwrap();
        assert!(transcript.text.starts_with(&previous));
        assert!(transcript.text.contains("dictation"), "{}", transcript.text);
    }
}

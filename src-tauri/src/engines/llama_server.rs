//! llama.cpp `llama-server` as a separate process on 127.0.0.1, for models that hear
//! audio through a multimodal projector (Qwen3-ASR, Voxtral and others).
//!
//! Launch mirrors whisper-server: a free port, readiness through `/health`, output to the engine
//! log. Every request carries a random key, given to the process in `LLAMA_API_KEY` rather than
//! on its command line: hidden from the list of processes and from other users' programs (not
//! from the user's own, which can read a process's environment). The WAV goes to
//! `/v1/chat/completions` as base64 `input_audio`, from memory.
//!
//! The models answer with text only. A recording longer than a file window is sent in windows
//! (`files::chunks`) and joined like a file. Word times for files are estimated: each word gets a
//! share of the window's speech frames by its length.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use serde::Deserialize;
use serde_json::json;

use super::languages;
use super::local_server::{self, Agents, ServerProcess};
use super::sherpa::{tidy, written_without_spaces};
use super::whisper_server::{EngineError, Transcript};
use crate::audio::resample::{self, TARGET_RATE};
use crate::audio::{speech, wav};
use crate::files::chunks::{self, Window};
use crate::files::words::{self, Word};
use crate::storage::catalog::{LlamaReply, LlamaSpec};
use crate::util::{base64, random_hex};

/// The official build `tools/fetch-llama-cpp` fetches.
pub const ENGINE_BUILD: &str = "b11871";

/// Context of the single request slot: a 32 s window is about 450 audio tokens for Qwen3-ASR,
/// plus its text. The model's own context (65 536 tokens for Qwen3-ASR) would reserve gigabytes.
const CONTEXT_TOKENS: u32 = 4096;
/// Longest answer for one window; stops a model that keeps repeating itself.
const MAX_ANSWER_TOKENS: u32 = 1024;

/// The official build fetched by `tools/fetch-llama-cpp` for development runs.
pub fn dev_engine_dir() -> PathBuf {
    let name = format!("llama-{ENGINE_BUILD}-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("runtimes").join(name)
}

/// Where the engine is: the one the installer carries (`engines/llama` of the app's resources),
/// else the build of `tools` for development runs.
pub fn engine_dir(resources: Option<&Path>) -> PathBuf {
    if let Some(dir) = resources.map(|resources| resources.join("engines").join("llama"))
        && executable_in(&dir).is_file()
    {
        return dir;
    }
    dev_engine_dir()
}

pub fn executable_in(dir: &Path) -> PathBuf {
    dir.join(format!("llama-server{}", std::env::consts::EXE_SUFFIX))
}

#[derive(Debug, Clone)]
pub struct LlamaConfig {
    pub executable: PathBuf,
    pub model: PathBuf,
    pub mmproj: PathBuf,
    pub log: PathBuf,
    pub threads: usize,
}

pub struct LlamaServer {
    process: ServerProcess,
    client: LlamaClient,
}

impl LlamaServer {
    /// Starts the process and waits until it answers `/health`.
    pub fn start(
        config: &LlamaConfig,
        spec: &LlamaSpec,
        cancel: &AtomicBool,
    ) -> Result<Self, EngineError> {
        let port = local_server::free_port()?;
        let key = random_hex()?;
        let mut command = Command::new(&config.executable);
        // Settings from the user's environment (`LLAMA_ARG_HOST` and the like) stay out.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("LLAMA_") {
                command.env_remove(name);
            }
        }
        command.args(server_args(config, port)).env("LLAMA_API_KEY", &key);
        let mut process = ServerProcess::spawn(command, &config.executable, &config.log)?;
        let client = LlamaClient::new(format!("http://127.0.0.1:{port}"), key, spec);
        process.wait_ready(cancel, || client.healthy())?;
        Ok(Self { process, client })
    }

    /// A dictation: one request, or windows joined like a file when the recording is long.
    pub fn transcribe(&self, samples: &[f32], rate: u32) -> Result<Transcript, EngineError> {
        let audio = resample::to_16k(samples, rate);
        let windows = chunks::plan(&audio);
        if windows.len() < 2 {
            return self.client.recognize(&wav::encode(&audio, TARGET_RATE));
        }
        let mut language = None;
        let mut parts = Vec::with_capacity(windows.len());
        for window in windows {
            let (words, heard) = self.window_words(&audio, &window)?;
            language = language.or(heard);
            parts.push((window, words));
        }
        Ok(Transcript { text: words::join_text(&words::merge(parts)), language })
    }

    /// A file window's words with estimated times, laid out like whisper-server's
    /// `verbose_json` with one segment per word; none when the answer was cut at its length
    /// (`recognize_window`), which the caller, knowing which window it is, says in the log.
    pub fn transcribe_words(&self, samples: &[f32], rate: u32) -> Result<WindowWords, EngineError> {
        let audio = resample::to_16k(samples, rate);
        Ok(match self.client.recognize_window(&wav::encode(&audio, TARGET_RATE))? {
            Some(transcript) => WindowWords {
                json: words_json(&estimate_times(&transcript.text, &audio)),
                cut: false,
            },
            None => WindowWords { json: words_json(&[]), cut: true },
        })
    }

    /// After a failed request: whether the server died.
    pub fn died(&mut self) -> bool {
        self.process.died()
    }

    pub fn stop(self) -> io::Result<()> {
        self.process.stop()
    }

    /// One window of a long dictation: its words in file time and the detected language.
    fn window_words(
        &self,
        audio: &[f32],
        window: &Window,
    ) -> Result<(Vec<Word>, Option<String>), EngineError> {
        let heard = window.audio(audio);
        let offset = window.offset();
        let Some(transcript) = self.client.recognize_window(&wav::encode(&heard, TARGET_RATE))?
        else {
            log_line!(
                "the dictation's window at {:.0} s: its answer was cut off at its length and the \
                 window is left empty",
                offset
            );
            return Ok((Vec::new(), None));
        };
        let words = estimate_times(&transcript.text, &heard)
            .into_iter()
            .map(|word| Word {
                start: offset + word.start,
                end: offset + word.end,
                text: word.text,
            })
            .collect();
        Ok((words, transcript.language))
    }
}

fn server_args(config: &LlamaConfig, port: u16) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--model".into(),
        config.model.clone().into(),
        "--mmproj".into(),
        config.mmproj.clone().into(),
    ];
    for (flag, value) in [
        ("--host", "127.0.0.1".to_string()),
        ("--port", port.to_string()),
        ("--threads", config.threads.to_string()),
        ("--ctx-size", CONTEXT_TOKENS.to_string()),
        ("--parallel", "1".to_string()),
    ] {
        args.push(flag.into());
        args.push(value.into());
    }
    args.push("--no-webui".into());
    args
}

/// HTTP side of the engine, separate from the process so it can be tested against a fake.
pub struct LlamaClient {
    base: String,
    key: String,
    reply: LlamaReply,
    prompt: Option<String>,
    agents: Agents,
}

impl LlamaClient {
    pub fn new(base: String, key: String, spec: &LlamaSpec) -> Self {
        Self {
            base,
            key,
            reply: spec.reply,
            prompt: spec.prompt.clone(),
            agents: Agents::default(),
        }
    }

    fn healthy(&self) -> bool {
        self.agents.healthy(&self.base)
    }

    /// One request: the WAV in, the model's transcript out. An answer cut at
    /// `MAX_ANSWER_TOKENS` is a model that kept repeating itself, not a transcript: an error, as
    /// the other language models' (`llm`).
    pub fn recognize(&self, wav: &[u8]) -> Result<Transcript, EngineError> {
        let answer = self.request(wav)?;
        if answer.cut {
            return Err(EngineError::BadResponse("the answer was cut off at its length".into()));
        }
        Ok(transcript(self.reply, &answer.text))
    }

    /// A window of a file, a meeting or a long dictation: an answer cut at its length (the model
    /// kept repeating itself over music or noise) is `None`, which leaves the window empty instead
    /// of failing the whole file, and the caller says in the log which window it was. At
    /// temperature 0 a retry would loop the same way.
    pub fn recognize_window(&self, wav: &[u8]) -> Result<Option<Transcript>, EngineError> {
        let answer = self.request(wav)?;
        Ok((!answer.cut).then(|| transcript(self.reply, &answer.text)))
    }

    fn request(&self, wav: &[u8]) -> Result<Answer, EngineError> {
        let mut content = vec![json!({
            "type": "input_audio",
            "input_audio": { "data": base64(wav), "format": "wav" },
        })];
        if let Some(prompt) = &self.prompt {
            content.push(json!({ "type": "text", "text": prompt }));
        }
        let body = json!({
            "messages": [{ "role": "user", "content": content }],
            "temperature": 0,
            "max_tokens": MAX_ANSWER_TOKENS,
            "stream": false,
        })
        .to_string()
        .into_bytes();
        let response = self
            .agents
            .inference
            .post(format!("{}/v1/chat/completions", self.base))
            .header("Authorization", &format!("Bearer {}", self.key))
            .header("Content-Type", "application/json")
            .send(&body[..]);
        let text = local_server::answer_text(response)?;
        answer(&text)
    }
}

/// The assistant's text in a chat completion, and whether it was cut at `MAX_ANSWER_TOKENS`.
#[derive(Debug, PartialEq)]
struct Answer {
    text: String,
    cut: bool,
}

fn answer(text: &str) -> Result<Answer, EngineError> {
    #[derive(Deserialize)]
    struct Reply {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        message: Message,
        finish_reason: Option<String>,
    }
    #[derive(Deserialize)]
    struct Message {
        content: Option<String>,
    }
    let reply: Reply =
        serde_json::from_str(text).map_err(|error| EngineError::BadResponse(error.to_string()))?;
    let choice = reply
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| EngineError::BadResponse("no choices".into()))?;
    Ok(Answer {
        text: choice.message.content.unwrap_or_default(),
        cut: choice.finish_reason.as_deref() == Some("length"),
    })
}

/// The transcript in a model's answer.
fn transcript(reply: LlamaReply, answer: &str) -> Transcript {
    let (language, text) = match (reply, answer.split_once("<asr_text>")) {
        (LlamaReply::Qwen3Asr, Some((head, text))) => {
            let name = head.trim().strip_prefix("language").unwrap_or(head);
            (languages::code_for(name), text)
        }
        _ => (None, answer),
    };
    Transcript { text: tidy(text), language: language.map(String::from) }
}

/// A word with times in seconds from the audio start.
#[derive(Debug, Clone, PartialEq)]
struct TimedWord {
    /// With a leading space when it starts a new word (see `files::words::Word`).
    text: String,
    start: f64,
    end: f64,
}

/// The words of `text` spread over the speech in `audio` (16 kHz): each word gets a share of the
/// speech frames by its letters and digits, so pauses get no words.
fn estimate_times(text: &str, audio: &[f32]) -> Vec<TimedWord> {
    let pieces = split_words(text);
    if pieces.is_empty() {
        return Vec::new();
    }
    let frames = speech::speech_frames(audio, TARGET_RATE);
    let mut voiced: Vec<usize> =
        frames.iter().enumerate().filter(|(_, speech)| **speech).map(|(i, _)| i).collect();
    if voiced.is_empty() {
        voiced = (0..frames.len().max(1)).collect();
    }
    let frame = 1.0 / f64::from(speech::FRAMES_PER_SECOND);
    let weight = |word: &str| word.chars().filter(|c| c.is_alphanumeric()).count().max(1) as f64;
    let scale =
        voiced.len() as f64 / pieces.iter().map(|piece| weight(piece.as_str())).sum::<f64>();
    let last = voiced.len() - 1;
    // Seconds at `position` voiced frames into the speech; an end that falls on a frame border
    // stays in the frame before it, so a word does not stretch over the pause after it.
    let start_at = |position: f64| {
        let index = (position.floor() as usize).min(last);
        (voiced[index] as f64 + (position - index as f64).clamp(0.0, 1.0)) * frame
    };
    let end_at = |position: f64| {
        let index = (position.ceil() as usize).saturating_sub(1).min(last);
        (voiced[index] as f64 + (position - index as f64).clamp(0.0, 1.0)) * frame
    };
    let mut done = 0.0;
    pieces
        .into_iter()
        .map(|text| {
            let from = done * scale;
            done += weight(text.as_str());
            TimedWord { start: start_at(from), end: end_at(done * scale), text }
        })
        .collect()
}

/// Words as the file pipeline joins them: a leading space starts a word, and every character of
/// a script written without spaces is a word of its own (the punctuation after it stays with it).
fn split_words(text: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for token in text.split_whitespace() {
        if !token.chars().any(written_without_spaces) {
            words.push(format!(" {token}"));
            continue;
        }
        for (i, c) in token.chars().enumerate() {
            if i == 0 {
                words.push(format!(" {c}"));
            } else if written_without_spaces(c) {
                words.push(c.to_string());
            } else if let Some(last) = words.last_mut() {
                last.push(c);
            }
        }
    }
    words
}

/// A file window's words as whisper-server lays them out, and whether the model's answer was
/// cut at its length (then there are none).
#[derive(Debug, PartialEq)]
pub struct WindowWords {
    pub json: String,
    pub cut: bool,
}

/// One segment per word, read by `files::words::parse_words` like whisper-server's reply.
fn words_json(words: &[TimedWord]) -> String {
    let segments: Vec<_> = words
        .iter()
        .map(|word| {
            json!({
                "text": word.text,
                "start": word.start,
                "end": word.end,
                "words": [{ "end": word.end }],
            })
        })
        .collect();
    json!({ "segments": segments }).to_string()
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;
    use std::time::Instant;

    use super::*;

    /// The engine the installer carries is used when it is there; without it, the development build.
    #[test]
    fn the_installed_engine_comes_first() {
        let resources = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/test-artifacts/llama-resources-{}", std::process::id()));
        assert_eq!(engine_dir(Some(&resources)), dev_engine_dir());
        assert_eq!(engine_dir(None), dev_engine_dir());
        let installed = resources.join("engines").join("llama");
        std::fs::create_dir_all(&installed).unwrap();
        assert_eq!(engine_dir(Some(&resources)), dev_engine_dir(), "a folder without the program");
        std::fs::write(executable_in(&installed), b"").unwrap();
        assert_eq!(engine_dir(Some(&resources)), installed);
        let _ = std::fs::remove_dir_all(&resources);
    }
    use crate::files::words::{join_text, parse_words};
    use crate::runtimes::test_server::{Response, TestServer};
    use crate::storage::catalog::ModelFile;

    fn spec(reply: LlamaReply, prompt: Option<&str>) -> LlamaSpec {
        LlamaSpec {
            mmproj: ModelFile {
                file: "mmproj.gguf".into(),
                url: String::new(),
                size: 1,
                sha256: String::new(),
            },
            reply,
            prompt: prompt.map(String::from),
        }
    }

    fn config() -> LlamaConfig {
        LlamaConfig {
            executable: PathBuf::from("llama-server"),
            model: PathBuf::from("model.gguf"),
            mmproj: PathBuf::from("mmproj.gguf"),
            log: PathBuf::from("engine.log"),
            threads: 6,
        }
    }

    fn completion(content: &str) -> Vec<u8> {
        json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
            .to_string()
            .into_bytes()
    }

    #[test]
    fn server_listens_on_loopback_without_the_key_in_its_arguments() {
        let args: Vec<String> =
            server_args(&config(), 4242).iter().map(|arg| arg.to_string_lossy().into()).collect();
        let joined = args.join(" ");
        assert!(joined.starts_with("--model model.gguf --mmproj mmproj.gguf"));
        assert!(joined.contains("--host 127.0.0.1 --port 4242"));
        assert!(joined.contains("--threads 6"));
        assert!(joined.contains("--ctx-size 4096 --parallel 1"));
        assert!(joined.ends_with("--no-webui"));
        assert!(!joined.contains("api-key"));
    }

    #[test]
    fn audio_goes_with_the_key_and_the_transcript_comes_back() {
        let server = TestServer::start(|_| {
            Response::new(200, completion("language English<asr_text>  Hello,\n world. "))
        });
        let client = LlamaClient::new(
            server.base.clone(),
            "secret".into(),
            &spec(LlamaReply::Qwen3Asr, None),
        );
        let transcript = client.recognize(b"RIFFfakewav").unwrap();
        assert_eq!(
            transcript,
            Transcript { text: "Hello, world.".into(), language: Some("en".into()) }
        );
        let request = &server.requests()[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/chat/completions");
        assert_eq!(request.header("authorization"), Some("Bearer secret"));
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "input_audio");
        assert_eq!(content[0]["input_audio"]["data"], base64(b"RIFFfakewav"));
        assert_eq!(content[0]["input_audio"]["format"], "wav");
        assert_eq!(content.as_array().unwrap().len(), 1, "no instruction for Qwen3-ASR");
        assert_eq!(body["temperature"], 0);
    }

    #[test]
    fn an_instruction_follows_the_audio() {
        let server = TestServer::start(|_| Response::new(200, completion("Hello.")));
        let spec = spec(LlamaReply::Text, Some("Transcribe the audio."));
        let client = LlamaClient::new(server.base.clone(), "k".into(), &spec);
        assert_eq!(client.recognize(b"wav").unwrap().text, "Hello.");
        let body: serde_json::Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
        let content = &body["messages"][0]["content"];
        assert_eq!(content[1], json!({ "type": "text", "text": "Transcribe the audio." }));
    }

    #[test]
    fn answers_become_transcripts() {
        let qwen = |answer| transcript(LlamaReply::Qwen3Asr, answer);
        assert_eq!(qwen("language Russian<asr_text>Привет.").language.as_deref(), Some("ru"));
        assert_eq!(
            qwen("language None<asr_text>"),
            Transcript { text: String::new(), language: None }
        );
        assert_eq!(qwen("no marker").text, "no marker");
        let plain = transcript(LlamaReply::Text, " language English<asr_text>x ");
        assert_eq!(plain.text, "language English<asr_text>x", "a plain answer is kept whole");
    }

    #[test]
    fn engine_errors_are_reported() {
        let server = TestServer::start(|_| Response::new(500, b"model failed".to_vec()));
        let client =
            LlamaClient::new(server.base.clone(), "k".into(), &spec(LlamaReply::Text, None));
        let error = client.recognize(b"wav").unwrap_err();
        assert!(matches!(error, EngineError::Http(detail) if detail == "HTTP 500: model failed"));
        assert!(matches!(answer("not json"), Err(EngineError::BadResponse(_))));
        assert!(matches!(answer(r#"{"choices":[]}"#), Err(EngineError::BadResponse(_))));
        let whole = r#"{"choices":[{"message":{"content":"Thanks."},"finish_reason":"stop"}]}"#;
        assert_eq!(answer(whole).unwrap(), Answer { text: "Thanks.".into(), cut: false });
    }

    /// Windows of a long dictation and of a file go as windows (`recognize_window`): a window
    /// whose answer is cut leaves only itself empty, and the dictation keeps the rest.
    #[cfg(unix)]
    #[test]
    fn windows_go_as_windows() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cut = r#"{"choices":[{"message":{"content":"x x x"},"finish_reason":"length"}]}"#;
        let whole = r#"{"choices":[{"message":{"content":"two"},"finish_reason":"stop"}]}"#;
        // Every other answer is cut: the first window's, then the file window's.
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&requests);
        let server = TestServer::start(move |_| {
            let body =
                if counted.fetch_add(1, Ordering::SeqCst).is_multiple_of(2) { cut } else { whole };
            Response::new(200, body.as_bytes().to_vec())
        });
        let mut sleep = Command::new("sleep");
        sleep.arg("60");
        let log = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/test-artifacts/llama-windows-{}.log", std::process::id()));
        let process =
            ServerProcess::spawn(sleep, std::path::Path::new("/bin/sleep"), &log).unwrap();
        let client =
            LlamaClient::new(server.base.clone(), "k".into(), &spec(LlamaReply::Text, None));
        let engine = LlamaServer { process, client };
        // 40 s of a tone: two windows, the first cut.
        let samples = tone(40.0);
        let transcript = engine.transcribe(&samples, 16_000).expect("not failed by one window");
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(transcript.text, "two");
        // A file window cut: no words, no error.
        let window = engine.transcribe_words(&samples[..16_000 * 5], 16_000).unwrap();
        assert_eq!(window, WindowWords { json: words_json(&[]), cut: true });
    }

    /// An answer cut at its length (a model repeating itself) fails a dictation of one request,
    /// and leaves a window of a file empty without failing the file.
    #[test]
    fn a_cut_answer_fails_a_dictation_and_empties_a_window() {
        let cut =
            r#"{"choices":[{"message":{"content":"Thanks. Thanks."},"finish_reason":"length"}]}"#;
        let server = TestServer::start(move |_| Response::new(200, cut.as_bytes().to_vec()));
        let client =
            LlamaClient::new(server.base.clone(), "k".into(), &spec(LlamaReply::Text, None));
        assert!(matches!(client.recognize(b"wav"), Err(EngineError::BadResponse(_))));
        assert_eq!(client.recognize_window(b"wav").unwrap(), None);
    }

    #[test]
    fn health_requires_ok_status() {
        let ok = TestServer::start(|_| Response::new(200, br#"{"status":"ok"}"#.to_vec()));
        let spec = spec(LlamaReply::Text, None);
        assert!(LlamaClient::new(ok.base.clone(), "k".into(), &spec).healthy());
        let loading =
            TestServer::start(|_| Response::new(503, br#"{"status":"loading"}"#.to_vec()));
        assert!(!LlamaClient::new(loading.base.clone(), "k".into(), &spec).healthy());
    }

    fn tone(seconds: f32) -> Vec<f32> {
        (0..(16_000.0 * seconds) as usize)
            .map(|i| 0.1 * (2.0 * PI * 220.0 * i as f32 / 16_000.0).sin())
            .collect()
    }

    /// Speech, a pause, speech: one second each.
    fn speech_pause_speech() -> Vec<f32> {
        let mut audio = tone(1.0);
        audio.extend(vec![0.0; 16_000]);
        audio.extend(tone(1.0));
        audio
    }

    #[test]
    fn estimated_words_keep_to_the_speech() {
        let words = estimate_times("one two", &speech_pause_speech());
        let texts: Vec<&str> = words.iter().map(|word| word.text.as_str()).collect();
        assert_eq!(texts, [" one", " two"]);
        assert!(words[0].start < 0.05 && (words[0].end - 1.0).abs() < 0.05, "{words:?}");
        assert!((words[1].start - 2.0).abs() < 0.05 && words[1].end > 2.95, "{words:?}");
    }

    #[test]
    fn longer_words_get_longer_times() {
        let words = estimate_times("a abcdefg", &tone(2.0));
        assert!(words[1].end - words[1].start > 5.0 * (words[0].end - words[0].start));
        assert!(words.windows(2).all(|pair| pair[0].end <= pair[1].start + 1e-9));
    }

    #[test]
    fn scripts_without_spaces_give_a_word_per_character() {
        assert_eq!(
            split_words("我们去吧。 AI模型"),
            [" 我", "们", "去", "吧。", " AI", "模", "型"]
        );
        assert!(estimate_times("", &tone(1.0)).is_empty());
    }

    /// The JSON goes through the same parser as whisper-server's reply.
    #[test]
    fn file_pipeline_reads_the_estimated_words() {
        let json = words_json(&estimate_times("Hello, world.", &speech_pause_speech()));
        let window = Window {
            index: 0,
            start: 32_000,
            end: 32_000 + 48_000,
            keep_from: 0.0,
            keep_to: 30.0,
            last: true,
        };
        let words = parse_words(&json, &window).unwrap();
        assert_eq!(join_text(&words), "Hello, world.");
        assert!(words[0].start >= 2.0 && words[1].end <= 5.0, "file time: {words:?}");
    }

    /// Real engine and models: `tools/fetch-llama-cpp`, both Qwen3-ASR models in
    /// `runtimes/models` (`AHEM_MODEL_ID=<id> cargo test -- --ignored downloads_turbo`),
    /// recordings in `runtimes/test-audio`.
    #[test]
    #[ignore = "needs the llama.cpp build and both Qwen3-ASR models: cargo test -- --ignored llama_end_to_end"]
    fn llama_end_to_end() {
        for id in ["qwen3-asr-1_7b", "qwen3-asr-0_6b"] {
            println!("{id}");
            transcribes_test_audio(id);
        }
    }

    fn transcribes_test_audio(id: &str) {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtimes = manifest.join("..").join("runtimes");
        let model = crate::storage::catalog::find(id).unwrap();
        let llama = model.llama.as_ref().unwrap();
        let dir = runtimes.join("models").join(id);
        let config = LlamaConfig {
            executable: executable_in(&dev_engine_dir()),
            model: dir.join(&model.file),
            mmproj: dir.join(&llama.mmproj.file),
            log: manifest.join(format!("target/test-artifacts/llama-end-to-end-{id}.log")),
            threads: 6,
        };
        let started = Instant::now();
        let server = LlamaServer::start(&config, llama, &AtomicBool::new(false)).unwrap();
        println!("ready in {:.1} s", started.elapsed().as_secs_f64());
        let read = |name: &str| {
            let path = runtimes.join("test-audio").join(name);
            sherpa_onnx::Wave::read(&path.to_string_lossy()).expect("test audio")
        };
        let plain = |text: &str| {
            let letters: String = text
                .to_lowercase()
                .replace('ё', "е")
                .chars()
                .filter(|c| c.is_alphanumeric() || c.is_whitespace())
                .collect();
            tidy(&letters)
        };
        let short = read("en-short.wav");
        let transcript = server.transcribe(short.samples(), short.sample_rate() as u32).unwrap();
        println!("en-short: {transcript:?}");
        assert!(plain(&transcript.text).contains("quick brown fox"));
        assert_eq!(transcript.language.as_deref(), Some("en"));

        let phrase = read("ru-short.wav");
        let started = Instant::now();
        let transcript = server.transcribe(phrase.samples(), phrase.sample_rate() as u32).unwrap();
        let took = started.elapsed().as_secs_f64();
        println!("ru-short (11.3 s) in {took:.2} s: {transcript:?}");
        assert_eq!(transcript.language.as_deref(), Some("ru"));

        let long = read("ru-long.wav");
        let rate = long.sample_rate() as u32;
        let started = Instant::now();
        let dictation = server.transcribe(long.samples(), rate).unwrap();
        let took = started.elapsed().as_secs_f64();
        println!("ru-long (71 s) as a dictation in {took:.2} s: {dictation:?}");
        assert_eq!(dictation.language.as_deref(), Some("ru"));
        let audio = resample::to_16k(long.samples(), rate);
        let parts: Vec<_> = chunks::plan(&audio)
            .into_iter()
            .map(|window| {
                let json =
                    server.transcribe_words(&window.audio(&audio), TARGET_RATE).unwrap().json;
                let words = parse_words(&json, &window).unwrap();
                (window, words)
            })
            .collect();
        assert!(parts.len() > 1, "the long recording spans several windows");
        let file = join_text(&words::merge(parts));
        println!("ru-long as file windows: {file}");
        for (how, text) in [("one dictation", &dictation.text), ("file windows", &file)] {
            let text = plain(text);
            for line in ["отошла давно", "трепещет луч лампады", "я разрешу тебя"]
            {
                assert!(text.contains(line), "{how}: no {line:?} in {text}");
            }
            // A seam that kept words twice repeats three of them in a row.
            let words: Vec<&str> = text.split(' ').collect();
            let mut seen = std::collections::HashSet::new();
            for three in words.windows(3) {
                assert!(seen.insert(three.join(" ")), "{how}: {three:?} twice in {text}");
            }
        }
        server.stop().unwrap();
    }
}

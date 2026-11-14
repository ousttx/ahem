//! whisper.cpp `whisper-server` as a separate process on 127.0.0.1.
//!
//! Launch: a free port, a random request path (against stray requests from other programs; it
//! is on the command line, so not a secret from the user's own processes), readiness through
//! `/health`, WAV audio posted to `/inference`, engine output written to a log file.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::{fmt, fs, io};

use serde::Deserialize;

use super::local_server::{self, Agents, ServerProcess};
use crate::files::WORD_TIMESTAMP_FIELDS;
use crate::util::random_hex;

pub const ENGINE_VERSION: &str = "1.9.5";

/// Engine built by `tools/build-engine` for development runs.
pub fn dev_engine_dir() -> PathBuf {
    let name =
        format!("whisper-{ENGINE_VERSION}-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("runtimes").join(name)
}

/// Where the engine is: the one the installer carries (`engines/whisper` of the app's resources),
/// else the build of `tools` for development runs.
pub fn engine_dir(resources: Option<&Path>) -> PathBuf {
    if let Some(dir) = resources.map(|resources| resources.join("engines").join("whisper"))
        && executable_in(&dir).is_file()
    {
        return dir;
    }
    dev_engine_dir()
}

pub fn executable_in(dir: &Path) -> PathBuf {
    dir.join(format!("whisper-server{}", std::env::consts::EXE_SUFFIX))
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub executable: PathBuf,
    pub model: PathBuf,
    pub log: PathBuf,
    pub threads: usize,
}

impl ServerConfig {
    /// Logical cores, at most six.
    pub fn default_threads() -> usize {
        std::thread::available_parallelism().map_or(4, |n| n.get()).min(6)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscribeOptions {
    /// `None` lets Whisper detect the language.
    pub language: Option<String>,
    pub translate: bool,
    /// Text that primes the decoder, e.g. the user's dictionary terms.
    pub prompt: Option<String>,
    /// Ask for one segment per word with its times (file transcription).
    pub words: bool,
}

/// Longest dictionary prompt, in `units`: the key terms of cloud engines (Deepgram,
/// gpt-transcribe).
const MAX_PROMPT_UNITS: usize = 600;
/// Longest prompt a Whisper engine gets, in `units`. Whisper reads only the last 223 tokens of a
/// prompt (whisper.cpp, and the Whisper of cloud services likewise), so a longer prompt would
/// lose its first terms.
const WHISPER_PROMPT_UNITS: usize = 300;

/// A prompt's length in rough thirds of a token: words of Latin, Cyrillic or Greek letters run
/// at two or three characters a token, while a character of three or more bytes in UTF-8
/// (Chinese, Japanese, Korean, Devanagari, Thai, Georgian and the like) is a token or more.
fn units(text: &str) -> usize {
    text.chars().map(char_units).sum()
}

fn char_units(c: char) -> usize {
    if c.len_utf8() >= 3 { 3 } else { 1 }
}

/// What separates dictionary terms: commas, semicolons and new lines, also as Chinese and
/// Japanese keyboards type them.
const TERM_SEPARATORS: [char; 6] = [',', ';', '\n', '，', '、', '；'];

/// Dictionary terms (separated by commas, semicolons or new lines) as one engine prompt.
pub fn prompt_from_dictionary(dictionary: &str) -> Option<String> {
    let terms: Vec<&str> =
        dictionary.split(TERM_SEPARATORS).map(str::trim).filter(|term| !term.is_empty()).collect();
    if terms.is_empty() {
        return None;
    }
    Some(whole_terms(&terms, MAX_PROMPT_UNITS))
}

/// The prompt as a Whisper engine gets it: as many whole terms as fit.
pub fn whisper_prompt(prompt: &str) -> String {
    let terms: Vec<&str> = prompt.split(", ").collect();
    whole_terms(&terms, WHISPER_PROMPT_UNITS)
}

/// Terms joined by ", " up to `limit` units. Whole terms only: a cut one ("Kubern") would be a
/// word nobody says, and cloud engines take each term as a key term of its own.
fn whole_terms(terms: &[&str], limit: usize) -> String {
    let mut prompt = String::new();
    for &term in terms {
        let length = units(&prompt) + if prompt.is_empty() { 0 } else { 2 };
        if length + units(term) <= limit {
            if !prompt.is_empty() {
                prompt.push_str(", ");
            }
            prompt.push_str(term);
            continue;
        }
        // One term longer than the whole prompt (text without separators): its whole words.
        if prompt.is_empty() {
            let mut used = 0;
            let fits: String = term
                .chars()
                .take_while(|&c| {
                    used += char_units(c);
                    used <= limit
                })
                .collect();
            let next = term[fits.len()..].chars().next();
            prompt = match fits.rfind(char::is_whitespace) {
                // Cut inside a word: back to its start.
                Some(end) if next.is_some_and(|c| !c.is_whitespace()) => {
                    fits[..end].trim_end().to_string()
                }
                _ => fits.trim_end().to_string(),
            };
        }
        break;
    }
    prompt
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
}

#[derive(Debug)]
pub enum EngineError {
    Start(String),
    Exited(Option<i32>),
    Timeout,
    Cancelled,
    Http(String),
    BadResponse(String),
    Io(io::Error),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start(detail) => write!(f, "cannot start the engine: {detail}"),
            Self::Exited(Some(code)) => write!(f, "engine exited with code {code}"),
            Self::Exited(None) => write!(f, "engine exited"),
            Self::Timeout => {
                let seconds = local_server::START_TIMEOUT.as_secs();
                write!(f, "engine did not become ready in {seconds}s")
            }
            Self::Cancelled => write!(f, "engine start cancelled"),
            Self::Http(detail) => write!(f, "engine request failed: {detail}"),
            Self::BadResponse(detail) => write!(f, "unexpected engine response: {detail}"),
            Self::Io(error) => write!(f, "engine I/O error: {error}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// The server ended before it was ready because its port was taken (its `log` says so).
fn port_was_taken(error: &EngineError, log: &[u8]) -> bool {
    const BIND_FAILED: &[u8] = b"couldn't bind to server socket";
    matches!(error, EngineError::Exited(_))
        && log.windows(BIND_FAILED.len()).any(|w| w == BIND_FAILED)
}

impl From<io::Error> for EngineError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct WhisperServer {
    process: ServerProcess,
    client: EngineClient,
}

impl WhisperServer {
    /// Starts the process and waits until it answers `/health`. whisper-server takes its port
    /// only after loading the model, seconds later: should another program have taken it
    /// meanwhile, it starts once more on another port.
    pub fn start(config: &ServerConfig, cancel: &AtomicBool) -> Result<Self, EngineError> {
        match Self::start_on_a_free_port(config, cancel) {
            Err(error) if port_was_taken(&error, &fs::read(&config.log).unwrap_or_default()) => {
                Self::start_on_a_free_port(config, cancel)
            }
            result => result,
        }
    }

    fn start_on_a_free_port(
        config: &ServerConfig,
        cancel: &AtomicBool,
    ) -> Result<Self, EngineError> {
        let port = local_server::free_port()?;
        let request_path = format!("/{}", random_hex()?);
        let mut command = Command::new(&config.executable);
        command.args(server_args(config, port, &request_path));
        let mut process = ServerProcess::spawn(command, &config.executable, &config.log)?;
        let client = EngineClient::new(format!("http://127.0.0.1:{port}{request_path}"));
        process.wait_ready(cancel, || client.healthy())?;
        Ok(Self { process, client })
    }

    pub fn transcribe(
        &self,
        wav: &[u8],
        options: &TranscribeOptions,
    ) -> Result<Transcript, EngineError> {
        self.client.transcribe(wav, options)
    }

    /// The engine's `verbose_json` reply as it is, for word times.
    pub fn transcribe_raw(
        &self,
        wav: &[u8],
        options: &TranscribeOptions,
    ) -> Result<String, EngineError> {
        self.client.transcribe_raw(wav, options)
    }

    /// After a failed request: whether the server died.
    pub fn died(&mut self) -> bool {
        self.process.died()
    }

    pub fn stop(self) -> io::Result<()> {
        self.process.stop()
    }
}

fn server_args(config: &ServerConfig, port: u16, request_path: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--model".into(), config.model.clone().into()];
    for (flag, value) in [
        ("--host", "127.0.0.1".to_string()),
        ("--port", port.to_string()),
        ("--request-path", request_path.to_string()),
        ("--threads", config.threads.to_string()),
        ("--language", "auto".to_string()),
    ] {
        args.push(flag.into());
        args.push(value.into());
    }
    args.push("--no-timestamps".into());
    args
}

/// HTTP side of the engine, separate from the process so it can be tested against a fake.
pub struct EngineClient {
    base: String,
    agents: Agents,
}

impl EngineClient {
    pub fn new(base: String) -> Self {
        Self { base, agents: Agents::default() }
    }

    fn healthy(&self) -> bool {
        self.agents.healthy(&self.base)
    }

    pub fn transcribe(
        &self,
        wav: &[u8],
        options: &TranscribeOptions,
    ) -> Result<Transcript, EngineError> {
        parse_transcript(&self.transcribe_raw(wav, options)?)
    }

    pub fn transcribe_raw(
        &self,
        wav: &[u8],
        options: &TranscribeOptions,
    ) -> Result<String, EngineError> {
        let boundary = format!("ahem-{}", random_hex()?);
        let body = multipart(&boundary, &fields(options), wav)?;
        let response = self
            .agents
            .inference
            .post(format!("{}/inference", self.base))
            .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
            .send(&body[..]);
        local_server::answer_text(response)
    }
}

fn fields(options: &TranscribeOptions) -> Vec<(&'static str, String)> {
    let mut fields = vec![
        ("language", options.language.clone().unwrap_or_else(|| "auto".into())),
        ("translate", options.translate.to_string()),
    ];
    if options.words {
        fields.extend(WORD_TIMESTAMP_FIELDS.iter().map(|(name, value)| (*name, value.to_string())));
    } else {
        fields.push(("response_format", "verbose_json".into()));
        fields.push(("no_timestamps", "true".into()));
    }
    // The server copies its defaults for every request, so an absent prompt means none.
    if let Some(prompt) = &options.prompt {
        fields.push(("prompt", whisper_prompt(prompt)));
    }
    fields
}

/// A `multipart/form-data` body: the fields, then the WAV as `file`.
pub(crate) fn multipart(
    boundary: &str,
    fields: &[(&str, String)],
    wav: &[u8],
) -> io::Result<Vec<u8>> {
    let mut body = Vec::with_capacity(wav.len() + 1024);
    for (name, value) in fields {
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        )?;
    }
    write!(
        body,
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n\
         Content-Type: audio/wav\r\n\r\n"
    )?;
    body.extend_from_slice(wav);
    write!(body, "\r\n--{boundary}--\r\n")?;
    Ok(body)
}

fn parse_transcript(text: &str) -> Result<Transcript, EngineError> {
    #[derive(Deserialize)]
    struct Reply {
        text: String,
        language: Option<String>,
    }
    let reply: Reply =
        serde_json::from_str(text).map_err(|error| EngineError::BadResponse(error.to_string()))?;
    let text = reply.text.split_whitespace().collect::<Vec<_>>().join(" ");
    Ok(Transcript { text, language: reply.language })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine the installer carries is used when it is there; without it, the development build.
    #[test]
    fn the_installed_engine_comes_first() {
        let resources = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/test-artifacts/whisper-resources-{}", std::process::id()));
        assert_eq!(engine_dir(Some(&resources)), dev_engine_dir());
        assert_eq!(engine_dir(None), dev_engine_dir());
        let installed = resources.join("engines").join("whisper");
        std::fs::create_dir_all(&installed).unwrap();
        assert_eq!(engine_dir(Some(&resources)), dev_engine_dir(), "a folder without the program");
        std::fs::write(executable_in(&installed), b"").unwrap();
        assert_eq!(engine_dir(Some(&resources)), installed);
        let _ = std::fs::remove_dir_all(&resources);
    }
    use crate::runtimes::test_server::{Response, TestServer};
    use std::fs;
    use std::time::Instant;

    fn config() -> ServerConfig {
        ServerConfig {
            executable: PathBuf::from("whisper-server"),
            model: PathBuf::from("model.bin"),
            log: PathBuf::from("engine.log"),
            threads: 6,
        }
    }

    #[test]
    fn server_listens_on_loopback_with_a_random_path() {
        let args: Vec<String> = server_args(&config(), 4242, "/abc")
            .iter()
            .map(|arg| arg.to_string_lossy().into())
            .collect();
        let joined = args.join(" ");
        assert!(joined.contains("--host 127.0.0.1 --port 4242 --request-path /abc"));
        assert!(joined.starts_with("--model model.bin"));
        assert!(joined.contains("--threads 6"));
    }

    #[test]
    fn transcribe_posts_wav_and_reads_text() {
        let server = TestServer::start(|_| {
            Response::new(200, br#"{"text":"  Hello,\n world. ","language":"en"}"#.to_vec())
        });
        let client = EngineClient::new(format!("{}/secret", server.base));
        let options = TranscribeOptions { language: Some("en".into()), ..Default::default() };
        let transcript = client.transcribe(b"RIFFfakewav", &options).unwrap();
        assert_eq!(
            transcript,
            Transcript { text: "Hello, world.".into(), language: Some("en".into()) }
        );

        let request = &server.requests()[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/secret/inference");
        let body = String::from_utf8_lossy(&request.body);
        assert!(body.contains("name=\"language\"\r\n\r\nen\r\n"));
        assert!(body.contains("name=\"translate\"\r\n\r\nfalse\r\n"));
        assert!(body.contains("RIFFfakewav"));
        let boundary = request.header("content-type").unwrap().split("boundary=").nth(1).unwrap();
        assert!(body.ends_with(&format!("--{boundary}--\r\n")));
    }

    #[test]
    fn auto_language_by_default() {
        let fields = fields(&TranscribeOptions::default());
        assert!(fields.contains(&("language", "auto".into())));
        assert!(fields.contains(&("no_timestamps", "true".into())));
        assert!(!fields.iter().any(|(name, _)| *name == "prompt"));
    }

    #[test]
    fn files_ask_for_word_times() {
        let fields = fields(&TranscribeOptions { words: true, ..Default::default() });
        assert!(fields.contains(&("no_timestamps", "false".into())));
        assert!(fields.contains(&("token_timestamps", "true".into())));
        assert!(fields.contains(&("max_len", "1".into())));
        assert_eq!(fields.iter().filter(|(name, _)| *name == "response_format").count(), 1);
    }

    #[test]
    fn language_and_dictionary_reach_the_engine() {
        let options = TranscribeOptions {
            language: Some("ru".into()),
            prompt: prompt_from_dictionary("Kubernetes, PostgreSQL\n Tauri ;; "),
            ..Default::default()
        };
        let fields = fields(&options);
        assert!(fields.contains(&("language", "ru".into())));
        assert!(fields.contains(&("prompt", "Kubernetes, PostgreSQL, Tauri".into())));
    }

    /// Only a server that ended because its port was taken starts again; a model that does not
    /// load is not loaded twice.
    #[test]
    fn a_taken_port_is_told_from_other_ends() {
        let taken =
            b"whisper_init...\n\ncouldn't bind to server socket: hostname=127.0.0.1 port=5\n\n";
        assert!(port_was_taken(&EngineError::Exited(Some(1)), taken));
        assert!(!port_was_taken(&EngineError::Exited(Some(1)), b"failed to load model"));
        assert!(!port_was_taken(&EngineError::Timeout, taken));
    }

    #[test]
    fn dictionary_prompt_is_bounded() {
        assert_eq!(prompt_from_dictionary(" , \n "), None);
        // Chinese and Japanese commas separate terms too.
        assert_eq!(
            prompt_from_dictionary("数据库，服务器、接口；API").as_deref(),
            Some("数据库, 服务器, 接口, API")
        );
        // Text without separators: its whole words.
        let long = "term ".repeat(500);
        let prompt = prompt_from_dictionary(&long).unwrap();
        assert!(units(&prompt) <= MAX_PROMPT_UNITS && prompt.ends_with("term"));
        // Many terms: as many whole ones as fit.
        let many = "Kubernetes, ".repeat(100);
        let prompt = prompt_from_dictionary(&many).unwrap();
        assert!(units(&prompt) <= MAX_PROMPT_UNITS);
        assert!(prompt.split(", ").all(|term| term == "Kubernetes"), "{prompt}");
        // A Whisper engine gets fewer: it would keep only the prompt's last tokens.
        let whisper = whisper_prompt(&prompt);
        assert!(units(&whisper) <= WHISPER_PROMPT_UNITS && whisper.starts_with("Kubernetes"));
        assert!(whisper.split(", ").all(|term| term == "Kubernetes"));
    }

    /// A character of Chinese is a token or more: such terms fill the prompt three times as fast.
    #[test]
    fn terms_without_spaces_count_as_more_tokens() {
        let many = "数据库, ".repeat(100);
        let whisper = whisper_prompt(&prompt_from_dictionary(&many).unwrap());
        assert_eq!(whisper.split(", ").count(), (WHISPER_PROMPT_UNITS + 2) / 11);
        // One long phrase without separators: cut at a character.
        let phrase = "数据库".repeat(100);
        assert_eq!(whisper_prompt(&phrase).chars().count(), WHISPER_PROMPT_UNITS / 3);
        // Other scripts of long characters too: Devanagari, Thai.
        for word in ["नमस्ते", "สวัสดี"] {
            assert_eq!(units(word), word.chars().count() * 3, "{word}");
        }
        assert_eq!(units("привет"), 6, "Cyrillic runs like Latin");
    }

    #[test]
    fn engine_errors_are_reported() {
        let server = TestServer::start(|_| Response::new(500, b"model failed".to_vec()));
        let client = EngineClient::new(server.base.clone());
        let error = client.transcribe(b"wav", &TranscribeOptions::default()).unwrap_err();
        assert!(matches!(error, EngineError::Http(detail) if detail == "HTTP 500: model failed"));
    }

    #[test]
    fn health_requires_ok_status() {
        let ok = TestServer::start(|_| Response::new(200, br#"{"status":"ok"}"#.to_vec()));
        assert!(EngineClient::new(ok.base.clone()).healthy());
        let loading =
            TestServer::start(|_| Response::new(503, br#"{"status":"loading model"}"#.to_vec()));
        assert!(!EngineClient::new(loading.base.clone()).healthy());
    }

    /// Real engine and model from `runtimes/`: `tools/build-engine`, the
    /// `downloads_turbo_from_hugging_face` test and synthesized audio in `runtimes/test-audio`.
    #[test]
    #[ignore = "needs the built engine, the Turbo model and test audio: cargo test -- --ignored end_to_end"]
    fn transcribes_speech_end_to_end() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtimes = manifest.join("..").join("runtimes");
        let config = ServerConfig {
            executable: executable_in(&dev_engine_dir()),
            model: runtimes.join("models/whisper-large-v3-turbo/ggml-large-v3-turbo.bin"),
            log: manifest.join("target/test-artifacts/engine-end-to-end.log"),
            threads: ServerConfig::default_threads(),
        };
        let started = Instant::now();
        let server = WhisperServer::start(&config, &AtomicBool::new(false)).unwrap();
        println!("ready in {:?}", started.elapsed());
        let wav = fs::read(runtimes.join("test-audio/en-short.wav")).unwrap();
        let started = Instant::now();
        let transcript = server.transcribe(&wav, &TranscribeOptions::default()).unwrap();
        println!("{transcript:?} in {:?}", started.elapsed());
        assert!(transcript.text.to_lowercase().contains("quick brown fox"));
        server.stop().unwrap();
    }

    /// Same method for every build: `AHEM_ENGINE_DIR` selects the engine (default: our build),
    /// `AHEM_MODEL` the model (default: Turbo), `AHEM_AUDIO` the clips (default:
    /// `en-short,en-long`). One warm-up, median of three runs.
    #[test]
    #[ignore = "benchmark: set AHEM_ENGINE_DIR, then cargo test -- --ignored benchmark --nocapture"]
    fn benchmark_engine() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtimes = manifest.join("..").join("runtimes");
        let env_path = |name| std::env::var_os(name).map(PathBuf::from);
        let dir = env_path("AHEM_ENGINE_DIR").unwrap_or_else(dev_engine_dir);
        let model = env_path("AHEM_MODEL").unwrap_or_else(|| {
            runtimes.join("models/whisper-large-v3-turbo/ggml-large-v3-turbo.bin")
        });
        let log = manifest.join("target/test-artifacts/engine-benchmark.log");
        let config = ServerConfig {
            executable: executable_in(&dir),
            model,
            log: log.clone(),
            threads: ServerConfig::default_threads(),
        };
        let started = Instant::now();
        let server = WhisperServer::start(&config, &AtomicBool::new(false)).unwrap();
        println!("engine: {}", dir.display());
        println!("load: {:.2} s", started.elapsed().as_secs_f64());
        let options = TranscribeOptions::default();
        let clips = std::env::var("AHEM_AUDIO").unwrap_or_else(|_| "en-short,en-long".into());
        for name in clips.split(',') {
            let wav = fs::read(runtimes.join(format!("test-audio/{name}.wav"))).unwrap();
            let audio = (wav.len() - 44) as f64 / 32_000.0;
            let first = Instant::now();
            let text = server.transcribe(&wav, &options).unwrap().text;
            println!("{name}: first request {:.2} s: {text}", first.elapsed().as_secs_f64());
            let mut runs: Vec<f64> = (0..3)
                .map(|_| {
                    let run = Instant::now();
                    server.transcribe(&wav, &options).unwrap();
                    run.elapsed().as_secs_f64()
                })
                .collect();
            runs.sort_by(f64::total_cmp);
            println!(
                "{name} ({audio:.1} s audio): median {:.2} s, {:.1}x realtime",
                runs[1],
                audio / runs[1]
            );
        }
        server.stop().unwrap();
        let log = fs::read_to_string(&log).unwrap();
        for line in log.lines().filter(|line| line.contains("backend")) {
            println!("{line}");
        }
    }

    #[test]
    fn bad_replies_are_errors() {
        assert!(matches!(parse_transcript("not json"), Err(EngineError::BadResponse(_))));
        assert!(matches!(parse_transcript(r#"{"error":"x"}"#), Err(EngineError::BadResponse(_))));
    }
}

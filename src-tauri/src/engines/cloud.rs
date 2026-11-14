//! Speech recognition by cloud services: Groq through its Whisper API, OpenAI with
//! `gpt-transcribe`, Deepgram with Nova-3. Audio leaves the computer only for an engine the user
//! chose instead of a local model.
//!
//! Errors carry only HTTP statuses and fixed descriptions, never the key, the audio, the text or
//! a response body: a service may echo the key or what was said. Redirects are not followed.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use ureq::Agent;

use super::languages;
use super::llm::OPENAI_BASE_URL;
use super::whisper_server::{TranscribeOptions, Transcript, multipart};
use crate::domain::services::{GROQ_BASE_URL, Service};
use crate::util::random_hex;

pub const DEEPGRAM_BASE_URL: &str = "https://api.deepgram.com/v1";

/// A long dictation takes a while to upload and recognize.
const AUDIO_TIMEOUT: Duration = Duration::from_secs(120);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// Groq and OpenAI take files up to 25 MB, about 13 minutes of the app's WAV.
const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;
/// Deepgram boosts at most 100 key terms per request; OpenAI gets as many keywords.
const MAX_KEYTERMS: usize = 100;

/// Deepgram Nova-3's languages (developers.deepgram.com, "Models & Languages", checked
/// 2026-10-25); the interface offers those Whisper also knows as the spoken language.
const NOVA_3_LANGUAGES: &[&str] = &[
    "af", "ar", "as", "be", "bg", "bn", "bs", "ca", "cs", "da", "de", "el", "en", "es", "et", "fa",
    "fi", "fr", "gl", "gu", "he", "hi", "hr", "hu", "hy", "id", "it", "ja", "ka", "kk", "kn", "ko",
    "lt", "lv", "mk", "mn", "mr", "ms", "ne", "nl", "no", "pa", "pl", "ps", "pt", "ro", "ru", "sk",
    "sl", "sr", "sv", "ta", "te", "th", "tl", "tr", "uk", "ur", "vi", "xh", "zh", "zu",
];

/// A cloud engine the user can choose instead of a local model; its id is stored as the
/// settings' `model_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudSpec {
    /// Never a catalog model id, and without dots: it is an interface translation key.
    pub id: &'static str,
    /// As the interface shows it: the service and its model.
    pub name: &'static str,
    pub service: Service,
    /// The service's model.
    pub model: &'static str,
    pub base_url: &'static str,
    pub api: CloudApi,
    /// The service translates speech to English.
    pub translate: bool,
}

/// How a service is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudApi {
    /// The OpenAI audio API with a Whisper model: translation, `prompt`, word times.
    Whisper,
    /// OpenAI's `gpt-transcribe`: `languages[]` and `keywords[]`; no translation, no word times.
    Transcribe,
    /// Deepgram's `listen`: `language` and `keyterm`, word times.
    Deepgram,
}

/// The cloud engines. OpenAI's model is `gpt-transcribe`, the one OpenAI recommends.
pub const ENGINES: [CloudSpec; 3] = [
    CloudSpec {
        id: "groq-whisper-large-v3",
        name: "Groq · whisper-large-v3",
        service: Service::Groq,
        model: "whisper-large-v3",
        base_url: GROQ_BASE_URL,
        api: CloudApi::Whisper,
        translate: true,
    },
    CloudSpec {
        id: "openai-gpt-transcribe",
        name: "OpenAI · gpt-transcribe",
        service: Service::OpenAi,
        model: "gpt-transcribe",
        base_url: OPENAI_BASE_URL,
        api: CloudApi::Transcribe,
        translate: false,
    },
    CloudSpec {
        id: "deepgram-nova-3",
        name: "Deepgram · nova-3",
        service: Service::Deepgram,
        model: "nova-3",
        base_url: DEEPGRAM_BASE_URL,
        api: CloudApi::Deepgram,
        translate: false,
    },
];

pub fn find(id: &str) -> Option<CloudSpec> {
    ENGINES.iter().copied().find(|spec| spec.id == id)
}

impl CloudSpec {
    /// Codes of the spoken languages it recognizes.
    pub fn languages(&self) -> Vec<&'static str> {
        let all = languages::all().iter().map(|(code, _)| *code);
        match self.service {
            Service::Deepgram => all.filter(|code| NOVA_3_LANGUAGES.contains(code)).collect(),
            _ => all.collect(),
        }
    }

    /// The service gives word times, which file transcription needs.
    pub fn word_times(&self) -> bool {
        self.api != CloudApi::Transcribe
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudError {
    /// HTTP 400: the service did not accept the audio or an option, e.g. the language.
    BadRequest,
    /// HTTP 401: the key is wrong, revoked or expired.
    Unauthorized,
    /// HTTP 402: no balance left.
    QuotaExceeded,
    /// HTTP 403: the key may not use this API.
    Forbidden,
    /// HTTP 404: the model or the API is not available.
    NotFound,
    /// Over the upload limit, or HTTP 413.
    TooLarge,
    /// HTTP 429: too many requests, or the balance is used up.
    RateLimited,
    /// The service gives no word times, so it cannot transcribe files.
    NoWordTimes,
    /// Any other HTTP error status.
    Server(u16),
    /// No HTTP answer: DNS, connection, TLS, timeout.
    Network(String),
    /// The reply is not in the expected format.
    BadResponse(String),
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadRequest => write!(f, "the service did not accept the audio or the options"),
            Self::Unauthorized => write!(f, "the API key was not accepted"),
            Self::QuotaExceeded => write!(f, "no balance left"),
            Self::Forbidden => write!(f, "the API key has no access to this API"),
            Self::NotFound => write!(f, "the recognition model or API is not available"),
            Self::TooLarge => write!(f, "the recording is larger than the service accepts"),
            Self::RateLimited => {
                write!(f, "too many requests or the balance is used up, try again later")
            }
            Self::NoWordTimes => write!(f, "the service gives no word times for files"),
            Self::Server(status) => write!(f, "HTTP {status}"),
            Self::Network(detail) => write!(f, "cannot reach the service: {detail}"),
            Self::BadResponse(detail) => write!(f, "unexpected reply: {detail}"),
        }
    }
}

impl std::error::Error for CloudError {}

impl CloudError {
    fn from_status(status: u16) -> Self {
        match status {
            400 => Self::BadRequest,
            401 => Self::Unauthorized,
            402 => Self::QuotaExceeded,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            413 => Self::TooLarge,
            429 => Self::RateLimited,
            status => Self::Server(status),
        }
    }
}

/// A cloud engine with the user's key.
pub struct CloudClient {
    agent: Agent,
    spec: CloudSpec,
    base: String,
    key: String,
}

impl CloudClient {
    /// `agent` must not treat HTTP statuses as errors (`runtimes::http::agent`).
    pub fn new(agent: Agent, spec: CloudSpec, key: String) -> Self {
        Self { agent, spec, base: spec.base_url.to_owned(), key: key.trim().to_owned() }
    }

    /// The service's name, for its errors.
    pub fn service_name(&self) -> &'static str {
        self.spec.service.name()
    }

    /// The text of a dictation and the language the service heard, if it says.
    pub fn transcribe(
        &self,
        wav: &[u8],
        options: &TranscribeOptions,
    ) -> Result<Transcript, CloudError> {
        let (text, language) = match self.spec.api {
            CloudApi::Deepgram => {
                let (channel, alternative) = self.deepgram(wav, options)?;
                (alternative.transcript, channel.detected_language)
            }
            CloudApi::Whisper => {
                let reply = self.whisper(wav, options, false)?;
                (reply.text, reply.language)
            }
            CloudApi::Transcribe => {
                let reply = self.gpt_transcribe(wav, options)?;
                (reply.text, reply.languages.into_iter().next().map(|language| language.code))
            }
        };
        Ok(Transcript { text: text.split_whitespace().collect::<Vec<_>>().join(" "), language })
    }

    /// A file window's words with their times, laid out like whisper-server's `verbose_json`
    /// (`files::parse_words`). A translation comes with the times of phrases only: their words
    /// share the phrase's time evenly. `gpt-transcribe` gives no times: nothing is sent.
    pub fn transcribe_words(
        &self,
        wav: &[u8],
        options: &TranscribeOptions,
    ) -> Result<String, CloudError> {
        let words = match self.spec.api {
            CloudApi::Transcribe => return Err(CloudError::NoWordTimes),
            CloudApi::Deepgram => {
                let (_, alternative) = self.deepgram(wav, options)?;
                alternative
                    .words
                    .into_iter()
                    .map(|word| (word.punctuated_word.unwrap_or(word.word), word.start, word.end))
                    .collect()
            }
            _ if self.translates(options) => {
                let reply = self.whisper(wav, options, true)?;
                spread(&reply.segments.unwrap_or_default())
            }
            _ => {
                let reply = self.whisper(wav, options, true)?;
                let words = reply.words.unwrap_or_default();
                if words.is_empty() && !reply.text.trim().is_empty() {
                    return Err(CloudError::BadResponse("no word times".into()));
                }
                let texts = punctuated(&reply.text, &words);
                texts
                    .into_iter()
                    .zip(words)
                    .map(|(text, word)| (text, word.start, word.end))
                    .collect()
            }
        };
        words_json(words)
    }

    /// Whisper translates only when the mode asks for it and the service can.
    fn translates(&self, options: &TranscribeOptions) -> bool {
        options.translate && self.spec.translate
    }

    /// Groq: the OpenAI audio API with a Whisper model.
    fn whisper(
        &self,
        wav: &[u8],
        options: &TranscribeOptions,
        words: bool,
    ) -> Result<WhisperReply, CloudError> {
        let translate = self.translates(options);
        let mut fields =
            vec![("model", self.spec.model.to_owned()), ("response_format", "verbose_json".into())];
        // A translation is always English, and gives no word times.
        if !translate {
            if let Some(language) = &options.language {
                fields.push(("language", language.clone()));
            }
            if words {
                fields.push(("timestamp_granularities[]", "word".into()));
            }
        }
        if let Some(prompt) = &options.prompt {
            fields.push(("prompt", super::whisper_server::whisper_prompt(prompt)));
        }
        let path = if translate { "/audio/translations" } else { "/audio/transcriptions" };
        self.upload(path, &fields, wav)
    }

    /// OpenAI's `gpt-transcribe`: the spoken language as `languages[]`, the dictionary terms as
    /// `keywords[]`. A keyword with `<` or `>` would make OpenAI reject the whole request, so
    /// those characters are left out.
    fn gpt_transcribe(
        &self,
        wav: &[u8],
        options: &TranscribeOptions,
    ) -> Result<TranscribeReply, CloudError> {
        let mut fields = vec![("model", self.spec.model.to_owned())];
        if let Some(language) = &options.language {
            fields.push(("languages[]", language.clone()));
        }
        for term in dictionary_terms(options) {
            let keyword: String = term.chars().filter(|c| !matches!(c, '<' | '>')).collect();
            if !keyword.trim().is_empty() {
                fields.push(("keywords[]", keyword.trim().to_owned()));
            }
        }
        self.upload("/audio/transcriptions", &fields, wav)
    }

    /// A `multipart/form-data` request to the OpenAI audio API: the fields and the WAV.
    fn upload<T: DeserializeOwned>(
        &self,
        path: &str,
        fields: &[(&str, String)],
        wav: &[u8],
    ) -> Result<T, CloudError> {
        if wav.len() >= MAX_UPLOAD_BYTES {
            return Err(CloudError::TooLarge);
        }
        let boundary = format!(
            "ahem-{}",
            random_hex().map_err(|error| CloudError::Network(error.to_string()))?
        );
        let body = multipart(&boundary, fields, wav)
            .map_err(|error| CloudError::Network(error.to_string()))?;
        let request = self
            .request(path, AUDIO_TIMEOUT)
            .header("Authorization", &format!("Bearer {}", self.key))
            .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"));
        parse(&self.read(request.send(&body[..]))?)
    }

    /// Deepgram's pre-recorded API; without a chosen language Nova-3 detects it (`multi`).
    fn deepgram(
        &self,
        wav: &[u8],
        options: &TranscribeOptions,
    ) -> Result<(DeepgramChannel, DeepgramAlternative), CloudError> {
        let language = options.language.as_deref().unwrap_or("multi");
        let mut request = self
            .request("/listen", AUDIO_TIMEOUT)
            .query("model", self.spec.model)
            .query("language", language)
            .query("punctuate", "true")
            .header("Authorization", &format!("Token {}", self.key))
            .header("Content-Type", "audio/wav");
        for term in dictionary_terms(options) {
            request = request.query("keyterm", term);
        }
        let reply: DeepgramReply = parse(&self.read(request.send(wav))?)?;
        let mut channel = reply
            .results
            .channels
            .into_iter()
            .next()
            .ok_or_else(|| CloudError::BadResponse("no channel".into()))?;
        let alternative = std::mem::take(&mut channel.alternatives)
            .into_iter()
            .next()
            .ok_or_else(|| CloudError::BadResponse("no transcript".into()))?;
        Ok((channel, alternative))
    }

    fn request(
        &self,
        path: &str,
        timeout: Duration,
    ) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        self.agent
            .post(format!("{}{path}", self.base))
            .config()
            .timeout_global(Some(timeout))
            .max_redirects(0)
            .build()
            .header("Accept", "application/json")
    }

    fn read(
        &self,
        result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<String, CloudError> {
        read(result).map_err(|error| redacted(error, &self.key))
    }
}

/// Checks a Deepgram key: `/auth/token` describes a valid key and rejects any other.
pub fn check_deepgram_key(agent: &Agent, key: &str) -> Result<(), CloudError> {
    check_deepgram_key_at(agent, DEEPGRAM_BASE_URL, key)
}

fn check_deepgram_key_at(agent: &Agent, base: &str, key: &str) -> Result<(), CloudError> {
    let key = key.trim();
    let request = agent
        .get(format!("{base}/auth/token"))
        .config()
        .timeout_global(Some(CHECK_TIMEOUT))
        .max_redirects(0)
        .build()
        .header("Accept", "application/json")
        .header("Authorization", &format!("Token {key}"));
    let body = read(request.call()).map_err(|error| redacted(error, key))?;
    match parse::<Value>(&body)? {
        Value::Object(_) => Ok(()),
        _ => Err(CloudError::BadResponse("not a key description".into())),
    }
}

fn read(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<String, CloudError> {
    let mut response = match result {
        Ok(response) => response,
        // Only if the caller's agent treats statuses as errors.
        Err(ureq::Error::StatusCode(status)) => return Err(CloudError::from_status(status)),
        Err(error) => return Err(CloudError::Network(error.to_string())),
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        // The body is never read: it may repeat the key or the text.
        return Err(CloudError::from_status(status));
    }
    response.body_mut().read_to_string().map_err(|error| CloudError::Network(error.to_string()))
}

/// A last guard: drops a network detail that repeats the key.
fn redacted(error: CloudError, key: &str) -> CloudError {
    match error {
        CloudError::Network(detail) if key.len() >= 8 && detail.contains(key) => {
            CloudError::Network("<redacted>".into())
        }
        error => error,
    }
}

/// The error names only the position: serde's messages can quote the data, i.e. the text.
fn parse<T: DeserializeOwned>(body: &str) -> Result<T, CloudError> {
    crate::runtimes::http::parse_json(body).map_err(CloudError::BadResponse)
}

/// The dictionary's terms: the prompt lists them separated by commas
/// (`whisper_server::prompt_from_dictionary`).
fn dictionary_terms(options: &TranscribeOptions) -> impl Iterator<Item = &str> {
    let prompt = options.prompt.as_deref().unwrap_or_default();
    prompt.split(',').map(str::trim).filter(|term| !term.is_empty()).take(MAX_KEYTERMS)
}

/// The reply of `gpt-transcribe`: the text and the languages it heard, if it is sure.
#[derive(Deserialize)]
struct TranscribeReply {
    text: String,
    #[serde(default)]
    languages: Vec<HeardLanguage>,
}

#[derive(Deserialize)]
struct HeardLanguage {
    code: String,
}

/// `verbose_json` of the OpenAI audio API: `words` when asked for word times, `segments` for
/// phrases.
#[derive(Deserialize)]
struct WhisperReply {
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    words: Option<Vec<WhisperWord>>,
    #[serde(default)]
    segments: Option<Vec<WhisperSegment>>,
}

#[derive(Deserialize)]
struct WhisperWord {
    word: String,
    start: f64,
    end: f64,
}

#[derive(Deserialize)]
struct WhisperSegment {
    text: String,
    start: f64,
    end: f64,
}

#[derive(Deserialize)]
struct DeepgramReply {
    results: DeepgramResults,
}

#[derive(Deserialize)]
struct DeepgramResults {
    channels: Vec<DeepgramChannel>,
}

#[derive(Deserialize)]
struct DeepgramChannel {
    #[serde(default)]
    alternatives: Vec<DeepgramAlternative>,
    #[serde(default)]
    detected_language: Option<String>,
}

#[derive(Deserialize)]
struct DeepgramAlternative {
    transcript: String,
    #[serde(default)]
    words: Vec<DeepgramWord>,
}

#[derive(Deserialize)]
struct DeepgramWord {
    word: String,
    #[serde(default)]
    punctuated_word: Option<String>,
    start: f64,
    end: f64,
}

/// Word times of the OpenAI audio API come without punctuation, so each word takes its part
/// of the full text instead: from its start to where the next word starts. Should a word not be
/// found in order, the words stay as returned.
fn punctuated(text: &str, words: &[WhisperWord]) -> Vec<String> {
    let as_returned = || words.iter().map(|word| word.word.trim().to_owned()).collect();
    let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
    let word_char = |c: char| c.is_alphanumeric() || c == '_';
    let chars: Vec<char> = text.chars().collect();
    let folded: Vec<char> = chars.iter().copied().map(lower).collect();
    let mut bounds = Vec::with_capacity(words.len());
    let mut position = 0;
    for word in words {
        let needle: Vec<char> = word.word.trim().chars().map(lower).collect();
        let (Some(&first), Some(&last)) = (needle.first(), needle.last()) else {
            return as_returned();
        };
        let found = (position..).take_while(|i| i + needle.len() <= folded.len()).find(|&i| {
            let end = i + needle.len();
            folded[i..end] == needle[..]
                && !(word_char(first) && i > 0 && word_char(folded[i - 1]))
                && !(word_char(last) && end < folded.len() && word_char(folded[end]))
        });
        let Some(start) = found else { return as_returned() };
        let gap = &chars[position..start];
        bounds
            .push(gap.iter().rposition(|c| c.is_whitespace()).map_or(start, |i| position + i + 1));
        position = start + needle.len();
    }
    if let Some(first) = bounds.first_mut() {
        *first = 0;
    }
    let ends = bounds.iter().skip(1).copied().chain([chars.len()]);
    bounds
        .iter()
        .zip(ends)
        .map(|(&from, to)| chars[from..to].iter().collect::<String>().trim().to_owned())
        .collect()
}

/// The words of phrases with times, each taking an equal share of its phrase.
fn spread(segments: &[WhisperSegment]) -> Vec<(String, f64, f64)> {
    let mut words = Vec::new();
    for segment in segments {
        let parts: Vec<&str> = segment.text.split_whitespace().collect();
        let step = (segment.end - segment.start).max(0.0) / parts.len().max(1) as f64;
        for (i, part) in parts.into_iter().enumerate() {
            let start = segment.start + step * i as f64;
            words.push((part.to_owned(), start, start + step));
        }
    }
    words
}

/// One segment per word, as whisper-server writes them with `max_len=1`.
fn words_json(words: Vec<(String, f64, f64)>) -> Result<String, CloudError> {
    let mut segments = Vec::with_capacity(words.len());
    for (text, start, end) in words {
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end < start {
            return Err(CloudError::BadResponse("bad word times".into()));
        }
        segments.push(json!({ "text": format!(" {}", text.trim()), "start": start, "end": end }));
    }
    Ok(json!({ "segments": segments }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtimes::http;
    use crate::runtimes::test_server::{Response, TestServer};
    use crate::storage::catalog;

    const KEY: &str = "sk-test-0123456789";

    fn client(id: &str, server: &TestServer) -> CloudClient {
        let mut client = CloudClient::new(http::agent(), find(id).unwrap(), KEY.into());
        client.base = server.base.clone();
        client
    }

    fn reply(body: &str) -> TestServer {
        let body = body.as_bytes().to_vec();
        TestServer::start(move |_| Response::new(200, body.clone()))
    }

    fn segments(json: &str) -> Vec<(String, f64, f64)> {
        let value: Value = serde_json::from_str(json).unwrap();
        value["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|segment| {
                let text = segment["text"].as_str().unwrap().to_owned();
                (text, segment["start"].as_f64().unwrap(), segment["end"].as_f64().unwrap())
            })
            .collect()
    }

    fn options() -> TranscribeOptions {
        TranscribeOptions {
            language: Some("ru".into()),
            prompt: Some("Kubernetes, Tauri".into()),
            ..TranscribeOptions::default()
        }
    }

    #[test]
    fn engines_have_their_own_ids() {
        let mut ids: Vec<&str> = ENGINES.iter().map(|spec| spec.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ENGINES.len());
        for spec in ENGINES {
            assert!(catalog::find(spec.id).is_none(), "{} is a catalog id", spec.id);
            assert!(!spec.id.contains('.'), "{} has a dot", spec.id);
        }
        let deepgram = find("deepgram-nova-3").unwrap().languages();
        assert!(deepgram.contains(&"ru") && deepgram.contains(&"en"));
        assert!(!deepgram.contains(&"la"), "Nova-3 does not know Latin");
        assert_eq!(
            find("groq-whisper-large-v3").unwrap().languages().len(),
            languages::all().len()
        );
    }

    #[test]
    fn whisper_api_gets_the_wav_language_and_dictionary() {
        let server = reply(r#"{"text":"  Привет,\n мир. ","language":"russian","segments":[]}"#);
        let transcript =
            client("groq-whisper-large-v3", &server).transcribe(b"RIFFfake", &options()).unwrap();
        assert_eq!(
            transcript,
            Transcript { text: "Привет, мир.".into(), language: Some("russian".into()) }
        );
        let request = &server.requests()[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/audio/transcriptions");
        assert_eq!(request.header("authorization"), Some(format!("Bearer {KEY}").as_str()));
        let body = String::from_utf8_lossy(&request.body);
        for field in [
            "name=\"model\"\r\n\r\nwhisper-large-v3\r\n",
            "name=\"response_format\"\r\n\r\nverbose_json\r\n",
            "name=\"language\"\r\n\r\nru\r\n",
            "name=\"prompt\"\r\n\r\nKubernetes, Tauri\r\n",
            "RIFFfake",
        ] {
            assert!(body.contains(field), "{field} in {body}");
        }
        assert!(!body.contains("timestamp_granularities"));
    }

    #[test]
    fn english_mode_asks_for_a_translation() {
        let server = reply(r#"{"text":"Hello.","language":"english"}"#);
        let options = TranscribeOptions { translate: true, ..options() };
        client("groq-whisper-large-v3", &server).transcribe(b"RIFF", &options).unwrap();
        let request = &server.requests()[0];
        assert_eq!(request.path, "/audio/translations");
        let body = String::from_utf8_lossy(&request.body);
        assert!(body.contains("whisper-large-v3"));
        assert!(!body.contains("name=\"language\""), "a translation is always English");
    }

    #[test]
    fn gpt_transcribe_gets_languages_and_keywords() {
        let server = reply(r#"{"text":" Привет,  Tauri. ","languages":[{"code":"ru"}]}"#);
        let openai = client("openai-gpt-transcribe", &server);
        let options = TranscribeOptions {
            translate: true,
            prompt: Some("Kubernetes, <Tauri>, <>".into()),
            ..options()
        };
        let transcript = openai.transcribe(b"RIFFfake", &options).unwrap();
        assert_eq!(
            transcript,
            Transcript { text: "Привет, Tauri.".into(), language: Some("ru".into()) }
        );
        let request = &server.requests()[0];
        assert_eq!(request.path, "/audio/transcriptions", "gpt-transcribe does not translate");
        assert_eq!(request.header("authorization"), Some(format!("Bearer {KEY}").as_str()));
        let body = String::from_utf8_lossy(&request.body);
        for field in [
            "name=\"model\"\r\n\r\ngpt-transcribe\r\n",
            "name=\"languages[]\"\r\n\r\nru\r\n",
            "name=\"keywords[]\"\r\n\r\nKubernetes\r\n",
            "name=\"keywords[]\"\r\n\r\nTauri\r\n",
            "RIFFfake",
        ] {
            assert!(body.contains(field), "{field} in {body}");
        }
        assert_eq!(body.matches("name=\"keywords[]\"").count(), 2, "an empty keyword is left out");
        for absent in ["name=\"language\"", "name=\"prompt\"", "response_format", "timestamp"] {
            assert!(!body.contains(absent), "{absent} in {body}");
        }

        let unsure = reply(r#"{"text":"Hi","languages":[]}"#);
        let options = TranscribeOptions::default();
        let transcript = client("openai-gpt-transcribe", &unsure).transcribe(b"RIFF", &options);
        assert_eq!(transcript.unwrap().language, None);
        let body = String::from_utf8_lossy(&unsure.requests()[0].body).into_owned();
        assert!(!body.contains("languages[]") && !body.contains("keywords[]"));
    }

    #[test]
    fn gpt_transcribe_cannot_do_files() {
        let server = reply(r#"{"text":"Hi"}"#);
        let spec = find("openai-gpt-transcribe").unwrap();
        assert!(!spec.word_times());
        assert!(find("groq-whisper-large-v3").unwrap().word_times());
        assert!(find("deepgram-nova-3").unwrap().word_times());
        let words = client(spec.id, &server).transcribe_words(b"RIFF", &options());
        assert_eq!(words, Err(CloudError::NoWordTimes));
        assert!(server.requests().is_empty(), "nothing is sent for nothing");
    }

    #[test]
    fn file_words_get_their_punctuation_back() {
        let server = reply(
            r#"{"text":"Hello, world. Bye-bye!","words":[
                {"word":"Hello","start":0.0,"end":0.4},
                {"word":"world","start":0.5,"end":0.9},
                {"word":"Bye-bye","start":1.2,"end":1.6}]}"#,
        );
        let json =
            client("groq-whisper-large-v3", &server).transcribe_words(b"RIFF", &options()).unwrap();
        assert_eq!(
            segments(&json),
            vec![
                (" Hello,".to_string(), 0.0, 0.4),
                (" world.".to_string(), 0.5, 0.9),
                (" Bye-bye!".to_string(), 1.2, 1.6)
            ]
        );
        let body = String::from_utf8_lossy(&server.requests()[0].body).into_owned();
        assert!(body.contains("name=\"timestamp_granularities[]\"\r\n\r\nword\r\n"));
    }

    #[test]
    fn words_stay_as_returned_when_the_text_differs() {
        let word = |text: &str| WhisperWord { word: text.into(), start: 0.0, end: 0.0 };
        assert_eq!(punctuated("Hello there.", &[word("hello"), word("world")]), ["hello", "world"]);
        assert_eq!(punctuated("Да, нет.", &[word("да"), word("нет")]), ["Да,", "нет."]);
        assert_eq!(punctuated("cat category", &[word("cat"), word("cat")]), ["cat", "cat"]);
    }

    #[test]
    fn translated_file_words_share_their_phrase_time() {
        let server = reply(
            r#"{"text":"One two. Three","segments":[{"text":" One two.","start":1.0,"end":2.0},{"text":" Three","start":2.0,"end":2.5}]}"#,
        );
        let options = TranscribeOptions { translate: true, ..options() };
        let json =
            client("groq-whisper-large-v3", &server).transcribe_words(b"RIFF", &options).unwrap();
        assert_eq!(
            segments(&json),
            vec![
                (" One".to_string(), 1.0, 1.5),
                (" two.".to_string(), 1.5, 2.0),
                (" Three".to_string(), 2.0, 2.5)
            ]
        );
        assert_eq!(server.requests()[0].path, "/audio/translations");
    }

    #[test]
    fn deepgram_gets_the_language_and_key_terms() {
        let server = reply(
            r#"{"results":{"channels":[{"alternatives":[{"transcript":"Привет, Tauri.","words":[
                {"word":"привет","punctuated_word":"Привет,","start":0.1,"end":0.5},
                {"word":"tauri","punctuated_word":"Tauri.","start":0.6,"end":1.0}]}]}]}}"#,
        );
        let deepgram = client("deepgram-nova-3", &server);
        let transcript = deepgram.transcribe(b"RIFFfake", &options()).unwrap();
        assert_eq!(transcript, Transcript { text: "Привет, Tauri.".into(), language: None });
        let request = &server.requests()[0];
        assert_eq!(
            request.path,
            "/listen?model=nova-3&language=ru&punctuate=true&keyterm=Kubernetes&keyterm=Tauri"
        );
        assert_eq!(request.header("authorization"), Some(format!("Token {KEY}").as_str()));
        assert_eq!(request.header("content-type"), Some("audio/wav"));
        assert_eq!(request.body, b"RIFFfake");

        let json = deepgram.transcribe_words(b"RIFF", &options()).unwrap();
        assert_eq!(
            segments(&json),
            vec![(" Привет,".to_string(), 0.1, 0.5), (" Tauri.".to_string(), 0.6, 1.0)]
        );
    }

    #[test]
    fn deepgram_detects_the_language_unless_one_is_chosen() {
        let server = reply(r#"{"results":{"channels":[{"alternatives":[{"transcript":""}]}]}}"#);
        let options = TranscribeOptions::default();
        assert_eq!(
            client("deepgram-nova-3", &server).transcribe(b"RIFF", &options).unwrap().text,
            ""
        );
        assert_eq!(server.requests()[0].path, "/listen?model=nova-3&language=multi&punctuate=true");
    }

    #[test]
    fn errors_name_only_the_status() {
        let cases = [
            (400, CloudError::BadRequest),
            (401, CloudError::Unauthorized),
            (402, CloudError::QuotaExceeded),
            (429, CloudError::RateLimited),
            (503, CloudError::Server(503)),
        ];
        for (status, expected) in cases {
            let body = format!(r#"{{"error":{{"message":"Incorrect API key {KEY}"}}}}"#);
            let server =
                TestServer::start(move |_| Response::new(status, body.clone().into_bytes()));
            let error = client("openai-gpt-transcribe", &server)
                .transcribe(b"RIFF", &options())
                .unwrap_err();
            assert_eq!(error, expected);
            assert!(!error.to_string().contains(KEY));
        }
        let html = reply("<html>Привет</html>");
        let error =
            client("groq-whisper-large-v3", &html).transcribe(b"RIFF", &options()).unwrap_err();
        assert!(matches!(&error, CloudError::BadResponse(detail) if !detail.contains("Привет")));
        let no_times = reply(r#"{"text":"Hello"}"#);
        let error =
            client("groq-whisper-large-v3", &no_times).transcribe_words(b"RIFF", &options());
        assert_eq!(error, Err(CloudError::BadResponse("no word times".into())));
    }

    #[test]
    fn a_recording_over_the_upload_limit_is_not_sent() {
        let server = reply("{}");
        let wav = vec![0u8; MAX_UPLOAD_BYTES];
        let error = client("groq-whisper-large-v3", &server).transcribe(&wav, &options());
        assert_eq!(error, Err(CloudError::TooLarge));
        assert!(server.requests().is_empty());
    }

    #[test]
    fn deepgram_key_is_checked_by_its_description() {
        let valid = reply(r#"{"version":"1.0","key":{"comment":"ahem"}}"#);
        assert_eq!(check_deepgram_key_at(&http::agent(), &valid.base, KEY), Ok(()));
        let request = &valid.requests()[0];
        assert_eq!((request.method.as_str(), request.path.as_str()), ("GET", "/auth/token"));
        assert_eq!(request.header("authorization"), Some(format!("Token {KEY}").as_str()));
        let rejected = TestServer::start(|_| Response::new(401, b"{}".to_vec()));
        assert_eq!(
            check_deepgram_key_at(&http::agent(), &rejected.base, KEY),
            Err(CloudError::Unauthorized)
        );
        let page = reply("[]");
        assert!(check_deepgram_key_at(&http::agent(), &page.base, KEY).is_err());
    }
}

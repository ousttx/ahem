//! Language models for post-processing dictated text: the Anthropic Messages API,
//! the OpenAI Responses API and any OpenAI-compatible Chat Completions server at a `base_url`
//! (DeepSeek, Groq, Mistral, OpenRouter, Ollama, LM Studio…).
//!
//! The text goes to the model as delimited
//! data under a protective system prompt, unfinished answers are errors, redirects are not
//! followed. Errors carry only HTTP statuses, provider error codes and fixed descriptions, never
//! the API key, the dictated text, the answer or a response body.

mod anthropic;
mod openai;
mod prompt;
#[cfg(test)]
mod tests;

use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use ureq::Agent;

use crate::domain::context::Context;
use crate::domain::modes::ProviderKind;
use prompt::Prompt;

pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Parts of model ids that mark speech, image, embedding, moderation and legacy completion
/// models: a mode cannot rewrite text with them, so the model list leaves them out.
const NOT_TEXT_MODELS: [&str; 14] = [
    "embed",
    "whisper",
    "tts",
    "transcribe",
    "audio",
    "realtime",
    "dall-e",
    "image",
    "sora",
    "moderation",
    "guard",
    "davinci",
    "babbage",
    "speech",
];

/// A long rewrite may take a while, a model list should not.
const PROCESS_TIMEOUT: Duration = Duration::from_secs(120);
const LIST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Responses API.
    OpenAi,
    /// OpenAI Chat Completions API at a required `base_url`.
    OpenAiCompatible,
    /// OpenAI Responses API on the user's ChatGPT plan, with their OAuth token as the key.
    ChatGptPlan,
}

impl Provider {
    /// The language-model API of a stored connection; `None` for a speech-only service.
    pub fn of(kind: ProviderKind) -> Option<Self> {
        match kind {
            ProviderKind::Anthropic => Some(Self::Anthropic),
            ProviderKind::OpenAi => Some(Self::OpenAi),
            ProviderKind::OpenAiCompatible => Some(Self::OpenAiCompatible),
            ProviderKind::ChatGptAccount => Some(Self::ChatGptPlan),
            ProviderKind::Deepgram => None,
        }
    }
}

#[derive(Clone)]
pub struct Connection {
    pub provider: Provider,
    /// `None` or empty: the provider's official URL. Required for `OpenAiCompatible`.
    pub base_url: Option<String>,
    /// May be empty for local servers: then no key header is sent.
    pub api_key: String,
    pub model: String,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = if self.api_key.is_empty() { "<none>" } else { "<redacted>" };
        f.debug_struct("Connection")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("api_key", &key)
            .field("model", &self.model)
            .finish()
    }
}

pub struct Request<'a> {
    /// The mode's prompt, written by the user.
    pub instruction: &'a str,
    /// The dictated or selected text.
    pub text: &'a str,
    /// Language of the result, a name or a code such as `"en"`; `None` keeps the input language.
    pub output_language: Option<&'a str>,
    /// What was around the speaker; `NO_CONTEXT` sends the request as without it.
    pub context: &'a Context,
}

impl fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("instruction_chars", &self.instruction.chars().count())
            .field("text_chars", &self.text.chars().count())
            .field("output_language", &self.output_language)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmError {
    /// `OpenAiCompatible` without a `base_url`.
    MissingBaseUrl,
    /// HTTP 401: the key is missing, wrong, revoked or expired.
    Unauthorized,
    /// HTTP 429 that clears with time.
    RateLimited,
    /// No credits or balance left, or a spend limit reached: retrying does not help.
    QuotaExceeded,
    /// No HTTP answer: DNS, connection, TLS, timeout.
    Network(String),
    /// Any other HTTP error status; `detail` is the provider's error code or type, if any.
    Server { status: u16, detail: String },
    /// The answer was cut off: output limit, context window, interrupted generation.
    Incomplete,
    /// The model declined, or its answer was filtered.
    Refused,
    /// Blank input text, no text in the answer, or an empty model list.
    Empty,
    /// The reply is not in the expected format.
    BadResponse(String),
    /// The share of the ChatGPT plan the user gave ahem, or the plan itself, is used up.
    PlanLimit,
    /// The ChatGPT account or workspace cannot use its plan in other apps (Plus or Pro needed).
    PlanNotEligible,
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBaseUrl => write!(f, "an OpenAI-compatible provider needs a base URL"),
            Self::Unauthorized => write!(f, "the API key was not accepted"),
            Self::RateLimited => write!(f, "too many requests, try again later"),
            Self::QuotaExceeded => write!(f, "no credits left or a spend limit was reached"),
            Self::Network(detail) => write!(f, "cannot reach the service: {detail}"),
            Self::Server { status, detail } if detail.is_empty() => write!(f, "HTTP {status}"),
            Self::Server { status, detail } => write!(f, "HTTP {status}: {detail}"),
            Self::Incomplete => write!(f, "the model's answer is incomplete"),
            Self::Refused => write!(f, "the model declined to process the text"),
            Self::Empty => write!(f, "empty result"),
            Self::BadResponse(detail) => write!(f, "unexpected reply: {detail}"),
            Self::PlanLimit => write!(f, "the ChatGPT plan's limit for ahem is reached"),
            Self::PlanNotEligible => write!(f, "this ChatGPT account cannot use its plan in ahem"),
        }
    }
}

impl std::error::Error for LlmError {}

impl LlmError {
    /// A last guard on top of reading only error codes: drops a detail that repeats a secret.
    /// Strings shorter than 8 characters are not checked, so a one-word dictation does not
    /// wipe every detail.
    fn redacted(self, secrets: &[&str]) -> Self {
        let leaks = |detail: &str| {
            secrets.iter().any(|secret| secret.chars().count() >= 8 && detail.contains(secret))
        };
        match self {
            Self::Network(detail) if leaks(&detail) => Self::Network("<redacted>".into()),
            Self::Server { status, detail } if leaks(&detail) => {
                Self::Server { status, detail: String::new() }
            }
            Self::BadResponse(detail) if leaks(&detail) => Self::BadResponse("<redacted>".into()),
            error => error,
        }
    }
}

/// Processes `request.text` by the mode's instruction and returns the result, trimmed.
/// Blocks for up to two minutes; the agent must not treat HTTP statuses as errors
/// (`runtimes::http::agent`).
pub fn process(
    agent: &Agent,
    connection: &Connection,
    request: &Request,
) -> Result<String, LlmError> {
    send(agent, connection, request)
        .map_err(|error| error.redacted(&[connection.api_key.trim(), request.text.trim()]))
}

fn send(agent: &Agent, connection: &Connection, request: &Request) -> Result<String, LlmError> {
    if request.text.trim().is_empty() {
        return Err(LlmError::Empty);
    }
    let api =
        Api::new(agent, connection.provider, connection.base_url.as_deref(), &connection.api_key)?;
    let prompt = Prompt::new(request);
    let model = connection.model.trim();
    let text = match connection.provider {
        Provider::Anthropic => anthropic::process(&api, model, &prompt)?,
        Provider::OpenAi => openai::respond(&api, model, &prompt)?,
        Provider::OpenAiCompatible => openai::complete(&api, model, &prompt)?,
        Provider::ChatGptPlan => openai::respond_plan(&api, model, &prompt)?,
    };
    let text = text.trim();
    if text.is_empty() { Err(LlmError::Empty) } else { Ok(text.to_owned()) }
}

/// A base URL the key may be sent to, without a trailing slash: HTTPS, or plain HTTP only to
/// this computer (Ollama, LM Studio). No credentials, query or fragment in the URL.
pub fn checked_base_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split('/').next().unwrap_or_default();
    if url.chars().any(|c| c.is_whitespace() || c == '?' || c == '#') || authority.contains('@') {
        return None;
    }
    let host = host_of(url)?;
    let allowed = match scheme.to_ascii_lowercase().as_str() {
        "https" => true,
        "http" => is_loopback(&host),
        _ => false,
    };
    (allowed && !host.is_empty()).then(|| url.to_owned())
}

/// The host of a URL, lowercase, without brackets and port.
fn host_of(url: &str) -> Option<String> {
    let (_, rest) = url.trim().split_once("://")?;
    let authority = rest.split('/').next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    Some(host.to_ascii_lowercase())
}

/// A loopback address, not a name that starts like one (`127.example.com`).
fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host == "::1"
        || host.parse::<std::net::Ipv4Addr>().is_ok_and(|address| address.is_loopback())
}

/// Text model ids available to the key, sorted case-insensitively. An empty list is `Empty`.
pub fn list_models(
    agent: &Agent,
    provider: Provider,
    base_url: Option<&str>,
    api_key: &str,
) -> Result<Vec<String>, LlmError> {
    let api = Api::new(agent, provider, base_url, api_key)?;
    let ids = match provider {
        Provider::Anthropic => anthropic::models(&api),
        Provider::OpenAi | Provider::OpenAiCompatible => openai::models(&api),
        Provider::ChatGptPlan => openai::plan_models(&api),
    };
    let mut ids = ids.map_err(|error| error.redacted(&[api_key.trim()]))?;
    ids.retain(|id| is_text_model(id));
    ids.sort_by_cached_key(|id| id.to_lowercase());
    ids.dedup();
    if ids.is_empty() { Err(LlmError::Empty) } else { Ok(ids) }
}

/// Whether a model can rewrite text, judged by its id (`NOT_TEXT_MODELS`).
pub fn is_text_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    !NOT_TEXT_MODELS.iter().any(|part| id.contains(part))
}

#[derive(Deserialize)]
struct ModelInfo {
    id: String,
}

/// A provider endpoint: the base URL and the headers every request carries, the key among them.
struct Api<'a> {
    agent: &'a Agent,
    base: String,
    headers: Vec<(&'static str, String)>,
    /// A server on this computer: reached directly, never through a proxy of the environment
    /// (`HTTP_PROXY` would get the key and the text over plain HTTP).
    local: bool,
}

impl<'a> Api<'a> {
    fn new(
        agent: &'a Agent,
        provider: Provider,
        base_url: Option<&str>,
        api_key: &str,
    ) -> Result<Self, LlmError> {
        let custom = base_url.map(str::trim).filter(|url| !url.is_empty());
        let base = match (provider, custom) {
            (_, Some(url)) => url,
            (Provider::Anthropic, None) => ANTHROPIC_BASE_URL,
            (Provider::OpenAi | Provider::ChatGptPlan, None) => OPENAI_BASE_URL,
            (Provider::OpenAiCompatible, None) => return Err(LlmError::MissingBaseUrl),
        };
        let key = api_key.trim();
        let mut headers = Vec::new();
        match provider {
            Provider::Anthropic => {
                if !key.is_empty() {
                    headers.push(("x-api-key", key.to_owned()));
                }
                headers.push(("anthropic-version", anthropic::API_VERSION.to_owned()));
            }
            Provider::OpenAi | Provider::OpenAiCompatible | Provider::ChatGptPlan => {
                if !key.is_empty() {
                    headers.push(("Authorization", format!("Bearer {key}")));
                }
            }
        }
        let local = host_of(base).is_some_and(|host| is_loopback(&host));
        Ok(Self { agent, base: base.trim_end_matches('/').to_owned(), headers, local })
    }

    /// The agent's proxy (from the environment), none for a server on this computer.
    fn proxy(&self) -> Option<ureq::Proxy> {
        if self.local { None } else { self.agent.config().proxy().cloned() }
    }

    fn post<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T, LlmError> {
        parse(&self.post_text(path, body, "application/json")?)
    }

    /// The whole answer as text, such as a stream of server-sent events.
    fn post_text(&self, path: &str, body: &Value, accept: &str) -> Result<String, LlmError> {
        let body = serde_json::to_vec(body).expect("a JSON value always serializes");
        let request = self
            .agent
            .post(format!("{}{path}", self.base))
            .config()
            .proxy(self.proxy())
            .timeout_global(Some(PROCESS_TIMEOUT))
            .max_redirects(0)
            .build()
            .header("Content-Type", "application/json")
            .header("Accept", accept);
        let request = self
            .headers
            .iter()
            .fold(request, |request, (name, value)| request.header(*name, value));
        read_text(request.send(&body[..]))
    }

    fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T, LlmError> {
        let request = self
            .agent
            .get(format!("{}{path}", self.base))
            .config()
            .proxy(self.proxy())
            .timeout_global(Some(LIST_TIMEOUT))
            .max_redirects(0)
            .build()
            .header("Accept", "application/json");
        let request =
            query.iter().fold(request, |request, (name, value)| request.query(name, value));
        let request = self
            .headers
            .iter()
            .fold(request, |request, (name, value)| request.header(*name, value));
        read(request.call())
    }
}

fn read<T: DeserializeOwned>(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<T, LlmError> {
    parse(&read_text(result)?)
}

fn read_text(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<String, LlmError> {
    let mut response = match result {
        Ok(response) => response,
        // Only if the caller's agent treats statuses as errors; the body is lost then.
        Err(ureq::Error::StatusCode(status)) => return Err(http_error(status, "")),
        Err(error) => return Err(LlmError::Network(error.to_string())),
    };
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| LlmError::Network(error.to_string()))?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &body));
    }
    Ok(body)
}

/// The error names only the position: serde's messages can quote the data, i.e. the answer.
fn parse<T: DeserializeOwned>(body: &str) -> Result<T, LlmError> {
    crate::runtimes::http::parse_json(body).map_err(LlmError::BadResponse)
}

/// OpenAI's 429 codes for exhausted credits and spend limits.
const QUOTA_CODES: [&str; 5] = [
    "insufficient_quota",
    "credit_balance_exhausted",
    "organization_spend_limit_exceeded",
    "project_spend_limit_exceeded",
    "organization_usage_limit_exceeded",
];

/// Reads only machine-readable fields of the error body. Messages are never kept: they can
/// quote the key (OpenAI shows part of a wrong one) or the request.
fn http_error(status: u16, body: &str) -> LlmError {
    let error = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .unwrap_or(Value::Null);
    let field = |name: &str| error.get(name).and_then(Value::as_str);
    // Anthropic's monthly spend cap: a 429 with this code instead of a rate limit.
    let details_code = error.pointer("/details/error_code").and_then(Value::as_str);
    let quota = field("type") == Some("insufficient_quota")
        || field("code").is_some_and(|code| QUOTA_CODES.contains(&code))
        || details_code == Some("enforced_spend_limit_reached");
    // Anthropic answers a spend limit set by the user with 400 and this message.
    let own_limit = field("message")
        .is_some_and(|message| message.starts_with("You have reached your specified"));
    if let Some(error) = field("code").and_then(plan_error) {
        return error;
    }
    match status {
        401 => LlmError::Unauthorized,
        402 => LlmError::QuotaExceeded,
        429 if quota => LlmError::QuotaExceeded,
        429 => LlmError::RateLimited,
        400 if own_limit => LlmError::QuotaExceeded,
        _ => LlmError::Server {
            status,
            detail: [field("code"), details_code, field("type")]
                .into_iter()
                .flatten()
                .find_map(safe_code)
                .unwrap_or_default(),
        },
    }
}

/// The ChatGPT plan's own error codes that the user can act on.
fn plan_error(code: &str) -> Option<LlmError> {
    match code {
        "subscription_sharing_usage_limit_exceeded" => Some(LlmError::PlanLimit),
        "subscription_sharing_user_not_eligible" => Some(LlmError::PlanNotEligible),
        "subscription_sharing_invalid_user" => Some(LlmError::Unauthorized),
        _ => None,
    }
}

/// A provider's error code or status word, if it looks like one: short ASCII identifiers only.
fn safe_code(value: &str) -> Option<String> {
    let safe = (1..=64).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_-.:".contains(&byte));
    safe.then(|| value.to_owned())
}

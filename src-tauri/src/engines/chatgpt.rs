//! ChatGPT through the user's account: "Sign in with ChatGPT" with plan usage, which OpenAI
//! offers open-source and locally run apps (developers.openai.com/siwc). Language-model
//! requests then count against the user's Plus or Pro plan instead of an API key.
//!
//! OAuth 2 with PKCE and a loopback redirect on `127.0.0.1`; the client is registered on the
//! first sign-in (`dynamic_agent_client`) and its issued id is kept. The tokens and the ID token
//! live in the secret store as one record per connection and never reach a log.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use ureq::Agent;

use crate::storage::secrets::SecretStore;
use crate::util::{self, base64url, from_base64url, now_ms};

pub const ISSUER: &str = "https://auth.openai.com";

/// Whether `url` is on the issuer's host: its origin followed by a path, not a longer host
/// name that starts the same (`auth.openai.com.example`).
fn at_issuer(url: &str) -> bool {
    url.strip_prefix(ISSUER).is_some_and(|rest| rest.starts_with('/'))
}
const DISCOVERY_URL: &str = "https://auth.openai.com/.well-known/openid-configuration";
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
/// What the tokens are for: the public OpenAI API.
pub const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// Granted only when the user lets the app use their plan.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// Registers a new client during the first sign-in.
const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
const APP_NAME: &str = "ahem";
const CALLBACK_PATH: &str = "/auth/callback";
/// Where the user sets how much of the plan ahem may use.
pub const MANAGE_USAGE_URL: &str = "https://chatgpt.com/settings/usage";
/// The secret store's entry for this installation's host id.
const HOST_ID_SECRET: &str = "chatgpt-host-id";
/// The secret store's entry for the client OpenAI registered: it outlives a sign-out, so signing
/// in again does not register ahem a second time in the user's ChatGPT settings.
const REGISTRATION_SECRET: &str = "chatgpt-registration";
/// The client of a registration OpenAI no longer knows: a connection's tokens from it are not
/// reused for the next sign-in.
const FORGOTTEN_SECRET: &str = "chatgpt-forgotten-client";
/// How long the browser may take.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
/// A token this close to its end is renewed before a request.
const REFRESH_MARGIN: Duration = Duration::from_secs(300);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// One renewal at a time: refresh tokens rotate, and a reused one is refused.
static REFRESH: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The user cancelled in ahem.
    Cancelled,
    /// The user declined in the browser.
    Denied,
    TimedOut,
    /// The browser's answer does not belong to this sign-in.
    Mismatch,
    /// Signed in, but plan usage was not allowed.
    PlanNotGranted,
    /// Another account than the saved registration's signed in; it is forgotten, so the next
    /// sign-in registers this account.
    OtherAccount,
    /// OpenAI no longer knows the saved registration (the user removed ahem in ChatGPT's
    /// settings): it is forgotten, so the next sign-in registers anew.
    ClientForgotten,
    /// The ID token did not pass a check; names the check.
    InvalidIdToken(&'static str),
    /// The saved sign-in no longer works: sign in again.
    SignInAgain,
    Network(String),
    /// An OAuth error code or HTTP status.
    Server(String),
    Storage(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "sign-in cancelled"),
            Self::Denied => write!(f, "sign-in declined in the browser"),
            Self::TimedOut => write!(f, "the browser did not answer in time"),
            Self::Mismatch => write!(f, "the browser's answer does not match this sign-in"),
            Self::PlanNotGranted => write!(f, "plan usage was not allowed"),
            Self::OtherAccount => write!(f, "another ChatGPT account signed in"),
            Self::ClientForgotten => write!(f, "ChatGPT no longer knows this registration"),
            Self::InvalidIdToken(check) => write!(f, "the ID token failed the {check} check"),
            Self::SignInAgain => write!(f, "sign in to ChatGPT again"),
            Self::Network(detail) => write!(f, "cannot reach ChatGPT: {detail}"),
            Self::Server(detail) => write!(f, "ChatGPT sign-in error: {detail}"),
            Self::Storage(detail) => write!(f, "cannot store the sign-in: {detail}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// The saved sign-in of one connection, kept whole in the secret store.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// Issued at the first sign-in (`oaiapp_…`); reused for every later one.
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Needed as `id_token_hint` when signing in again.
    pub id_token: String,
    pub expires_at_ms: i64,
    pub scope: String,
    /// The account (`sub` of the ID token).
    pub subject: String,
    pub email: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("client_id", &self.client_id)
            .field("tokens", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("scope", &self.scope)
            .finish()
    }
}

/// The client OpenAI registered at the first sign-in and the account it belongs to.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub client_id: String,
    pub subject: String,
    pub email: Option<String>,
    /// The last ID token, for `id_token_hint`; none after a sign-out, so the next sign-in lets
    /// the user choose the account (OpenAI: omit the hint after signing out).
    pub id_token: Option<String>,
    /// Plan usage was declined at the last sign-in: the next one asks for it again.
    #[serde(default)]
    pub plan_declined: bool,
}

impl Registration {
    pub fn of(credentials: &Credentials) -> Self {
        Self {
            client_id: credentials.client_id.clone(),
            subject: credentials.subject.clone(),
            email: credentials.email.clone(),
            id_token: Some(credentials.id_token.clone()),
            plan_declined: !credentials.plan_granted(),
        }
    }

    /// Without the ID token, after a sign-out or a sign-in that is not kept: the next sign-in
    /// lets the user choose the account.
    pub fn signed_out(credentials: &Credentials) -> Self {
        Self { id_token: None, ..Self::of(credentials) }
    }

    fn load(secrets: &SecretStore) -> Option<Self> {
        let text = secrets.get(REGISTRATION_SECRET).ok().flatten()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self, secrets: &SecretStore) -> Result<(), AuthError> {
        let text = serde_json::to_string(self).expect("a registration always serializes");
        secrets
            .set(REGISTRATION_SECRET, &text)
            .map_err(|error| AuthError::Storage(error.to_string()))
    }
}

impl Credentials {
    pub fn plan_granted(&self) -> bool {
        self.scope.split_whitespace().any(|scope| scope == PLAN_SCOPE)
    }

    pub fn load(secrets: &SecretStore, id: &str) -> Result<Option<Self>, AuthError> {
        let Some(text) = secrets.get(id).map_err(|error| AuthError::Storage(error.to_string()))?
        else {
            return Ok(None);
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|_| AuthError::Storage("damaged ChatGPT sign-in".into()))
    }

    pub fn save(&self, secrets: &SecretStore, id: &str) -> Result<(), AuthError> {
        let text = serde_json::to_string(self).expect("credentials always serialize");
        secrets.set(id, &text).map_err(|error| AuthError::Storage(error.to_string()))
    }
}

// --- Encodings -----------------------------------------------------------------------------

/// Percent-encoding of a query value: everything but the unreserved characters.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(name, value)| format!("{name}={}", encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn random_bytes<const N: usize>() -> Result<[u8; N], AuthError> {
    util::random_bytes().map_err(|error| AuthError::Storage(error.to_string()))
}

// --- Authorization request -----------------------------------------------------------------

#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// The verifier is a secret of the sign-in: never printed.
impl fmt::Debug for Pkce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkce").finish_non_exhaustive()
    }
}

impl Pkce {
    pub fn from_verifier(verifier: String) -> Self {
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge }
    }

    fn new() -> Result<Self, AuthError> {
        Ok(Self::from_verifier(base64url(&random_bytes::<32>()?)))
    }
}

/// A random UUIDv4 as `urn:uuid:…`: this installation's stable, anonymous host id.
pub fn new_host_id() -> Result<String, AuthError> {
    let mut bytes = random_bytes::<16>()?;
    bytes[6] = bytes[6] & 0x0f | 0x40;
    bytes[8] = bytes[8] & 0x3f | 0x80;
    let hex = util::hex(&bytes);
    Ok(format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// The host id, made on the first sign-in and kept.
fn host_id(secrets: &SecretStore) -> Result<String, AuthError> {
    let storage = |error: io::Error| AuthError::Storage(error.to_string());
    if let Some(id) = secrets.get(HOST_ID_SECRET).map_err(storage)? {
        return Ok(id);
    }
    let id = new_host_id()?;
    secrets.set(HOST_ID_SECRET, &id).map_err(storage)?;
    Ok(id)
}

/// A sign-in in the browser: a new registration, or again with the saved client.
pub struct Authorize<'a> {
    pub host_id: &'a str,
    pub redirect_uri: &'a str,
    pub state: &'a str,
    pub nonce: &'a str,
    pub challenge: &'a str,
    /// The saved registration: its client id, ID token and email.
    pub previous: Option<&'a Registration>,
}

pub fn authorize_url(request: &Authorize) -> String {
    let mut pairs = vec![
        ("response_type", "code"),
        ("redirect_uri", request.redirect_uri),
        ("scope", SCOPE),
        ("resource", RESOURCE),
        ("state", request.state),
        ("nonce", request.nonce),
        ("code_challenge_method", "S256"),
        ("code_challenge", request.challenge),
        ("ext_agent_host_id", request.host_id),
    ];
    match request.previous {
        None => pairs.extend([("client_id", DYNAMIC_CLIENT), ("agent_name_hint", APP_NAME)]),
        Some(previous) => {
            pairs.push(("client_id", previous.client_id.as_str()));
            // The same account again while signed in; after a sign-out the user chooses.
            if let Some(id_token) = &previous.id_token {
                pairs.push(("id_token_hint", id_token));
                if let Some(email) = &previous.email {
                    pairs.push(("login_hint", email));
                }
            }
            // An ordinary sign-in skips the consent screen; after a decline it is asked for.
            if previous.plan_declined {
                pairs.push(("prompt", "consent"));
            }
        }
    }
    format!("{AUTHORIZE_URL}?{}", form(&pairs))
}

#[derive(Clone, PartialEq, Eq)]
pub struct Callback {
    pub code: String,
    pub client_id: String,
}

/// The authorization code is a secret of the sign-in: never printed.
impl fmt::Debug for Callback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Callback").field("client_id", &self.client_id).finish_non_exhaustive()
    }
}

/// Reads the browser's request target (`/auth/callback?code=…`). `None` for another path,
/// such as the browser asking for an icon.
pub fn parse_callback(
    target: &str,
    state: &str,
    client_id: Option<&str>,
) -> Option<Result<Callback, AuthError>> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH {
        return None;
    }
    let value = |name: &str| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then(|| decode(value)).flatten()
        })
    };
    // Not this sign-in's answer (another program or a web page trying the port): it must not
    // end the wait for the real one.
    if value("state").as_deref() != Some(state) {
        return None;
    }
    if let Some(error) = value("error") {
        return Some(Err(if error == "access_denied" {
            AuthError::Denied
        } else {
            AuthError::Server(error)
        }));
    }
    let Some(code) = value("code").filter(|code| !code.is_empty()) else {
        return Some(Err(AuthError::Mismatch));
    };
    let returned = value("client_id");
    let client_id = match (client_id, returned) {
        // OpenAI gave this computer another client: the saved one is gone.
        (Some(known), Some(returned)) if known != returned => {
            return Some(Err(AuthError::ClientForgotten));
        }
        (Some(known), _) => known.to_string(),
        (None, Some(issued)) if !issued.is_empty() => issued,
        (None, _) => return Some(Err(AuthError::Mismatch)),
    };
    Some(Ok(Callback { code, client_id }))
}

// --- Loopback listener ----------------------------------------------------------------------

/// Shown before the code is exchanged and checked, so it promises nothing: ahem itself says
/// whether ChatGPT is connected. In both interface languages, since the browser is not ahem.
const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>ahem</title>\
<body style=\"font:16px system-ui;margin:3em\"><p>Return to ahem to finish. You can close this \
tab.</p><p>Вернитесь в ahem, чтобы закончить. Эту вкладку можно закрыть.</p>";
const FAILED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>ahem</title>\
<body style=\"font:16px system-ui;margin:3em\"><p>ChatGPT was not connected. Return to ahem and try \
again.</p><p>ChatGPT не подключён. Вернитесь в ahem и попробуйте снова.</p>";

pub struct Listener {
    socket: TcpListener,
    pub redirect_uri: String,
}

impl Listener {
    /// On `127.0.0.1` at a free port: only the port may differ between sign-ins.
    pub fn open() -> Result<Self, AuthError> {
        let network = |error: io::Error| AuthError::Network(error.to_string());
        let socket = TcpListener::bind(("127.0.0.1", 0)).map_err(network)?;
        socket.set_nonblocking(true).map_err(network)?;
        let port = socket.local_addr().map_err(network)?.port();
        Ok(Self { socket, redirect_uri: format!("http://127.0.0.1:{port}{CALLBACK_PATH}") })
    }

    /// Waits for the browser's callback; `cancel` stops it from another thread.
    pub fn wait(
        &self,
        state: &str,
        client_id: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<Callback, AuthError> {
        let deadline = Instant::now() + SIGN_IN_TIMEOUT;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(AuthError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(AuthError::TimedOut);
            }
            match self.socket.accept() {
                Ok((stream, _)) => {
                    if let Some(result) = answer(stream, state, client_id) {
                        return result;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(AuthError::Network(error.to_string())),
            }
        }
    }
}

/// Reads one request and answers it; the callback's result, `None` for anything else.
fn answer(
    mut stream: TcpStream,
    state: &str,
    client_id: Option<&str>,
) -> Option<Result<Callback, AuthError>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buffer = [0u8; 8192];
    let read = stream.read(&mut buffer).ok()?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let target = request.lines().next()?.strip_prefix("GET ")?.split(' ').next()?.to_string();
    let result = parse_callback(&target, state, client_id);
    let page = |page: &str| {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{page}",
            page.len()
        )
    };
    let reply = match &result {
        Some(Ok(_)) => page(DONE_PAGE),
        Some(Err(_)) => page(FAILED_PAGE),
        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    };
    let _ = stream.write_all(reply.as_bytes());
    result
}

// --- Tokens ---------------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

/// OAuth error codes that mean the saved sign-in is gone.
const SIGN_IN_AGAIN: [&str; 6] = [
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

fn token_request(agent: &Agent, pairs: &[(&str, &str)]) -> Result<TokenReply, AuthError> {
    let text = read_body(post_form(agent, TOKEN_URL, HTTP_TIMEOUT, pairs))?;
    serde_json::from_str(&text).map_err(|_| AuthError::Server("unexpected token reply".into()))
}

/// A form posted to the sign-in server, which answers in JSON; no redirect is followed.
fn post_form(
    agent: &Agent,
    url: &str,
    timeout: Duration,
    pairs: &[(&str, &str)],
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    agent
        .post(url)
        .config()
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .build()
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(form(pairs).as_bytes())
}

/// The body of a 2xx answer; an error answer becomes its OAuth code, never its text.
fn read_body(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<String, AuthError> {
    let mut response = response.map_err(|error| AuthError::Network(error.to_string()))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| AuthError::Network(error.to_string()))?;
    if (200..300).contains(&status) {
        return Ok(body);
    }
    let code = serde_json::from_str::<Value>(&body).ok().and_then(|value| {
        let code = value.get("error")?;
        code.as_str().or_else(|| code.get("code")?.as_str()).map(String::from)
    });
    Err(match code {
        Some(code) if SIGN_IN_AGAIN.contains(&code.as_str()) => AuthError::SignInAgain,
        Some(code)
            if code.len() <= 64 && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
        {
            AuthError::Server(code)
        }
        _ => AuthError::Server(format!("HTTP {status}")),
    })
}

fn get(
    agent: &Agent,
    url: &str,
    timeout: Duration,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    agent
        .get(url)
        .config()
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .build()
        .header("Accept", "application/json")
        .call()
}

fn parse_json(text: &str) -> Result<Value, AuthError> {
    serde_json::from_str(text).map_err(|_| AuthError::Server("unexpected reply".into()))
}

fn get_json(agent: &Agent, url: &str) -> Result<Value, AuthError> {
    parse_json(&read_body(get(agent, url, HTTP_TIMEOUT))?)
}

// --- ID token -------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub subject: String,
    pub email: Option<String>,
}

/// The claims checks: issuer, audience, expiry and the nonce of this sign-in.
pub fn check_claims(
    claims: &Value,
    client_id: &str,
    nonce: &str,
    now_secs: i64,
) -> Result<Identity, AuthError> {
    if claims["iss"].as_str().map(|iss| iss.trim_end_matches('/')) != Some(ISSUER) {
        return Err(AuthError::InvalidIdToken("issuer"));
    }
    let audience = match &claims["aud"] {
        Value::String(aud) => aud == client_id,
        Value::Array(list) => list.iter().any(|aud| aud.as_str() == Some(client_id)),
        _ => false,
    };
    if !audience {
        return Err(AuthError::InvalidIdToken("audience"));
    }
    if claims["exp"].as_i64().is_none_or(|exp| exp <= now_secs) {
        return Err(AuthError::InvalidIdToken("expiry"));
    }
    if claims["nonce"].as_str() != Some(nonce) {
        return Err(AuthError::InvalidIdToken("nonce"));
    }
    let subject = claims["sub"].as_str().filter(|sub| !sub.is_empty());
    let subject = subject.ok_or(AuthError::InvalidIdToken("subject"))?.to_string();
    Ok(Identity { subject, email: claims["email"].as_str().map(String::from) })
}

/// Checks the RS256 signature against the key set and returns the claims.
pub fn verify_signature(token: &str, keys: &Value) -> Result<Value, AuthError> {
    let invalid = || AuthError::InvalidIdToken("signature");
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(AuthError::InvalidIdToken("format"));
    };
    let json = |part: &str| -> Result<Value, AuthError> {
        let bytes = from_base64url(part).ok_or(AuthError::InvalidIdToken("format"))?;
        serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidIdToken("format"))
    };
    let head = json(header)?;
    if head["alg"] != "RS256" {
        return Err(AuthError::InvalidIdToken("algorithm"));
    }
    let kid = head["kid"].as_str();
    let key = keys["keys"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|key| key["kty"] == "RSA" && (kid.is_none() || key["kid"].as_str() == kid))
        .ok_or_else(invalid)?;
    let n = key["n"].as_str().and_then(from_base64url).ok_or_else(invalid)?;
    let e = key["e"].as_str().and_then(from_base64url).ok_or_else(invalid)?;
    let signature = from_base64url(signature).ok_or_else(invalid)?;
    let message = format!("{header}.{payload}");
    ring::signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(&ring::signature::RSA_PKCS1_2048_8192_SHA256, message.as_bytes(), &signature)
        .map_err(|_| invalid())?;
    json(payload)
}

fn verify_id_token(
    agent: &Agent,
    token: &str,
    client_id: &str,
    nonce: &str,
) -> Result<Identity, AuthError> {
    let discovery = get_json(agent, DISCOVERY_URL)?;
    let jwks_uri = discovery["jwks_uri"]
        .as_str()
        .filter(|uri| at_issuer(uri))
        .ok_or_else(|| AuthError::Server("no key set".into()))?;
    let keys = get_json(agent, jwks_uri)?;
    let claims = verify_signature(token, &keys)?;
    check_claims(&claims, client_id, nonce, now_ms() / 1000)
}

// --- Sign-in, renewal, sign-out -------------------------------------------------------------

/// The registration of a connection from before registrations were kept, unless OpenAI no
/// longer knows its client (`forgotten`): then the next sign-in registers anew instead of
/// failing the same way again.
fn from_connection(
    connected: Option<&Credentials>,
    forgotten: Option<&str>,
) -> Option<Registration> {
    connected
        .filter(|credentials| forgotten != Some(credentials.client_id.as_str()))
        .map(Registration::of)
}

/// The whole sign-in: the browser, the callback, the code exchange and the ID token checks.
/// `open` shows the authorization page. Plan usage must be granted. The saved registration is
/// reused (or that of `connected`, a connection from before registrations were kept); signing in
/// with another account than the registration's forgets it, so the next sign-in registers anew.
pub fn sign_in(
    agent: &Agent,
    secrets: &SecretStore,
    connected: Option<&Credentials>,
    cancel: &AtomicBool,
    open: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Credentials, AuthError> {
    let forgotten = secrets.get(FORGOTTEN_SECRET).ok().flatten();
    let registration =
        Registration::load(secrets).or_else(|| from_connection(connected, forgotten.as_deref()));
    let previous = registration.as_ref();
    let host_id = host_id(secrets)?;
    let pkce = Pkce::new()?;
    let state = base64url(&random_bytes::<16>()?);
    let nonce = base64url(&random_bytes::<16>()?);
    let listener = Listener::open()?;
    let url = authorize_url(&Authorize {
        host_id: &host_id,
        redirect_uri: &listener.redirect_uri,
        state: &state,
        nonce: &nonce,
        challenge: &pkce.challenge,
        previous,
    });
    open(&url).map_err(AuthError::Network)?;
    let known = previous.map(|previous| previous.client_id.as_str());
    // A registration OpenAI no longer knows would fail every sign-in: it is forgotten.
    let forget = |error: AuthError| {
        let gone = match &error {
            AuthError::ClientForgotten => true,
            AuthError::Server(code) => {
                previous.is_some()
                    && matches!(code.as_str(), "invalid_client" | "unauthorized_client")
            }
            _ => false,
        };
        if !gone {
            return error;
        }
        let _ = secrets.delete(REGISTRATION_SECRET);
        if let Some(previous) = previous {
            let _ = secrets.set(FORGOTTEN_SECRET, &previous.client_id);
        }
        AuthError::ClientForgotten
    };
    let callback = listener.wait(&state, known, cancel).map_err(forget)?;
    let reply = token_request(
        agent,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &callback.client_id),
            ("code", &callback.code),
            ("code_verifier", &pkce.verifier),
            ("redirect_uri", &listener.redirect_uri),
            ("resource", RESOURCE),
        ],
    )
    .map_err(forget)?;
    // OpenAI keeps a session from here on: one that is not kept is ended.
    let session = reply.refresh_token.clone();
    let client_id = callback.client_id;
    let kept = (|| -> Result<Credentials, AuthError> {
        let id_token = reply.id_token.ok_or(AuthError::InvalidIdToken("presence"))?;
        let identity = verify_id_token(agent, &id_token, &client_id, &nonce)?;
        if previous.is_some_and(|previous| previous.subject != identity.subject) {
            let _ = secrets.delete(REGISTRATION_SECRET);
            return Err(AuthError::OtherAccount);
        }
        let refresh_token =
            reply.refresh_token.ok_or(AuthError::Server("no refresh token".into()))?;
        let credentials = Credentials {
            client_id: client_id.clone(),
            access_token: reply.access_token,
            refresh_token,
            id_token,
            expires_at_ms: now_ms() + reply.expires_in.unwrap_or(3600) * 1000,
            scope: reply.scope.unwrap_or_default(),
            subject: identity.subject,
            email: identity.email,
        };
        Registration::of(&credentials).save(secrets)?;
        if !credentials.plan_granted() {
            return Err(AuthError::PlanNotGranted);
        }
        Ok(credentials)
    })();
    if let (Err(_), Some(refresh_token)) = (&kept, session) {
        end_session(agent, client_id, refresh_token, "sign-in not kept");
    }
    kept
}

/// A valid access token for the connection, renewed first when it is about to end. The rotated
/// refresh token is saved before the access token is used.
pub fn access_token(agent: &Agent, secrets: &SecretStore, id: &str) -> Result<String, AuthError> {
    let _one = REFRESH.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut credentials = Credentials::load(secrets, id)?.ok_or(AuthError::SignInAgain)?;
    let margin = i64::try_from(REFRESH_MARGIN.as_millis()).unwrap_or(i64::MAX);
    if credentials.expires_at_ms - now_ms() > margin {
        return Ok(credentials.access_token);
    }
    let reply = token_request(
        agent,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &credentials.client_id),
            ("refresh_token", &credentials.refresh_token),
            ("resource", RESOURCE),
        ],
    )?;
    credentials.access_token = reply.access_token;
    if let Some(refresh) = reply.refresh_token {
        credentials.refresh_token = refresh;
    }
    if let Some(id_token) = reply.id_token {
        credentials.id_token = id_token;
    }
    if let Some(scope) = reply.scope {
        credentials.scope = scope;
    }
    credentials.expires_at_ms = now_ms() + reply.expires_in.unwrap_or(3600) * 1000;
    credentials.save(secrets, id)?;
    Ok(credentials.access_token)
}

/// Forgets the connection's sign-in and returns it for [`revoke_retrying`]. Under the renewal
/// lock: a renewal running now ends first, so these are the latest tokens and none is saved
/// after. The registration stays, so signing in again does not register ahem a second time,
/// but without the ID token: the next sign-in lets the user choose the account. A record that
/// cannot be read is deleted too and returned as the error: its tokens cannot be revoked.
pub fn sign_out(secrets: &SecretStore, id: &str) -> Result<Option<Credentials>, AuthError> {
    let _one = REFRESH.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let credentials = Credentials::load(secrets, id);
    if let Ok(Some(credentials)) = &credentials
        && let Err(error) = Registration::signed_out(credentials).save(secrets)
    {
        log_line!("ChatGPT sign-out: {error}");
    }
    if let Err(error) = secrets.delete(id) {
        log_line!("ChatGPT sign-out: cannot delete the sign-in: {:?}", error.kind());
    }
    credentials
}

/// What became of the refresh token at sign-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revocation {
    /// OpenAI accepted the revocation.
    Revoked,
    /// OpenAI publishes no revocation endpoint: the token runs out by itself.
    NoEndpoint,
    /// `again`: no answer, a server error or too many requests, worth trying again.
    Failed { error: AuthError, again: bool },
}

impl std::fmt::Display for Revocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Revoked => write!(f, "revoked"),
            Self::NoEndpoint => write!(f, "no revocation endpoint, the token expires by itself"),
            Self::Failed { error, .. } => write!(f, "not revoked: {error}"),
        }
    }
}

/// A revocation's requests wait less than the others: the user waits for the sign-out.
const REVOKE_TIMEOUT: Duration = Duration::from_secs(5);
/// The pauses before a revocation is tried again.
const REVOKE_RETRIES: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];

/// Whether a failed request is tried again, as OpenAI advises for a revocation: no answer
/// (`None`), a server error or too many requests.
fn temporary(status: Option<u16>) -> bool {
    status.is_none_or(|status| status >= 500 || status == 429)
}

fn status_of(response: &Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Option<u16> {
    response.as_ref().ok().map(|response| response.status().as_u16())
}

/// [`revoke`], tried again after a temporary failure.
pub fn revoke_retrying(agent: &Agent, client_id: &str, refresh_token: &str) -> Revocation {
    let mut outcome = revoke(agent, client_id, refresh_token);
    for pause in REVOKE_RETRIES {
        if !matches!(outcome, Revocation::Failed { again: true, .. }) {
            break;
        }
        thread::sleep(pause);
        outcome = revoke(agent, client_id, refresh_token);
    }
    outcome
}

/// Ends a session that is not kept, in the background; the outcome goes to the log only.
pub fn end_session(agent: &Agent, client_id: String, refresh_token: String, why: &'static str) {
    let agent = agent.clone();
    thread::spawn(move || {
        let outcome = revoke_retrying(&agent, &client_id, &refresh_token);
        log_line!("ChatGPT {why}: {outcome}");
    });
}

/// Revokes the refresh token, which also ends its access tokens (RFC 7009), where OpenAI
/// publishes a revocation endpoint. The registration stays in the user's ChatGPT settings.
fn revoke(agent: &Agent, client_id: &str, refresh_token: &str) -> Revocation {
    let failed = |status, error| Revocation::Failed { error, again: temporary(status) };
    let response = get(agent, DISCOVERY_URL, REVOKE_TIMEOUT);
    let status = status_of(&response);
    let discovery = match read_body(response).and_then(|text| parse_json(&text)) {
        Ok(discovery) => discovery,
        Err(error) => return failed(status, error),
    };
    let endpoint = discovery["revocation_endpoint"].as_str().filter(|url| at_issuer(url));
    let Some(endpoint) = endpoint else { return Revocation::NoEndpoint };
    let pairs =
        [("token", refresh_token), ("token_type_hint", "refresh_token"), ("client_id", client_id)];
    let response = post_form(agent, endpoint, REVOKE_TIMEOUT, &pairs);
    let status = status_of(&response);
    // Accepted even when the body breaks off after the status.
    if accepted(status) {
        return Revocation::Revoked;
    }
    match read_body(response) {
        Ok(_) => Revocation::Revoked,
        Err(error) => failed(status, error),
    }
}

/// OpenAI accepts a revocation with an empty 2xx answer, even for a token that no longer works.
fn accepted(status: Option<u16>) -> bool {
    status.is_some_and(|status| (200..300).contains(&status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_issuers_own_host_is_trusted() {
        assert!(at_issuer("https://auth.openai.com/.well-known/jwks.json"));
        assert!(!at_issuer("https://auth.openai.com.example/jwks.json"));
        assert!(!at_issuer("https://auth.openai.community/revoke"));
        assert!(!at_issuer("https://auth.openai.com"));
        assert!(!at_issuer("http://auth.openai.com/revoke"));
    }

    /// RFC 7636, appendix B.
    #[test]
    fn pkce_challenge_is_the_rfc_example() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(pkce.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn host_id_is_a_version_4_uuid_urn() {
        let id = new_host_id().unwrap();
        let uuid = id.strip_prefix("urn:uuid:").unwrap();
        assert_eq!(uuid.len(), 36);
        assert_eq!(&uuid[14..15], "4");
        assert!("89ab".contains(&uuid[19..20]));
        assert_ne!(id, new_host_id().unwrap());
    }

    fn registration() -> Registration {
        Registration::of(&previous())
    }

    fn previous() -> Credentials {
        Credentials {
            client_id: "oaiapp_123".into(),
            access_token: "a".into(),
            refresh_token: "r".into(),
            id_token: "header.payload.sig".into(),
            expires_at_ms: 0,
            scope: SCOPE.into(),
            subject: "user-1".into(),
            email: Some("me@example.com".into()),
        }
    }

    #[test]
    fn a_first_sign_in_registers_a_client() {
        let url = authorize_url(&Authorize {
            host_id: "urn:uuid:1",
            redirect_uri: "http://127.0.0.1:5000/auth/callback",
            state: "s",
            nonce: "n",
            challenge: "c",
            previous: None,
        });
        assert!(
            url.starts_with("https://auth.openai.com/api/accounts/authorize?response_type=code&")
        );
        assert!(url.contains("&redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2Fauth%2Fcallback&"));
        assert!(url.contains(
            "&scope=openid%20profile%20email%20offline_access%20resource.invoke%20chatgpt.tokens.use.direct&"
        ));
        assert!(url.contains("&resource=https%3A%2F%2Fapi.openai.com%2Fv1&"));
        assert!(url.contains("&code_challenge_method=S256&code_challenge=c&"));
        assert!(url.contains("&ext_agent_host_id=urn%3Auuid%3A1&"));
        assert!(url.ends_with("&client_id=dynamic_agent_client&agent_name_hint=ahem"));
    }

    #[test]
    fn signing_in_again_reuses_the_client() {
        let previous = registration();
        let url = authorize_url(&Authorize {
            host_id: "h",
            redirect_uri: "http://127.0.0.1:1/auth/callback",
            state: "s",
            nonce: "n",
            challenge: "c",
            previous: Some(&previous),
        });
        assert!(url.contains("&client_id=oaiapp_123&id_token_hint=header.payload.sig"));
        assert!(url.ends_with("&login_hint=me%40example.com"));
        assert!(!url.contains("dynamic_agent_client") && !url.contains("agent_name_hint"));
    }

    fn again(previous: &Registration) -> String {
        authorize_url(&Authorize {
            host_id: "h",
            redirect_uri: "http://127.0.0.1:1/auth/callback",
            state: "s",
            nonce: "n",
            challenge: "c",
            previous: Some(previous),
        })
    }

    #[test]
    fn after_a_sign_out_the_user_chooses_the_account() {
        let url = again(&Registration::signed_out(&previous()));
        assert!(url.ends_with("&client_id=oaiapp_123"), "{url}");
        assert!(!url.contains("id_token_hint") && !url.contains("login_hint"));
    }

    #[test]
    fn a_declined_plan_is_asked_for_again() {
        let mut credentials = previous();
        credentials.scope = "openid profile email offline_access".into();
        let declined = Registration::of(&credentials);
        assert!(declined.plan_declined);
        assert!(again(&declined).ends_with("&prompt=consent"));
        assert!(!registration().plan_declined);
        assert!(!again(&registration()).contains("prompt="), "not at every sign-in");
    }

    #[test]
    fn a_revocation_is_tried_again_only_after_temporary_failures() {
        assert!(temporary(None), "no answer");
        assert!(temporary(Some(503)) && temporary(Some(500)));
        assert!(temporary(Some(429)));
        assert!(!temporary(Some(400)) && !temporary(Some(401)));
        assert!(!temporary(Some(200)), "an answer that cannot be read");
        assert!(accepted(Some(200)) && accepted(Some(204)), "whatever the body");
        assert!(!accepted(Some(400)) && !accepted(None));
    }

    /// A connection whose client OpenAI forgot is not the next sign-in's registration.
    #[test]
    fn a_forgotten_client_is_not_reused() {
        let connected = previous();
        assert_eq!(from_connection(Some(&connected), None).unwrap().client_id, "oaiapp_123");
        assert_eq!(
            from_connection(Some(&connected), Some("oaiapp_9")).unwrap().client_id,
            "oaiapp_123"
        );
        assert!(from_connection(Some(&connected), Some("oaiapp_123")).is_none());
        assert!(from_connection(None, None).is_none());
    }

    #[test]
    fn the_callback_is_checked_against_this_sign_in() {
        let ok = parse_callback("/auth/callback?code=abc&state=s1&client_id=oaiapp_9", "s1", None);
        assert_eq!(ok, Some(Ok(Callback { code: "abc".into(), client_id: "oaiapp_9".into() })));
        // Signing in again: the callback may leave out the client id, never change it.
        let again = parse_callback("/auth/callback?code=abc&state=s1", "s1", Some("oaiapp_9"));
        assert_eq!(again.unwrap().unwrap().client_id, "oaiapp_9");
        let other = "/auth/callback?code=abc&state=s1&client_id=oaiapp_X";
        assert_eq!(
            parse_callback(other, "s1", Some("oaiapp_9")),
            Some(Err(AuthError::ClientForgotten)),
            "another client for this computer: the saved one is gone"
        );
        // A request with another state is not this sign-in's answer: the wait goes on.
        let forged = "/auth/callback?code=abc&state=other&client_id=oaiapp_9";
        assert_eq!(parse_callback(forged, "s1", None), None);
        assert_eq!(parse_callback("/auth/callback?code=abc", "s1", None), None);
        let denied = "/auth/callback?error=access_denied&state=s1";
        assert_eq!(parse_callback(denied, "s1", None), Some(Err(AuthError::Denied)));
        let no_client = "/auth/callback?code=abc&state=s1";
        assert_eq!(parse_callback(no_client, "s1", None), Some(Err(AuthError::Mismatch)));
        assert_eq!(parse_callback("/favicon.ico", "s1", None), None);
        assert_eq!(parse_callback("/callback?code=a&state=s1", "s1", None), None);
        let encoded =
            parse_callback("/auth/callback?code=a%2Bb&state=s%3D1&client_id=c", "s=1", None);
        assert_eq!(encoded.unwrap().unwrap().code, "a+b");
    }

    #[test]
    fn the_listener_takes_the_callback_and_ignores_other_requests() {
        let listener = Listener::open().unwrap();
        let address =
            listener.redirect_uri.trim_start_matches("http://").replace(CALLBACK_PATH, "");
        assert!(listener.redirect_uri.starts_with("http://127.0.0.1:"));
        let browser = thread::spawn(move || {
            let mut answers = Vec::new();
            for target in ["/favicon.ico", "/auth/callback?code=c1&state=s&client_id=oaiapp_1"] {
                let mut stream = TcpStream::connect(&address).unwrap();
                write!(stream, "GET {target} HTTP/1.1\r\nHost: {address}\r\n\r\n").unwrap();
                let mut reply = String::new();
                stream.read_to_string(&mut reply).unwrap();
                answers.push(reply.lines().next().unwrap().to_string());
            }
            answers
        });
        let callback = listener.wait("s", None, &AtomicBool::new(false)).unwrap();
        assert_eq!(callback, Callback { code: "c1".into(), client_id: "oaiapp_1".into() });
        assert_eq!(browser.join().unwrap(), ["HTTP/1.1 404 Not Found", "HTTP/1.1 200 OK"]);
    }

    #[test]
    fn a_cancelled_sign_in_stops_waiting() {
        let listener = Listener::open().unwrap();
        let result = listener.wait("s", None, &AtomicBool::new(true));
        assert_eq!(result, Err(AuthError::Cancelled));
    }

    #[test]
    fn claims_are_checked() {
        let claims = json!({
            "iss": "https://auth.openai.com",
            "aud": ["oaiapp_1"],
            "exp": 2_000,
            "nonce": "n1",
            "sub": "user-1",
            "email": "me@example.com",
        });
        let identity = check_claims(&claims, "oaiapp_1", "n1", 1_000).unwrap();
        assert_eq!(
            identity,
            Identity { subject: "user-1".into(), email: Some("me@example.com".into()) }
        );
        let bad = |change: &dyn Fn(&mut Value), check| {
            let mut claims = claims.clone();
            change(&mut claims);
            assert_eq!(
                check_claims(&claims, "oaiapp_1", "n1", 1_000),
                Err(AuthError::InvalidIdToken(check))
            );
        };
        bad(&|c| c["iss"] = json!("https://evil.example"), "issuer");
        bad(&|c| c["aud"] = json!("oaiapp_2"), "audience");
        bad(&|c| c["exp"] = json!(999), "expiry");
        bad(&|c| c["nonce"] = json!("other"), "nonce");
        bad(&|c| c["sub"] = json!(""), "subject");
    }

    /// A token signed by a key made for the test, and the key set with its public half.
    fn signed_token() -> (String, Value) {
        use rsa::signature::{SignatureEncoding, Signer};
        use rsa::traits::PublicKeyParts;
        let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        let public = private.to_public_key();
        let keys = json!({"keys": [{
            "kty": "RSA", "kid": "test-key", "alg": "RS256", "use": "sig",
            "n": base64url(&public.n().to_bytes_be()),
            "e": base64url(&public.e().to_bytes_be()),
        }]});
        let header = base64url(br#"{"alg":"RS256","kid":"test-key","typ":"JWT"}"#);
        let claims = base64url(
            br#"{"iss":"https://auth.openai.com","aud":"oaiapp_1","sub":"user-1","nonce":"n1","exp":4102444800}"#,
        );
        let signing_key = rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(private);
        let signature = signing_key.sign(format!("{header}.{claims}").as_bytes());
        (format!("{header}.{claims}.{}", base64url(&signature.to_bytes())), keys)
    }

    #[test]
    fn the_signature_is_checked_with_the_key_set() {
        let (token, keys) = signed_token();
        let token = token.as_str();
        let claims = verify_signature(token, &keys).unwrap();
        assert_eq!(claims["sub"], "user-1");
        // A changed payload no longer matches the signature.
        let mut parts: Vec<&str> = token.split('.').collect();
        let forged = base64url(br#"{"sub":"user-2"}"#);
        parts[1] = &forged;
        assert_eq!(
            verify_signature(&parts.join("."), &keys),
            Err(AuthError::InvalidIdToken("signature"))
        );
        // An unknown key id, or no keys at all.
        assert!(verify_signature(token, &json!({"keys": []})).is_err());
        let unsigned = format!("{}.{}.", base64url(br#"{"alg":"none"}"#), parts[1]);
        assert_eq!(verify_signature(&unsigned, &keys), Err(AuthError::InvalidIdToken("algorithm")));
    }

    /// A sign-out keeps the registration, so the next sign-in reuses its client.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "uses the login Keychain: cargo test -- --ignored the_registration_outlives"
    )]
    fn the_registration_outlives_the_sign_in() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("chatgpt-registration-{}", std::process::id()));
        let secrets = SecretStore::new(dir.join("secrets.json"));
        assert!(Registration::load(&secrets).is_none());
        let credentials = previous();
        credentials.save(&secrets, "connection-1").unwrap();
        Registration::of(&credentials).save(&secrets).unwrap();
        secrets.delete("connection-1").unwrap();
        let kept = Registration::load(&secrets).unwrap();
        assert_eq!((kept.client_id.as_str(), kept.subject.as_str()), ("oaiapp_123", "user-1"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The sign-out keeps the registration without the ID token, and nothing renews after it.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "uses the login Keychain: cargo test -- --ignored signing_out_forgets"
    )]
    fn signing_out_forgets_the_tokens_and_the_hint() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("chatgpt-sign-out-{}", std::process::id()));
        let secrets = SecretStore::new(dir.join("secrets.json"));
        previous().save(&secrets, "connection-1").unwrap();
        let signed_out = sign_out(&secrets, "connection-1").unwrap().expect("the sign-in");
        assert_eq!(signed_out.refresh_token, "r", "returned for the revocation");
        assert_eq!(Credentials::load(&secrets, "connection-1"), Ok(None));
        let kept = Registration::load(&secrets).unwrap();
        assert_eq!(kept.client_id, "oaiapp_123");
        assert_eq!(kept.id_token, None);
        let agent = crate::runtimes::http::agent();
        assert_eq!(access_token(&agent, &secrets, "connection-1"), Err(AuthError::SignInAgain));
        assert_eq!(sign_out(&secrets, "connection-1"), Ok(None));
        // A record that cannot be read is gone too, and says so: nothing could be revoked.
        secrets.set("connection-2", "damaged").unwrap();
        assert!(sign_out(&secrets, "connection-2").is_err());
        assert_eq!(Credentials::load(&secrets, "connection-2"), Ok(None));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn plan_usage_needs_its_scope() {
        let mut credentials = previous();
        assert!(credentials.plan_granted());
        credentials.scope = "openid profile email offline_access".into();
        assert!(!credentials.plan_granted());
        assert!(!format!("{credentials:?}").contains("header.payload.sig"));
    }
}

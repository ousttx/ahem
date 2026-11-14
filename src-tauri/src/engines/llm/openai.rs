//! OpenAI Responses API (`POST /responses`), Chat Completions (`POST /chat/completions`) for
//! OpenAI-compatible servers, and their shared model list (`GET /models`).

use serde::Deserialize;
use serde_json::{Value, json};

use super::prompt::Prompt;
use super::{Api, LlmError, ModelInfo, plan_error, safe_code};

/// Reasoning tokens count toward `max_output_tokens`. Responses requests carry no
/// `temperature`: OpenAI's reasoning models reject it unless reasoning is turned off.
const RESPONSES_OUTPUT_CAP: u32 = 16_384;
/// Servers differ; DeepSeek's non-thinking default is 8K.
const CHAT_OUTPUT_CAP: u32 = 8192;
const CHAT_TEMPERATURE: f64 = 0.2;

pub(super) fn respond(api: &Api, model: &str, prompt: &Prompt) -> Result<String, LlmError> {
    let body = json!({
        "model": model,
        "instructions": prompt.system,
        "input": prompt.user,
        "store": false,
        "max_output_tokens": prompt.max_output_tokens(RESPONSES_OUTPUT_CAP),
    });
    let reply: Response = api.post("/responses", &body)?;
    reply.text()
}

/// The Responses API on the user's ChatGPT plan: streamed and not stored, as plan usage
/// requires, and without an output limit, which it does not accept.
pub(super) fn respond_plan(api: &Api, model: &str, prompt: &Prompt) -> Result<String, LlmError> {
    let body = plan_body(model, prompt);
    plan_text(&api.post_text("/responses", &body, "text/event-stream")?)
}

/// The system prompt goes as `instructions`: plan usage rejects system messages.
fn plan_body(model: &str, prompt: &Prompt) -> Value {
    json!({
        "model": model,
        "instructions": prompt.system,
        "input": [{"role": "user", "content": prompt.user}],
        "store": false,
        "stream": true,
    })
}

/// The answer from the stream's events; it counts only once `response.completed` arrived.
fn plan_text(events: &str) -> Result<String, LlmError> {
    let mut text = String::new();
    let read = |value: &Value| value.as_str().and_then(safe_code);
    for line in events.lines() {
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
        let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
        match event["type"].as_str() {
            Some("response.output_text.delta") => {
                text.push_str(event["delta"].as_str().unwrap_or_default());
            }
            Some("response.completed") => {
                // The final response has the whole text; the deltas are the fallback.
                let full = match serde_json::from_value::<Response>(event["response"].clone()) {
                    // A refusal is a refusal, not the empty text of the deltas.
                    Ok(response) => match response.text() {
                        Err(LlmError::Refused) => return Err(LlmError::Refused),
                        result => result.ok(),
                    },
                    Err(_) => None,
                };
                return Ok(full.filter(|full| !full.trim().is_empty()).unwrap_or(text));
            }
            Some("response.incomplete") => {
                let reason = event["response"]["incomplete_details"]["reason"].as_str();
                return Err(if reason == Some("content_filter") {
                    LlmError::Refused
                } else {
                    LlmError::Incomplete
                });
            }
            Some("response.failed" | "error") => {
                let error = &event["response"]["error"];
                let code = read(&error["code"])
                    .or_else(|| read(&event["code"]))
                    .or_else(|| read(&event["error"]["code"]));
                return Err(match code.as_deref().and_then(plan_error) {
                    Some(error) => error,
                    None => LlmError::BadResponse(format!(
                        "generation failed: {}",
                        code.as_deref().unwrap_or("no code")
                    )),
                });
            }
            _ => {}
        }
    }
    Err(LlmError::Incomplete)
}

/// The models of the user's plan: `models[]` with a slug, only those meant to be listed.
pub(super) fn plan_models(api: &Api) -> Result<Vec<String>, LlmError> {
    let reply: Value = api.get("/models", &[])?;
    let listed =
        reply["models"].as_array().into_iter().flatten().filter(|model| {
            model["visibility"].as_str().is_none_or(|visibility| visibility == "list")
        });
    Ok(listed.filter_map(|model| model["slug"].as_str().map(String::from)).collect())
}

#[derive(Deserialize)]
struct Response {
    status: Option<String>,
    incomplete_details: Option<Value>,
    error: Option<Value>,
    /// Items are read loosely: reasoning, tool and future item types have other shapes.
    output: Option<Vec<Value>>,
}

impl Response {
    fn text(self) -> Result<String, LlmError> {
        let field = |value: &Option<Value>, name: &str| {
            value
                .as_ref()
                .and_then(|value| value.get(name))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        match self.status.as_deref() {
            Some("completed") => {}
            Some("incomplete") => {
                return Err(match field(&self.incomplete_details, "reason").as_deref() {
                    Some("content_filter") => LlmError::Refused,
                    // max_output_tokens and others
                    _ => LlmError::Incomplete,
                });
            }
            Some("failed") => {
                let code = field(&self.error, "code").and_then(|code| safe_code(&code));
                return Err(LlmError::BadResponse(format!(
                    "generation failed: {}",
                    code.as_deref().unwrap_or("no code")
                )));
            }
            status => {
                let status = status.and_then(safe_code);
                return Err(LlmError::BadResponse(format!(
                    "status {}",
                    status.as_deref().unwrap_or("missing")
                )));
            }
        }
        let mut text = String::new();
        let mut refused = false;
        for item in self.output.iter().flatten().filter(|item| item["type"] == "message") {
            for part in item["content"].as_array().into_iter().flatten() {
                match part["type"].as_str() {
                    Some("output_text") => text.push_str(part["text"].as_str().unwrap_or_default()),
                    Some("refusal") => refused = true,
                    _ => {}
                }
            }
        }
        if refused && text.trim().is_empty() { Err(LlmError::Refused) } else { Ok(text) }
    }
}

pub(super) fn complete(api: &Api, model: &str, prompt: &Prompt) -> Result<String, LlmError> {
    let body = chat_body(model, prompt, is_deepseek(&api.base));
    let reply: Completion = api.post("/chat/completions", &body)?;
    reply.text()
}

fn chat_body(model: &str, prompt: &Prompt, deepseek: bool) -> Value {
    let mut body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": prompt.system},
            {"role": "user", "content": prompt.user},
        ],
        "temperature": CHAT_TEMPERATURE,
        "max_tokens": prompt.max_output_tokens(CHAT_OUTPUT_CAP),
        "stream": false,
    });
    if deepseek {
        // DeepSeek thinks by default; thinking is turned off. Other servers may reject
        // the unknown field, so it goes to DeepSeek only.
        body["thinking"] = json!({"type": "disabled"});
    }
    body
}

/// The official DeepSeek API, by host.
fn is_deepseek(base: &str) -> bool {
    super::host_of(base).as_deref() == Some("api.deepseek.com")
}

#[derive(Deserialize)]
struct Completion {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    finish_reason: Option<String>,
    message: Option<ChatMessage>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
    refusal: Option<String>,
}

impl Completion {
    fn text(self) -> Result<String, LlmError> {
        let Some(choice) = self.choices.into_iter().next() else {
            return Err(LlmError::BadResponse("no choices".into()));
        };
        match choice.finish_reason.as_deref() {
            Some("stop") => {}
            Some("content_filter") => return Err(LlmError::Refused),
            // length, insufficient_system_resource, aborted, tool_calls, missing…
            _ => return Err(LlmError::Incomplete),
        }
        let Some(message) = choice.message else {
            return Err(LlmError::BadResponse("no message".into()));
        };
        let content = message.content.unwrap_or_default();
        let refused = message.refusal.is_some_and(|refusal| !refusal.trim().is_empty());
        if refused && content.trim().is_empty() { Err(LlmError::Refused) } else { Ok(content) }
    }
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelInfo>,
}

pub(super) fn models(api: &Api) -> Result<Vec<String>, LlmError> {
    let list: ModelList = api.get("/models", &[])?;
    Ok(list.data.into_iter().map(|model| model.id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::llm::Request;

    #[test]
    fn plan_requests_stream_unstored_and_without_limits() {
        let request = Request {
            instruction: "Fix it.",
            text: "text",
            output_language: None,
            context: &crate::domain::context::NO_CONTEXT,
        };
        let prompt = Prompt::new(&request);
        let body = plan_body("gpt-6.1-sol", &prompt);
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["instructions"], prompt.system);
        assert_eq!(body["input"], json!([{"role": "user", "content": prompt.user}]));
        let fields: Vec<&String> = body.as_object().unwrap().keys().collect();
        assert_eq!(fields, ["input", "instructions", "model", "store", "stream"]);
    }

    fn stream(events: &[Value]) -> String {
        events.iter().map(|event| format!("event: x\ndata: {event}\n\n")).collect()
    }

    #[test]
    fn a_plan_answer_counts_once_the_stream_completes() {
        let delta = |text: &str| json!({"type": "response.output_text.delta", "delta": text});
        let completed = json!({"type": "response.completed", "response": {"status": "completed"}});
        let events = stream(&[delta("Hello, "), delta("world"), completed]);
        assert_eq!(plan_text(&events), Ok("Hello, world".into()));
        // The final response's own text wins over the pieces.
        let full = json!({"type": "response.completed", "response": {
            "status": "completed",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "Whole"}]}],
        }});
        assert_eq!(plan_text(&stream(&[delta("Who"), full])), Ok("Whole".into()));
        // Cut off before the end: not an answer.
        assert_eq!(plan_text(&stream(&[delta("Hel")])), Err(LlmError::Incomplete));
        let incomplete = json!({"type": "response.incomplete", "response": {
            "incomplete_details": {"reason": "content_filter"}}});
        assert_eq!(plan_text(&stream(&[incomplete])), Err(LlmError::Refused));
        // A completed answer that is only a refusal is a refusal, not an empty text.
        let refused = json!({"type": "response.completed", "response": {
            "status": "completed",
            "output": [{"type": "message", "content": [{"type": "refusal", "refusal": "No."}]}],
        }});
        assert_eq!(plan_text(&stream(&[refused])), Err(LlmError::Refused));
    }

    #[test]
    fn plan_errors_are_read_from_the_stream_and_the_status() {
        let failed =
            |code: &str| json!({"type": "response.failed", "response": {"error": {"code": code}}});
        let limit = stream(&[failed("subscription_sharing_usage_limit_exceeded")]);
        assert_eq!(plan_text(&limit), Err(LlmError::PlanLimit));
        let other = stream(&[failed("server_error")]);
        assert_eq!(
            plan_text(&other),
            Err(LlmError::BadResponse("generation failed: server_error".into()))
        );
        let body = |code: &str| json!({"error": {"code": code}}).to_string();
        let http_error = super::super::http_error;
        assert_eq!(
            http_error(429, &body("subscription_sharing_usage_limit_exceeded")),
            LlmError::PlanLimit
        );
        assert_eq!(
            http_error(403, &body("subscription_sharing_user_not_eligible")),
            LlmError::PlanNotEligible
        );
        assert_eq!(
            http_error(401, &body("subscription_sharing_invalid_user")),
            LlmError::Unauthorized
        );
    }

    #[test]
    fn thinking_is_disabled_only_for_deepseek() {
        let request = Request {
            instruction: "Fix it.",
            text: "text",
            output_language: None,
            context: &crate::domain::context::NO_CONTEXT,
        };
        let prompt = Prompt::new(&request);
        let body = chat_body("deepseek-v4-flash", &prompt, true);
        assert_eq!(body["thinking"], json!({"type": "disabled"}));
        assert!(chat_body("llama3", &prompt, false).get("thinking").is_none());

        assert!(is_deepseek("https://api.deepseek.com"));
        assert!(is_deepseek("https://API.deepseek.com/v1"));
        assert!(!is_deepseek("https://api.deepseek.com.example.org/v1"));
        assert!(!is_deepseek("https://api.groq.com/openai/v1"));
        assert!(!is_deepseek("http://127.0.0.1:11434/v1"));
    }
}

//! Anthropic Messages API (`POST /v1/messages`) and model list (`GET /v1/models`).

use serde::Deserialize;
use serde_json::json;

use super::prompt::Prompt;
use super::{Api, LlmError, ModelInfo};

/// The latest API version. It removes `temperature`, and Claude 4.7 and later accept only the
/// default anyway, so requests carry no sampling parameters.
pub(super) const API_VERSION: &str = "2026-10-21";
/// Thinking counts toward `max_tokens`; current Claude models allow far more than this.
const OUTPUT_CAP: u32 = 16_384;
/// Up to 1000 models per page; more pages than this means the cursor is broken.
const MAX_PAGES: usize = 10;

pub(super) fn process(api: &Api, model: &str, prompt: &Prompt) -> Result<String, LlmError> {
    let body = json!({
        "model": model,
        "max_tokens": prompt.max_output_tokens(OUTPUT_CAP),
        "system": prompt.system,
        "messages": [{"role": "user", "content": prompt.user}],
    });
    let reply: Message = api.post("/v1/messages", &body)?;
    reply.text()
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Vec<Block>,
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

impl Message {
    fn text(self) -> Result<String, LlmError> {
        match self.stop_reason.as_deref() {
            Some("end_turn" | "stop_sequence") => {}
            Some("refusal") => return Err(LlmError::Refused),
            // max_tokens, model_context_window_exceeded, pause_turn, tool_use…
            _ => return Err(LlmError::Incomplete),
        }
        // Thinking blocks are skipped.
        Ok(self
            .content
            .into_iter()
            .filter(|block| block.kind == "text")
            .filter_map(|block| block.text)
            .collect())
    }
}

#[derive(Deserialize)]
struct Page {
    data: Vec<ModelInfo>,
    #[serde(default)]
    has_more: bool,
    last_id: Option<String>,
}

pub(super) fn models(api: &Api) -> Result<Vec<String>, LlmError> {
    let mut ids = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut query = vec![("limit", "1000")];
        if let Some(cursor) = &after {
            query.push(("after_id", cursor.as_str()));
        }
        let page: Page = api.get("/v1/models", &query)?;
        ids.extend(page.data.into_iter().map(|model| model.id));
        if !page.has_more {
            return Ok(ids);
        }
        match page.last_id {
            Some(last) if after.as_ref() != Some(&last) => after = Some(last),
            _ => break,
        }
    }
    Err(LlmError::BadResponse("the model list does not end".into()))
}

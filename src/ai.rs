/// SQL completion via the Anthropic Messages API.
///
/// The API key comes from the config's `ai_api_key`, falling back to
/// `ANTHROPIC_API_KEY`; the model from the config's `ai_model`, falling
/// back to `PG_GUI_AI_MODEL` (defaults to `claude-opus-4-8`). The config's
/// `ai_prompt` is appended to the system prompt when set.
const API_URL: &str = "https://api.anthropic.com/v1/messages";
const DEFAULT_MODEL: &str = "claude-opus-4-8";

const SYSTEM_PROMPT: &str = "You are a PostgreSQL autocomplete engine embedded in a SQL editor. \
The user provides the SQL text before and after the cursor. \
Respond with ONLY the text to insert at the cursor position so the statement becomes valid, \
useful PostgreSQL. Do not repeat text that is already before the cursor. \
No markdown fences, no commentary, no explanation — output raw SQL text only.";

/// The key to use for completions: the one configured in the app config,
/// or `ANTHROPIC_API_KEY` from the environment when the config is empty.
/// The config wins so an app-specific key can override a globally set one.
pub fn api_key(configured: &str) -> Option<String> {
    let configured = configured.trim();
    if !configured.is_empty() {
        return Some(configured.to_string());
    }
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

/// The model to use for completions: the one configured in the app config,
/// or `PG_GUI_AI_MODEL` from the environment, or the built-in default.
/// The config wins so an app-specific model can override a globally set one.
pub fn model(configured: &str) -> String {
    let configured = configured.trim();
    if !configured.is_empty() {
        return configured.to_string();
    }
    std::env::var("PG_GUI_AI_MODEL")
        .ok()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

pub fn complete(
    api_key: &str,
    model: &str,
    prompt_addition: &str,
    before: &str,
    after: &str,
) -> Result<String, String> {
    let user_message = format!(
        "<sql_before_cursor>{before}</sql_before_cursor>\n<sql_after_cursor>{after}</sql_after_cursor>\n\
         Provide the completion to insert at the cursor."
    );

    let prompt_addition = prompt_addition.trim();
    let system = if prompt_addition.is_empty() {
        SYSTEM_PROMPT.to_string()
    } else {
        format!("{SYSTEM_PROMPT}\n\n{prompt_addition}")
    };

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 512,
        "system": system,
        "output_config": { "effort": "low" },
        "messages": [
            { "role": "user", "content": user_message }
        ],
    });

    let response = ureq::post(API_URL)
        .set("x-api-key", api_key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .send_json(body);

    let json: serde_json::Value = match response {
        Ok(resp) => resp.into_json().map_err(|e| format!("bad response: {e}"))?,
        Err(ureq::Error::Status(code, resp)) => {
            let detail = resp
                .into_json::<serde_json::Value>()
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(String::from))
                .unwrap_or_default();
            return Err(format!("API error {code}: {detail}"));
        }
        Err(e) => return Err(format!("request failed: {e}")),
    };

    if json["stop_reason"].as_str() == Some("refusal") {
        return Err("the model declined this request".to_string());
    }

    let text: String = json["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect()
        })
        .unwrap_or_default();

    if text.is_empty() {
        Err("empty completion".to_string())
    } else {
        Ok(text)
    }
}

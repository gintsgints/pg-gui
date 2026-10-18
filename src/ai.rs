/// SQL completion via the Anthropic Messages API.
///
/// The API key comes from the config's `ai_api_key`, falling back to
/// `ANTHROPIC_API_KEY`; the model from the config's `ai_model`, falling
/// back to `PG_GUI_AI_MODEL` (defaults to `claude-opus-4-8`). The config's
/// `ai_prompt` is appended to the system prompt when set.
use std::time::Duration;

const API_URL: &str = "https://api.anthropic.com/v1/messages";

/// Retries after the first attempt when the API is rate limiting or
/// overloaded. Completion is interactive: someone is waiting at the cursor,
/// so a couple of short retries are worth it and a long queue is not.
const MAX_RETRIES: u32 = 2;

/// Longest single wait before a retry. A `retry-after` beyond this fails
/// straight away and says how long to wait, rather than freezing the request
/// that long.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(8);
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

    let mut attempt = 0;
    let json = loop {
        match send(api_key, &body)? {
            Reply::Ok(json) => break json,
            Reply::Failed {
                status,
                retry_after,
                detail,
            } => {
                let error = format!("API error {status}: {detail}");
                match retry_delay(status, retry_after.as_deref(), attempt) {
                    Retry::After(delay) => std::thread::sleep(delay),
                    Retry::TooLong(wait) => {
                        return Err(format!(
                            "{error} (rate limited — try again in {}s)",
                            wait.as_secs()
                        ));
                    }
                    Retry::No => return Err(error),
                }
                attempt += 1;
            }
        }
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

/// One request's answer: the parsed body of a success, or what a failure
/// said — its status, its `retry-after` header, and the API's own message.
enum Reply {
    Ok(serde_json::Value),
    Failed {
        status: u16,
        retry_after: Option<String>,
        detail: String,
    },
}

/// Send one Messages request. Transport failures are errors; an HTTP error
/// status is a [`Reply::Failed`] the caller may retry.
fn send(api_key: &str, body: &serde_json::Value) -> Result<Reply, String> {
    // ureq 3 turns a non-2xx status into `Error::StatusCode` without the body;
    // the API puts its explanation there, so read the status off the response
    // ourselves instead.
    let mut response = ureq::post(API_URL)
        .config()
        .http_status_as_error(false)
        .build()
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .send_json(body)
        .map_err(|e| format!("request failed: {e}"))?;

    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(String::from);
    let raw = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("bad response: {e}"))?;
    let parsed = serde_json::from_str::<serde_json::Value>(&raw);

    if !status.is_success() {
        let detail = parsed
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or_default();
        return Ok(Reply::Failed {
            status: status.as_u16(),
            retry_after,
            detail,
        });
    }
    parsed
        .map(Reply::Ok)
        .map_err(|e| format!("bad response: {e}"))
}

/// Whether, and when, to retry a failed request.
#[derive(Debug, PartialEq, Eq)]
enum Retry {
    /// Retry after this long.
    After(Duration),
    /// Retryable, but the server asked for a longer wait than
    /// [`MAX_RETRY_WAIT`]; give up and pass the wait on.
    TooLong(Duration),
    /// Not worth retrying: a client error, or the retries are used up.
    No,
}

/// Decide on a retry for the `attempt`th failure (0-based). Rate limits
/// (429), overload (529) and transient server errors are retried; the
/// `retry-after` header (whole seconds) sets the wait when present, else the
/// wait doubles from one second.
fn retry_delay(status: u16, retry_after: Option<&str>, attempt: u32) -> Retry {
    if !matches!(status, 429 | 500 | 502 | 503 | 504 | 529) || attempt >= MAX_RETRIES {
        return Retry::No;
    }
    let delay = retry_after
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or_else(|| Duration::from_secs(1 << attempt), Duration::from_secs);
    if delay > MAX_RETRY_WAIT {
        Retry::TooLong(delay)
    } else {
        Retry::After(delay)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{MAX_RETRIES, Retry, retry_delay};

    #[test]
    fn rate_limits_wait_as_long_as_the_server_asks() {
        assert_eq!(
            retry_delay(429, Some("3"), 0),
            Retry::After(Duration::from_secs(3))
        );
        assert_eq!(
            retry_delay(529, Some(" 1 "), 1),
            Retry::After(Duration::from_secs(1))
        );
    }

    #[test]
    fn without_retry_after_the_wait_doubles() {
        assert_eq!(
            retry_delay(503, None, 0),
            Retry::After(Duration::from_secs(1))
        );
        assert_eq!(
            retry_delay(503, None, 1),
            Retry::After(Duration::from_secs(2))
        );
        // An HTTP-date (or junk) is not whole seconds; fall back the same way.
        assert_eq!(
            retry_delay(429, Some("Wed, 21 Oct 2026 07:28:00 GMT"), 0),
            Retry::After(Duration::from_secs(1))
        );
    }

    #[test]
    fn a_long_wait_is_handed_back_instead_of_slept() {
        assert_eq!(
            retry_delay(429, Some("30"), 0),
            Retry::TooLong(Duration::from_secs(30))
        );
    }

    #[test]
    fn client_errors_and_spent_retries_are_final() {
        for status in [400, 401, 403, 404, 413] {
            assert_eq!(retry_delay(status, Some("1"), 0), Retry::No, "{status}");
        }
        assert_eq!(retry_delay(429, Some("1"), MAX_RETRIES), Retry::No);
    }
}

use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use super::log as agent_log;

/// Maximum number of concurrent LLM API requests across all tasks.
/// All calls to `chat_completion` acquire a permit before sending a request,
/// ensuring that pipeline bursts, user-triggered tasks, and repair calls
/// collectively stay within this limit.
const MAX_CONCURRENT_REQUESTS: usize = 10;

static REQUEST_SEMAPHORE: std::sync::LazyLock<Semaphore> =
  std::sync::LazyLock::new(|| Semaphore::new(MAX_CONCURRENT_REQUESTS));

const LARGE_MODEL: &str = "z-ai/glm-5.1"; // "anthropic/claude-opus-4.6";
const MEDIUM_MODEL: &str = "z-ai/glm-5.1";
const SMALL_MODEL: &str = "google/gemma-4-31b-it";

pub const SYSTEM_MESSAGE_CODE: &str = "\
You are an expert smart contract security auditor. \
Analyze the provided Solidity smart contract code with extreme rigor and precision. \
Adherence to documentation is critical — verify every assertion, invariant, and contract requirement based on the documentation. \
Consider how each piece of code interacts with the broader documented system, \
paying close attention to access control, reentrancy, integer arithmetic, \
external calls, state mutations, and protocol-level logic. \
Do not gloss over details — if something is subtle or ambiguous, call it out. \
Be thorough but concise in your response.";

pub const SYSTEM_MESSAGE_DOCUMENTATION: &str = "\
You are an expert smart contract technical lead. \
Analyze the provided smart contract project documentation with insight into project goals and requirements. \
Consider the system's architecture, access control, and protocol-level logic. \
Consider goals for the customer-facing interface and logic, \
as well as goals for api interactions, admin operations, and security guarantees. \
Think not only about the happy path feature set, \
but also about how the project is designed to handle the edge cases, error conditions, and deliberate attacks. \
Provide precise, structured analysis when requested. Only respond with \
structured JSON, do not include any additional text or explanations in your response.";

pub enum TaskSize {
  Large,
  Medium,
  Small,
}

impl TaskSize {
  pub fn author_id(&self) -> i64 {
    match self {
      TaskSize::Large => 4,
      TaskSize::Medium => 3,
      TaskSize::Small => 2,
    }
  }

  pub fn model_name(&self) -> &'static str {
    match self {
      TaskSize::Large => LARGE_MODEL,
      TaskSize::Medium => MEDIUM_MODEL,
      TaskSize::Small => SMALL_MODEL,
    }
  }
}

#[derive(Debug, Serialize)]
struct ChatMessage {
  role: &'static str,
  content: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
  choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
  message: ResponseMessage,
  finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
  content: Option<String>,
  reasoning: Option<String>,
}

/// Send a prompt to the OpenRouter API with the audit system message prepended.
///
/// When `AGENT_DRY_RUN` is set to a file path (set via
/// `export AGENT_DRY_RUN=./agent_prompt.txt`, unset via `unset AGENT_DRY_RUN`),
/// the full prompt (system + user messages) is written to that file and the
/// function returns an error indicating dry run mode, without making any
/// API call.
pub async fn chat_completion(
  task_size: TaskSize,
  system_message: &str,
  prompt: &str,
  dry_run_label: Option<&str>,
  json_mode: bool,
) -> Result<String, String> {
  if let Ok(base_path) = std::env::var("AGENT_DRY_RUN") {
    let path = match dry_run_label {
      Some(label) => {
        let p = std::path::Path::new(&base_path);
        let stem = p.file_stem().unwrap_or_default().to_string_lossy();
        let ext = p
          .extension()
          .map(|e| format!(".{}", e.to_string_lossy()))
          .unwrap_or_default();
        let parent = p.parent().unwrap_or(std::path::Path::new(""));
        parent
          .join(format!("{}_{}{}", stem, label, ext))
          .to_string_lossy()
          .to_string()
      }
      None => base_path,
    };
    let model = match task_size {
      TaskSize::Large => LARGE_MODEL,
      TaskSize::Medium => MEDIUM_MODEL,
      TaskSize::Small => SMALL_MODEL,
    };
    let output = format!(
      "=== DRY RUN ===\nModel: {}\n\n\
       === SYSTEM MESSAGE ===\n{}\n\n\
       === USER PROMPT ===\n{}",
      model, system_message, prompt
    );
    std::fs::write(&path, &output)
      .map_err(|e| format!("Failed to write dry run to '{}': {}", path, e))?;
    println!("Dry run prompt written to: {}", path);
    // Return empty JSON array so the pipeline continues with empty results,
    // allowing all passes to fire and write their prompt files.
    return Ok("[]".to_string());
  }

  let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| {
    "OPENROUTER_API_KEY environment variable not set".to_string()
  })?;

  let model = match task_size {
    TaskSize::Large => LARGE_MODEL,
    TaskSize::Medium => MEDIUM_MODEL,
    TaskSize::Small => SMALL_MODEL,
  };

  let messages = vec![
    ChatMessage {
      role: "system",
      content: system_message.to_string(),
    },
    ChatMessage {
      role: "user",
      content: prompt.to_string(),
    },
  ];

  let mut body = serde_json::json!({
    "model": model,
    "messages": messages,
  });
  if json_mode {
    body["response_format"] = serde_json::json!({ "type": "json_object" });
  }

  let task_label = dry_run_label.unwrap_or("unknown");

  let _permit = REQUEST_SEMAPHORE
    .acquire()
    .await
    .map_err(|_| "Request semaphore closed".to_string())?;

  let client = reqwest::Client::new();

  let mut attempts = 0u32;
  let max_retries = 5;
  let raw_body = loop {
    attempts += 1;

    let response = match client
      .post("https://openrouter.ai/api/v1/chat/completions")
      .header("Authorization", format!("Bearer {}", api_key))
      .json(&body)
      .send()
      .await
    {
      Ok(resp) => resp,
      Err(e) => {
        if attempts > max_retries {
          agent_log::error(
            "network_error",
            Some(model),
            Some(task_label),
            &format!("Request failed after {} attempts: {}", attempts, e),
            Some(prompt),
            None,
          );
          return Err(format!("Request failed: {}", e));
        }
        let wait_secs = 2u64.pow(attempts - 1).min(60);
        agent_log::warn(
          "network_error",
          Some(model),
          Some(task_label),
          &format!(
            "Request failed: {} — retrying in {}s (attempt {}/{})",
            e,
            wait_secs,
            attempts,
            max_retries + 1
          ),
          None,
          None,
        );
        tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
        continue;
      }
    };

    let status = response.status();

    if status.is_success() {
      let resp_body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {}", e))?;

      // Some providers return HTTP 200 with an error object in the body
      // (e.g. 504 timeouts wrapped as `{"error": {"message": "...", "code": 504}}`).
      if let Ok(value) = serde_json::from_str::<serde_json::Value>(&resp_body) {
        if let Some(err_obj) = value.get("error") {
          let err_code =
            err_obj.get("code").and_then(|c| c.as_u64()).unwrap_or(0);
          let err_msg = err_obj
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown");
          let is_retryable = err_code == 429
            || err_code == 502
            || err_code == 503
            || err_code == 504;

          if is_retryable && attempts <= max_retries {
            let wait_secs = 2u64.pow(attempts - 1).min(60);
            agent_log::warn(
              "api_error_in_200",
              Some(model),
              Some(task_label),
              &format!(
                "Error {} in 200 body: {} — retrying in {}s (attempt {}/{})",
                err_code,
                err_msg,
                wait_secs,
                attempts,
                max_retries + 1
              ),
              None,
              None,
            );
            tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
            continue;
          }

          agent_log::error(
            "api_error_in_200",
            Some(model),
            Some(task_label),
            &format!("Error {} in 200 body: {}", err_code, err_msg),
            Some(prompt),
            Some(&resp_body),
          );
          return Err(format!(
            "API error in 200 response ({}): {}",
            err_code, err_msg
          ));
        }
      }

      break resp_body;
    }

    let is_retryable = status.as_u16() == 429
      || status.as_u16() == 502
      || status.as_u16() == 503
      || status.as_u16() == 504;

    if !is_retryable || attempts > max_retries {
      let resp_body = response.text().await.unwrap_or_default();
      agent_log::error(
        "api_error",
        Some(model),
        Some(task_label),
        &format!("HTTP {} (attempt {}/{})", status, attempts, max_retries + 1),
        Some(prompt),
        Some(&resp_body),
      );
      return Err(format!("API error ({}): {}", status, resp_body));
    }

    // Parse Retry-After header (seconds), fall back to exponential backoff.
    let retry_after = response
      .headers()
      .get("retry-after")
      .and_then(|v| v.to_str().ok())
      .and_then(|v| v.parse::<u64>().ok());

    let wait_secs = retry_after.unwrap_or_else(|| 2u64.pow(attempts - 1));
    let wait_secs = wait_secs.min(60);

    agent_log::warn(
      "rate_limited",
      Some(model),
      Some(task_label),
      &format!(
        "HTTP {} — retrying in {}s (attempt {}/{})",
        status,
        wait_secs,
        attempts,
        max_retries + 1
      ),
      None,
      None,
    );

    tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
  };

  let parsed: ChatCompletionResponse = serde_json::from_str(&raw_body)
    .map_err(|e| {
      agent_log::error(
        "response_parse_error",
        Some(model),
        Some(task_label),
        &format!("Failed to deserialize API response: {}", e),
        Some(prompt),
        Some(&raw_body),
      );
      format!("Failed to parse response: {}", e)
    })?;

  let choice = parsed.choices.into_iter().next().ok_or_else(|| {
    agent_log::error(
      "no_choices",
      Some(model),
      Some(task_label),
      "API response contained no choices",
      Some(prompt),
      Some(&raw_body),
    );
    "No choices in response".to_string()
  })?;

  // Detect truncated responses — the model hit its output token limit
  // and never produced a complete answer.
  if choice.finish_reason.as_deref() == Some("length") {
    agent_log::error(
      "response_truncated",
      Some(model),
      Some(task_label),
      "Model hit output token limit (finish_reason=length)",
      Some(prompt),
      Some(&raw_body),
    );
    return Err("Response truncated: model hit output token limit".to_string());
  }

  let message = choice.message;

  // Prefer content; fall back to reasoning only if it looks like it
  // contains actual JSON data (not just chain-of-thought or newlines).
  if let Some(content) = message.content {
    if !content.trim().is_empty() {
      agent_log::prompt(model, task_label, prompt, &content);
      return Ok(content);
    }
  }

  if let Some(reasoning) = message.reasoning {
    let trimmed = reasoning.trim();
    // Only use reasoning if it starts with a JSON-like character,
    // not chain-of-thought text or garbage.
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
      agent_log::prompt(model, task_label, prompt, &reasoning);
      return Ok(reasoning);
    }
    agent_log::error(
      "reasoning_not_json",
      Some(model),
      Some(task_label),
      "content=null and reasoning does not contain JSON",
      Some(prompt),
      Some(trimmed),
    );
  }

  agent_log::error(
    "no_usable_content",
    Some(model),
    Some(task_label),
    "API response has no usable content",
    Some(prompt),
    Some(&raw_body),
  );
  Err("API response has no usable content".to_string())
}

/// Extract the last valid JSON array from an LLM response.
///
/// Scans backwards for `]`, finds the matching `[`, and attempts to parse.
/// Falls back to stripping markdown fences. If all local parsing fails,
/// sends the malformed response to the small model for repair.
pub async fn extract_json<T: serde::de::DeserializeOwned>(
  response: &str,
  label: &str,
  expected_format: &str,
  original_prompt: &str,
) -> Result<T, String> {
  if let Some(parsed) = try_parse_json::<T>(response) {
    return Ok(parsed);
  }

  let trimmed = response.trim();

  // If the response contains no JSON-like characters at all, the model
  // produced no structured output. Return the empty form of the expected
  // type (e.g. `[]` for arrays) rather than asking the repair model to
  // hallucinate data.
  if !trimmed.contains('[') && !trimmed.contains('{') {
    agent_log::warn(
      "no_json_in_response",
      None,
      Some(label),
      "Response contains no JSON, returning empty default",
      Some(original_prompt),
      Some(trimmed),
    );
    return serde_json::from_str::<T>("[]")
      .or_else(|_| serde_json::from_str::<T>("{}"))
      .map_err(|e| {
        format!("Failed to parse {} (no JSON in response): {}", label, e)
      });
  }

  // If the response contains valid JSON but it doesn't match the expected
  // type, try deterministic shape coercions before giving up. This avoids
  // wasting an API call on repair when the data is correct but the
  // structure is wrong.
  let json_source = serde_json::from_str::<serde_json::Value>(trimmed)
    .ok()
    .map(|v| (v, trimmed))
    .or_else(|| {
      extract_first_json_candidate(trimmed).and_then(|c| {
        serde_json::from_str::<serde_json::Value>(c)
          .ok()
          .map(|v| (v, c))
      })
    });

  if let Some((value, source)) = json_source {
    // Try coercions to recover the expected type from a wrong shape.
    if let Some(result) = try_coerce_json::<T>(&value) {
      agent_log::prompt("json_shape_coerced", label, original_prompt, source);
      return Ok(result);
    }

    agent_log::error(
      "json_wrong_shape",
      None,
      Some(label),
      "Response is valid JSON but does not match expected type and could not be coerced",
      Some(original_prompt),
      Some(source),
    );
    return Err(format!(
      "Failed to parse {}: response is valid JSON but wrong shape",
      label
    ));
  }

  // Local parsing failed and response is malformed — ask the small model to fix it.
  agent_log::warn(
    "json_parse_failed",
    None,
    Some(label),
    "Local JSON parse failed, attempting LLM repair",
    Some(original_prompt),
    Some(response),
  );
  let repair_prompt = format!(
    "The following LLM response should be valid JSON matching the expected \
    format below, but is malformed or contains extra text. Your job is to \
    extract and return ONLY the valid JSON, preserving all data.\n\n\
    CRITICAL RULES:\n\
    - Return ONLY the JSON value — no explanation, reasoning, or markdown.\n\
    - The returned JSON MUST match the structure shown in the expected \
    format exactly (same type: array, object, etc.).\n\
    - Do NOT modify the data values — only fix structural issues like \
    trailing commas, missing brackets, duplicated values, or extra text \
    before/after the JSON.\n\n\
    Expected format:\n{}\n\n\
    Malformed response:\n{}",
    expected_format, response
  );
  let repaired = chat_completion(
    TaskSize::Small,
    "You are a JSON repair utility. Return ONLY valid JSON, nothing else.",
    &repair_prompt,
    None,
    true,
  )
  .await
  .map_err(|e| {
    agent_log::error(
      "repair_failed",
      Some(SMALL_MODEL),
      Some(label),
      &format!("LLM repair request failed: {}", e),
      Some(original_prompt),
      Some(response),
    );
    format!("Failed to parse {} (repair also failed: {})", label, e)
  })?;

  try_parse_json::<T>(&repaired).ok_or_else(|| {
    agent_log::error(
      "repair_invalid_json",
      Some(SMALL_MODEL),
      Some(label),
      "LLM repair did not produce valid JSON",
      Some(original_prompt),
      Some(&repaired),
    );
    format!("Failed to parse {} (repair produced invalid JSON)", label)
  })
}

/// Try to parse a JSON value from a response string using bracket matching
/// and markdown fence stripping. Returns `None` if all attempts fail.
fn try_parse_json<T: serde::de::DeserializeOwned>(response: &str) -> Option<T> {
  // Try bracket-matching from the end of the string for both arrays and objects.
  for (open, close_ch) in [(b'[', b']'), (b'{', b'}')] {
    let bytes = response.as_bytes();
    let mut end = bytes.len();
    while end > 0 {
      let close = match bytes[..end].iter().rposition(|&b| b == close_ch) {
        Some(i) => i,
        None => break,
      };
      let mut depth = 0i32;
      let mut pos = close;
      loop {
        match bytes[pos] {
          b if b == close_ch => depth += 1,
          b if b == open => {
            depth -= 1;
            if depth == 0 {
              break;
            }
          }
          _ => {}
        }
        if pos == 0 {
          break;
        }
        pos -= 1;
      }
      if depth == 0 {
        let candidate = &response[pos..=close];
        if let Ok(parsed) = serde_json::from_str::<T>(candidate) {
          return Some(parsed);
        }
      }
      end = close;
    }
  }

  // Fallback: strip markdown fences and try the whole thing.
  let stripped = response
    .trim()
    .strip_prefix("```json")
    .or_else(|| response.trim().strip_prefix("```"))
    .unwrap_or(response.trim());
  let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();

  serde_json::from_str::<T>(stripped).ok()
}

/// Extract the last bracket-matched JSON candidate as a string slice,
/// without attempting to deserialize. Used to check if valid JSON exists
/// but doesn't match the expected type.
fn extract_first_json_candidate(response: &str) -> Option<&str> {
  for (open, close_ch) in [(b'[', b']'), (b'{', b'}')] {
    let bytes = response.as_bytes();
    let mut end = bytes.len();
    while end > 0 {
      let close = match bytes[..end].iter().rposition(|&b| b == close_ch) {
        Some(i) => i,
        None => break,
      };
      let mut depth = 0i32;
      let mut pos = close;
      loop {
        match bytes[pos] {
          b if b == close_ch => depth += 1,
          b if b == open => {
            depth -= 1;
            if depth == 0 {
              break;
            }
          }
          _ => {}
        }
        if pos == 0 {
          break;
        }
        pos -= 1;
      }
      if depth == 0 {
        return Some(&response[pos..=close]);
      }
      end = close;
    }
  }
  None
}

/// Try deterministic shape coercions on a valid JSON value to match the
/// expected deserialization target `T`. Returns `None` if no coercion works.
///
/// Coercions attempted:
/// - Object → wrap in array: `{...}` → `[{...}]`
/// - Object → extract keys as string array: `{"K1": ..., "K2": ...}` → `["K1", "K2"]`
/// - Object → extract values as array: `{"k": [...]}` → `[...]` (single-key object wrapping an array)
/// - Scalar/string → wrap in array: `"foo"` → `["foo"]`
fn try_coerce_json<T: serde::de::DeserializeOwned>(
  value: &serde_json::Value,
) -> Option<T> {
  use serde_json::Value;

  match value {
    Value::Object(map) => {
      // Empty object → empty array (model returned `{}` meaning "nothing").
      if map.is_empty() {
        if let Ok(parsed) = serde_json::from_value::<T>(Value::Array(vec![])) {
          return Some(parsed);
        }
      }

      // Object with a single key whose value is an array — unwrap it.
      // e.g. `{"results": [...]}` → `[...]`
      if map.len() == 1 {
        if let Some(inner) = map.values().next() {
          if inner.is_array() {
            if let Ok(parsed) = serde_json::from_value::<T>(inner.clone()) {
              return Some(parsed);
            }
          }
        }
      }

      // Wrap the object in an array: `{...}` → `[{...}]`
      // e.g. `{"declaration_topic": "N-1234", "semantic_text": "..."}` → `[{...}]`
      let wrapped = Value::Array(vec![value.clone()]);
      if let Ok(parsed) = serde_json::from_value::<T>(wrapped) {
        return Some(parsed);
      }

      // Extract values as array (when values are objects, e.g. a map keyed by ID):
      // `{"N1": {"declaration_topic": "N1", ...}, "N2": {...}}` → `[{...}, {...}]`
      if map.values().all(|v| v.is_object()) {
        let values: Vec<Value> = map.values().cloned().collect();
        let values_array = Value::Array(values);
        if let Ok(parsed) = serde_json::from_value::<T>(values_array) {
          return Some(parsed);
        }
      }

      // Extract keys as a string array: `{"K1": v1, "K2": v2}` → `["K1", "K2"]`
      let keys: Vec<Value> =
        map.keys().map(|k| Value::String(k.clone())).collect();
      let key_array = Value::Array(keys);
      if let Ok(parsed) = serde_json::from_value::<T>(key_array) {
        return Some(parsed);
      }

      None
    }
    Value::String(_) | Value::Number(_) | Value::Bool(_) => {
      // Wrap scalar in an array.
      let wrapped = Value::Array(vec![value.clone()]);
      serde_json::from_value::<T>(wrapped).ok()
    }
    _ => None,
  }
}

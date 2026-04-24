use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use super::log as agent_log;
use super::task::TaskError;

/// Maximum number of concurrent LLM API requests across all tasks.
/// All calls to `chat_completion` acquire a permit before sending a request,
/// ensuring that pipeline bursts and user-triggered tasks collectively stay
/// within this limit.
const MAX_CONCURRENT_REQUESTS: usize = 10;

static REQUEST_SEMAPHORE: LazyLock<Semaphore> =
  LazyLock::new(|| Semaphore::new(MAX_CONCURRENT_REQUESTS));

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

/// A JSON Schema used to constrain an LLM response via OpenRouter's
/// `response_format: { type: "json_schema", strict: true }` mode.
///
/// `empty_response` is returned during `AGENT_DRY_RUN` so that downstream
/// parsing stays on the valid-shape path without sending a real request.
pub struct JsonSchema {
  pub name: &'static str,
  pub schema: serde_json::Value,
  pub empty_response: &'static str,
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
  response_schema: Option<&JsonSchema>,
) -> Result<String, TaskError> {
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
    std::fs::write(&path, &output)?;
    tracing::debug!("Dry run prompt written to: {}", path);
    // Return the schema's empty-shape sentinel so downstream parsing sees a
    // well-formed response and the pipeline continues with empty results,
    // letting every pass fire and write its prompt file. Callers that do not
    // pass a schema (raw text output) get an empty string.
    return Ok(
      response_schema
        .map(|s| s.empty_response.to_string())
        .unwrap_or_default(),
    );
  }

  let api_key = std::env::var("OPENROUTER_API_KEY")
    .map_err(|_| TaskError::MissingEnv("OPENROUTER_API_KEY".to_string()))?;

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
    "provider": { "require_parameters": true },
  });
  if let Some(schema) = response_schema {
    body["response_format"] = serde_json::json!({
      "type": "json_schema",
      "json_schema": {
        "name": schema.name,
        "strict": true,
        "schema": schema.schema,
      }
    });
  }

  let task_label = dry_run_label.unwrap_or("unknown");

  let _permit = REQUEST_SEMAPHORE
    .acquire()
    .await
    .map_err(|_| TaskError::Other("Request semaphore closed".to_string()))?;

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
          return Err(TaskError::HttpError(e));
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
      let resp_body = response.text().await?;

      // Some providers return HTTP 200 with an error object in the body
      // (e.g. 504 timeouts wrapped as `{"error": {"message": "...", "code": 504}}`).
      if let Ok(value) = serde_json::from_str::<serde_json::Value>(&resp_body)
        && let Some(err_obj) = value.get("error")
      {
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
        return Err(TaskError::Other(format!(
          "API error in 200 response ({}): {}",
          err_code, err_msg
        )));
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
      return Err(TaskError::Other(format!(
        "API error ({}): {}",
        status, resp_body
      )));
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
      TaskError::JsonParse {
        label: "API response".to_string(),
        source: e,
      }
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
    TaskError::MissingField("choices")
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
    return Err(TaskError::Other(
      "Response truncated: model hit output token limit".to_string(),
    ));
  }

  let message = choice.message;

  // Prefer content; fall back to reasoning only if it looks like it
  // contains actual JSON data (not just chain-of-thought or newlines).
  if let Some(content) = message.content
    && !content.trim().is_empty()
  {
    agent_log::prompt(model, task_label, prompt, &content);
    return Ok(content);
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
  Err(TaskError::MissingField("content"))
}

/// Deserialize a schema-constrained LLM response into `T`.
///
/// Responses are constrained server-side by a `json_schema` response format,
/// so the payload is guaranteed to be a well-formed JSON object matching the
/// schema. A parse failure here means a model silently ignored the schema or
/// the schema and target type have drifted apart — fail loudly in either case.
pub fn parse_response<T: serde::de::DeserializeOwned>(
  response: &str,
  label: &str,
  prompt: &str,
) -> Result<T, TaskError> {
  serde_json::from_str(response).map_err(|e| {
    agent_log::error(
      "response_parse_error",
      None,
      Some(label),
      &format!("Failed to deserialize schema-constrained response: {}", e),
      Some(prompt),
      Some(response),
    );
    TaskError::JsonParse {
      label: label.to_string(),
      source: e,
    }
  })
}

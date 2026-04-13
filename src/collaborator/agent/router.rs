use serde::{Deserialize, Serialize};

const LARGE_MODEL: &str = "anthropic/claude-opus-4.6";
const SMALL_MODEL: &str = "z-ai/glm-5";

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
  Small,
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
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
  content: String,
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

  let body = serde_json::json!({
    "model": model,
    "messages": messages,
  });

  let client = reqwest::Client::new();
  let response = client
    .post("https://openrouter.ai/api/v1/chat/completions")
    .header("Authorization", format!("Bearer {}", api_key))
    .json(&body)
    .send()
    .await
    .map_err(|e| format!("Request failed: {}", e))?;

  if !response.status().is_success() {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    return Err(format!("API error ({}): {}", status, body));
  }

  let parsed: ChatCompletionResponse = response
    .json()
    .await
    .map_err(|e| format!("Failed to parse response: {}", e))?;

  parsed
    .choices
    .into_iter()
    .next()
    .map(|c| c.message.content)
    .ok_or_else(|| "No choices in response".to_string())
}

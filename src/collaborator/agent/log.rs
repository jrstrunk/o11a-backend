//! Persistent JSONL logs for agent task events.
//!
//! Two log files:
//! - `data/agent.log` — errors and warnings with response previews and prompts
//! - `data/agent_prompts.log` — every successful prompt/response pair
//!
//! Both are append-only JSONL (one JSON object per line).

use std::fs::OpenOptions;
use std::io::Write;
use std::time::SystemTime;

use serde::Serialize;

const ERROR_LOG_PATH: &str = "data/agent_errors.log";
const PROMPT_LOG_PATH: &str = "data/agent_prompts.log";

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
  Info,
  Warn,
  Error,
}

#[derive(Serialize)]
struct ErrorLogEntry<'a> {
  timestamp: String,
  level: Level,
  event: &'a str,
  model: Option<&'a str>,
  task: Option<&'a str>,
  message: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  prompt_preview: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  response_preview: Option<&'a str>,
}

#[derive(Serialize)]
struct PromptLogEntry<'a> {
  timestamp: String,
  model: &'a str,
  task: &'a str,
  prompt: &'a str,
  response: &'a str,
}

fn truncate(s: &str, max: usize) -> &str {
  let trimmed = s.trim();
  if trimmed.len() <= max {
    trimmed
  } else {
    &trimmed[..max]
  }
}

fn iso_timestamp() -> String {
  let duration = SystemTime::now()
    .duration_since(SystemTime::UNIX_EPOCH)
    .unwrap_or_default();
  let secs = duration.as_secs();

  let days = secs / 86400;
  let time_secs = secs % 86400;
  let hours = time_secs / 3600;
  let minutes = (time_secs % 3600) / 60;
  let seconds = time_secs % 60;

  let mut y = 1970i64;
  let mut remaining = days as i64;
  loop {
    let year_days =
      if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
    if remaining < year_days {
      break;
    }
    remaining -= year_days;
    y += 1;
  }
  let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
  let month_days = [
    31,
    if leap { 29 } else { 28 },
    31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
  ];
  let mut m = 0usize;
  for &md in &month_days {
    if remaining < md {
      break;
    }
    remaining -= md;
    m += 1;
  }

  format!(
    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
    y,
    m + 1,
    remaining + 1,
    hours,
    minutes,
    seconds
  )
}

fn append_to_file(path: &str, json: &str) {
  if let Ok(mut file) = OpenOptions::new()
    .create(true)
    .append(true)
    .open(path)
  {
    let _ = writeln!(file, "{}", json);
  }
}

/// Log an agent event to the error log file.
pub fn log(
  level: Level,
  event: &str,
  model: Option<&str>,
  task: Option<&str>,
  message: &str,
  prompt: Option<&str>,
  response: Option<&str>,
) {
  let entry = ErrorLogEntry {
    timestamp: iso_timestamp(),
    level,
    event,
    model,
    task,
    message,
    prompt_preview: prompt.map(|p| truncate(p, 2000)),
    response_preview: response.map(|r| truncate(r, 2000)),
  };

  if let Ok(json) = serde_json::to_string(&entry) {
    append_to_file(ERROR_LOG_PATH, &json);
  }
}

/// Log a successful prompt/response pair to the prompts log.
pub fn prompt(
  model: &str,
  task: &str,
  prompt_text: &str,
  response: &str,
) {
  let entry = PromptLogEntry {
    timestamp: iso_timestamp(),
    model,
    task,
    prompt: prompt_text,
    response,
  };

  if let Ok(json) = serde_json::to_string(&entry) {
    append_to_file(PROMPT_LOG_PATH, &json);
  }
}

/// Convenience: log an error.
pub fn error(
  event: &str,
  model: Option<&str>,
  task: Option<&str>,
  message: &str,
  prompt: Option<&str>,
  response: Option<&str>,
) {
  log(Level::Error, event, model, task, message, prompt, response);
}

/// Convenience: log a warning.
pub fn warn(
  event: &str,
  model: Option<&str>,
  task: Option<&str>,
  message: &str,
  prompt: Option<&str>,
  response: Option<&str>,
) {
  log(Level::Warn, event, model, task, message, prompt, response);
}

/// Convenience: log info.
pub fn info(
  event: &str,
  model: Option<&str>,
  task: Option<&str>,
  message: &str,
  prompt: Option<&str>,
  response: Option<&str>,
) {
  log(Level::Info, event, model, task, message, prompt, response);
}

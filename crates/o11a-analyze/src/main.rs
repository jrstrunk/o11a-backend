//! o11a-analyze — the batch analysis binary.
//!
//! Subcommands:
//!   analyze <project_root> <audit_id>
//!     Runs the analysis pipeline end-to-end and writes outputs into
//!     `<project_root>/o11a/`.
//!
//!   dump <project_root> <audit_id> <kind> [<kind> ...]
//!     Writes diagnostic JSON dumps of internal audit-data state to
//!     `<project_root>/o11a/dumps/`. No LLM, no full pipeline. See
//!     `o11a-analyze dump --help`-style usage in `cmd/dump.rs`.
//!
//!   normalize-docs <project_root>
//!     Reads `documents.txt` from the project root, normalizes each
//!     documentation file via LLM, and writes results back in-place.
//!
//! Requires OPENROUTER_API_KEY (or AGENT_DRY_RUN) for normalize-docs.

mod cmd;

use std::process::ExitCode;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

fn init_tracing() {
  // Ensure the agent-log directory exists so JSONL logging in
  // `o11a_core::collaborator::agent::log` doesn't silently drop every
  // write. The server creates this in its main(); the analyzer must do
  // the same.
  let _ = std::fs::create_dir_all("data");

  tracing_subscriber::registry()
    .with(fmt::layer().with_target(false))
    .with(
      EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info")),
    )
    .init();
}

const USAGE: &str = "\
Usage: o11a-analyze <subcommand> [args]

Subcommands:
  analyze <project_root> <audit_id>
      Run the analysis pipeline and write the report and binary artifact.

  dump <project_root> <audit_id> <kind> [<kind> ...]
      Write diagnostic JSON dumps of internal audit-data state to
      <project_root>/o11a/dumps/. No LLM. Kinds: interface-mapping,
      name-index, all (or comma-separated).

  normalize-docs <project_root>
      Normalize documentation files from documents.txt via LLM.
";

#[tokio::main]
async fn main() -> ExitCode {
  init_tracing();

  let args: Vec<String> = std::env::args().collect();
  if args.len() < 2 {
    eprintln!("{USAGE}");
    return ExitCode::from(2);
  }

  match args[1].as_str() {
    "analyze" => cmd::analyze::run(&args[2..]).await,
    "dump" => cmd::dump::run(&args[2..]).await,
    "normalize-docs" => cmd::normalize_docs::run(&args[2..]).await,
    other => {
      eprintln!("Unknown subcommand: {other}\n{USAGE}");
      ExitCode::from(2)
    }
  }
}

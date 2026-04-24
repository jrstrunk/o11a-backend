use std::path::PathBuf;

use o11a_core::collaborator::agent::task::{
  DocumentationFile, normalize_documentation,
};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Standalone binary to normalize documentation files for plain text readability.
///
/// Usage:
///   cargo run --bin normalize_docs -- <project_root>
///
/// Reads `documents.txt` from the project root to discover documentation files,
/// sends each through the LLM normalization task, and writes the result back
/// in place.
///
/// Requires OPENROUTER_API_KEY to be set (or AGENT_DRY_RUN for testing).
#[tokio::main]
async fn main() {
  tracing_subscriber::registry()
    .with(fmt::layer().with_target(false))
    .with(
      EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info")),
    )
    .init();

  let args: Vec<String> = std::env::args().collect();
  if args.len() != 2 {
    eprintln!("Usage: normalize_docs <project_root>");
    std::process::exit(1);
  }

  let project_root = PathBuf::from(&args[1]);
  if !project_root.is_dir() {
    tracing::error!("'{}' is not a directory", project_root.display());
    std::process::exit(1);
  }

  // Parse documents.txt to discover documentation files
  let doc_list_path = project_root.join("documents.txt");
  let doc_list = match std::fs::read_to_string(&doc_list_path) {
    Ok(content) => content,
    Err(e) => {
      tracing::error!("failed to read '{}': {}", doc_list_path.display(), e);
      std::process::exit(1);
    }
  };

  let mut documentation_files = Vec::new();
  for line in doc_list.lines() {
    let line = line.trim();
    if line.is_empty() {
      continue;
    }
    // Strip optional "technical:" prefix
    let relative_path = line
      .strip_prefix("technical:")
      .map(|p| p.trim())
      .unwrap_or(line);

    let absolute_path = project_root.join(relative_path);
    let source_content = match std::fs::read_to_string(&absolute_path) {
      Ok(content) => content,
      Err(e) => {
        tracing::warn!("skipping '{}': {}", absolute_path.display(), e);
        continue;
      }
    };

    tracing::info!("Loaded: {}", relative_path);
    documentation_files.push(DocumentationFile {
      file_path: relative_path.to_string(),
      source_content,
    });
  }

  if documentation_files.is_empty() {
    tracing::error!("No documentation files found");
    std::process::exit(1);
  }

  tracing::info!(
    "Normalizing {} documentation files...",
    documentation_files.len()
  );

  let normalized = match normalize_documentation(&documentation_files).await {
    Ok(result) => result,
    Err(e) => {
      tracing::error!("Normalization failed: {}", e);
      std::process::exit(1);
    }
  };

  // Write normalized files back to disk
  for (relative_path, content) in &normalized.files {
    let absolute_path = project_root.join(relative_path);
    match std::fs::write(&absolute_path, content) {
      Ok(()) => tracing::info!("Wrote: {}", relative_path),
      Err(e) => {
        tracing::error!("failed to write '{}': {}", absolute_path.display(), e);
      }
    }
  }

  tracing::info!("Done. Normalized {} files.", normalized.files.len());
}

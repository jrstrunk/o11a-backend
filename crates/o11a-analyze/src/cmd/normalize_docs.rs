//! `normalize-docs` subcommand — normalizes documentation files via LLM.
//!
//! Reads `documents.txt` from the project root to discover documentation
//! files, sends each through the LLM normalization task, and writes the
//! result back in-place.

use o11a_core::collaborator::agent::task::{
  DocumentationFile, normalize_documentation,
};
use std::path::PathBuf;
use std::process::ExitCode;

pub async fn run(args: &[String]) -> ExitCode {
  if args.len() != 1 {
    eprintln!("Usage: o11a-analyze normalize-docs <project_root>");
    return ExitCode::from(2);
  }

  let project_root = PathBuf::from(&args[0]);
  if !project_root.is_dir() {
    tracing::error!("'{}' is not a directory", project_root.display());
    return ExitCode::FAILURE;
  }

  // Parse documents.txt to discover documentation files
  let doc_list_path = project_root.join("documents.txt");
  let doc_list = match std::fs::read_to_string(&doc_list_path) {
    Ok(content) => content,
    Err(e) => {
      tracing::error!("failed to read '{}': {}", doc_list_path.display(), e);
      return ExitCode::FAILURE;
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
    return ExitCode::FAILURE;
  }

  tracing::info!(
    "Normalizing {} documentation files...",
    documentation_files.len()
  );

  let normalized = match normalize_documentation(&documentation_files).await {
    Ok(result) => result,
    Err(e) => {
      tracing::error!("Normalization failed: {}", e);
      return ExitCode::FAILURE;
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
  ExitCode::SUCCESS
}

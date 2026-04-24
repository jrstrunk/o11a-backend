//! o11a-analyze — the batch analysis binary.
//!
//! Runs the analysis pipeline end-to-end against a Solidity project and
//! writes an `audit.json` report. Intended for CI use and as the producer
//! side of the handoff to `o11a-server`.
//!
//! Usage:
//!   o11a-analyze <project_root> <audit_id> [output_path]
//!
//! If `output_path` is omitted, writes to `<project_root>/audit.json`.

use o11a_core::collaborator::agent::pipeline::{self, PipelineState};
use o11a_core::core::{self, project};
use o11a_core::report;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() -> ExitCode {
  let args: Vec<String> = std::env::args().collect();
  if args.len() < 3 || args.len() > 4 {
    eprintln!(
      "Usage: o11a-analyze <project_root> <audit_id> [output_path]\n\n\
       Runs the analysis pipeline and writes audit.json.\n\
       If output_path is omitted, writes to <project_root>/audit.json."
    );
    return ExitCode::from(2);
  }

  let project_root = PathBuf::from(&args[1]);
  if !project_root.is_dir() {
    eprintln!(
      "Error: project root '{}' is not a directory",
      project_root.display()
    );
    return ExitCode::FAILURE;
  }

  let audit_id = args[2].clone();

  let output_path = args
    .get(3)
    .map(PathBuf::from)
    .unwrap_or_else(|| project_root.join("audit.json"));

  match run(&project_root, &audit_id, &output_path).await {
    Ok(()) => ExitCode::SUCCESS,
    Err(e) => {
      eprintln!("Error: {}", e);
      ExitCode::FAILURE
    }
  }
}

async fn run(
  project_root: &Path,
  audit_id: &str,
  output_path: &Path,
) -> Result<(), String> {
  println!("Loading project from {}", project_root.display());
  let data_context = core::new_data_context();
  let data_context = Arc::new(Mutex::new(data_context));

  project::load_project(project_root, audit_id, &data_context)
    .map_err(|e| format!("Failed to load project: {}", e))?;

  let pipeline_state = PipelineState {
    data_context: data_context.clone(),
  };

  pipeline::run_full_pipeline(&pipeline_state, audit_id)
    .await
    .map_err(|e| format!("Pipeline failed: {}", e))?;

  let generated_at = o11a_core::ids::now_iso8601();

  // Rebuild reverse indexes so the exported state is consistent.
  {
    let mut ctx = data_context
      .lock()
      .map_err(|e| format!("DataContext mutex poisoned: {}", e))?;
    if let Some(audit_data) = ctx.get_audit_mut(audit_id) {
      core::rebuild_feature_context(audit_data);
    }
  }

  // Export the pipeline output as a versioned JSON report.
  let ctx = data_context
    .lock()
    .map_err(|e| format!("DataContext mutex poisoned: {}", e))?;
  let audit_data = ctx.get_audit(audit_id).ok_or_else(|| {
    format!("Audit '{}' not present after pipeline run", audit_id)
  })?;

  let report = report::build_report(audit_id, audit_data, generated_at);
  let json = serde_json::to_string_pretty(&report)
    .map_err(|e| format!("Failed to serialize report: {}", e))?;

  if let Some(parent) = output_path.parent() {
    if !parent.as_os_str().is_empty() {
      std::fs::create_dir_all(parent).map_err(|e| {
        format!(
          "Failed to create output directory {}: {}",
          parent.display(),
          e
        )
      })?;
    }
  }

  std::fs::write(output_path, json)
    .map_err(|e| format!("Failed to write {}: {}", output_path.display(), e))?;

  println!("Wrote report to {}", output_path.display());
  Ok(())
}

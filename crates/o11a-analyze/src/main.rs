//! o11a-analyze — the batch analysis binary.
//!
//! Runs the analysis pipeline end-to-end against a Solidity project and
//! writes two outputs into `<project_root>/o11a/`:
//!   - `audit.json` — the canonical pipeline report (human-readable JSON).
//!   - `audit.analysis.bin` — a bincode-encoded snapshot of the analyzed
//!     `AuditData` (ASTs, topic metadata, etc.), consumed by `o11a-server`
//!     so it can serve code views without re-running the analyzer.
//!
//! Usage:
//!   o11a-analyze <project_root> <audit_id>
//!
//! Both output files are written atomically (tmp + rename). The output
//! directory is created if it does not exist.

use o11a_analyze::analysis;
use o11a_core::analysis_artifact::{
  self, ARTIFACT_SCHEMA_VERSION, AnalysisArtifact,
};
use o11a_core::collaborator::agent::pipeline::{self, PipelineState};
use o11a_core::domain;
use o11a_core::report;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const OUTPUT_DIR_NAME: &str = "o11a";
const REPORT_FILE_NAME: &str = "audit.json";
const ARTIFACT_FILE_NAME: &str = "audit.analysis.bin";

#[tokio::main]
async fn main() -> ExitCode {
  tracing_subscriber::registry()
    .with(fmt::layer().with_target(false))
    .with(
      EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info")),
    )
    .init();

  let args: Vec<String> = std::env::args().collect();
  if args.len() != 3 {
    eprintln!(
      "Usage: o11a-analyze <project_root> <audit_id>\n\n\
       Runs the analysis pipeline and writes the report and binary\n\
       artifact into <project_root>/{}/ (creating the directory if\n\
       necessary).",
      OUTPUT_DIR_NAME
    );
    return ExitCode::from(2);
  }

  let project_root = PathBuf::from(&args[1]);
  if !project_root.is_dir() {
    tracing::error!(
      "project root '{}' is not a directory",
      project_root.display()
    );
    return ExitCode::FAILURE;
  }

  let audit_id = args[2].clone();

  match run(&project_root, &audit_id).await {
    Ok(()) => ExitCode::SUCCESS,
    Err(e) => {
      tracing::error!("{}", e);
      ExitCode::FAILURE
    }
  }
}

async fn run(project_root: &Path, audit_id: &str) -> Result<(), String> {
  tracing::info!("Loading project from {}", project_root.display());
  let data_context = domain::new_data_context();
  let data_context = Arc::new(Mutex::new(data_context));

  analysis::run_analysis(project_root, audit_id, &data_context)
    .map_err(|e| e.to_string())?;

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
      domain::rebuild_feature_context(audit_data);
    }
  }

  let output_dir = project_root.join(OUTPUT_DIR_NAME);
  std::fs::create_dir_all(&output_dir).map_err(|e| {
    format!(
      "Failed to create output directory {}: {}",
      output_dir.display(),
      e
    )
  })?;

  let ctx = data_context
    .lock()
    .map_err(|e| format!("DataContext mutex poisoned: {}", e))?;
  let audit_data = ctx.get_audit(audit_id).ok_or_else(|| {
    format!("Audit '{}' not present after pipeline run", audit_id)
  })?;

  let report = report::build_report(audit_id, audit_data, generated_at.clone());
  let report_path = output_dir.join(REPORT_FILE_NAME);
  write_json_atomic(&report_path, &report)?;
  tracing::info!("Wrote report to {}", report_path.display());

  let artifact = AnalysisArtifact {
    schema_version: ARTIFACT_SCHEMA_VERSION,
    generator: report::GENERATOR_NAME.to_string(),
    generator_version: report::GENERATOR_VERSION.to_string(),
    generated_at,
    audit_id: audit_id.to_string(),
    payload: analysis_artifact::snapshot_from_audit_data(audit_data),
  };
  let artifact_path = output_dir.join(ARTIFACT_FILE_NAME);
  analysis_artifact::write_artifact(&artifact_path, &artifact)
    .map_err(|e| format!("Failed to write analysis artifact: {}", e))?;
  tracing::info!("Wrote artifact to {}", artifact_path.display());

  Ok(())
}

/// Serialize `value` as pretty JSON to `path` atomically (tmp + rename).
fn write_json_atomic<T: serde::Serialize>(
  path: &Path,
  value: &T,
) -> Result<(), String> {
  let json = serde_json::to_string_pretty(value)
    .map_err(|e| format!("Failed to serialize report: {}", e))?;

  let tmp_path = match path.file_name() {
    Some(name) => {
      let mut tmp_name = name.to_os_string();
      tmp_name.push(".tmp");
      path.with_file_name(tmp_name)
    }
    None => {
      return Err(format!("report path {} has no file name", path.display()));
    }
  };

  std::fs::write(&tmp_path, json)
    .map_err(|e| format!("Failed to write {}: {}", tmp_path.display(), e))?;
  std::fs::rename(&tmp_path, path).map_err(|e| {
    format!(
      "Failed to rename {} to {}: {}",
      tmp_path.display(),
      path.display(),
      e
    )
  })?;
  Ok(())
}

//! `analyze` subcommand — runs the full analysis pipeline.

use o11a_analyze::analysis::{self, AnalysisError};
use o11a_core::analysis_artifact::{
  self, ARTIFACT_SCHEMA_VERSION, AnalysisArtifact, ArtifactError,
};
use o11a_core::collaborator::agent::pipeline::{
  self, PipelineError, PipelineState,
};
use o11a_core::collaborator::agent::semantic_linking::{
  self, SemanticLinkingConfig,
};
use o11a_core::domain;
use o11a_core::report;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

/// Errors produced by the analyze subcommand.
#[derive(Debug, thiserror::Error)]
enum RunError {
  #[error("analysis failed: {0}")]
  Analysis(#[from] AnalysisError),
  #[error("pipeline failed: {0}")]
  Pipeline(#[from] PipelineError),
  #[error("failed to write analysis artifact: {0}")]
  Artifact(#[from] ArtifactError),
  #[error("I/O error: {0}")]
  Io(#[from] std::io::Error),
  #[error("failed to serialize report: {0}")]
  ReportSerialization(#[from] serde_json::Error),
  #[error("DataContext mutex poisoned: {0}")]
  LockPoisoned(String),
  #[error("audit '{0}' not present after pipeline run")]
  AuditMissing(String),
  #[error("report path {0} has no file name")]
  ReportPathInvalid(PathBuf),
}

const OUTPUT_DIR_NAME: &str = "o11a";
const REPORT_FILE_NAME: &str = "audit.json";
const ARTIFACT_FILE_NAME: &str = "audit.analysis.bin";

pub async fn run(args: &[String]) -> ExitCode {
  // Parse the semantic-linking flags first; they're optional and may appear
  // anywhere in the argument list. Remaining args are the positional
  // <project_root> <audit_id>.
  let (semantic_linking_cfg, positional) =
    match o11a_core::collaborator::agent::semantic_linking::parse_cli(args) {
      Ok(v) => v,
      Err(e) => {
        eprintln!("{e}");
        return ExitCode::from(2);
      }
    };

  if positional.len() != 2 {
    eprintln!(
      "Usage: o11a-analyze analyze [flags] <project_root> <audit_id>\n\n\
       Runs the analysis pipeline and writes the report and binary\n\
       artifact into <project_root>/{}/ (creating the directory if\n\
       necessary).\n\n\
       Flags:\n\
         --semantic-linking-mechanical-trace\n\
             Run only the mechanical halves of step 1 + step 3 (no LLM,\n\
             no synthesis steps), write a pretty-printed JSON trace of\n\
             every section's resolved / unresolved inline-code references\n\
             and derived contract / member candidates to\n\
             <project_root>/{}/mechanical-trace.json, then exit. Used to\n\
             validate the deterministic name resolver.",
      OUTPUT_DIR_NAME, OUTPUT_DIR_NAME
    );
    return ExitCode::from(2);
  }

  let project_root = PathBuf::from(&positional[0]);
  if !project_root.is_dir() {
    tracing::error!(
      "project root '{}' is not a directory",
      project_root.display()
    );
    return ExitCode::FAILURE;
  }

  let audit_id = positional[1].clone();

  tracing::info!(
    "Semantic linking config: mechanical_trace={}",
    semantic_linking_cfg.mechanical_trace,
  );

  match do_run(&project_root, &audit_id, semantic_linking_cfg).await {
    Ok(()) => ExitCode::SUCCESS,
    Err(e) => {
      tracing::error!("{}", e);
      ExitCode::FAILURE
    }
  }
}

async fn do_run(
  project_root: &Path,
  audit_id: &str,
  semantic_linking: SemanticLinkingConfig,
) -> Result<(), RunError> {
  tracing::info!("Loading project from {}", project_root.display());
  let data_context = domain::new_data_context();
  let data_context = Arc::new(Mutex::new(data_context));

  analysis::run_analysis(project_root, audit_id, &data_context)?;

  let output_dir = project_root.join(OUTPUT_DIR_NAME);

  // Mechanical-trace mode: run the mechanical halves of step 1 + step 3
  // only, write trace, exit. No LLM calls, no audit.json, no
  // audit.analysis.bin — diagnostic-only mode for the deterministic name
  // resolver.
  if semantic_linking.mechanical_trace {
    std::fs::create_dir_all(&output_dir)?;
    let path = semantic_linking::trace::run_mechanical_trace(
      data_context.clone(),
      audit_id,
      &output_dir,
    )?;
    tracing::info!(
      "mechanical-trace complete; exiting before pipeline. Trace: {}",
      path.display()
    );
    return Ok(());
  }

  let pipeline_state = PipelineState {
    data_context: data_context.clone(),
    semantic_linking,
    output_dir: Some(output_dir.clone()),
  };

  pipeline::run_full_pipeline(&pipeline_state, audit_id).await?;

  let generated_at = o11a_core::ids::now_iso8601();

  // Rebuild reverse indexes so the exported state is consistent.
  {
    let mut ctx = data_context
      .lock()
      .map_err(|e| RunError::LockPoisoned(e.to_string()))?;
    if let Some(audit_data) = ctx.get_audit_mut(audit_id) {
      domain::rebuild_feature_context(audit_data);
    }
  }

  std::fs::create_dir_all(&output_dir)?;

  let ctx = data_context
    .lock()
    .map_err(|e| RunError::LockPoisoned(e.to_string()))?;
  let audit_data = ctx
    .get_audit(audit_id)
    .ok_or_else(|| RunError::AuditMissing(audit_id.to_string()))?;

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
  analysis_artifact::write_artifact(&artifact_path, &artifact)?;
  tracing::info!("Wrote artifact to {}", artifact_path.display());

  Ok(())
}

/// Serialize `value` as pretty JSON to `path` atomically (tmp + rename).
fn write_json_atomic<T: serde::Serialize>(
  path: &Path,
  value: &T,
) -> Result<(), RunError> {
  let json = serde_json::to_string_pretty(value)?;

  let tmp_path = match path.file_name() {
    Some(name) => {
      let mut tmp_name = name.to_os_string();
      tmp_name.push(".tmp");
      path.with_file_name(tmp_name)
    }
    None => {
      return Err(RunError::ReportPathInvalid(path.to_path_buf()));
    }
  };

  std::fs::write(&tmp_path, json)?;
  std::fs::rename(&tmp_path, path)?;
  Ok(())
}

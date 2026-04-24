//! End-to-end entry point for the o11a-analyze binary's analysis
//! workflow: parse the Solidity project's solc output, run the Solidity
//! analyzer, then run the documentation analyzer. Populates the shared
//! `DataContext` in place.

use crate::documentation;
use crate::solidity;
use o11a_core::domain;
use o11a_core::domain::DataContext;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Errors produced by `run_analysis`.
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
  #[error("failed to load project configuration: {0}")]
  Config(String),
  #[error("DataContext mutex poisoned: {0}")]
  LockPoisoned(String),
  #[error("audit '{0}' already exists")]
  AuditExists(String),
  #[error("failed to analyze Solidity project: {0}")]
  Solidity(String),
  #[error("failed to analyze documentation files: {0}")]
  Documentation(String),
}

pub fn run_analysis(
  project_root: &Path,
  audit_id: &str,
  data_context: &Arc<Mutex<DataContext>>,
) -> Result<(), AnalysisError> {
  // Load in-scope files from scope.txt
  let in_scope_files = domain::load_in_scope_files(project_root)
    .map_err(|e| AnalysisError::Config(format!("scope.txt: {}", e)))?;

  let audit_name = domain::load_audit_name(project_root)
    .map_err(|e| AnalysisError::Config(format!("name.txt: {}", e)))?;

  // Load ordered document file list from documents.txt
  let document_files = domain::load_document_files(project_root)
    .map_err(|e| AnalysisError::Config(format!("documents.txt: {}", e)))?;

  // Load security notes from security.md (optional)
  let security_notes = domain::load_security_notes(project_root)
    .map_err(|e| AnalysisError::Config(format!("security.md: {}", e)))?;

  // Create the audit if it doesn't exist
  {
    let mut ctx = data_context
      .lock()
      .map_err(|e| AnalysisError::LockPoisoned(e.to_string()))?;
    if !ctx.create_audit(
      audit_id.to_string(),
      audit_name,
      in_scope_files,
      security_notes,
    ) {
      return Err(AnalysisError::AuditExists(audit_id.to_string()));
    }
  }

  tracing::info!("Analyzing Solidity project at: {}", project_root.display());

  // Analyze Solidity project and populate AuditData
  {
    let mut ctx = data_context
      .lock()
      .map_err(|e| AnalysisError::LockPoisoned(e.to_string()))?;
    solidity::analyzer::analyze(project_root, audit_id, &mut ctx)
      .map_err(AnalysisError::Solidity)?;
  }

  tracing::info!("Analyzing documentation files...");

  // Analyze documentation and augment AuditData
  {
    let mut ctx = data_context
      .lock()
      .map_err(|e| AnalysisError::LockPoisoned(e.to_string()))?;
    documentation::analyzer::analyze(
      project_root,
      audit_id,
      &mut ctx,
      &document_files,
    )
    .map_err(AnalysisError::Documentation)?;
  }

  tracing::info!("Done loading audit: {}", audit_id);

  Ok(())
}

//! End-to-end entry point for the o11a-analyze binary's analysis
//! workflow: parse the Solidity project's solc output, run the Solidity
//! analyzer, then run the documentation analyzer. Populates the shared
//! `DataContext` in place.

use crate::documentation;
use crate::solidity;
use o11a_core::core;
use o11a_core::core::DataContext;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub fn run_analysis(
  project_root: &Path,
  audit_id: &str,
  data_context: &Arc<Mutex<DataContext>>,
) -> Result<(), String> {
  // Load in-scope files from scope.txt
  let in_scope_files =
    core::load_in_scope_files(project_root).map_err(|e| {
      format!("Failed to load in-scope files from scope.txt: {}", e)
    })?;

  let audit_name = core::load_audit_name(project_root)
    .map_err(|e| format!("Failed to load audit name from name.txt: {}", e))?;

  // Load ordered document file list from documents.txt
  let document_files =
    core::load_document_files(project_root).map_err(|e| {
      format!("Failed to load document files from documents.txt: {}", e)
    })?;

  // Load security notes from security.md (optional)
  let security_notes = core::load_security_notes(project_root)
    .map_err(|e| format!("Failed to load security notes: {}", e))?;

  // Create the audit if it doesn't exist
  {
    let mut ctx = data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned while creating audit: {}", e))?;
    if !ctx.create_audit(
      audit_id.to_string(),
      audit_name,
      in_scope_files,
      security_notes,
    ) {
      return Err(format!("Audit '{}' already exists", audit_id));
    }
  }

  println!("Analyzing Solidity project at: {}", project_root.display());

  // Analyze Solidity project and populate AuditData
  {
    let mut ctx = data_context.lock().map_err(|e| {
      format!("Mutex poisoned while analyzing Solidity project: {}", e)
    })?;
    solidity::analyzer::analyze(project_root, audit_id, &mut ctx)
      .map_err(|e| format!("Failed to analyze Solidity project: {}", e))?;
  }

  println!("Analyzing documentation files...");

  // Analyze documentation and augment AuditData
  {
    let mut ctx = data_context.lock().map_err(|e| {
      format!("Mutex poisoned while analyzing documentation: {}", e)
    })?;
    documentation::analyzer::analyze(
      project_root,
      audit_id,
      &mut ctx,
      &document_files,
    )
    .map_err(|e| format!("Failed to analyze documentation files: {}", e))?;
  }

  println!("Done loading audit: {}", audit_id);

  Ok(())
}

//! `dump` subcommand — runs the analysis (no LLM, no pipeline) and writes
//! one or more diagnostic JSON dumps of internal audit-data state to
//! `<project_root>/o11a/dumps/<kind>.json`. Used to manually inspect the
//! parsed-data slices that drive resolution edge cases.

use o11a_analyze::analysis::{self, AnalysisError};
use o11a_core::audit_dump::{self, DumpKind};
use o11a_core::domain;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error)]
enum RunError {
  #[error("analysis failed: {0}")]
  Analysis(#[from] AnalysisError),
  #[error("I/O error: {0}")]
  Io(#[from] std::io::Error),
  #[error("DataContext mutex poisoned: {0}")]
  LockPoisoned(String),
  #[error("audit '{0}' not present after analysis")]
  AuditMissing(String),
}

const OUTPUT_DIR_NAME: &str = "o11a";
const DUMPS_DIR_NAME: &str = "dumps";

const USAGE: &str = "\
Usage: o11a-analyze dump <project_root> <audit_id> <kind> [<kind> ...]

Writes diagnostic JSON dumps of internal audit-data state to
<project_root>/o11a/dumps/<kind>.json. Each `<kind>` is a curated view
into parsed audit data — useful for inspecting resolution edge cases
without running the full pipeline.

Kinds may be passed as separate args or comma-separated. Pass `all` to
emit every kind in one invocation. Names accept both kebab-case
(interface-mapping) and snake_case (interface_mapping).

Available kinds:
  interface-mapping   Every transitive (proxy → target) topic mapping —
                      typically interface stubs to their implementations.
                      Lets you spot interface methods that should map to
                      an implementation but don't.

  name-index          Every simple identifier name and the full set of
                      candidate topics that share it, with resolution
                      status. Lets you spot names that fail to resolve
                      due to ambiguity (e.g. one StateVariable plus
                      several function parameters).

  all                 Shorthand for every kind above.

Examples:
  o11a-analyze dump ./project myaudit interface-mapping
  o11a-analyze dump ./project myaudit interface-mapping name-index
  o11a-analyze dump ./project myaudit all
";

pub async fn run(args: &[String]) -> ExitCode {
  if args.len() < 3 {
    eprintln!("{USAGE}");
    return ExitCode::from(2);
  }

  let project_root = PathBuf::from(&args[0]);
  if !project_root.is_dir() {
    eprintln!(
      "project root '{}' is not a directory",
      project_root.display()
    );
    return ExitCode::FAILURE;
  }
  let audit_id = args[1].clone();
  let kind_args = &args[2..];

  let kinds = match audit_dump::parse_kinds(kind_args) {
    Ok(k) if k.is_empty() => {
      eprintln!("dump: no kinds requested\n\n{USAGE}");
      return ExitCode::from(2);
    }
    Ok(k) => k,
    Err(e) => {
      eprintln!("{e}\n\n{USAGE}");
      return ExitCode::from(2);
    }
  };

  match do_run(&project_root, &audit_id, &kinds).await {
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
  kinds: &[DumpKind],
) -> Result<(), RunError> {
  tracing::info!("Loading project from {}", project_root.display());
  let data_context = domain::new_data_context();
  let data_context = Arc::new(Mutex::new(data_context));

  analysis::run_analysis(project_root, audit_id, &data_context)?;

  let dumps_dir = project_root.join(OUTPUT_DIR_NAME).join(DUMPS_DIR_NAME);
  std::fs::create_dir_all(&dumps_dir)?;

  let ctx = data_context
    .lock()
    .map_err(|e| RunError::LockPoisoned(e.to_string()))?;
  let audit_data = ctx
    .get_audit(audit_id)
    .ok_or_else(|| RunError::AuditMissing(audit_id.to_string()))?;

  for kind in kinds {
    let path = audit_dump::dump_to_file(*kind, audit_data, &dumps_dir)?;
    tracing::info!("dump: wrote {} -> {}", kind.file_name(), path.display());
  }

  Ok(())
}

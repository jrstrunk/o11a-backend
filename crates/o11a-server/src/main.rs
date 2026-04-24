mod api;
mod websocket;

use o11a_core::analysis_artifact::{self, ArtifactError};
use o11a_core::collaborator::db as collab_db;
use o11a_core::domain;
use o11a_core::db;
use o11a_core::report::{self, AuditReport};
use o11a_core::state::AppState;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tower_http::trace::TraceLayer;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::api::routes;

const OUTPUT_DIR_NAME: &str = "o11a";
const REPORT_FILE_NAME: &str = "audit.json";
const ARTIFACT_FILE_NAME: &str = "audit.analysis.bin";

#[tokio::main]
async fn main() {
  tracing_subscriber::registry()
    .with(fmt::layer().with_target(false))
    .with(
      EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info")),
    )
    .init();

  // Ensure data directory exists
  let data_dir = Path::new("data");
  if !data_dir.exists() {
    std::fs::create_dir_all(data_dir).expect("Failed to create data directory");
  }

  // Database setup
  let database_url = std::env::var("DATABASE_URL")
    .unwrap_or_else(|_| "sqlite://data/o11a.db".to_string());

  tracing::info!(database_url = %database_url, "Connecting to database");

  let pool = db::create_pool(&database_url)
    .await
    .expect("Failed to create database pool");

  tracing::info!("Initializing schema...");
  db::init_schema(&pool)
    .await
    .expect("Failed to initialize database schema");

  tracing::info!("Creating DataContext...");

  // Create empty DataContext
  let data_context = domain::new_data_context();
  let data_context = Arc::new(Mutex::new(data_context));

  tracing::info!("DataContext created successfully");

  // Get project root and audit ID from environment variables
  let project_root = std::env::var("PROJECT_ROOT")
    .unwrap_or_else(|_| "/home/john/audits/nudgexyz".to_string());
  let project_root = Path::new(&project_root);

  let audit_id =
    std::env::var("AUDIT_ID").unwrap_or_else(|_| "nudgexyz".to_string());

  // Load the binary artifact first. It contains the analyzed AuditData
  // snapshot (ASTs, topic metadata, source contexts, etc.). The server no
  // longer reads the project's source tree; all of that state comes from
  // the artifact produced by `o11a-analyze`.
  let artifact_path = audit_artifact_path(project_root);
  tracing::info!(
    audit_id = %audit_id,
    path = %artifact_path.display(),
    "Loading analysis artifact"
  );
  let artifact = match analysis_artifact::read_artifact(&artifact_path) {
    Ok(artifact) => artifact,
    Err(ArtifactError::VersionMismatch { found, expected }) => {
      tracing::error!(
        "error: analysis artifact at {} has schema version {} but this \
         server supports {}. Re-run `o11a-analyze <project_root> <audit_id>` \
         to regenerate the artifact.",
        artifact_path.display(),
        found,
        expected
      );
      std::process::exit(1);
    }
    Err(e) => {
      tracing::error!(
        "error: could not read analysis artifact at {}: {}. Run \
         `o11a-analyze <project_root> <audit_id>` first.",
        artifact_path.display(),
        e
      );
      std::process::exit(1);
    }
  };

  if artifact.audit_id != audit_id {
    tracing::error!(
      "error: artifact audit id mismatch: expected '{}', artifact is for '{}'",
      audit_id,
      artifact.audit_id
    );
    std::process::exit(1);
  }

  // Load the audit report (pipeline output, e.g. features/requirements).
  let report_path = audit_report_path(project_root);
  let report = match load_report(&report_path) {
    Ok(Some(report)) => report,
    Ok(None) => {
      tracing::error!(
        "error: no audit report found at {}. Run `o11a-analyze` first to produce audit.json.",
        report_path.display()
      );
      std::process::exit(1);
    }
    Err(e) => {
      tracing::error!(
        "error: could not read audit report at {}: {}",
        report_path.display(),
        e
      );
      std::process::exit(1);
    }
  };

  // Create the audit entry and hydrate it from the artifact snapshot, then
  // apply the pipeline report on top.
  {
    let mut ctx = data_context.lock().unwrap();
    ctx.create_audit(
      audit_id.clone(),
      artifact.payload.audit_name.clone(),
      artifact.payload.in_scope_files.clone(),
      artifact.payload.security_notes.clone(),
    );
    let audit_data = ctx
      .get_audit_mut(&audit_id)
      .expect("audit entry must exist after create_audit");
    analysis_artifact::apply_snapshot(audit_data, artifact.payload);

    if let Err(e) = report::apply_report(&audit_id, audit_data, &report) {
      tracing::error!("error: failed to apply audit report: {}", e);
      std::process::exit(1);
    }
    tracing::info!(
      "Applied audit report from {} (schema v{}, generated {})",
      report_path.display(),
      report.schema_version,
      report.generated_at
    );
  }

  // Hydrate user-created entities from the collaboration DB. This runs after
  // `apply_report` has reseeded the ID counters, so user IDs and pipeline IDs
  // share the same `i32` space without collision. Loading happens outside the
  // mutex so we never hold a `std::sync::Mutex` guard across an `.await`.
  let user_entities_snapshot =
    collab_db::load_user_entities_snapshot(&pool, &audit_id)
      .await
      .unwrap_or_else(|e| {
        tracing::error!("error: failed to load user-created entities: {}", e);
        std::process::exit(1);
      });
  {
    let mut ctx = data_context.lock().unwrap();
    let audit_data = ctx.get_audit_mut(&audit_id).unwrap_or_else(|| {
      tracing::error!(
        "error: audit '{}' not initialized before user-entity load",
        audit_id
      );
      std::process::exit(1);
    });
    collab_db::apply_user_entities_snapshot(audit_data, user_entities_snapshot);
  }

  // Load and parse all comments (collaboration state, unaffected by the
  // JSON handoff). Same split-load/sync-apply pattern as above.
  tracing::info!("Loading comments...");
  let comments = collab_db::load_visible_comments(&pool)
    .await
    .unwrap_or_else(|e| {
      tracing::warn!("Warning: Failed to load comments: {}", e);
      Vec::new()
    });
  let comment_count = {
    let mut ctx = data_context.lock().unwrap();
    collab_db::ingest_loaded_comments(&mut ctx, &comments)
  };

  tracing::info!("Loaded {} comments", comment_count);

  // Rebuild reverse indexes after pipeline data is in place.
  {
    let mut ctx = data_context.lock().unwrap();
    for audit_data in ctx.audits.values_mut() {
      domain::rebuild_feature_context(audit_data);
    }
  }

  // Extract DataContext from Arc<Mutex<>> for AppState
  let data_context = Arc::try_unwrap(data_context)
    .ok()
    .expect("Multiple references to data_context")
    .into_inner()
    .expect("Mutex poisoned");

  // Create app state with all components
  let state = AppState::new(pool, data_context);

  // Build routers: core serves the JSON + pipeline endpoints; the frontend
  // crate serves the HTML-returning endpoints. They run in the same process
  // and share the same `AppState` (the frontend wraps it with rendering
  // caches). Merging keeps a single listener socket.
  let frontend_state =
    o11a_web_backend::state::FrontendState::new(state.clone());
  let app = routes::create_router(state)
    .merge(o11a_web_backend::routes::create_router(frontend_state))
    .layer(TraceLayer::new_for_http());

  // Start server
  let addr = "0.0.0.0:3058";
  tracing::info!("Server running on http://{}", addr);

  let listener = tokio::net::TcpListener::bind(addr)
    .await
    .expect("Failed to bind to address");

  axum::serve(listener, app)
    .await
    .expect("Failed to start server");
}

/// Resolve the path to the audit report JSON. Overridable via `AUDIT_REPORT`.
/// Defaults to `<project_root>/o11a/audit.json`.
fn audit_report_path(project_root: &Path) -> PathBuf {
  if let Ok(explicit) = std::env::var("AUDIT_REPORT") {
    return PathBuf::from(explicit);
  }
  project_root.join(OUTPUT_DIR_NAME).join(REPORT_FILE_NAME)
}

/// Resolve the path to the binary analysis artifact. Overridable via
/// `AUDIT_ARTIFACT`. Defaults to `<project_root>/o11a/audit.analysis.bin`.
fn audit_artifact_path(project_root: &Path) -> PathBuf {
  if let Ok(explicit) = std::env::var("AUDIT_ARTIFACT") {
    return PathBuf::from(explicit);
  }
  project_root.join(OUTPUT_DIR_NAME).join(ARTIFACT_FILE_NAME)
}

/// Load the audit report from `path`. Returns `Ok(None)` when the file
/// is absent so the caller can produce its own error message.
fn load_report(path: &Path) -> Result<Option<AuditReport>, String> {
  match std::fs::read_to_string(path) {
    Ok(body) => {
      let report: AuditReport = serde_json::from_str(&body)
        .map_err(|e| format!("parse error: {}", e))?;
      Ok(Some(report))
    }
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(e) => Err(e.to_string()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn router_construction_does_not_panic() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
      .connect("sqlite::memory:")
      .await
      .expect("in-memory sqlite pool");
    let state = AppState::new(pool, domain::new_data_context());
    let _ = routes::create_router(state);
  }
}

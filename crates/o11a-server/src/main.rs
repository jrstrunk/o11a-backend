mod api;
mod websocket;

use o11a_core::collaborator::db as collab_db;
use o11a_core::core::{self, project};
use o11a_core::db;
use o11a_core::report::{self, AuditReport};
use o11a_core::state::AppState;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::api::routes;

#[tokio::main]
async fn main() {
  // Ensure data directory exists
  let data_dir = Path::new("data");
  if !data_dir.exists() {
    std::fs::create_dir_all(data_dir).expect("Failed to create data directory");
  }

  // Database setup
  let database_url = std::env::var("DATABASE_URL")
    .unwrap_or_else(|_| "sqlite://data/o11a.db".to_string());

  println!("Connecting to database: {}", database_url);

  let pool = db::create_pool(&database_url)
    .await
    .expect("Failed to create database pool");

  println!("Initializing schema...");
  db::init_schema(&pool)
    .await
    .expect("Failed to initialize database schema");

  println!("Creating DataContext...");

  // Create empty DataContext
  let data_context = core::new_data_context();
  let data_context = Arc::new(Mutex::new(data_context));

  println!("DataContext created successfully");

  // Get project root and audit ID from environment variables
  let project_root = std::env::var("PROJECT_ROOT")
    .unwrap_or_else(|_| "/home/john/audits/nudgexyz".to_string());
  let project_root = Path::new(&project_root);

  let audit_id =
    std::env::var("AUDIT_ID").unwrap_or_else(|_| "nudgexyz".to_string());

  println!(
    "Loading audit '{}' from project: {}",
    audit_id,
    project_root.display()
  );

  project::load_project(project_root, &audit_id, &data_context)
    .expect("Unable to load project");

  // Load pipeline output from audit.json. This is the handoff from
  // `o11a-analyze`; without it the server has no pipeline data to serve,
  // so the server refuses to start.
  let report_path = audit_report_path(project_root);
  let report = match load_report(&report_path) {
    Ok(Some(report)) => report,
    Ok(None) => {
      eprintln!(
        "error: no audit report found at {}. Run `o11a-analyze` first to produce audit.json.",
        report_path.display()
      );
      std::process::exit(1);
    }
    Err(e) => {
      eprintln!(
        "error: could not read audit report at {}: {}",
        report_path.display(),
        e
      );
      std::process::exit(1);
    }
  };

  {
    let mut ctx = data_context.lock().unwrap();
    let audit_data = ctx.get_audit_mut(&audit_id).unwrap_or_else(|| {
      eprintln!(
        "error: audit '{}' not initialized after load_project",
        audit_id
      );
      std::process::exit(1);
    });
    if let Err(e) = report::apply_report(&audit_id, audit_data, &report) {
      eprintln!("error: failed to apply audit report: {}", e);
      std::process::exit(1);
    }
    println!(
      "Applied audit report from {} (schema v{}, generated {})",
      report_path.display(),
      report.schema_version,
      report.generated_at
    );
  }

  // Hydrate user-created entities from the collaboration DB. This runs after
  // `apply_report` has reseeded the ID counters, so user IDs and pipeline IDs
  // share the same `i32` space without collision.
  {
    let mut ctx = data_context.lock().unwrap();
    let audit_data = ctx.get_audit_mut(&audit_id).unwrap_or_else(|| {
      eprintln!(
        "error: audit '{}' not initialized before user-entity load",
        audit_id
      );
      std::process::exit(1);
    });
    if let Err(e) =
      collab_db::apply_user_entities(&pool, &audit_id, audit_data).await
    {
      eprintln!("error: failed to load user-created entities: {}", e);
      std::process::exit(1);
    }
  }

  // Load and parse all comments (collaboration state, unaffected by the
  // JSON handoff).
  println!("Loading comments...");
  let comment_count = {
    let mut ctx = data_context.lock().unwrap();
    collab_db::load_and_parse_all_comments(&pool, &mut ctx)
      .await
      .unwrap_or_else(|e| {
        eprintln!("Warning: Failed to load comments: {}", e);
        0
      })
  };

  println!("Loaded {} comments", comment_count);

  // Rebuild reverse indexes after pipeline data is in place.
  {
    let mut ctx = data_context.lock().unwrap();
    for audit_data in ctx.audits.values_mut() {
      core::rebuild_feature_context(audit_data);
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
    .merge(o11a_web_backend::routes::create_router(frontend_state));

  // Start server
  let addr = "0.0.0.0:3058";
  println!("Server running on http://{}", addr);

  let listener = tokio::net::TcpListener::bind(addr)
    .await
    .expect("Failed to bind to address");

  axum::serve(listener, app)
    .await
    .expect("Failed to start server");
}

/// Resolve the path to the audit report JSON. Overridable via `AUDIT_REPORT`.
/// Defaults to `<project_root>/audit.json`.
fn audit_report_path(project_root: &Path) -> PathBuf {
  if let Ok(explicit) = std::env::var("AUDIT_REPORT") {
    return PathBuf::from(explicit);
  }
  project_root.join("audit.json")
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
    let state = AppState::new(pool, core::new_data_context());
    let _ = routes::create_router(state);
  }
}

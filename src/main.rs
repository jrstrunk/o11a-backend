use o11a_backend::api::{AppState, routes};
use o11a_backend::collaborator::db as collab_db;
use o11a_backend::core::{self, project};
use o11a_backend::db;
use std::path::Path;
use std::sync::{Arc, Mutex};

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

  println!("Running migrations...");
  db::run_migrations(&pool)
    .await
    .expect("Failed to run migrations");

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

  // Load and parse all comments
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

  // Load and restore all features
  println!("Loading features...");
  let feature_count = {
    let mut ctx = data_context.lock().unwrap();
    let count = collab_db::load_all_features(&pool, &mut ctx)
      .await
      .unwrap_or_else(|e| {
        eprintln!("Warning: Failed to load features: {}", e);
        0
      });
    // Rebuild feature context now that features are loaded
    for audit_data in ctx.audits.values_mut() {
      core::rebuild_feature_context(audit_data);
    }
    count
  };

  println!("Loaded {} features", feature_count);

  // Extract DataContext from Arc<Mutex<>> for AppState
  let data_context = Arc::try_unwrap(data_context)
    .ok()
    .expect("Multiple references to data_context")
    .into_inner()
    .expect("Mutex poisoned");

  // Create app state with all components
  let state = AppState::new(pool, data_context);

  // Build router
  let app = routes::create_router(state);

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

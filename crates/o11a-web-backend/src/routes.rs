use axum::{
  Router,
  routing::{get, post},
};

use crate::handlers;
use crate::state::FrontendState;

/// Build the router for the HTML-serving endpoints. The returned router has
/// its state baked in and can be merged with the core router in `main.rs`.
pub fn create_router(state: FrontendState) -> Router {
  Router::new()
    .route(
      "/api/v1/audits/:audit_id/source_text/:topic_id",
      get(handlers::get_source_text),
    )
    .route(
      "/api/v1/audits/:audit_id/topic_view/:topic_id",
      get(handlers::get_topic_view),
    )
    .route(
      "/api/v1/audits/:audit_id/conversation/:topic_id",
      get(handlers::get_conversation),
    )
    .route(
      "/api/v1/audits/:audit_id/thread/:topic_id",
      get(handlers::get_thread),
    )
    .route(
      "/api/v1/audits/:audit_id/documentation",
      post(handlers::get_documentation_panel),
    )
    .route(
      "/api/v1/audits/:audit_id/invalidated_invariants",
      get(handlers::get_invalidated_invariants),
    )
    .with_state(state)
}

use axum::{
  Router,
  routing::{delete, get, post},
};
use tower_http::cors::CorsLayer;

use o11a_core::state::AppState;

use crate::api::handlers;
use crate::websocket;

pub fn create_router(state: AppState) -> Router {
  Router::new()
    .route("/health", get(handlers::health_check))
    // Audit management
    .route("/api/v1/audits", get(handlers::list_audits))
    .route("/api/v1/audits", post(handlers::create_audit))
    .route("/api/v1/audits/:audit_id", delete(handlers::delete_audit))
    // Audit-specific data
    .route(
      "/api/v1/audits/:audit_id/data-context",
      get(handlers::get_data_context),
    )
    .route(
      "/api/v1/audits/:audit_id/boundaries",
      get(handlers::get_boundaries),
    )
    .route(
      "/api/v1/audits/:audit_id/in_scope_files",
      get(handlers::get_in_scope_files),
    )
    // Chat endpoints (global for now)
    .route("/api/v1/chats", get(handlers::get_chats))
    .route("/api/v1/chats", post(handlers::create_chat))
    // Implemented
    .route(
      "/api/v1/audits/:audit_id/contracts",
      get(handlers::get_contracts),
    )
    .route(
      "/api/v1/audits/:audit_id/qualified_names",
      get(handlers::get_qualified_names),
    )
    .route(
      "/api/v1/audits/:audit_id/documents",
      get(handlers::get_documents),
    )
    .route(
      "/api/v1/audits/:audit_id/metadata/:topic_id",
      get(handlers::get_metadata),
    )
    .route(
      "/api/v1/audits/:audit_id/delimiter/:topic_id",
      get(handlers::get_delimiter),
    )
    .route(
      "/api/v1/audits/:audit_id/agent_context/:topic_id",
      get(handlers::get_agent_context),
    )
    // ============================================
    // Collaborator comment routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/comments/:comment_type/:status",
      get(handlers::list_comments_by_type_and_status),
    )
    .route(
      "/api/v1/audits/:audit_id/comments",
      post(handlers::create_comment),
    )
    .route(
      "/api/v1/audits/:audit_id/comments/:comment_id/status",
      get(handlers::get_comment_status).put(handlers::update_comment_status),
    )
    // ============================================
    // Feature, requirement, behavior, functional-semantic routes.
    // GET lists pipeline + user entries together; POST creates a user entry.
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/features",
      get(handlers::get_features).post(handlers::create_user_feature),
    )
    .route(
      "/api/v1/audits/:audit_id/features/:topic_id",
      get(handlers::get_feature),
    )
    .route(
      "/api/v1/audits/:audit_id/features/:topic_id/requirements",
      get(handlers::get_feature_requirements),
    )
    .route(
      "/api/v1/audits/:audit_id/requirements",
      get(handlers::get_requirements).post(handlers::create_user_requirement),
    )
    .route(
      "/api/v1/audits/:audit_id/requirements/:topic_id",
      get(handlers::get_requirement),
    )
    .route(
      "/api/v1/audits/:audit_id/behaviors",
      get(handlers::get_behaviors).post(handlers::create_user_behavior),
    )
    .route(
      "/api/v1/audits/:audit_id/behaviors/:topic_id",
      get(handlers::get_behavior),
    )
    .route(
      "/api/v1/audits/:audit_id/functional_semantics",
      get(handlers::get_all_functional_semantics)
        .post(handlers::create_user_functional_semantic),
    )
    .route(
      "/api/v1/audits/:audit_id/functional_semantics/:topic_id",
      get(handlers::get_functional_semantic),
    )
    // ============================================
    // Topic property routes (read-only)
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/topics/:topic_id/semantics",
      get(handlers::get_functional_semantics),
    )
    // WebSocket for the real-time audit event stream
    .route(
      "/api/v1/audits/:audit_id/events/ws",
      get(websocket::event_websocket),
    )
    // ============================================
    // Vote routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/votes/unvoted",
      get(handlers::get_unvoted_comment_ids),
    )
    .route(
      "/api/v1/audits/:audit_id/votes/:comment_id",
      get(handlers::get_vote_summary)
        .post(handlers::cast_vote)
        .delete(handlers::remove_vote),
    )
    .layer(CorsLayer::permissive())
    .with_state(state)
}

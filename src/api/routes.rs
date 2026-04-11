use axum::{
  Router,
  routing::{delete, get, post},
};
use tower_http::cors::CorsLayer;

use crate::api::{AppState, handlers};
use crate::collaborator::websocket;

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
      "/api/v1/audits/:audit_id/source_text/:topic_id",
      get(handlers::get_source_text),
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
    // Feature routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/features",
      get(handlers::get_features).post(handlers::create_feature),
    )
    .route(
      "/api/v1/audits/:audit_id/features/:feature_id/requirements",
      get(handlers::get_feature_requirements),
    )
    .route(
      "/api/v1/audits/:audit_id/analyze",
      post(handlers::analyze),
    )
    .route(
      "/api/v1/audits/:audit_id/pipeline/semantic_links",
      post(handlers::pipeline_semantic_links),
    )
    .route(
      "/api/v1/audits/:audit_id/pipeline/requirements",
      post(handlers::pipeline_requirements),
    )
    .route(
      "/api/v1/audits/:audit_id/pipeline/behaviors",
      post(handlers::pipeline_behaviors),
    )
    .route(
      "/api/v1/audits/:audit_id/pipeline/synthesize",
      post(handlers::pipeline_synthesize),
    )
    .route(
      "/api/v1/audits/:audit_id/requirements/:requirement_id/documentation_topics",
      post(handlers::add_requirement_documentation_topic),
    )
    .route(
      "/api/v1/audits/:audit_id/requirements/:requirement_id/documentation_topics/:topic_id",
      delete(handlers::remove_requirement_documentation_topic),
    )
    // ============================================
    // Documentation routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/documentation",
      post(handlers::get_documentation_panel),
    )
    .route(
      "/api/v1/audits/:audit_id/requirements/topic/:topic_id",
      get(handlers::get_topic_requirements),
    )
    // ============================================
    // Requirement routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/features/:feature_id/requirements",
      post(handlers::create_requirement),
    )
    .route(
      "/api/v1/audits/:audit_id/features/:feature_id/requirements/:requirement_id",
      delete(handlers::delete_requirement),
    )
    .route(
      "/api/v1/audits/:audit_id/requirements/:requirement_id",
      get(handlers::get_requirement),
    )
    // ============================================
    // Source-to-feature link routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/source_feature_links",
      post(handlers::add_source_feature_link),
    )
    .route(
      "/api/v1/audits/:audit_id/source_feature_links/:source_topic/:feature_id",
      delete(handlers::remove_source_feature_link),
    )
    // ============================================
    // Reconciliation routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/reconciliation/:feature_id",
      get(handlers::get_reconciliation),
    )
    // ============================================
    // Subject property routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/subjects/:topic_id/purpose",
      get(handlers::get_functional_purpose).put(handlers::set_functional_purpose),
    )
    .route(
      "/api/v1/audits/:audit_id/subjects/:topic_id/semantics",
      get(handlers::get_functional_semantics).put(handlers::set_functional_semantics),
    )
    // ============================================
    // Impact analysis routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/impact_analysis",
      post(handlers::create_threat_feature_link),
    )
    .route(
      "/api/v1/audits/:audit_id/impact_analysis/:threat_id/:feature_id",
      delete(handlers::delete_threat_feature_link),
    )
    // ============================================
    // Condition routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/conditions",
      post(handlers::create_condition),
    )
    .route(
      "/api/v1/audits/:audit_id/conditions/:subject_topic",
      get(handlers::get_subject_conditions),
    )
    .route(
      "/api/v1/audits/:audit_id/conditions/id/:condition_id",
      delete(handlers::delete_condition),
    )
    // ============================================
    // Behavior routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/behaviors",
      get(handlers::get_behaviors).post(handlers::create_behavior),
    )
    .route(
      "/api/v1/audits/:audit_id/behaviors/:behavior_id",
      get(handlers::get_behavior).delete(handlers::delete_behavior),
    )
    // ============================================
    // Threat routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/threats",
      post(handlers::create_threat),
    )
    .route(
      "/api/v1/audits/:audit_id/threats/:threat_id",
      get(handlers::get_threat).delete(handlers::delete_threat),
    )
    // ============================================
    // Invariant routes
    // ============================================
    .route(
      "/api/v1/audits/:audit_id/threats/:threat_id/invariants",
      get(handlers::get_threat_invariants).post(handlers::create_invariant),
    )
    .route(
      "/api/v1/audits/:audit_id/threats/:threat_id/invariants/:invariant_id",
      delete(handlers::delete_invariant),
    )
    .route(
      "/api/v1/audits/:audit_id/invariants/:invariant_id",
      get(handlers::get_invariant),
    )
    .route(
      "/api/v1/audits/:audit_id/invariants/:invariant_id/source_topics",
      post(handlers::add_invariant_source_topic),
    )
    .route(
      "/api/v1/audits/:audit_id/invariants/:invariant_id/source_topics/:topic_id",
      delete(handlers::remove_invariant_source_topic),
    )
    // WebSocket for real-time comment updates
    .route(
      "/api/v1/audits/:audit_id/comments/ws",
      get(websocket::comment_websocket),
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

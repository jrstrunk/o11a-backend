use axum::{
  Json,
  extract::{Path, State},
  http::StatusCode,
  response::{Html, IntoResponse},
};
use serde::Deserialize;

use crate::state::{CachedTopicView, FrontendState};
use o11a_core::domain::{
  self,
  topic::{self, new_topic},
};
use o11a_core::feature_lookup::features_for_topic;

// ============================================================================
// Source text handler (HTML)
// ============================================================================

/// GET /api/v1/audits/:audit_id/source_text/:topic_id
/// Returns the syntax-highlighted HTML for the source of a topic.
pub async fn get_source_text(
  State(state): State<FrontendState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
  tracing::debug!("GET /api/v1/audits/{}/source_text/{}", audit_id, topic_id);

  let ctx = state.app.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_source_text: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let topic = new_topic(&topic_id);

  let mut source_text_cache = state.source_text_cache.lock().map_err(|e| {
    tracing::warn!("Source text cache poisoned: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let cache = source_text_cache.entry(audit_id.clone()).or_default();

  let source_text =
    crate::topic_view::render_source_text_as_block(&topic, audit_data, cache)
      .ok_or_else(|| {
        tracing::warn!(
          "Topic '{}' not found in audit '{}'",
          topic_id,
          audit_id
        );
        StatusCode::NOT_FOUND
      })?;

  Ok(Html(source_text))
}

// ============================================================================
// Topic view handler (JSON with embedded HTML)
// ============================================================================

/// GET /api/v1/audits/:audit_id/topic_view/:topic_id
/// Returns pre-rendered topic view panels. Static panels (topic, expanded
/// references, breadcrumb, highlight CSS) are cached forever since they are
/// AST-derived. Dynamic panels (comments, mentions) are rendered fresh.
pub async fn get_topic_view(
  State(state): State<FrontendState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<crate::topic_view::TopicViewResponse>, StatusCode> {
  tracing::debug!("GET /api/v1/audits/{}/topic_view/{}", audit_id, topic_id);

  let ctx = state.app.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_topic_view: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let mut source_text_cache = state.source_text_cache.lock().map_err(|e| {
    tracing::warn!("Source text cache poisoned: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let cache = source_text_cache.entry(audit_id.clone()).or_default();

  let prefix =
    crate::topic_view::build_topic_panel_prefix(&topic_id, audit_data, cache);

  let cached = {
    let topic_view_cache = state.topic_view_cache.lock().map_err(|e| {
      tracing::warn!("Topic view cache poisoned: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    topic_view_cache
      .get(&audit_id)
      .and_then(|m| m.get(&topic_id))
      .cloned()
  };

  let response = crate::topic_view::build_topic_view(
    &topic_id,
    audit_data,
    cache,
    cached.as_ref(),
    &prefix,
  )
  .ok_or_else(|| {
    tracing::warn!(
      "Metadata for topic '{}' not found in audit '{}'",
      topic_id,
      audit_id
    );
    StatusCode::NOT_FOUND
  })?;

  if cached.is_none() {
    let static_topic_panel = if prefix.is_empty() {
      response.topic_panel_html.clone()
    } else {
      response.topic_panel_html[prefix.len()..].to_string()
    };

    let mut topic_view_cache = state.topic_view_cache.lock().map_err(|e| {
      tracing::warn!("Topic view cache poisoned: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    topic_view_cache
      .entry(audit_id.clone())
      .or_default()
      .insert(
        topic_id.clone(),
        CachedTopicView {
          topic_panel_html: static_topic_panel,
          expanded_references_panel_html: response
            .expanded_references_panel_html
            .clone(),
          breadcrumb_html: response.breadcrumb_html.clone(),
          highlight_css: response.highlight_css.clone(),
        },
      );
  }

  Ok(Json(response))
}

// ============================================================================
// Conversation handler (JSON with embedded HTML)
// ============================================================================

/// GET /api/v1/audits/:audit_id/conversation/:topic_id
/// Returns the conversation for a topic: direct comments and mentions,
/// each with metadata and rendered thread HTML.
pub async fn get_conversation(
  State(state): State<FrontendState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<crate::topic_view::ConversationResponse>, StatusCode> {
  tracing::debug!("GET /api/v1/audits/{}/conversation/{}", audit_id, topic_id);

  let ctx = state.app.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_conversation: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let mut source_text_cache = state.source_text_cache.lock().map_err(|e| {
    tracing::warn!("Source text cache poisoned: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let cache = source_text_cache.entry(audit_id.clone()).or_default();

  let response =
    crate::topic_view::build_conversation(&topic_id, audit_data, cache)
      .ok_or_else(|| {
        tracing::warn!(
          "Topic '{}' not found in audit '{}'",
          topic_id,
          audit_id
        );
        StatusCode::NOT_FOUND
      })?;

  Ok(Json(response))
}

// ============================================================================
// Thread handler (HTML)
// ============================================================================

/// GET /api/v1/audits/:audit_id/thread/:topic_id
/// Returns thread HTML for a single topic. Used to refetch after invalidation.
pub async fn get_thread(
  State(state): State<FrontendState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
  tracing::debug!("GET /api/v1/audits/{}/thread/{}", audit_id, topic_id);

  let ctx = state.app.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_thread: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let mut source_text_cache = state.source_text_cache.lock().map_err(|e| {
    tracing::warn!("Source text cache poisoned: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let cache = source_text_cache.entry(audit_id.clone()).or_default();

  let html = crate::topic_view::build_thread(&topic_id, audit_data, cache)
    .ok_or_else(|| {
      tracing::warn!("Topic '{}' not found in audit '{}'", topic_id, audit_id);
      StatusCode::NOT_FOUND
    })?;

  Ok(Html(html))
}

// ============================================================================
// Documentation panel handler (HTML)
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct DocumentationPanelRequest {
  pub feature_topics: Vec<String>,
}

/// POST /api/v1/audits/:audit_id/documentation
/// Returns rendered HTML panel of documentation linked to the given topics.
/// Accepts feature (F), requirement (R), or any other topic IDs.
/// - Feature topics: collect documentation from all their requirements
/// - Requirement topics: use their documentation_topics directly
/// - Other topics: look up features via feature_behavior_links, then requirements
pub async fn get_documentation_panel(
  State(state): State<FrontendState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<DocumentationPanelRequest>,
) -> Result<impl IntoResponse, StatusCode> {
  tracing::debug!("POST /api/v1/audits/{}/documentation", audit_id);

  let ctx = state.app.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_documentation_panel: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let mut feature_topics_resolved: Vec<topic::Topic> = Vec::new();
  let mut has_direct_feature_input = false;
  for id in &payload.feature_topics {
    let t = new_topic(id);
    match t {
      topic::Topic::Feature(_) => {
        has_direct_feature_input = true;
        if !feature_topics_resolved.contains(&t) {
          feature_topics_resolved.push(t);
        }
      }
      _ => {
        for ft in features_for_topic(&t, audit_data) {
          if !feature_topics_resolved.contains(&ft) {
            feature_topics_resolved.push(ft);
          }
        }
      }
    }
  }
  let show_features_as_headers = !has_direct_feature_input;

  let mut source_text_cache = state.source_text_cache.lock().map_err(|e| {
    tracing::warn!("Source text cache poisoned: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let cache = source_text_cache.entry(audit_id.clone()).or_default();

  let mut mention_topics: Vec<topic::Topic> = Vec::new();
  let mut related_topics: Vec<topic::Topic> = Vec::new();
  for id in &payload.feature_topics {
    let t = new_topic(id);
    related_topics.push(t);

    if matches!(t, topic::Topic::Requirement(_))
      && let Some(req) = audit_data.requirements.get(&t)
    {
      for dt in &req.documentation_topics {
        if !mention_topics.contains(dt) {
          mention_topics.push(*dt);
        }
      }
    }

    if matches!(t, topic::Topic::FunctionalProperty(_))
      && let Some(domain::TopicMetadata::FunctionalSemanticTopic {
        documentation_topics,
        ..
      }) = audit_data.topic_metadata.get(&t)
    {
      for dt in documentation_topics {
        if !mention_topics.contains(dt) {
          mention_topics.push(*dt);
        }
      }
    }

    if let Some(metadata) = audit_data.topic_metadata.get(&t) {
      let member = match metadata.scope() {
        domain::Scope::Member { member, .. }
        | domain::Scope::ContainingBlock { member, .. } => Some(*member),
        _ => None,
      };
      if let Some(mt) = member
        && !related_topics.contains(&mt)
      {
        related_topics.push(mt);
      }
    }
  }

  for t in &related_topics {
    if let Some(domain::TopicMetadata::NamedTopic { doc_references, .. }) =
      audit_data.topic_metadata.get(t)
    {
      for mt in doc_references {
        if !mention_topics.contains(mt) {
          mention_topics.push(*mt);
        }
      }
    }

    if let Some(sem_topics) = audit_data.declaration_semantics.get(t) {
      for sem_topic in sem_topics {
        if let Some(domain::TopicMetadata::FunctionalSemanticTopic {
          documentation_topics,
          ..
        }) = audit_data.topic_metadata.get(sem_topic)
        {
          for dt in documentation_topics {
            if !mention_topics.contains(dt) {
              mention_topics.push(*dt);
            }
          }
        }
      }
    }
  }

  let html = crate::topic_view::build_documentation_panel(
    &feature_topics_resolved,
    &mention_topics,
    show_features_as_headers,
    audit_data,
    cache,
  );

  Ok(Html(html))
}

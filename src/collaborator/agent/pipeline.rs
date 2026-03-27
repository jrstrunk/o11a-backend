//! Orchestrates agent pipeline steps: building features, threats/invariants,
//! and linking source code members to features.
//!
//! Functions in this module handle the full lifecycle of an agent-generated
//! result: running the LLM task, persisting to the database, and updating
//! in-memory audit data. They use `String` errors so callers (HTTP handlers,
//! background tasks) can map to their own error types.

use sqlx::SqlitePool;

use crate::collaborator::agent::task;
use crate::collaborator::db;
use crate::collaborator::models::AUTHOR_AGENT;
use crate::core::{self, topic, DataContext};

use std::sync::{Arc, Mutex};

/// Shared state needed by pipeline functions — mirrors the relevant fields of
/// `AppState` without depending on the HTTP layer.
pub struct PipelineState {
  pub db: SqlitePool,
  pub data_context: Arc<Mutex<DataContext>>,
}

// ---------------------------------------------------------------------------
// Full-audit pipeline steps (used by the `analyze` endpoint)
// ---------------------------------------------------------------------------

/// Build features and requirements from documentation via LLM.
pub async fn build_features(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  println!("pipeline::build_features for audit {}", audit_id);

  let documentation_files = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned in build_features: {}", e))?;
    let audit_data = ctx
      .get_audit(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
    task::render_documentation_files(audit_data)
  };

  let parsed =
    task::build_features_from_documentation(&documentation_files).await?;

  // Persist to database: clear old features, insert new ones
  db::delete_all_features_for_audit(&state.db, audit_id)
    .await
    .map_err(|e| format!("delete_all_features_for_audit failed: {}", e))?;

  for (feat_topic, feature) in &parsed.features {
    let (name, description) = match parsed.topic_metadata.get(feat_topic) {
      Some(core::TopicMetadata::FeatureTopic {
        name, description, ..
      }) => (name.as_str(), description.as_str()),
      _ => continue,
    };
    let row = db::create_feature(
      &state.db,
      audit_id,
      name,
      description,
      AUTHOR_AGENT,
    )
    .await
    .map_err(|e| format!("create_feature failed: {}", e))?;

    for req_topic in &feature.requirement_topics {
      let req_desc = match parsed.topic_metadata.get(req_topic) {
        Some(core::TopicMetadata::RequirementTopic { description, .. }) => {
          description.as_str()
        }
        _ => continue,
      };
      let req_row = db::create_requirement(&state.db, row.id, req_desc, 0)
        .await
        .map_err(|e| format!("create_requirement failed: {}", e))?;
      if let Some(req) = parsed.requirements.get(req_topic) {
        for dt in &req.documentation_topics {
          let _ = db::add_requirement_documentation_topic(
            &state.db, req_row.id, dt.id(),
          )
          .await;
        }
      }
    }
  }

  // Update in-memory state
  let mut ctx = state
    .data_context
    .lock()
    .map_err(|e| format!("Mutex poisoned in build_features (store): {}", e))?;
  let audit_data = ctx
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

  audit_data.topic_metadata.retain(|_, m| {
    !matches!(
      m,
      core::TopicMetadata::FeatureTopic { .. }
        | core::TopicMetadata::RequirementTopic { .. }
        | core::TopicMetadata::ThreatTopic { .. }
        | core::TopicMetadata::InvariantTopic { .. }
    )
  });

  audit_data.topic_metadata.extend(parsed.topic_metadata);
  audit_data.features = parsed.features;
  audit_data.requirements = parsed.requirements;
  audit_data.threats.clear();
  audit_data.invariants.clear();
  audit_data.source_feature_links.clear();
  core::rebuild_feature_context(audit_data);

  Ok(())
}

// NOTE: build_threats and build_threats_for_feature have been removed.
// Threat generation now happens on-demand per non-pure subject after
// condition evaluation, not as a batch pipeline step per feature.

/// Link source code members to features across all contracts and features.
pub async fn link_source_to_features(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  println!("pipeline::link_source_to_features for audit {}", audit_id);

  let pairs = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned in link_source_to_features: {}", e))?;
    let audit_data = ctx
      .get_audit(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
    let source_text_cache = ctx
      .source_text_cache
      .get(audit_id)
      .cloned()
      .unwrap_or_default();
    task::collect_contract_feature_pairs(audit_data, &source_text_cache)
  };

  if pairs.is_empty() {
    return Ok(());
  }

  let links = task::link_source_to_features(&pairs).await?;
  persist_source_feature_links(state, audit_id, &links).await
}

// ---------------------------------------------------------------------------
// Single-feature pipeline steps (used by reactive triggers)
// ---------------------------------------------------------------------------


/// Link source code members to a single feature.
pub async fn link_source_to_feature(
  state: &PipelineState,
  audit_id: &str,
  feature_topic: &topic::Topic,
) -> Result<(), String> {
  println!(
    "pipeline::link_source_to_feature {} for audit {}",
    feature_topic.id(),
    audit_id
  );

  let pairs = {
    let ctx = state.data_context.lock().map_err(|e| {
      format!("Mutex poisoned in link_source_to_feature: {}", e)
    })?;
    let audit_data = ctx
      .get_audit(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
    let source_text_cache = ctx
      .source_text_cache
      .get(audit_id)
      .cloned()
      .unwrap_or_default();
    task::collect_single_feature_pairs(
      feature_topic,
      audit_data,
      &source_text_cache,
    )
  };

  if pairs.is_empty() {
    return Ok(());
  }

  let links = task::link_source_to_features(&pairs).await?;
  persist_source_feature_links(state, audit_id, &links).await
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Persist source-to-feature links to database and in-memory state.
async fn persist_source_feature_links(
  state: &PipelineState,
  audit_id: &str,
  links: &task::ParsedSourceFeatureLinks,
) -> Result<(), String> {
  for (source_topic, feature_topics) in &links.links {
    for ft in feature_topics {
      let feature_id = match ft.numeric_id() {
        Some(id) => id,
        None => {
          eprintln!("Invalid feature topic: {}", ft.id());
          continue;
        }
      };
      let _ = db::add_source_feature_link(
        &state.db,
        audit_id,
        source_topic.id(),
        feature_id,
      )
      .await;
    }
  }

  let mut ctx = state.data_context.lock().map_err(|e| {
    format!("Mutex poisoned in persist_source_feature_links: {}", e)
  })?;
  let audit_data = ctx
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

  for (source_topic, feature_topics) in &links.links {
    let existing = audit_data
      .source_feature_links
      .entry(source_topic.clone())
      .or_default();
    for ft in feature_topics {
      if !existing.contains(ft) {
        existing.push(ft.clone());
      }
    }
  }

  Ok(())
}


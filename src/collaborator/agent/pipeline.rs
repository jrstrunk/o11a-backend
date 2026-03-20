//! Orchestrates agent pipeline steps: building features, threats/invariants,
//! and linking requirements to source code members.
//!
//! Functions in this module handle the full lifecycle of an agent-generated
//! result: running the LLM task, persisting to the database, and updating
//! in-memory audit data. They use `String` errors so callers (HTTP handlers,
//! background tasks) can map to their own error types.

use std::collections::BTreeMap;

use sqlx::SqlitePool;

use crate::collaborator::agent::{context, task};
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
        for st in &req.source_topics {
          let _ =
            db::add_requirement_source_topic(&state.db, req_row.id, st.id())
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
  core::rebuild_feature_context(audit_data);

  Ok(())
}

/// Build threats and invariants for all features via LLM, including a security
/// coverage review pass when security notes are present.
pub async fn build_threats(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  println!("pipeline::build_threats for audit {}", audit_id);

  let (feature_entries, security_notes): (
    Vec<(topic::Topic, String)>,
    Option<String>,
  ) = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned in build_threats: {}", e))?;
    let audit_data = ctx
      .get_audit(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

    let entries = audit_data
      .features
      .iter()
      .filter_map(|(ft, feature)| {
        let json =
          context::render_feature_to_json(ft, feature, audit_data, None)?;
        Some((ft.clone(), json))
      })
      .collect();

    (entries, audit_data.security_notes.clone())
  };

  if feature_entries.is_empty() {
    return Err("No features found".to_string());
  }

  // Fire one LLM call per feature concurrently
  let security_notes = std::sync::Arc::new(security_notes);
  let mut handles = Vec::new();
  for (i, (ft, json)) in feature_entries.into_iter().enumerate() {
    let threat_start = (i * 1000) as i32;
    let invariant_start = (i * 1000) as i32;
    let notes = security_notes.clone();
    handles.push(tokio::spawn(async move {
      let result = task::build_threats_for_feature(
        &ft,
        &json,
        threat_start,
        invariant_start,
        notes.as_deref(),
      )
      .await;
      (ft, result)
    }));
  }

  let mut all_parsed = Vec::new();
  for handle in handles {
    match handle.await {
      Ok((_, Ok(parsed))) => all_parsed.push(parsed),
      Ok((ft, Err(e))) => {
        eprintln!("build_threats failed for {}: {}", ft.id(), e);
      }
      Err(e) => {
        eprintln!("build_threats task panicked: {}", e);
      }
    }
  }

  // Delete existing threats/invariants
  db::delete_all_threats_for_audit(&state.db, audit_id)
    .await
    .map_err(|e| format!("delete_all_threats_for_audit failed: {}", e))?;

  // Persist and build in-memory state
  let (new_threats, new_invariants, new_topic_metadata, new_feature_threats) =
    persist_parsed_threats(state, &all_parsed).await?;

  // Update in-memory audit data and extract review input
  let review_input = {
    let mut ctx = state
      .data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned in build_threats (store): {}", e))?;
    let audit_data = ctx
      .get_audit_mut(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

    audit_data.topic_metadata.retain(|_, m| {
      !matches!(
        m,
        core::TopicMetadata::ThreatTopic { .. }
          | core::TopicMetadata::InvariantTopic { .. }
      )
    });

    audit_data.topic_metadata.extend(new_topic_metadata);
    audit_data.threats = new_threats;
    audit_data.invariants = new_invariants;

    for feature in audit_data.features.values_mut() {
      feature.threat_topics.clear();
    }
    for (feat_topic, threat_topics) in new_feature_threats {
      if let Some(feature) = audit_data.features.get_mut(&feat_topic) {
        feature.threat_topics = threat_topics;
      }
    }

    core::rebuild_feature_context(audit_data);

    // Extract review input if security notes exist
    if let Some(ref notes) = *security_notes {
      let features_json =
        context::render_all_features_with_threats(audit_data);

      let review_threat_start = audit_data
        .threats
        .keys()
        .filter_map(|t| t.numeric_id())
        .max()
        .unwrap_or(0) as i32;
      let review_inv_start = audit_data
        .invariants
        .keys()
        .filter_map(|t| t.numeric_id())
        .max()
        .unwrap_or(0) as i32;

      Some((
        notes.clone(),
        features_json,
        review_threat_start,
        review_inv_start,
      ))
    } else {
      None
    }
  };

  // Security coverage review pass
  if let Some((notes, features_json, review_threat_start, review_inv_start)) =
    review_input
  {
    let review_results = task::review_security_coverage(
      &notes,
      &features_json,
      review_threat_start,
      review_inv_start,
    )
    .await?;

    if !review_results.is_empty() {
      let (
        review_threats,
        review_invariants,
        review_metadata,
        review_feature_threats,
      ) = persist_parsed_threats(state, &review_results).await?;

      let mut ctx = state.data_context.lock().map_err(|e| {
        format!("Mutex poisoned in build_threats (review store): {}", e)
      })?;
      let audit_data = ctx
        .get_audit_mut(audit_id)
        .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

      audit_data.topic_metadata.extend(review_metadata);
      audit_data.threats.extend(review_threats);
      audit_data.invariants.extend(review_invariants);

      for (feat_topic, threat_topics) in review_feature_threats {
        if let Some(feature) = audit_data.features.get_mut(&feat_topic) {
          feature.threat_topics.extend(threat_topics);
        }
      }

      core::rebuild_feature_context(audit_data);
    }
  }

  Ok(())
}

/// Link all requirements to source code members across all contracts and features.
pub async fn link_requirements(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  println!("pipeline::link_requirements for audit {}", audit_id);

  let pairs = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned in link_requirements: {}", e))?;
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

  let links = task::link_requirements_to_source(&pairs).await?;
  persist_requirement_links(state, audit_id, &links).await
}

// ---------------------------------------------------------------------------
// Single-feature pipeline steps (used by reactive triggers)
// ---------------------------------------------------------------------------

/// Build threats for a single feature (additive, does not delete existing threats).
pub async fn build_threats_for_feature(
  state: &PipelineState,
  audit_id: &str,
  feature_topic: &topic::Topic,
) -> Result<(), String> {
  println!(
    "pipeline::build_threats_for_feature {} for audit {}",
    feature_topic.id(),
    audit_id
  );

  let (feature_json, security_notes, threat_start, invariant_start) = {
    let ctx = state.data_context.lock().map_err(|e| {
      format!("Mutex poisoned in build_threats_for_feature: {}", e)
    })?;
    let audit_data = ctx
      .get_audit(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

    let feature = audit_data
      .features
      .get(feature_topic)
      .ok_or_else(|| format!("Feature not found: {}", feature_topic.id()))?;

    let json =
      context::render_feature_to_json(feature_topic, feature, audit_data, None)
        .ok_or_else(|| {
          format!(
            "Failed to render feature to JSON: {}",
            feature_topic.id()
          )
        })?;

    let threat_start = audit_data
      .threats
      .keys()
      .filter_map(|t| t.numeric_id())
      .max()
      .unwrap_or(0) as i32
      + 1;
    let invariant_start = audit_data
      .invariants
      .keys()
      .filter_map(|t| t.numeric_id())
      .max()
      .unwrap_or(0) as i32
      + 1;

    (
      json,
      audit_data.security_notes.clone(),
      threat_start,
      invariant_start,
    )
  };

  let parsed = task::build_threats_for_feature(
    feature_topic,
    &feature_json,
    threat_start,
    invariant_start,
    security_notes.as_deref(),
  )
  .await?;

  // Persist threats and invariants
  let feature_id = feature_topic.numeric_id().ok_or_else(|| {
    format!("Invalid feature topic: {}", feature_topic.id())
  })?;

  let mut new_threats = BTreeMap::new();
  let mut new_invariants = BTreeMap::new();
  let mut new_metadata = BTreeMap::new();
  let mut new_threat_topics = Vec::new();

  for (_feat_topic, threat_topics) in &parsed.feature_threat_topics {
    for tt in threat_topics {
      let (description, severity) = match parsed.topic_metadata.get(tt) {
        Some(core::TopicMetadata::ThreatTopic {
          description,
          severity,
          ..
        }) => (description.clone(), *severity),
        _ => continue,
      };

      let threat_row = db::create_threat(
        &state.db,
        feature_id,
        &description,
        AUTHOR_AGENT,
        severity.as_str(),
      )
      .await
      .map_err(|e| format!("create_threat failed: {}", e))?;

      let real_threat_topic =
        topic::new_attack_vector_topic(threat_row.id as i32);

      let mut real_invariant_topics = Vec::new();
      if let Some(threat) = parsed.threats.get(tt) {
        for inv_topic in &threat.invariant_topics {
          let inv_desc = match parsed.topic_metadata.get(inv_topic) {
            Some(core::TopicMetadata::InvariantTopic {
              description, ..
            }) => description.clone(),
            _ => continue,
          };

          let inv_row = db::create_invariant(
            &state.db,
            threat_row.id,
            &inv_desc,
            AUTHOR_AGENT,
            severity.as_str(),
          )
          .await
          .map_err(|e| format!("create_invariant failed: {}", e))?;

          let real_inv_topic = topic::new_invariant_topic(inv_row.id as i32);

          new_metadata.insert(
            real_inv_topic.clone(),
            core::TopicMetadata::InvariantTopic {
              topic: real_inv_topic.clone(),
              description: inv_desc,
              threat_topic: real_threat_topic.clone(),
              author_id: AUTHOR_AGENT,
              created_at: inv_row.created_at,
              severity,
            },
          );
          new_invariants.insert(
            real_inv_topic.clone(),
            core::Invariant {
              source_topics: Vec::new(),
            },
          );
          real_invariant_topics.push(real_inv_topic);
        }
      }

      new_metadata.insert(
        real_threat_topic.clone(),
        core::TopicMetadata::ThreatTopic {
          topic: real_threat_topic.clone(),
          description,
          feature_topic: feature_topic.clone(),
          author_id: AUTHOR_AGENT,
          created_at: threat_row.created_at,
          severity,
        },
      );
      new_threats.insert(
        real_threat_topic.clone(),
        core::Threat {
          invariant_topics: real_invariant_topics,
        },
      );
      new_threat_topics.push(real_threat_topic);
    }
  }

  // Merge into in-memory state (additive)
  let mut ctx = state.data_context.lock().map_err(|e| {
    format!(
      "Mutex poisoned in build_threats_for_feature (store): {}",
      e
    )
  })?;
  let audit_data = ctx
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

  audit_data.topic_metadata.extend(new_metadata);
  audit_data.threats.extend(new_threats);
  audit_data.invariants.extend(new_invariants);

  if let Some(feature) = audit_data.features.get_mut(feature_topic) {
    feature.threat_topics.extend(new_threat_topics);
  }

  core::rebuild_feature_context(audit_data);

  Ok(())
}

/// Link requirements to source for a single feature, optionally filtered to a
/// single requirement.
pub async fn link_feature_requirements(
  state: &PipelineState,
  audit_id: &str,
  feature_topic: &topic::Topic,
  requirement_filter: Option<&topic::Topic>,
) -> Result<(), String> {
  println!(
    "pipeline::link_feature_requirements {} for audit {}",
    feature_topic.id(),
    audit_id
  );

  let pairs = {
    let ctx = state.data_context.lock().map_err(|e| {
      format!("Mutex poisoned in link_feature_requirements: {}", e)
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
      requirement_filter,
    )
  };

  if pairs.is_empty() {
    return Ok(());
  }

  let links = task::link_requirements_to_source(&pairs).await?;
  persist_requirement_links(state, audit_id, &links).await
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Persist requirement-to-source links to database and in-memory state.
async fn persist_requirement_links(
  state: &PipelineState,
  audit_id: &str,
  links: &task::ParsedRequirementLinks,
) -> Result<(), String> {
  for (req_topic, source_topics) in &links.links {
    let req_id = match req_topic.numeric_id() {
      Some(id) => id as i64,
      None => {
        eprintln!("Invalid requirement topic: {}", req_topic.id());
        continue;
      }
    };
    for st in source_topics {
      let _ =
        db::add_requirement_source_topic(&state.db, req_id, st.id()).await;
    }
  }

  let mut ctx = state.data_context.lock().map_err(|e| {
    format!("Mutex poisoned in persist_requirement_links: {}", e)
  })?;
  let audit_data = ctx
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

  for (req_topic, source_topics) in &links.links {
    if let Some(req) = audit_data.requirements.get_mut(req_topic) {
      for st in source_topics {
        if !req.source_topics.contains(st) {
          req.source_topics.push(st.clone());
        }
      }
    }
  }

  Ok(())
}

/// Persist parsed threats/invariants to the database and return the in-memory
/// structures ready for insertion into audit data.
///
/// This is shared between the initial threat build and the security review pass.
async fn persist_parsed_threats(
  state: &PipelineState,
  all_parsed: &[task::ParsedThreats],
) -> Result<
  (
    BTreeMap<topic::Topic, core::Threat>,
    BTreeMap<topic::Topic, core::Invariant>,
    BTreeMap<topic::Topic, core::TopicMetadata>,
    BTreeMap<topic::Topic, Vec<topic::Topic>>,
  ),
  String,
> {
  let mut new_threats = BTreeMap::new();
  let mut new_invariants = BTreeMap::new();
  let mut new_topic_metadata = BTreeMap::new();
  let mut new_feature_threats: BTreeMap<topic::Topic, Vec<topic::Topic>> =
    BTreeMap::new();

  for parsed in all_parsed {
    for (feat_topic, threat_topics) in &parsed.feature_threat_topics {
      let feature_id = feat_topic.numeric_id().ok_or_else(|| {
        format!("Invalid feature topic: {}", feat_topic.id())
      })?;

      for tt in threat_topics {
        let (description, severity) = match parsed.topic_metadata.get(tt) {
          Some(core::TopicMetadata::ThreatTopic {
            description,
            severity,
            ..
          }) => (description.clone(), *severity),
          _ => continue,
        };

        let threat_row = db::create_threat(
          &state.db,
          feature_id,
          &description,
          AUTHOR_AGENT,
          severity.as_str(),
        )
        .await
        .map_err(|e| format!("create_threat failed: {}", e))?;

        let real_threat_topic =
          topic::new_attack_vector_topic(threat_row.id as i32);

        let mut real_invariant_topics = Vec::new();
        if let Some(threat) = parsed.threats.get(tt) {
          for inv_topic in &threat.invariant_topics {
            let inv_desc = match parsed.topic_metadata.get(inv_topic) {
              Some(core::TopicMetadata::InvariantTopic {
                description,
                ..
              }) => description.clone(),
              _ => continue,
            };

            let inv_row = db::create_invariant(
              &state.db,
              threat_row.id,
              &inv_desc,
              AUTHOR_AGENT,
              severity.as_str(),
            )
            .await
            .map_err(|e| format!("create_invariant failed: {}", e))?;

            let real_inv_topic =
              topic::new_invariant_topic(inv_row.id as i32);

            new_topic_metadata.insert(
              real_inv_topic.clone(),
              core::TopicMetadata::InvariantTopic {
                topic: real_inv_topic.clone(),
                description: inv_desc,
                threat_topic: real_threat_topic.clone(),
                author_id: AUTHOR_AGENT,
                created_at: inv_row.created_at,
                severity,
              },
            );
            new_invariants.insert(
              real_inv_topic.clone(),
              core::Invariant {
                source_topics: Vec::new(),
              },
            );
            real_invariant_topics.push(real_inv_topic);
          }
        }

        new_topic_metadata.insert(
          real_threat_topic.clone(),
          core::TopicMetadata::ThreatTopic {
            topic: real_threat_topic.clone(),
            description,
            feature_topic: feat_topic.clone(),
            author_id: AUTHOR_AGENT,
            created_at: threat_row.created_at,
            severity,
          },
        );
        new_threats.insert(
          real_threat_topic.clone(),
          core::Threat {
            invariant_topics: real_invariant_topics,
          },
        );
        new_feature_threats
          .entry(feat_topic.clone())
          .or_default()
          .push(real_threat_topic);
      }
    }
  }

  Ok((
    new_threats,
    new_invariants,
    new_topic_metadata,
    new_feature_threats,
  ))
}

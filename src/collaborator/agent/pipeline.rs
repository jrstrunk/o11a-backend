//! Orchestrates the analysis pipeline: requirement extraction, semantic
//! linking, behavior extraction, and feature synthesis via reconciliation.
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

/// Run the full analysis pipeline:
/// build_requirements → build_semantic_links → build_behaviors → synthesize_features
pub async fn run_full_pipeline(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  build_requirements(state, audit_id).await?;
  build_semantic_links(state, audit_id).await?;
  build_behaviors(state, audit_id).await?;
  synthesize_features(state, audit_id).await?;
  Ok(())
}

/// Extract requirements from documentation, grouped by section.
/// This is the first step of the new pipeline (Phase 1).
pub async fn build_requirements(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  println!("pipeline::build_requirements for audit {}", audit_id);

  let documentation_files = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned in build_requirements: {}", e))?;
    let audit_data = ctx
      .get_audit(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
    task::render_documentation_files(audit_data)
  };

  let parsed =
    task::extract_requirements_from_documentation(&documentation_files).await?;

  // Delete old requirements (and features, since they'll be re-synthesized)
  db::delete_all_features_for_audit(&state.db, audit_id)
    .await
    .map_err(|e| format!("delete_all_features_for_audit failed: {}", e))?;

  // Persist requirements grouped by section
  for (section_topic, req_topics) in &parsed.section_requirements {
    for req_topic in req_topics {
      let req_desc = match parsed.topic_metadata.get(req_topic) {
        Some(core::TopicMetadata::RequirementTopic { description, .. }) => {
          description.as_str()
        }
        _ => continue,
      };

      // Create requirement without a feature (feature_id = NULL via 0 sentinel)
      // The feature association will be established during reconciliation (Phase 4)
      let req_row =
        db::create_requirement(&state.db, 0, req_desc, AUTHOR_AGENT)
          .await
          .map_err(|e| format!("create_requirement failed: {}", e))?;

      // Persist documentation topic links
      if let Some(req) = parsed.requirements.get(req_topic) {
        for dt in &req.documentation_topics {
          let _ = db::add_requirement_documentation_topic(
            &state.db,
            req_row.id,
            dt.id(),
          )
          .await;
        }
      }

      // Persist section association
      let _ = db::set_requirement_section(
        &state.db,
        req_row.id,
        section_topic.id(),
      )
      .await;
    }
  }

  // Update in-memory state
  let mut ctx = state
    .data_context
    .lock()
    .map_err(|e| format!("Mutex poisoned in build_requirements (store): {}", e))?;
  let audit_data = ctx
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

  // Clear old feature/requirement metadata
  audit_data.topic_metadata.retain(|_, m| {
    !matches!(
      m,
      core::TopicMetadata::FeatureTopic { .. }
        | core::TopicMetadata::RequirementTopic { .. }
    )
  });

  audit_data.features.clear();
  audit_data.requirements = parsed.requirements;
  audit_data.topic_metadata.extend(parsed.topic_metadata);
  audit_data.source_feature_links.clear();
  core::rebuild_feature_context(audit_data);

  Ok(())
}

/// Synthesize features by reconciling requirements with behaviors in a single LLM pass.
pub async fn synthesize_features(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  println!("pipeline::synthesize_features for audit {}", audit_id);

  let (requirements_json, behaviors_json) = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned in synthesize_features: {}", e))?;
    let audit_data = ctx
      .get_audit(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
    task::render_reconciliation_context(audit_data)
  };

  let synthesized =
    task::synthesize_features(&requirements_json, &behaviors_json).await?;

  // Delete old features (but keep requirements and behaviors — we're reassigning them)
  // Only delete feature rows, not requirements
  {
    let mut ctx = state
      .data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned in synthesize_features (clear): {}", e))?;
    let audit_data = ctx
      .get_audit_mut(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

    audit_data.topic_metadata.retain(|_, m| {
      !matches!(m, core::TopicMetadata::FeatureTopic { .. })
    });
    audit_data.features.clear();
    audit_data.source_feature_links.clear();
  }

  // Persist features to database
  for (feat_topic, feature) in &synthesized.features {
    let (name, description) = match synthesized.topic_metadata.get(feat_topic) {
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

    // Update requirement feature_id assignments
    for rt in &feature.requirement_topics {
      if let Some(req_id) = rt.numeric_id() {
        let _ = sqlx::query("UPDATE requirements SET feature_id = ? WHERE id = ?")
          .bind(row.id)
          .bind(req_id)
          .execute(&state.db)
          .await;
      }
    }

    // Derive source_feature_links from behaviors' member_topics
    // Collect member topics while holding the lock, then persist after releasing
    let member_topics_for_feature: Vec<String> = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| format!("Mutex poisoned in synthesize_features (links): {}", e))?;
      let audit_data = ctx
        .get_audit(audit_id)
        .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

      synthesized
        .behavior_to_feature
        .iter()
        .filter(|(_, ft)| *ft == feat_topic)
        .filter_map(|(bt, _)| {
          if let Some(core::TopicMetadata::BehaviorTopic { member_topic, .. }) =
            audit_data.topic_metadata.get(bt)
          {
            Some(member_topic.id().to_string())
          } else {
            None
          }
        })
        .collect()
    };

    for mt_id in &member_topics_for_feature {
      let _ = db::add_source_feature_link(
        &state.db,
        audit_id,
        mt_id,
        row.id,
      )
      .await;
    }
  }

  // Rebuild in-memory state with the real DB IDs
  // Reload features from DB to get the real IDs
  {
    let mut ctx = state
      .data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned in synthesize_features (store): {}", e))?;
    let audit_data = ctx
      .get_audit_mut(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

    // Store the synthesized features (with temporary topic IDs for now)
    audit_data.topic_metadata.extend(synthesized.topic_metadata);
    audit_data.features = synthesized.features;

    // Update RequirementTopic.feature_topic for assigned requirements
    for (rt, ft) in &synthesized.requirement_to_feature {
      if let Some(core::TopicMetadata::RequirementTopic {
        feature_topic, ..
      }) = audit_data.topic_metadata.get_mut(rt)
      {
        *feature_topic = ft.clone();
      }
    }

    core::rebuild_feature_context(audit_data);
  }

  println!("  Completed feature synthesis");

  Ok(())
}

/// Extract behaviors from source code with functional semantics in context.
pub async fn build_behaviors(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  use crate::collaborator::agent::context;

  println!("pipeline::build_behaviors for audit {}", audit_id);

  let contracts = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned in build_behaviors: {}", e))?;
    let audit_data = ctx
      .get_audit(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
    let stc = ctx
      .source_text_cache
      .get(audit_id)
      .cloned()
      .unwrap_or_default();
    context::collect_contracts_for_behavior_extraction(audit_data, &stc)
  };

  if contracts.is_empty() {
    println!("  No contracts found, skipping behavior extraction");
    return Ok(());
  }

  // Extract behaviors per contract in parallel
  let mut handles = Vec::new();
  for contract in &contracts {
    let json = contract.json.clone();
    let name = contract.contract_name.clone();
    handles.push(tokio::spawn(async move {
      task::extract_behaviors_from_contract(&json, &name).await
    }));
  }

  let mut all_behaviors: Vec<(topic::Topic, String)> = Vec::new(); // (member_topic, description)
  for handle in handles {
    match handle.await {
      Ok(Ok(parsed)) => all_behaviors.extend(parsed.behaviors),
      Ok(Err(e)) => eprintln!("extract_behaviors failed: {}", e),
      Err(e) => eprintln!("extract_behaviors panicked: {}", e),
    }
  }

  // Persist to database and build in-memory state
  let mut new_metadata = std::collections::BTreeMap::new();
  let mut new_behaviors = std::collections::BTreeMap::new();

  for (member_topic, description) in &all_behaviors {
    let row = db::create_behavior(
      &state.db,
      audit_id,
      member_topic.id(),
      description,
      AUTHOR_AGENT,
    )
    .await
    .map_err(|e| format!("create_behavior failed: {}", e))?;

    let beh_topic = topic::new_behavior_topic(row.id as i32);

    new_metadata.insert(
      beh_topic.clone(),
      core::TopicMetadata::BehaviorTopic {
        topic: beh_topic.clone(),
        description: description.clone(),
        member_topic: member_topic.clone(),
        author_id: AUTHOR_AGENT,
        created_at: row.created_at,
      },
    );

    new_behaviors.insert(beh_topic, core::Behavior {});
  }

  // Update in-memory state
  let mut ctx = state
    .data_context
    .lock()
    .map_err(|e| format!("Mutex poisoned in build_behaviors (store): {}", e))?;
  let audit_data = ctx
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

  // Clear old behaviors
  audit_data.topic_metadata.retain(|_, m| {
    !matches!(m, core::TopicMetadata::BehaviorTopic { .. })
  });
  audit_data.behaviors.clear();

  audit_data.topic_metadata.extend(new_metadata);
  audit_data.behaviors.extend(new_behaviors);
  core::rebuild_feature_context(audit_data);

  println!(
    "  Completed behavior extraction: {} behaviors",
    all_behaviors.len()
  );

  Ok(())
}

/// Build semantic links between documentation sections and code declarations.
/// Three layers: mechanical resolution, LLM pass 1, LLM pass 2.
pub async fn build_semantic_links(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  use crate::collaborator::agent::context;

  println!("pipeline::build_semantic_links for audit {}", audit_id);

  // Step 1: Mechanical resolution
  let (mechanical, sections, contracts) = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| format!("Mutex poisoned in build_semantic_links: {}", e))?;
    let audit_data = ctx
      .get_audit(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
    let stc = ctx
      .source_text_cache
      .get(audit_id)
      .cloned()
      .unwrap_or_default();

    let mechanical = context::mechanical_semantic_links(audit_data);
    let sections = task::collect_documentation_sections(audit_data);
    let contracts =
      context::render_contract_list_for_semantic_linking(audit_data, &stc);

    (mechanical, sections, contracts)
  };

  if sections.is_empty() || contracts.is_empty() {
    println!("  No sections or contracts found, skipping semantic linking");
    return Ok(());
  }

  // Build a compact contract list JSON for pass 1
  let contract_list_json = {
    let list: Vec<serde_json::Value> = contracts
      .iter()
      .map(|(ct, json)| {
        serde_json::json!({
          "contract_topic": ct.id(),
          "contract": serde_json::from_str::<serde_json::Value>(json).unwrap_or_default(),
        })
      })
      .collect();
    serde_json::to_string(&list).unwrap_or_default()
  };

  // Build contract JSON lookup by topic
  let contract_json_by_topic: std::collections::HashMap<&topic::Topic, &str> =
    contracts.iter().map(|(ct, json)| (ct, json.as_str())).collect();

  // Step 2: LLM pass 1 — for each section, identify relevant contracts
  let mut section_contracts: std::collections::HashMap<
    topic::Topic,
    Vec<topic::Topic>,
  > = std::collections::HashMap::new();

  let mut pass1_handles = Vec::new();
  for section_topic in &sections {
    let section_text = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| format!("Mutex poisoned in build_semantic_links (pass1): {}", e))?;
      let audit_data = ctx
        .get_audit(audit_id)
        .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
      context::render_section_text(section_topic, audit_data)
        .unwrap_or_default()
    };

    if section_text.is_empty() {
      continue;
    }

    let confirmed = mechanical
      .section_to_contracts
      .get(section_topic)
      .cloned()
      .unwrap_or_default();

    let st = section_topic.clone();
    let clj = contract_list_json.clone();
    pass1_handles.push(tokio::spawn(async move {
      task::semantic_link_pass1(&st, &section_text, &clj, &confirmed).await
    }));
  }

  for handle in pass1_handles {
    match handle.await {
      Ok(Ok(result)) => {
        section_contracts.insert(result.section_topic, result.contract_topics);
      }
      Ok(Err(e)) => eprintln!("semantic_link_pass1 failed: {}", e),
      Err(e) => eprintln!("semantic_link_pass1 panicked: {}", e),
    }
  }

  // Step 3: LLM pass 2 — for each (section, contract) pair, extract semantics
  let mut all_links: Vec<core::SemanticLink> = Vec::new();

  let mut pass2_handles = Vec::new();
  for (section_topic, contract_topics) in &section_contracts {
    let section_text = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| format!("Mutex poisoned in build_semantic_links (pass2): {}", e))?;
      let audit_data = ctx
        .get_audit(audit_id)
        .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
      context::render_section_text(section_topic, audit_data)
        .unwrap_or_default()
    };

    for ct in contract_topics {
      let contract_json = match contract_json_by_topic.get(ct) {
        Some(json) => json.to_string(),
        None => continue,
      };

      let confirmed_decls = mechanical
        .section_to_declarations
        .get(section_topic)
        .cloned()
        .unwrap_or_default();

      let st = section_topic.clone();
      let stxt = section_text.clone();
      pass2_handles.push(tokio::spawn(async move {
        task::semantic_link_pass2(&st, &stxt, &contract_json, &confirmed_decls)
          .await
      }));
    }
  }

  for handle in pass2_handles {
    match handle.await {
      Ok(Ok(result)) => all_links.extend(result.links),
      Ok(Err(e)) => eprintln!("semantic_link_pass2 failed: {}", e),
      Err(e) => eprintln!("semantic_link_pass2 panicked: {}", e),
    }
  }

  // Persist to database
  for link in &all_links {
    let _ = db::add_semantic_link(
      &state.db,
      audit_id,
      link.documentation_topic.id(),
      link.declaration_topic.id(),
      &link.semantic_text,
    )
    .await;
  }

  // Update in-memory state
  let mut ctx = state
    .data_context
    .lock()
    .map_err(|e| format!("Mutex poisoned in build_semantic_links (store): {}", e))?;
  let audit_data = ctx
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

  audit_data.semantic_links = all_links.clone();

  // Populate functional_semantics with provenance
  for link in &all_links {
    audit_data.functional_semantics.insert(
      link.declaration_topic.clone(),
      core::FunctionalSemantic {
        text: link.semantic_text.clone(),
        documentation_topic: Some(link.documentation_topic.clone()),
      },
    );
  }

  println!(
    "  Completed semantic linking: {} links",
    all_links.len()
  );

  Ok(())
}


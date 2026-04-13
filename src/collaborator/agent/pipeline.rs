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
/// build_semantic_links → build_requirements → build_behaviors → synthesize_features
///
/// Semantic linking runs first so that functional semantics are available
/// when rendering documentation for requirement extraction. This means
/// inline code references like `pID` are annotated with their project-specific
/// meaning (e.g., "participation identifier"), giving the LLM proper context
/// to produce behavioral requirements without using declaration names.
pub async fn run_full_pipeline(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  build_semantic_links(state, audit_id).await?;
  build_requirements(state, audit_id).await?;
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
/// Three passes: section→contracts, section×contract→members, section×member→semantics.
/// Each pass has a mechanical pre-step followed by LLM refinement.
pub async fn build_semantic_links(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  use crate::collaborator::agent::context;

  println!("pipeline::build_semantic_links for audit {}", audit_id);

  // Mechanical resolution (shared by passes 1 and 2)
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

  println!(
    "  Mechanical: {} sections, {} contracts, {} section-contract links, {} section-declaration links",
    sections.len(),
    contracts.len(),
    mechanical.section_to_contracts.len(),
    mechanical.section_to_declarations.len(),
  );

  if sections.is_empty() || contracts.is_empty() {
    println!("  No sections or contracts found, skipping semantic linking");
    return Ok(());
  }

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

  let contract_json_by_topic: std::collections::HashMap<&topic::Topic, &str> =
    contracts.iter().map(|(ct, json)| (ct, json.as_str())).collect();

  // ---- Pass 1: section → contracts (mechanical + LLM) ----
  // Seed with mechanical results so they survive even if LLM returns empty
  let mut section_contracts: std::collections::HashMap<
    topic::Topic,
    Vec<topic::Topic>,
  > = mechanical.section_to_contracts.clone();

  let mut pass1_handles = Vec::new();
  let mut sections_with_text = 0;
  let mut sections_empty = 0;
  for section_topic in &sections {
    let section_text = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| format!("Mutex poisoned (pass1): {}", e))?;
      let audit_data = ctx
        .get_audit(audit_id)
        .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
      context::render_section_text(section_topic, audit_data)
        .unwrap_or_default()
    };

    if section_text.is_empty() {
      sections_empty += 1;
      continue;
    }

    let confirmed = mechanical
      .section_to_contracts
      .get(section_topic)
      .cloned()
      .unwrap_or_default();

    sections_with_text += 1;
    let st = section_topic.clone();
    let clj = contract_list_json.clone();
    pass1_handles.push(tokio::spawn(async move {
      task::semantic_link_pass1(&st, &section_text, &clj, &confirmed).await
    }));
  }

  println!(
    "  Pass 1: {} sections with text, {} empty, {} LLM calls queued",
    sections_with_text, sections_empty, pass1_handles.len()
  );

  for handle in pass1_handles {
    match handle.await {
      Ok(Ok(result)) => {
        // Merge LLM results with mechanical results
        let contracts = section_contracts
          .entry(result.section_topic)
          .or_default();
        for ct in result.contract_topics {
          if !contracts.contains(&ct) {
            contracts.push(ct);
          }
        }
      }
      Ok(Err(e)) => eprintln!("semantic_link pass1 failed: {}", e),
      Err(e) => eprintln!("semantic_link pass1 panicked: {}", e),
    }
  }

  println!("  Pass 1 complete: {} section-contract pairs",
    section_contracts.values().map(|v| v.len()).sum::<usize>());

  // ---- Pass 2: section × contract → members (mechanical + LLM) ----
  let mut section_members: std::collections::HashMap<
    topic::Topic,
    Vec<topic::Topic>,
  > = std::collections::HashMap::new();

  let mut pass2_handles = Vec::new();
  for (section_topic, contract_topics) in &section_contracts {
    let (section_text, confirmed_members) = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| format!("Mutex poisoned (pass2): {}", e))?;
      let audit_data = ctx
        .get_audit(audit_id)
        .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
      let stxt = context::render_section_text(section_topic, audit_data)
        .unwrap_or_default();

      // Mechanical: resolve section declarations to containing members
      let section_decls = mechanical
        .section_to_declarations
        .get(section_topic)
        .cloned()
        .unwrap_or_default();

      let mut mech_members = Vec::new();
      for ct in contract_topics {
        let members = context::mechanical_section_to_members(
          &section_decls,
          ct,
          audit_data,
        );
        for m in members {
          if !mech_members.contains(&m) {
            mech_members.push(m);
          }
        }
      }

      // Seed section_members with mechanical results
      if !mech_members.is_empty() {
        let existing = section_members
          .entry(section_topic.clone())
          .or_default();
        for m in &mech_members {
          if !existing.contains(m) {
            existing.push(m.clone());
          }
        }
      }

      (stxt, mech_members)
    };

    for ct in contract_topics {
      let contract_json = match contract_json_by_topic.get(ct) {
        Some(json) => json.to_string(),
        None => continue,
      };

      let st = section_topic.clone();
      let stxt = section_text.clone();
      let confirmed = confirmed_members.clone();
      pass2_handles.push(tokio::spawn(async move {
        task::semantic_link_pass2(&st, &stxt, &contract_json, &confirmed).await
      }));
    }
  }

  for handle in pass2_handles {
    match handle.await {
      Ok(Ok(result)) => {
        let members = section_members
          .entry(result.section_topic)
          .or_default();
        for mt in result.member_topics {
          if !members.contains(&mt) {
            members.push(mt);
          }
        }
      }
      Ok(Err(e)) => eprintln!("semantic_link pass2 failed: {}", e),
      Err(e) => eprintln!("semantic_link pass2 panicked: {}", e),
    }
  }

  println!("  Pass 2 complete: {} section-member pairs",
    section_members.values().map(|v| v.len()).sum::<usize>());

  // ---- Pass 3: semantics extraction (doc-first, code for disambiguation) ----
  // Two kinds of targets:
  //   a) section × member → semantics for declarations within functions/modifiers
  //   b) section × contract → semantics for component-scoped declarations (state vars, events, etc.)
  let mut all_links: Vec<core::SemanticLink> = Vec::new();

  let mut pass3_handles = Vec::new();

  // (a) Member-scoped: for each (section, member) pair
  for (section_topic, member_topics) in &section_members {
    let section_text = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| format!("Mutex poisoned (pass3): {}", e))?;
      let audit_data = ctx
        .get_audit(audit_id)
        .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
      context::render_section_text(section_topic, audit_data)
        .unwrap_or_default()
    };

    for mt in member_topics {
      let (declarations_json, member_source) = {
        let ctx = state
          .data_context
          .lock()
          .map_err(|e| format!("Mutex poisoned (pass3 member): {}", e))?;
        let audit_data = ctx
          .get_audit(audit_id)
          .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
        let stc = ctx
          .source_text_cache
          .get(audit_id)
          .cloned()
          .unwrap_or_default();

        let decls = context::render_member_declarations_for_semantics(
          mt, audit_data,
        );
        let source = context::render_member_source_for_semantics(
          mt, audit_data, &stc,
        )
        .unwrap_or_default();

        (decls, source)
      };

      let st = section_topic.clone();
      let stxt = section_text.clone();
      pass3_handles.push(tokio::spawn(async move {
        task::semantic_link_pass3(&st, &stxt, &declarations_json, &member_source)
          .await
      }));
    }
  }

  // (b) Contract-scoped: for each (section, contract) pair, extract semantics
  // for state variables, events, structs, enums at the contract level
  for (section_topic, contract_topics) in &section_contracts {
    let section_text = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| format!("Mutex poisoned (pass3 contract): {}", e))?;
      let audit_data = ctx
        .get_audit(audit_id)
        .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
      context::render_section_text(section_topic, audit_data)
        .unwrap_or_default()
    };

    for ct in contract_topics {
      let (declarations_json, signatures_source) = {
        let ctx = state
          .data_context
          .lock()
          .map_err(|e| format!("Mutex poisoned (pass3 contract render): {}", e))?;
        let audit_data = ctx
          .get_audit(audit_id)
          .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
        let stc = ctx
          .source_text_cache
          .get(audit_id)
          .cloned()
          .unwrap_or_default();

        let decls = context::render_contract_declarations_for_semantics(
          ct, audit_data,
        );
        let sigs = context::render_contract_declaration_signatures(
          ct, audit_data, &stc,
        );

        (decls, sigs)
      };

      // Skip if no component-scoped declarations to assign
      if declarations_json == "[]" {
        continue;
      }

      let st = section_topic.clone();
      let stxt = section_text.clone();
      pass3_handles.push(tokio::spawn(async move {
        task::semantic_link_pass3(&st, &stxt, &declarations_json, &signatures_source)
          .await
      }));
    }
  }

  for handle in pass3_handles {
    match handle.await {
      Ok(Ok(result)) => all_links.extend(result.links),
      Ok(Err(e)) => eprintln!("semantic_link pass3 failed: {}", e),
      Err(e) => eprintln!("semantic_link pass3 panicked: {}", e),
    }
  }

  println!("  Pass 3 complete: {} semantic links", all_links.len());

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
        author_id: AUTHOR_AGENT,
        created_at: String::new(),
      },
    );
  }

  println!(
    "  Completed semantic linking: {} links",
    all_links.len()
  );

  Ok(())
}


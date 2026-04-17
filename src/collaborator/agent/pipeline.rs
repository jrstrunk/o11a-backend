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
use crate::collaborator::models::AUTHOR_AGENT_LARGE;
use crate::core::{self, DataContext, topic};

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
  println!("Starting full analysis pipeline for audit {}", audit_id);

  println!("\n[1/4] Semantic Linking");
  build_semantic_links(state, audit_id).await?;

  println!("\n[2/4] Requirement Extraction");
  build_requirements(state, audit_id).await?;

  println!("\n[3/4] Behavior Extraction");
  build_behaviors(state, audit_id).await?;

  println!("\n[4/4] Feature Synthesis");
  synthesize_features(state, audit_id).await?;

  println!("\nPipeline complete for audit {}", audit_id);
  Ok(())
}

/// Extract requirements from documentation, grouped by section.
/// This is the first step of the new pipeline (Phase 1).
pub async fn build_requirements(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
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

  println!(
    "  Extracting requirements from {} documentation files",
    documentation_files.len()
  );
  let mut parsed =
    task::extract_requirements_from_documentation(&documentation_files).await?;
  println!(
    "  Extracted {} requirements across {} sections",
    parsed.requirements.len(),
    parsed.section_requirements.len()
  );

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

      let req_row =
        db::create_requirement(&state.db, req_desc, AUTHOR_AGENT_LARGE)
          .await
          .map_err(|e| format!("create_requirement failed: {}", e))?;

      // Update in-memory metadata with DB timestamp
      if let Some(core::TopicMetadata::RequirementTopic { created_at, .. }) =
        parsed.topic_metadata.get_mut(req_topic)
      {
        *created_at = req_row.created_at.clone();
      }

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
      let _ =
        db::set_requirement_section(&state.db, req_row.id, section_topic.id())
          .await;
    }
  }

  // Update in-memory state
  let mut ctx = state.data_context.lock().map_err(|e| {
    format!("Mutex poisoned in build_requirements (store): {}", e)
  })?;
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

  let req_count = parsed.requirements.len();
  audit_data.requirements = parsed.requirements;
  audit_data.topic_metadata.extend(parsed.topic_metadata);
  audit_data.feature_requirement_links.clear();
  audit_data.feature_behavior_links.clear();
  core::rebuild_feature_context(audit_data);

  println!("  Persisted {} requirements to database", req_count);
  Ok(())
}

/// Synthesize features by reconciling requirements with behaviors in a single LLM pass.
pub async fn synthesize_features(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
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

  println!("  Reconciling requirements and behaviors into features...");
  let mut synthesized =
    task::synthesize_features(&requirements_json, &behaviors_json).await?;
  let feature_count = synthesized.feature_requirement_links.len();
  println!("  Synthesized {} features", feature_count);

  // Delete old features (but keep requirements and behaviors — we're reassigning them)
  // Only delete feature rows, not requirements
  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      format!("Mutex poisoned in synthesize_features (clear): {}", e)
    })?;
    let audit_data = ctx
      .get_audit_mut(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

    audit_data
      .topic_metadata
      .retain(|_, m| !matches!(m, core::TopicMetadata::FeatureTopic { .. }));
    audit_data.feature_requirement_links.clear();
    audit_data.feature_behavior_links.clear();
  }

  // Delete old links
  let _ = db::delete_all_feature_links_for_audit(&state.db, audit_id).await;

  // Persist features to database
  let feat_topics: Vec<topic::Topic> =
    synthesized.feature_requirement_links.keys().cloned().collect();
  for feat_topic in &feat_topics {
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
      AUTHOR_AGENT_LARGE,
    )
    .await
    .map_err(|e| format!("create_feature failed: {}", e))?;

    // Update in-memory metadata with DB timestamp
    if let Some(core::TopicMetadata::FeatureTopic { created_at, .. }) =
      synthesized.topic_metadata.get_mut(feat_topic)
    {
      *created_at = row.created_at.clone();
    }

    // Persist feature-requirement links
    if let Some(req_topics) = synthesized.feature_requirement_links.get(feat_topic) {
      for rt in req_topics {
        if let Some(req_id) = rt.numeric_id() {
          let _ = db::add_feature_requirement_link(
            &state.db, audit_id, row.id, req_id,
          ).await;
        }
      }
    }

    // Persist feature-behavior links
    if let Some(beh_topics) = synthesized.feature_behavior_links.get(feat_topic) {
      for bt in beh_topics {
        if let Some(beh_id) = bt.numeric_id() {
          let _ = db::add_feature_behavior_link(
            &state.db, audit_id, row.id, beh_id,
          ).await;
        }
      }
    }
  }

  // Rebuild in-memory state
  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      format!("Mutex poisoned in synthesize_features (store): {}", e)
    })?;
    let audit_data = ctx
      .get_audit_mut(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

    audit_data.topic_metadata.extend(synthesized.topic_metadata);
    audit_data.feature_requirement_links = synthesized.feature_requirement_links;
    audit_data.feature_behavior_links = synthesized.feature_behavior_links;


    core::rebuild_feature_context(audit_data);
    println!("  Persisted {} features to database", feature_count);
  }

  Ok(())
}

/// Extract behaviors from source code with functional semantics in context.
pub async fn build_behaviors(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), String> {
  use crate::collaborator::agent::context;

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

  println!("  Extracting behaviors from {} contracts", contracts.len());

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

  println!(
    "  Extracted {} behaviors from {} contracts",
    all_behaviors.len(),
    contracts.len()
  );

  // Persist to database and build in-memory state
  let mut new_metadata = std::collections::BTreeMap::new();

  for (member_topic, description) in &all_behaviors {
    let row = db::create_behavior(
      &state.db,
      audit_id,
      member_topic.id(),
      description,
      AUTHOR_AGENT_LARGE,
    )
    .await
    .map_err(|e| format!("create_behavior failed: {}", e))?;

    let beh_topic = topic::new_behavior_topic(row.id as i32);

    new_metadata.insert(
      beh_topic.clone(),
      core::TopicMetadata::BehaviorTopic {
        topic: beh_topic,
        description: description.clone(),
        member_topic: member_topic.clone(),
        author_id: AUTHOR_AGENT_LARGE,
        created_at: row.created_at,
      },
    );
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
  audit_data
    .topic_metadata
    .retain(|_, m| !matches!(m, core::TopicMetadata::BehaviorTopic { .. }));

  audit_data.topic_metadata.extend(new_metadata);
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

  println!("  Building semantic links for audit {}", audit_id);

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
    contracts
      .iter()
      .map(|(ct, json)| (ct, json.as_str()))
      .collect();

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
    sections_with_text,
    sections_empty,
    pass1_handles.len()
  );

  for handle in pass1_handles {
    match handle.await {
      Ok(Ok(result)) => {
        // Merge LLM results with mechanical results
        let contracts =
          section_contracts.entry(result.section_topic).or_default();
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

  println!(
    "  Pass 1 complete: {} section-contract pairs",
    section_contracts.values().map(|v| v.len()).sum::<usize>()
  );

  // ---- Pass 2: section × contract → members (mechanical + LLM) ----
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

  // Collect pass2 results: build a map of section -> doc_topic -> [member_topics]
  // This groups members by the specific doc child sections they relate to,
  // enabling batched pass3 calls per doc child section.
  let mut section_doc_members: std::collections::BTreeMap<
    topic::Topic,
    std::collections::BTreeMap<topic::Topic, Vec<topic::Topic>>,
  > = std::collections::BTreeMap::new();

  for handle in pass2_handles {
    match handle.await {
      Ok(Ok(result)) => {
        let doc_members = section_doc_members
          .entry(result.section_topic.clone())
          .or_default();

        for mapping in result.member_mappings {
          // If no doc_topics, use the section topic as fallback
          let doc_topics = if mapping.doc_topics.is_empty() {
            vec![result.section_topic.clone()]
          } else {
            mapping.doc_topics
          };

          for dt in doc_topics {
            let entry = doc_members.entry(dt).or_default();
            if !entry.contains(&mapping.member_topic) {
              entry.push(mapping.member_topic.clone());
            }
          }
        }
      }
      Ok(Err(e)) => eprintln!("semantic_link pass2 failed: {}", e),
      Err(e) => eprintln!("semantic_link pass2 panicked: {}", e),
    }
  }

  let total_doc_groups: usize =
    section_doc_members.values().map(|dm| dm.len()).sum();
  println!(
    "  Pass 2 complete: {} doc-topic groups for pass3",
    total_doc_groups
  );

  // ---- Pass 3: semantics extraction (doc-first, code for disambiguation) ----
  // Batched by doc child section: for each (section, doc_topic) group, gather
  // all matched members' declarations and source, send one pass3 call.
  let mut all_links: Vec<core::SemanticLink> = Vec::new();
  let mut pass3_handles = Vec::new();

  // (a) Member-scoped: batched by doc_topic groups from pass2
  for (section_topic, doc_member_map) in &section_doc_members {
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

    for (doc_topic, member_topics) in doc_member_map {
      let (declarations_json, source_code) = {
        let ctx = state
          .data_context
          .lock()
          .map_err(|e| format!("Mutex poisoned (pass3 batch): {}", e))?;
        let audit_data = ctx
          .get_audit(audit_id)
          .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
        let stc = ctx
          .source_text_cache
          .get(audit_id)
          .cloned()
          .unwrap_or_default();

        let decls = context::render_batched_member_declarations_for_semantics(
          member_topics,
          audit_data,
        );
        let source = context::render_batched_member_sources_for_semantics(
          member_topics,
          audit_data,
          &stc,
        );

        (decls, source)
      };

      if declarations_json == "[]" {
        continue;
      }

      let st = section_topic.clone();
      let stxt = section_text.clone();
      let fallback_dt = doc_topic.clone();
      pass3_handles.push(tokio::spawn(async move {
        task::semantic_link_pass3(
          &st,
          &stxt,
          &declarations_json,
          &source_code,
          &fallback_dt,
        )
        .await
      }));
    }
  }

  // (b) Contract-scoped: batch all contracts' state vars/events/structs per section
  for (section_topic, contract_topics) in &section_contracts {
    let (section_text, declarations_json, signatures_source) = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| format!("Mutex poisoned (pass3 contract): {}", e))?;
      let audit_data = ctx
        .get_audit(audit_id)
        .ok_or_else(|| format!("Audit not found: {}", audit_id))?;
      let stc = ctx
        .source_text_cache
        .get(audit_id)
        .cloned()
        .unwrap_or_default();

      let stxt = context::render_section_text(section_topic, audit_data)
        .unwrap_or_default();
      let decls = context::render_batched_contract_declarations_for_semantics(
        contract_topics,
        audit_data,
      );
      let sigs = context::render_batched_contract_declaration_signatures(
        contract_topics,
        audit_data,
        &stc,
      );

      (stxt, decls, sigs)
    };

    if declarations_json == "[]" {
      continue;
    }

    let st = section_topic.clone();
    let fallback_dt = section_topic.clone();
    pass3_handles.push(tokio::spawn(async move {
      task::semantic_link_pass3(
        &st,
        &section_text,
        &declarations_json,
        &signatures_source,
        &fallback_dt,
      )
      .await
    }));
  }

  println!("  Pass 3: {} LLM calls queued", pass3_handles.len());

  for handle in pass3_handles {
    match handle.await {
      Ok(Ok(result)) => all_links.extend(result.links),
      Ok(Err(e)) => eprintln!("semantic_link pass3 failed: {}", e),
      Err(e) => eprintln!("semantic_link pass3 panicked: {}", e),
    }
  }

  println!("  Pass 3 complete: {} semantic links", all_links.len());

  // Resolve transitive topics before condensation so that semantics from
  // interface stubs are grouped with their base implementation. After this
  // step, all links carry the base (non-transitive) declaration topic.
  {
    let ctx = state.data_context.lock().map_err(|e| {
      format!("Mutex poisoned in build_semantic_links (resolve transitive): {}", e)
    })?;
    let audit_data = ctx
      .get_audit(audit_id)
      .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

    for link in &mut all_links {
      if let Some(base) = audit_data
        .topic_metadata
        .get(&link.declaration_topic)
        .and_then(|m| m.transitive_topic())
      {
        link.declaration_topic = base.clone();
      }
    }
  }

  // Condense repetitive semantics — now grouped by base topic, so
  // transitive semantics are condensed alongside their base.
  let unique_declarations = {
    let mut decls = std::collections::BTreeSet::new();
    for link in &all_links {
      decls.insert(link.declaration_topic.clone());
    }
    decls.len()
  };
  println!(
    "  Condensing semantics: {} links across {} declarations",
    all_links.len(),
    unique_declarations
  );

  let mut by_declaration: std::collections::BTreeMap<
    topic::Topic,
    Vec<core::SemanticLink>,
  > = std::collections::BTreeMap::new();
  for link in all_links {
    by_declaration
      .entry(link.declaration_topic.clone())
      .or_default()
      .push(link);
  }

  let mut condense_handles = Vec::new();
  let mut pass_through: Vec<core::SemanticLink> = Vec::new();
  let mut condense_count = 0usize;
  for (decl_topic, links) in &by_declaration {
    if links.len() <= 1 {
      pass_through.extend(links.iter().cloned());
    } else {
      let decl_id = decl_topic.id().to_string();
      let texts: Vec<String> =
        links.iter().map(|l| l.description.clone()).collect();
      let original_links = links.clone();
      let decl_topic = decl_topic.clone();
      condense_count += 1;
      condense_handles.push(tokio::spawn(async move {
        let result = task::condense_semantics(&decl_id, &texts).await;
        (decl_topic, original_links, result)
      }));
    }
  }

  println!(
    "  Condensation: {} declarations need condensing, {} passed through",
    condense_count,
    by_declaration.len() - condense_count
  );

  let mut all_links = pass_through;
  for handle in condense_handles {
    match handle.await {
      Ok((decl_topic, original_links, Ok(condensed))) => {
        for entry in condensed {
          // Collect all doc topics from the original links that were merged.
          let mut doc_topics: Vec<topic::Topic> = entry
            .source_indices
            .iter()
            .filter_map(|&i| original_links.get(i))
            .flat_map(|l| l.documentation_topics.iter().cloned())
            .collect();
          doc_topics.dedup();

          if doc_topics.is_empty() {
            doc_topics = original_links[0].documentation_topics.clone();
          }

          all_links.push(core::SemanticLink {
            documentation_topics: doc_topics,
            declaration_topic: decl_topic.clone(),
            description: entry.text,
          });
        }
      }
      Ok((decl_topic, original_links, Err(e))) => {
        eprintln!(
          "condense_semantics failed for {}: {}, keeping originals",
          decl_topic.id(),
          e
        );
        all_links.extend(original_links);
      }
      Err(e) => {
        eprintln!("condense_semantics task panicked: {}", e);
      }
    }
  }

  println!("  After condensation: {} semantic links", all_links.len());

  // Persist to database, collecting the assigned IDs for P-topics.
  let mut link_ids: Vec<i64> = Vec::with_capacity(all_links.len());
  for link in &all_links {
    let doc_topic_ids: Vec<&str> = link
      .documentation_topics
      .iter()
      .map(|dt| dt.id())
      .collect();
    match db::add_semantic_link(
      &state.db,
      audit_id,
      link.declaration_topic.id(),
      &link.description,
      AUTHOR_AGENT_LARGE.into(),
      &doc_topic_ids,
    )
    .await
    {
      Ok(id) => link_ids.push(id),
      Err(e) => {
        eprintln!("add_semantic_link failed: {}", e);
        link_ids.push(0);
      }
    }
  }

  // Update in-memory state
  let mut ctx = state.data_context.lock().map_err(|e| {
    format!("Mutex poisoned in build_semantic_links (store): {}", e)
  })?;
  let audit_data = ctx
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit not found: {}", audit_id))?;

  // Populate FunctionalSemanticTopic entries in topic_metadata with P-topic
  // IDs. Transitive topics have already been resolved to their base topics
  // before condensation, so declaration_topic is always the base.
  let now = crate::collaborator::agent::log::iso_timestamp();
  for (link, &db_id) in all_links.iter().zip(link_ids.iter()) {
    let sem_topic = topic::new_functional_property_topic(db_id as i32);

    audit_data.topic_metadata.insert(
      sem_topic.clone(),
      core::TopicMetadata::FunctionalSemanticTopic {
        topic: sem_topic,
        description: link.description.clone(),
        declaration_topic: link.declaration_topic.clone(),
        documentation_topics: link.documentation_topics.clone(),
        author_id: AUTHOR_AGENT_LARGE,
        created_at: now.clone(),
      },
    );
  }

  // Rebuild the declaration_semantics reverse index from topic_metadata.
  core::rebuild_feature_context(audit_data);

  println!(
    "  Persisted {} semantic links across {} declarations",
    all_links.len(),
    audit_data.declaration_semantics.len()
  );

  Ok(())
}

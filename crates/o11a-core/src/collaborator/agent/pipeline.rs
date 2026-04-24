//! Orchestrates the analysis pipeline: requirement extraction, semantic
//! linking, behavior extraction, and feature synthesis via reconciliation.
//!
//! Functions in this module handle the full lifecycle of an agent-generated
//! result: running the LLM task and updating in-memory audit data. Pipeline
//! output lives only in `DataContext` — persistence of the pipeline's output
//! is handled by the caller (the `o11a-analyze` binary writes `audit.json`;
//! the server hydrates from the same report). They use `String` errors so
//! callers (HTTP handlers, background tasks) can map to their own error
//! types.

use crate::collaborator::agent::task::{self, TaskError};
use crate::collaborator::models::Author;
use crate::domain::{self, DataContext, topic};
use crate::ids;

use std::sync::{Arc, Mutex};

/// Errors produced by the analysis pipeline.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
  #[error("audit not found: {audit_id}")]
  AuditNotFound { audit_id: String },
  #[error("DataContext mutex poisoned: {0}")]
  LockPoisoned(String),
  #[error("agent task failed: {0}")]
  AgentTask(#[from] TaskError),
  #[error("database error: {0}")]
  Database(#[from] sqlx::Error),
  #[error("{0}")]
  Other(String),
}

/// Shared state needed by pipeline functions — mirrors the relevant fields of
/// `AppState` without depending on the HTTP layer.
pub struct PipelineState {
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
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn run_full_pipeline(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  tracing::info!("Starting full analysis pipeline for audit {}", audit_id);

  tracing::info!("[1/4] Semantic Linking");
  build_semantic_links(state, audit_id).await?;

  tracing::info!("[2/4] Requirement Extraction");
  build_requirements(state, audit_id).await?;

  tracing::info!("[3/4] Behavior Extraction");
  build_behaviors(state, audit_id).await?;

  tracing::info!("[4/4] Feature Synthesis");
  synthesize_features(state, audit_id).await?;

  tracing::info!("Pipeline complete for audit {}", audit_id);
  Ok(())
}

/// Extract requirements from documentation, grouped by section.
/// This is the first step of the new pipeline (Phase 1).
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn build_requirements(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  let documentation_files = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
    let audit_data =
      ctx
        .get_audit(audit_id)
        .ok_or_else(|| PipelineError::AuditNotFound {
          audit_id: audit_id.to_string(),
        })?;
    task::render_documentation_files(audit_data)
  };

  tracing::info!(
    "Extracting requirements from {} documentation files",
    documentation_files.len()
  );
  let parsed =
    task::extract_requirements_from_documentation(&documentation_files).await?;
  tracing::info!(
    "Extracted {} requirements across {} sections",
    parsed.requirements.len(),
    parsed.section_requirements.len()
  );

  // Re-key parsed entities with allocated IDs from the atomic counter.
  // The parser assigns local R-topic IDs starting from 1; replace them with
  // process-wide allocated IDs so pipeline runs don't collide with existing
  // IDs already in the counter's range.
  let task::ParsedRequirements {
    requirements: parsed_requirements,
    topic_metadata,
    section_requirements: parsed_section_requirements,
  } = parsed;

  let mut id_remap: std::collections::HashMap<topic::Topic, topic::Topic> =
    std::collections::HashMap::new();
  let mut new_requirements: std::collections::BTreeMap<
    topic::Topic,
    domain::Requirement,
  > = std::collections::BTreeMap::new();
  let mut new_topic_metadata: std::collections::BTreeMap<
    topic::Topic,
    domain::TopicMetadata,
  > = std::collections::BTreeMap::new();
  let mut new_section_requirements: std::collections::BTreeMap<
    topic::Topic,
    Vec<topic::Topic>,
  > = std::collections::BTreeMap::new();

  for (section_topic, req_topics) in parsed_section_requirements {
    let mut new_req_topics = Vec::with_capacity(req_topics.len());
    for old_req_topic in req_topics {
      let new_req_topic = *id_remap.entry(old_req_topic).or_insert_with(|| {
        topic::new_requirement_topic(ids::allocate_requirement_id())
      });
      new_req_topics.push(new_req_topic);
    }
    new_section_requirements.insert(section_topic, new_req_topics);
  }

  for (old_req_topic, requirement) in parsed_requirements {
    let new_req_topic = match id_remap.get(&old_req_topic) {
      Some(t) => *t,
      None => {
        let t = topic::new_requirement_topic(ids::allocate_requirement_id());
        id_remap.insert(old_req_topic, t);
        t
      }
    };
    new_requirements.insert(new_req_topic, requirement);
  }

  for (old_req_topic, metadata) in topic_metadata {
    let new_req_topic = match id_remap.get(&old_req_topic) {
      Some(t) => *t,
      None => continue,
    };
    if let domain::TopicMetadata::RequirementTopic {
      description,
      section_topic,
      ..
    } = metadata
    {
      new_topic_metadata.insert(
        new_req_topic,
        domain::TopicMetadata::RequirementTopic {
          topic: new_req_topic,
          description,
          section_topic,
          author: Author::System,
          created_at: None,
        },
      );
    }
  }

  let mut ctx = state
    .data_context
    .lock()
    .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
  let audit_data = ctx.get_audit_mut(audit_id).ok_or_else(|| {
    PipelineError::AuditNotFound {
      audit_id: audit_id.to_string(),
    }
  })?;

  // Clear old feature/requirement metadata — requirements are being
  // replaced and features will be re-synthesized against the new set.
  audit_data.topic_metadata.retain(|_, m| {
    !matches!(
      m,
      domain::TopicMetadata::FeatureTopic { .. }
        | domain::TopicMetadata::RequirementTopic { .. }
    )
  });

  let req_count = new_requirements.len();
  audit_data.requirements = new_requirements;
  audit_data.topic_metadata.extend(new_topic_metadata);
  audit_data.feature_requirement_links.clear();
  audit_data.feature_behavior_links.clear();
  domain::rebuild_feature_context(audit_data);

  tracing::info!("Stored {} requirements in DataContext", req_count);
  Ok(())
}

/// Synthesize features by reconciling requirements with behaviors in a single LLM pass.
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn synthesize_features(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  let (requirements_json, behaviors_json) = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
    let audit_data =
      ctx
        .get_audit(audit_id)
        .ok_or_else(|| PipelineError::AuditNotFound {
          audit_id: audit_id.to_string(),
        })?;
    task::render_reconciliation_context(audit_data)
  };

  tracing::info!("Reconciling requirements and behaviors into features...");
  let synthesized =
    task::synthesize_features(&requirements_json, &behaviors_json).await?;
  let feature_count = synthesized.feature_requirement_links.len();
  tracing::info!("Synthesized {} features", feature_count);

  // Re-key synthesized features with allocated F IDs.
  let task::SynthesizedFeatures {
    topic_metadata,
    feature_requirement_links,
    feature_behavior_links,
  } = synthesized;

  let mut id_remap: std::collections::HashMap<topic::Topic, topic::Topic> =
    std::collections::HashMap::new();
  let mut new_topic_metadata: std::collections::BTreeMap<
    topic::Topic,
    domain::TopicMetadata,
  > = std::collections::BTreeMap::new();
  let mut new_feature_requirement_links: std::collections::BTreeMap<
    topic::Topic,
    Vec<topic::Topic>,
  > = std::collections::BTreeMap::new();
  let mut new_feature_behavior_links: std::collections::BTreeMap<
    topic::Topic,
    Vec<topic::Topic>,
  > = std::collections::BTreeMap::new();

  for (old_feat_topic, metadata) in topic_metadata {
    let new_feat_topic = *id_remap
      .entry(old_feat_topic)
      .or_insert_with(|| topic::new_feature_topic(ids::allocate_feature_id()));
    if let domain::TopicMetadata::FeatureTopic {
      name, description, ..
    } = metadata
    {
      new_topic_metadata.insert(
        new_feat_topic,
        domain::TopicMetadata::FeatureTopic {
          topic: new_feat_topic,
          name,
          description,
          author: Author::System,
          created_at: None,
        },
      );
    }
  }

  for (old_feat_topic, req_topics) in feature_requirement_links {
    let new_feat_topic = match id_remap.get(&old_feat_topic) {
      Some(t) => *t,
      None => {
        let t = topic::new_feature_topic(ids::allocate_feature_id());
        id_remap.insert(old_feat_topic, t);
        t
      }
    };
    new_feature_requirement_links.insert(new_feat_topic, req_topics);
  }

  for (old_feat_topic, beh_topics) in feature_behavior_links {
    let new_feat_topic = match id_remap.get(&old_feat_topic) {
      Some(t) => *t,
      None => {
        let t = topic::new_feature_topic(ids::allocate_feature_id());
        id_remap.insert(old_feat_topic, t);
        t
      }
    };
    new_feature_behavior_links.insert(new_feat_topic, beh_topics);
  }

  let mut ctx = state
    .data_context
    .lock()
    .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
  let audit_data = ctx.get_audit_mut(audit_id).ok_or_else(|| {
    PipelineError::AuditNotFound {
      audit_id: audit_id.to_string(),
    }
  })?;

  audit_data
    .topic_metadata
    .retain(|_, m| !matches!(m, domain::TopicMetadata::FeatureTopic { .. }));
  audit_data.feature_requirement_links.clear();
  audit_data.feature_behavior_links.clear();

  audit_data.topic_metadata.extend(new_topic_metadata);
  audit_data.feature_requirement_links = new_feature_requirement_links;
  audit_data.feature_behavior_links = new_feature_behavior_links;

  domain::rebuild_feature_context(audit_data);
  tracing::info!("Stored {} features in DataContext", feature_count);

  Ok(())
}

/// Extract behaviors from source code with functional semantics in context.
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn build_behaviors(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use crate::collaborator::agent::context;

  let contracts = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
    let audit_data =
      ctx
        .get_audit(audit_id)
        .ok_or_else(|| PipelineError::AuditNotFound {
          audit_id: audit_id.to_string(),
        })?;
    context::collect_contracts_for_behavior_extraction(audit_data)
  };

  if contracts.is_empty() {
    tracing::info!("No contracts found, skipping behavior extraction");
    return Ok(());
  }

  tracing::info!("Extracting behaviors from {} contracts", contracts.len());

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
      Ok(Err(e)) => tracing::error!("extract_behaviors failed: {}", e),
      Err(e) => tracing::error!("extract_behaviors panicked: {}", e),
    }
  }

  tracing::info!(
    "Extracted {} behaviors from {} contracts",
    all_behaviors.len(),
    contracts.len()
  );

  // Build in-memory metadata with allocated B ids.
  let mut new_metadata = std::collections::BTreeMap::new();

  for (member_topic, description) in &all_behaviors {
    let beh_topic = topic::new_behavior_topic(ids::allocate_behavior_id());

    new_metadata.insert(
      beh_topic,
      domain::TopicMetadata::BehaviorTopic {
        topic: beh_topic,
        description: description.clone(),
        member_topic: *member_topic,
        author: Author::System,
        created_at: None,
      },
    );
  }

  // Update in-memory state
  let mut ctx = state
    .data_context
    .lock()
    .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
  let audit_data = ctx.get_audit_mut(audit_id).ok_or_else(|| {
    PipelineError::AuditNotFound {
      audit_id: audit_id.to_string(),
    }
  })?;

  // Clear old behaviors
  audit_data
    .topic_metadata
    .retain(|_, m| !matches!(m, domain::TopicMetadata::BehaviorTopic { .. }));

  audit_data.topic_metadata.extend(new_metadata);
  domain::rebuild_feature_context(audit_data);

  tracing::info!(
    "Completed behavior extraction: {} behaviors",
    all_behaviors.len()
  );

  Ok(())
}

/// Build semantic links between documentation sections and code declarations.
/// Three passes: section→contracts, section×contract→members, section×member→semantics.
/// Each pass has a mechanical pre-step followed by LLM refinement.
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn build_semantic_links(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use crate::collaborator::agent::context;

  tracing::info!("Building semantic links for audit {}", audit_id);

  // Mechanical resolution (shared by passes 1 and 2)
  let (mechanical, sections, contracts) = {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
    let audit_data =
      ctx
        .get_audit(audit_id)
        .ok_or_else(|| PipelineError::AuditNotFound {
          audit_id: audit_id.to_string(),
        })?;

    let mechanical = context::mechanical_semantic_links(audit_data);
    let sections = task::collect_documentation_sections(audit_data);
    let contracts =
      context::render_contract_list_for_semantic_linking(audit_data);

    (mechanical, sections, contracts)
  };

  tracing::info!(
    "Mechanical: {} sections, {} contracts, {} section-contract links, {} section-declaration links",
    sections.len(),
    contracts.len(),
    mechanical.section_to_contracts.len(),
    mechanical.section_to_declarations.len(),
  );

  if sections.is_empty() || contracts.is_empty() {
    tracing::info!("No sections or contracts found, skipping semantic linking");
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
        .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
      let audit_data = ctx.get_audit(audit_id).ok_or_else(|| {
        PipelineError::AuditNotFound {
          audit_id: audit_id.to_string(),
        }
      })?;
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
    let st = *section_topic;
    let clj = contract_list_json.clone();
    pass1_handles.push(tokio::spawn(async move {
      task::semantic_link_pass1(&st, &section_text, &clj, &confirmed).await
    }));
  }

  tracing::info!(
    "Pass 1: {} sections with text, {} empty, {} LLM calls queued",
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
      Ok(Err(e)) => tracing::error!("semantic_link pass1 failed: {}", e),
      Err(e) => tracing::error!("semantic_link pass1 panicked: {}", e),
    }
  }

  tracing::info!(
    "Pass 1 complete: {} section-contract pairs",
    section_contracts.values().map(|v| v.len()).sum::<usize>()
  );

  // ---- Pass 2: section × contract → members (mechanical + LLM) ----
  let mut pass2_handles = Vec::new();
  for (section_topic, contract_topics) in &section_contracts {
    let (section_text, confirmed_members) = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
      let audit_data = ctx.get_audit(audit_id).ok_or_else(|| {
        PipelineError::AuditNotFound {
          audit_id: audit_id.to_string(),
        }
      })?;
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

      let st = *section_topic;
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
        let doc_members =
          section_doc_members.entry(result.section_topic).or_default();

        for mapping in result.member_mappings {
          // If no doc_topics, use the section topic as fallback
          let doc_topics = if mapping.doc_topics.is_empty() {
            vec![result.section_topic]
          } else {
            mapping.doc_topics
          };

          for dt in doc_topics {
            let entry = doc_members.entry(dt).or_default();
            if !entry.contains(&mapping.member_topic) {
              entry.push(mapping.member_topic);
            }
          }
        }
      }
      Ok(Err(e)) => tracing::error!("semantic_link pass2 failed: {}", e),
      Err(e) => tracing::error!("semantic_link pass2 panicked: {}", e),
    }
  }

  let total_doc_groups: usize =
    section_doc_members.values().map(|dm| dm.len()).sum();
  tracing::info!(
    "Pass 2 complete: {} doc-topic groups for pass3",
    total_doc_groups
  );

  // ---- Pass 3: semantics extraction (doc-first, code for disambiguation) ----
  // Batched by doc child section: for each (section, doc_topic) group, gather
  // all matched members' declarations and source, send one pass3 call.
  let mut all_links: Vec<domain::SemanticLink> = Vec::new();
  let mut pass3_handles = Vec::new();

  // (a) Member-scoped: batched by doc_topic groups from pass2
  for (section_topic, doc_member_map) in &section_doc_members {
    let section_text = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
      let audit_data = ctx.get_audit(audit_id).ok_or_else(|| {
        PipelineError::AuditNotFound {
          audit_id: audit_id.to_string(),
        }
      })?;
      context::render_section_text(section_topic, audit_data)
        .unwrap_or_default()
    };

    for (doc_topic, member_topics) in doc_member_map {
      let (declarations_json, source_code) = {
        let ctx = state
          .data_context
          .lock()
          .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
        let audit_data = ctx.get_audit(audit_id).ok_or_else(|| {
          PipelineError::AuditNotFound {
            audit_id: audit_id.to_string(),
          }
        })?;

        let decls = context::render_batched_member_declarations_for_semantics(
          member_topics,
          audit_data,
        );
        let source = context::render_batched_member_sources_for_semantics(
          member_topics,
          audit_data,
        );

        (decls, source)
      };

      if declarations_json == "[]" {
        continue;
      }

      let st = *section_topic;
      let stxt = section_text.clone();
      let fallback_dt = *doc_topic;
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
        .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
      let audit_data = ctx.get_audit(audit_id).ok_or_else(|| {
        PipelineError::AuditNotFound {
          audit_id: audit_id.to_string(),
        }
      })?;

      let stxt = context::render_section_text(section_topic, audit_data)
        .unwrap_or_default();
      let decls = context::render_batched_contract_declarations_for_semantics(
        contract_topics,
        audit_data,
      );
      let sigs = context::render_batched_contract_declaration_signatures(
        contract_topics,
        audit_data,
      );

      (stxt, decls, sigs)
    };

    if declarations_json == "[]" {
      continue;
    }

    let st = *section_topic;
    let fallback_dt = *section_topic;
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

  tracing::info!("Pass 3: {} LLM calls queued", pass3_handles.len());

  for handle in pass3_handles {
    match handle.await {
      Ok(Ok(result)) => all_links.extend(result.links),
      Ok(Err(e)) => tracing::error!("semantic_link pass3 failed: {}", e),
      Err(e) => tracing::error!("semantic_link pass3 panicked: {}", e),
    }
  }

  tracing::info!("Pass 3 complete: {} semantic links", all_links.len());

  // Resolve transitive topics before condensation so that semantics from
  // interface stubs are grouped with their base implementation. After this
  // step, all links carry the base (non-transitive) declaration topic.
  {
    let ctx = state
      .data_context
      .lock()
      .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
    let audit_data =
      ctx
        .get_audit(audit_id)
        .ok_or_else(|| PipelineError::AuditNotFound {
          audit_id: audit_id.to_string(),
        })?;

    for link in &mut all_links {
      if let Some(base) = audit_data
        .topic_metadata
        .get(&link.declaration_topic)
        .and_then(|m| m.transitive_topic())
      {
        link.declaration_topic = *base;
      }
    }
  }

  // Condense repetitive semantics — now grouped by base topic, so
  // transitive semantics are condensed alongside their base.
  let unique_declarations = {
    let mut decls = std::collections::BTreeSet::new();
    for link in &all_links {
      decls.insert(link.declaration_topic);
    }
    decls.len()
  };
  tracing::info!(
    "Condensing semantics: {} links across {} declarations",
    all_links.len(),
    unique_declarations
  );

  let mut by_declaration: std::collections::BTreeMap<
    topic::Topic,
    Vec<domain::SemanticLink>,
  > = std::collections::BTreeMap::new();
  for link in all_links {
    by_declaration
      .entry(link.declaration_topic)
      .or_default()
      .push(link);
  }

  let mut condense_handles = Vec::new();
  let mut pass_through: Vec<domain::SemanticLink> = Vec::new();
  let mut condense_count = 0usize;
  for (decl_topic, links) in &by_declaration {
    if links.len() <= 1 {
      pass_through.extend(links.iter().cloned());
    } else {
      let decl_id = decl_topic.id().to_string();
      let texts: Vec<String> =
        links.iter().map(|l| l.description.clone()).collect();
      let original_links = links.clone();
      let decl_topic = *decl_topic;
      condense_count += 1;
      condense_handles.push(tokio::spawn(async move {
        let result = task::condense_semantics(&decl_id, &texts).await;
        (decl_topic, original_links, result)
      }));
    }
  }

  tracing::info!(
    "Condensation: {} declarations need condensing, {} passed through",
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

          all_links.push(domain::SemanticLink {
            documentation_topics: doc_topics,
            declaration_topic: decl_topic,
            description: entry.text,
          });
        }
      }
      Ok((decl_topic, original_links, Err(e))) => {
        tracing::error!(
          "condense_semantics failed for {}: {}, keeping originals",
          decl_topic.id(),
          e
        );
        all_links.extend(original_links);
      }
      Err(e) => {
        tracing::error!("condense_semantics task panicked: {}", e);
      }
    }
  }

  tracing::info!("After condensation: {} semantic links", all_links.len());

  // Update in-memory state
  let mut ctx = state
    .data_context
    .lock()
    .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
  let audit_data = ctx.get_audit_mut(audit_id).ok_or_else(|| {
    PipelineError::AuditNotFound {
      audit_id: audit_id.to_string(),
    }
  })?;

  // Clear old functional-semantic metadata so repeated runs don't accumulate.
  audit_data.topic_metadata.retain(|_, m| {
    !matches!(m, domain::TopicMetadata::FunctionalSemanticTopic { .. })
  });

  // Populate FunctionalSemanticTopic entries in topic_metadata with P-topic
  // IDs allocated from the process-wide counter. Transitive topics have
  // already been resolved to their base topics before condensation, so
  // declaration_topic is always the base.
  let link_count = all_links.len();
  for link in all_links {
    let sem_topic = topic::new_functional_property_topic(
      ids::allocate_functional_semantic_id(),
    );

    audit_data.topic_metadata.insert(
      sem_topic,
      domain::TopicMetadata::FunctionalSemanticTopic {
        topic: sem_topic,
        description: link.description,
        declaration_topic: link.declaration_topic,
        documentation_topics: link.documentation_topics,
        author: Author::System,
        created_at: None,
      },
    );
  }

  // Rebuild the declaration_semantics reverse index from topic_metadata.
  domain::rebuild_feature_context(audit_data);

  tracing::info!(
    "Stored {} semantic links across {} declarations",
    link_count,
    audit_data.declaration_semantics.len()
  );

  Ok(())
}

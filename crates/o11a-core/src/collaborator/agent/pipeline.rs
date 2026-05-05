//! Orchestrates the analysis pipeline: requirement extraction, semantic
//! linking, behavior extraction, and feature synthesis via reconciliation.
//!
//! Functions in this module handle the full lifecycle of an agent-generated
//! result: running the LLM task and updating in-memory audit data. Pipeline
//! output lives only in `DataContext` — persistence of the pipeline's output
//! is handled by the caller (the `o11a-analyze` binary writes `audit.json`;
//! the server hydrates from the same report). Errors propagate as
//! [`PipelineError`] so callers (HTTP handlers, background tasks) can pattern
//! match on variants instead of parsing formatted strings.

use crate::collaborator::agent::semantic_linking::SemanticLinkingConfig;
use crate::collaborator::agent::task::{self, TaskError};
use crate::collaborator::models::Author;
use crate::domain::{self, DataContext, topic};
use crate::ids;

use std::path::PathBuf;
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
  /// Configuration for the semantic-linking step. The CLI populates this
  /// from `--semantic-linking-*` flags; only `mechanical_trace` is wired
  /// today.
  pub semantic_linking: SemanticLinkingConfig,
  /// Output directory for side effects that don't go into the main
  /// artifact (currently only `--semantic-linking-mechanical-trace`).
  pub output_dir: Option<PathBuf>,
}

impl PipelineState {
  /// Construct a `PipelineState` with default semantic-linking config and no
  /// side-output directory. Used by callers (HTTP handlers, tests) that
  /// don't need the trace mode.
  pub fn new(data_context: Arc<Mutex<DataContext>>) -> Self {
    PipelineState {
      data_context,
      semantic_linking: SemanticLinkingConfig::default(),
      output_dir: None,
    }
  }
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
/// Five steps that alternate between association (mechanical + BM25) and
/// synthesis (LLM); per-step condensation collapses any declaration that
/// gathered multiple links into a single one before the next step runs:
///
/// 1. **Step 1** — associate document sections to contracts (mechanical
///    anchors plus BM25 contract discovery).
/// 2. **Step 2** — add semantic links to contracts (LLM per section,
///    rendered with each contract's name + NatSpec + public-member names).
/// 3. **Step 3** — associate document sections to contract members
///    (mechanical seed plus BM25 member expansion within each anchored
///    contract).
/// 4. **Step 4** — add semantic links to contract members (member-scoped
///    batch covering function/modifier signatures + params/returns; plus
///    contract-scoped batch covering non-function component-scoped
///    declarations). Step 2 contract semantics are injected as context.
/// 5. **Step 5** — add semantic links to contract member bodies (locals
///    in `Scope::ContainingBlock`). Step 2 contract and step 4 member
///    semantics are injected as context.
///
/// One workflow for every section, regardless of `is_technical`. See
/// `docs/specs/semantic-linking.md`.
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn build_semantic_links(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use crate::collaborator::agent::context;
  use crate::collaborator::agent::semantic_linking::bm25;
  use crate::collaborator::agent::task::SemanticLinkStep;
  use crate::domain::{NamedTopicKind, TopicMetadata};
  use std::collections::{BTreeMap, HashMap};
  use std::time::Instant;

  let total_start = Instant::now();
  tracing::info!("Building semantic links for audit {}", audit_id);

  // Mechanical resolution + section-text rendering — the seed for every
  // section. Performed once up front so the lock isn't reacquired per step.
  let (mechanical, sections, contracts, section_texts) = {
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

    let mut section_texts: HashMap<topic::Topic, String> = HashMap::new();
    for st in &sections {
      let txt =
        context::render_section_text(st, audit_data).unwrap_or_default();
      section_texts.insert(*st, txt);
    }

    (mechanical, sections, contracts, section_texts)
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

  // The cross-step accumulator. Each synthesis step appends; per-step
  // condensation collapses any declaration with multiple links to one.
  let mut all_links: Vec<domain::SemanticLink> = Vec::new();

  // ----------------------------------------------------------------------
  // Step 1 — associate document sections to contracts
  // ----------------------------------------------------------------------
  let step1_start = Instant::now();
  let mut section_contracts: HashMap<
    topic::Topic,
    Vec<(topic::Topic, domain::MatchSource)>,
  > = HashMap::new();

  for (st, ctrs) in &mechanical.section_to_contracts {
    let v: Vec<_> = ctrs
      .iter()
      .map(|c| (*c, domain::MatchSource::Mechanical))
      .collect();
    section_contracts.insert(*st, v);
  }

  let mut bm25_step1_added = 0usize;
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

    for section_topic in &sections {
      let section_text = match section_texts.get(section_topic) {
        Some(s) if !s.is_empty() => s.as_str(),
        _ => continue,
      };
      let discovered = bm25::discover_top_k_contracts(
        section_text,
        audit_data,
        bm25::SummaryCorpusVariant::Body,
      );
      let entry = section_contracts.entry(*section_topic).or_default();
      for (ct, _score) in discovered {
        if !entry.iter().any(|(c, _)| *c == ct) {
          entry.push((ct, domain::MatchSource::Bm25));
          bm25_step1_added += 1;
        }
      }
    }
  }
  tracing::info!(
    "Step 1 complete in {:?}: {} section-contract pairs ({} added by BM25)",
    step1_start.elapsed(),
    section_contracts.values().map(|v| v.len()).sum::<usize>(),
    bm25_step1_added,
  );

  // ----------------------------------------------------------------------
  // Step 2 — add semantic links to contracts
  // ----------------------------------------------------------------------
  let step2_start = Instant::now();
  let mut step2_handles = Vec::new();
  for (section_topic, contract_pairs) in &section_contracts {
    if contract_pairs.is_empty() {
      continue;
    }
    let section_text = match section_texts.get(section_topic) {
      Some(s) if !s.is_empty() => s.clone(),
      _ => continue,
    };
    let contract_topics: Vec<topic::Topic> =
      contract_pairs.iter().map(|(t, _)| *t).collect();
    let batch_source = contract_pairs
      .iter()
      .map(|(_, s)| *s)
      .reduce(|a, b| a.merge(b))
      .unwrap_or(domain::MatchSource::Mechanical);

    let (declarations_json, source_summaries) = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
      let audit_data = ctx.get_audit(audit_id).ok_or_else(|| {
        PipelineError::AuditNotFound {
          audit_id: audit_id.to_string(),
        }
      })?;
      let decls = context::render_contract_entities_for_semantics(
        &contract_topics,
        audit_data,
      );
      let summaries = context::render_contract_summaries_for_semantics(
        &contract_topics,
        audit_data,
      );
      (decls, summaries)
    };

    if declarations_json == "[]" {
      continue;
    }

    let st = *section_topic;
    let fallback_dt = *section_topic;
    step2_handles.push(tokio::spawn(async move {
      task::link_step(
        SemanticLinkStep::Contracts,
        &st,
        &section_text,
        "",
        &declarations_json,
        &source_summaries,
        &fallback_dt,
        batch_source,
      )
      .await
    }));
  }

  tracing::info!("Step 2: {} LLM calls queued", step2_handles.len());
  for handle in step2_handles {
    match handle.await {
      Ok(Ok(result)) => all_links.extend(result.links),
      Ok(Err(e)) => tracing::error!("link_contracts (step 2) failed: {}", e),
      Err(e) => tracing::error!("link_contracts (step 2) panicked: {}", e),
    }
  }
  condense_in_place(&mut all_links, "step 2", state, audit_id).await?;
  tracing::info!(
    "Step 2 complete in {:?}: accumulator now holds {} links",
    step2_start.elapsed(),
    all_links.len()
  );

  // ----------------------------------------------------------------------
  // Step 3 — associate document sections to contract members
  // ----------------------------------------------------------------------
  let step3_start = Instant::now();
  let mut section_members: BTreeMap<
    topic::Topic,
    Vec<(topic::Topic, domain::MatchSource)>,
  > = BTreeMap::new();

  // Mechanical seed: walk each section's resolved declarations to their
  // containing members per contract.
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

    for (section_topic, contract_pairs) in &section_contracts {
      let section_decls = mechanical
        .section_to_declarations
        .get(section_topic)
        .cloned()
        .unwrap_or_default();

      let entry = section_members.entry(*section_topic).or_default();
      for (ct, _) in contract_pairs {
        let mems = context::mechanical_section_to_members(
          &section_decls,
          ct,
          audit_data,
        );
        for m in mems {
          if !entry.iter().any(|(t, _)| *t == m) {
            entry.push((m, domain::MatchSource::Mechanical));
          }
        }
      }
    }
  }

  // BM25 expansion per (section, contract).
  let mut bm25_step3_added = 0usize;
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

    for (section_topic, contract_pairs) in &section_contracts {
      let section_text = match section_texts.get(section_topic) {
        Some(s) if !s.is_empty() => s.as_str(),
        _ => continue,
      };

      let entry = section_members.entry(*section_topic).or_default();
      for (contract_topic, _) in contract_pairs {
        let new_members =
          bm25::expand_members(section_text, contract_topic, audit_data);
        for (m, _score) in new_members {
          if !entry.iter().any(|(t, _)| *t == m) {
            entry.push((m, domain::MatchSource::Bm25));
            bm25_step3_added += 1;
          }
        }
      }
    }
  }

  let total_member_pairs: usize =
    section_members.values().map(|v| v.len()).sum();
  tracing::info!(
    "Step 3 complete in {:?}: {} section-member pairs ({} added by BM25)",
    step3_start.elapsed(),
    total_member_pairs,
    bm25_step3_added,
  );

  // ----------------------------------------------------------------------
  // Step 4 — add semantic links to contract members (two batches)
  // ----------------------------------------------------------------------
  let step4_start = Instant::now();
  let mut step4_handles = Vec::new();

  // (a) Member-scoped batch: function/modifier topics with their
  //     params/returns. Filter section_members down to function/modifier
  //     kinds — the BM25 corpus also indexes events/errors/structs/enums/
  //     state vars, but those are handled by the contract-scoped batch.
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

    for (section_topic, member_pairs) in &section_members {
      let section_text = match section_texts.get(section_topic) {
        Some(s) if !s.is_empty() => s.clone(),
        _ => continue,
      };

      let function_pairs: Vec<&(topic::Topic, domain::MatchSource)> =
        member_pairs
          .iter()
          .filter(|(t, _)| {
            matches!(
              audit_data.topic_metadata.get(t),
              Some(TopicMetadata::NamedTopic {
                kind: NamedTopicKind::Function(_) | NamedTopicKind::Modifier,
                ..
              })
            )
          })
          .collect();

      if function_pairs.is_empty() {
        continue;
      }

      let member_topics: Vec<topic::Topic> =
        function_pairs.iter().map(|(t, _)| *t).collect();
      let batch_source = function_pairs
        .iter()
        .map(|(_, s)| *s)
        .reduce(|a, b| a.merge(b))
        .unwrap_or(domain::MatchSource::Mechanical);

      let declarations_json =
        context::render_member_signature_declarations_for_semantics(
          &member_topics,
          audit_data,
        );
      if declarations_json == "[]" {
        continue;
      }

      let signatures_source =
        context::render_member_signature_sources_for_semantics(
          &member_topics,
          audit_data,
        );

      // Prior context: the section's matched contracts' semantics.
      let containing_contracts: Vec<topic::Topic> = section_contracts
        .get(section_topic)
        .map(|pairs| pairs.iter().map(|(t, _)| *t).collect())
        .unwrap_or_default();
      let prior_block = context::render_prior_semantics_block(
        &containing_contracts,
        &all_links,
        audit_data,
      );

      let st = *section_topic;
      let fallback_dt = *section_topic;
      step4_handles.push(tokio::spawn(async move {
        task::link_step(
          SemanticLinkStep::MemberSignaturesFunctions,
          &st,
          &section_text,
          &prior_block,
          &declarations_json,
          &signatures_source,
          &fallback_dt,
          batch_source,
        )
        .await
      }));
    }
  }

  // (b) Contract-scoped batch: non-function component-scoped declarations
  //     for the section's matched contracts.
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

    for (section_topic, contract_pairs) in &section_contracts {
      let section_text = match section_texts.get(section_topic) {
        Some(s) if !s.is_empty() => s.clone(),
        _ => continue,
      };
      let contract_topics: Vec<topic::Topic> =
        contract_pairs.iter().map(|(t, _)| *t).collect();
      let batch_source = contract_pairs
        .iter()
        .map(|(_, s)| *s)
        .reduce(|a, b| a.merge(b))
        .unwrap_or(domain::MatchSource::Mechanical);

      let declarations_json =
        context::render_contract_level_declarations_for_semantics(
          &contract_topics,
          audit_data,
        );
      if declarations_json == "[]" {
        continue;
      }
      let signatures_source =
        context::render_contract_level_signatures_for_semantics(
          &contract_topics,
          audit_data,
        );
      let prior_block = context::render_prior_semantics_block(
        &contract_topics,
        &all_links,
        audit_data,
      );

      let st = *section_topic;
      let fallback_dt = *section_topic;
      step4_handles.push(tokio::spawn(async move {
        task::link_step(
          SemanticLinkStep::MemberSignaturesContractLevel,
          &st,
          &section_text,
          &prior_block,
          &declarations_json,
          &signatures_source,
          &fallback_dt,
          batch_source,
        )
        .await
      }));
    }
  }

  tracing::info!("Step 4: {} LLM calls queued", step4_handles.len());
  for handle in step4_handles {
    match handle.await {
      Ok(Ok(result)) => all_links.extend(result.links),
      Ok(Err(e)) => {
        tracing::error!("link_member_signatures (step 4) failed: {}", e)
      }
      Err(e) => {
        tracing::error!("link_member_signatures (step 4) panicked: {}", e)
      }
    }
  }
  condense_in_place(&mut all_links, "step 4", state, audit_id).await?;
  tracing::info!(
    "Step 4 complete in {:?}: accumulator now holds {} links",
    step4_start.elapsed(),
    all_links.len()
  );

  // ----------------------------------------------------------------------
  // Step 5 — add semantic links to contract member bodies (locals)
  // ----------------------------------------------------------------------
  let step5_start = Instant::now();
  let mut step5_handles = Vec::new();
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

    for (section_topic, member_pairs) in &section_members {
      let section_text = match section_texts.get(section_topic) {
        Some(s) if !s.is_empty() => s.clone(),
        _ => continue,
      };

      let function_pairs: Vec<&(topic::Topic, domain::MatchSource)> =
        member_pairs
          .iter()
          .filter(|(t, _)| {
            matches!(
              audit_data.topic_metadata.get(t),
              Some(TopicMetadata::NamedTopic {
                kind: NamedTopicKind::Function(_) | NamedTopicKind::Modifier,
                ..
              })
            )
          })
          .collect();

      if function_pairs.is_empty() {
        continue;
      }

      let member_topics: Vec<topic::Topic> =
        function_pairs.iter().map(|(t, _)| *t).collect();
      let batch_source = function_pairs
        .iter()
        .map(|(_, s)| *s)
        .reduce(|a, b| a.merge(b))
        .unwrap_or(domain::MatchSource::Mechanical);

      let declarations_json =
        context::render_member_body_local_declarations_for_semantics(
          &member_topics,
          audit_data,
        );
      if declarations_json == "[]" {
        continue;
      }

      let body_source = context::render_member_body_sources_for_semantics(
        &member_topics,
        audit_data,
      );

      // Prior context: every topic that lives inside any of the section's
      // matched contracts. This covers step 2's contract semantics, step
      // 4's member-scoped output (functions/modifiers + params/returns),
      // and step 4's contract-scoped output (state vars, events, errors,
      // struct/enum defs + fields/members) — all of which a body local
      // might reference.
      let containing_contracts: Vec<topic::Topic> = section_contracts
        .get(section_topic)
        .map(|pairs| pairs.iter().map(|(t, _)| *t).collect())
        .unwrap_or_default();
      let context_topic_list =
        context::topics_within_contracts(&containing_contracts, audit_data);
      let prior_block = context::render_prior_semantics_block(
        &context_topic_list,
        &all_links,
        audit_data,
      );

      let st = *section_topic;
      let fallback_dt = *section_topic;
      step5_handles.push(tokio::spawn(async move {
        task::link_step(
          SemanticLinkStep::MemberBodies,
          &st,
          &section_text,
          &prior_block,
          &declarations_json,
          &body_source,
          &fallback_dt,
          batch_source,
        )
        .await
      }));
    }
  }

  tracing::info!("Step 5: {} LLM calls queued", step5_handles.len());
  for handle in step5_handles {
    match handle.await {
      Ok(Ok(result)) => all_links.extend(result.links),
      Ok(Err(e)) => {
        tracing::error!("link_member_bodies (step 5) failed: {}", e)
      }
      Err(e) => {
        tracing::error!("link_member_bodies (step 5) panicked: {}", e)
      }
    }
  }
  condense_in_place(&mut all_links, "step 5", state, audit_id).await?;
  tracing::info!(
    "Step 5 complete in {:?}: accumulator now holds {} links",
    step5_start.elapsed(),
    all_links.len()
  );

  // ----------------------------------------------------------------------
  // Final write — clear FunctionalSemanticTopic entries and store the
  // condensed links. The lock is scoped to this block so the MutexGuard is
  // dropped before any subsequent `.await`.
  // ----------------------------------------------------------------------
  {
    let mut ctx = state
      .data_context
      .lock()
      .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
    let audit_data = ctx.get_audit_mut(audit_id).ok_or_else(|| {
      PipelineError::AuditNotFound {
        audit_id: audit_id.to_string(),
      }
    })?;

    audit_data.topic_metadata.retain(|_, m| {
      !matches!(m, domain::TopicMetadata::FunctionalSemanticTopic { .. })
    });

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
          match_source: Some(link.match_source),
        },
      );
    }

    domain::rebuild_feature_context(audit_data);

    tracing::info!(
      "Stored {} semantic links across {} declarations",
      link_count,
      audit_data.declaration_semantics.len()
    );
  }

  tracing::info!("Semantic linking complete in {:?}", total_start.elapsed());

  Ok(())
}

/// Per-step in-place condensation. Resolves transitive topics, then groups
/// the accumulator by declaration topic; any group of size > 1 fires a
/// `condense_semantics` LLM call and is replaced with the condensed
/// entries. Single-link groups pass through unchanged. Match-source merging
/// follows `MatchSource::merge` (mechanical > bm25).
async fn condense_in_place(
  links: &mut Vec<domain::SemanticLink>,
  step_label: &str,
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use std::collections::BTreeMap;

  // 1. Resolve transitive topics so that interface-stub semantics group
  //    with their base implementation in step 4/5 condensation.
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
    for link in links.iter_mut() {
      if let Some(base) = audit_data
        .topic_metadata
        .get(&link.declaration_topic)
        .and_then(|m| m.transitive_topic())
      {
        link.declaration_topic = *base;
      }
    }
  }

  // 2. Group by declaration topic.
  let mut by_topic: BTreeMap<topic::Topic, Vec<domain::SemanticLink>> =
    BTreeMap::new();
  for link in links.drain(..) {
    by_topic
      .entry(link.declaration_topic)
      .or_default()
      .push(link);
  }

  // 3. Pass-through singletons; spawn condense calls for the rest.
  let mut handles = Vec::new();
  let mut pass_through: Vec<domain::SemanticLink> = Vec::new();
  let mut condense_count = 0usize;
  for (decl_topic, group) in by_topic {
    if group.len() <= 1 {
      pass_through.extend(group);
      continue;
    }
    condense_count += 1;
    let decl_id = decl_topic.id().to_string();
    let texts: Vec<String> =
      group.iter().map(|l| l.description.clone()).collect();
    handles.push(tokio::spawn(async move {
      let result = task::condense_semantics(&decl_id, &texts).await;
      (decl_topic, group, result)
    }));
  }

  if condense_count > 0 {
    tracing::info!(
      "{}: condensing {} declarations with multiple links",
      step_label,
      condense_count
    );
  }

  *links = pass_through;
  for handle in handles {
    match handle.await {
      Ok((decl_topic, originals, Ok(condensed))) => {
        for entry in condensed {
          links.push(merge_condensed_entry(decl_topic, &originals, &entry));
        }
      }
      Ok((decl_topic, originals, Err(e))) => {
        tracing::error!(
          "{}: condense_semantics failed for {}: {}, keeping originals",
          step_label,
          decl_topic.id(),
          e
        );
        links.extend(originals);
      }
      Err(e) => {
        tracing::error!(
          "{}: condense_semantics task panicked: {}",
          step_label,
          e
        );
      }
    }
  }

  Ok(())
}

/// Build one `SemanticLink` from a condensed-semantics LLM result entry by
/// merging the documentation topics and match sources of every source link
/// it cites.
///
/// Defensive behaviors:
/// - Source indices outside `originals.len()` are silently skipped (LLM
///   sometimes returns 1-based indices we already converted, or indices
///   from a stale prompt).
/// - `documentation_topics` is sorted before dedup so non-adjacent
///   duplicates collapse correctly.
/// - If the entry's source indices yield no documentation topics at all,
///   fall back to the first original's topics so the resulting link still
///   carries provenance.
/// - `match_source` is merged with `MatchSource::merge` (mechanical >
///   bm25); if no valid sources, fall back to the first original's source.
fn merge_condensed_entry(
  decl_topic: topic::Topic,
  originals: &[domain::SemanticLink],
  entry: &task::CondensedSemantic,
) -> domain::SemanticLink {
  let mut doc_topics: Vec<topic::Topic> = entry
    .source_indices
    .iter()
    .filter_map(|&i| originals.get(i))
    .flat_map(|l| l.documentation_topics.iter().cloned())
    .collect();
  doc_topics.sort();
  doc_topics.dedup();
  if doc_topics.is_empty() {
    doc_topics = originals[0].documentation_topics.clone();
  }
  let merged_source = entry
    .source_indices
    .iter()
    .filter_map(|&i| originals.get(i))
    .map(|l| l.match_source)
    .reduce(|a, b| a.merge(b))
    .unwrap_or(originals[0].match_source);
  domain::SemanticLink {
    documentation_topics: doc_topics,
    declaration_topic: decl_topic,
    description: entry.text.clone(),
    match_source: merged_source,
  }
}

#[cfg(test)]
mod merge_condensed_entry_tests {
  use super::*;
  use crate::collaborator::agent::task::CondensedSemantic;
  use crate::domain::{MatchSource, SemanticLink};

  fn link(
    decl: i32,
    docs: &[i32],
    desc: &str,
    src: MatchSource,
  ) -> SemanticLink {
    SemanticLink {
      documentation_topics: docs
        .iter()
        .copied()
        .map(topic::new_documentation_topic)
        .collect(),
      declaration_topic: topic::new_node_topic(&decl),
      description: desc.to_string(),
      match_source: src,
    }
  }

  #[test]
  fn merges_doc_topics_with_sort_then_dedup() {
    // Three originals reference docs out of order with duplicates that
    // are non-adjacent in flat_map output: [10, 20], [10, 30], [20, 30].
    // Adjacency-only dedup would leave duplicate 10 and 20.
    let originals = vec![
      link(100, &[10, 20], "first", MatchSource::Bm25),
      link(100, &[10, 30], "second", MatchSource::Bm25),
      link(100, &[20, 30], "third", MatchSource::Bm25),
    ];
    let entry = CondensedSemantic {
      text: "merged".to_string(),
      source_indices: vec![0, 1, 2],
    };
    let merged =
      merge_condensed_entry(topic::new_node_topic(&100), &originals, &entry);
    let ids: Vec<String> = merged
      .documentation_topics
      .iter()
      .map(|t| t.id().to_string())
      .collect();
    assert_eq!(ids, vec!["D10", "D20", "D30"]);
  }

  #[test]
  fn match_source_merge_promotes_mechanical_over_bm25() {
    let originals = vec![
      link(100, &[10], "a", MatchSource::Bm25),
      link(100, &[10], "b", MatchSource::Mechanical),
      link(100, &[10], "c", MatchSource::Bm25),
    ];
    let entry = CondensedSemantic {
      text: "merged".to_string(),
      source_indices: vec![0, 1, 2],
    };
    let merged =
      merge_condensed_entry(topic::new_node_topic(&100), &originals, &entry);
    assert_eq!(merged.match_source, MatchSource::Mechanical);
  }

  #[test]
  fn out_of_range_source_indices_are_skipped() {
    let originals = vec![link(100, &[10], "a", MatchSource::Bm25)];
    let entry = CondensedSemantic {
      text: "merged".to_string(),
      source_indices: vec![0, 99, 100],
    };
    let merged =
      merge_condensed_entry(topic::new_node_topic(&100), &originals, &entry);
    assert_eq!(merged.documentation_topics.len(), 1);
    assert_eq!(merged.match_source, MatchSource::Bm25);
  }

  #[test]
  fn falls_back_to_first_original_when_no_indices_resolve() {
    let originals = vec![link(100, &[42, 7], "a", MatchSource::Mechanical)];
    let entry = CondensedSemantic {
      text: "merged".to_string(),
      source_indices: vec![99, 100], // all out of range
    };
    let merged =
      merge_condensed_entry(topic::new_node_topic(&100), &originals, &entry);
    let ids: Vec<String> = merged
      .documentation_topics
      .iter()
      .map(|t| t.id().to_string())
      .collect();
    // Falls back to originals[0]'s topics in their original order — no
    // sort is applied because we didn't compute the merged set.
    assert_eq!(ids, vec!["D42", "D7"]);
    assert_eq!(merged.match_source, MatchSource::Mechanical);
  }
}

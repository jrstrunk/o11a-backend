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

/// Run the full analysis pipeline in seven steps:
///
/// 1. **Semantic Linking** — establish functional semantics on declarations.
/// 2. **Requirement Extraction** — pull documentation requirements with
///    semantics in context.
/// 3. **Behavior Extraction** — DAG-batched per-function behavior generation
///    with callee context.
/// 4. **Feature Synthesis** — reconcile requirements and behaviors.
/// 5. **Functional Purpose & Placement** — for every non-pure subject in
///    every in-scope function with a feature link, generate purpose and
///    placement rationale (per-function).
/// 6. **Condition Generation** — for every non-pure subject with a purpose
///    and placement, generate the assertions that must hold for that
///    purpose and placement to be fulfilled (per-function). Each condition
///    is its own A-prefixed topic.
/// 7. **Threat Generation** — for every condition on every non-pure subject,
///    generate adversarial scenarios that falsify the assertion (per-
///    function). Each threat is its own A-prefixed topic that names exactly
///    one `falsifies_condition` and one `controlled_by` actor; one condition
///    can be the target of many threats. Step 8 (invariants) will derive
///    codebase-level defenses from these threats.
///
/// Semantic linking runs first so functional semantics are available when
/// rendering documentation for requirement extraction — inline code
/// references like `pID` get annotated with their project-specific meaning.
/// Step 5 runs after feature synthesis so it can use both feature context
/// (from step 4) and prior callee behaviors (from step 3); step 6 runs
/// after step 5 because every condition is grounded in a subject's
/// functional purpose and placement rationale; step 7 runs after step 6
/// because every threat is the adversarial inversion of a specific
/// condition.
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn run_full_pipeline(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  tracing::info!("Starting full analysis pipeline for audit {}", audit_id);

  tracing::info!("[1/7] Semantic Linking");
  build_semantic_links(state, audit_id).await?;

  tracing::info!("[2/7] Requirement Extraction");
  build_requirements(state, audit_id).await?;

  tracing::info!("[3/7] Behavior Extraction");
  build_behaviors(state, audit_id).await?;

  tracing::info!("[4/7] Feature Synthesis");
  synthesize_features(state, audit_id).await?;

  tracing::info!("[5/7] Functional Purpose & Placement Generation");
  build_functional_properties(state, audit_id).await?;

  tracing::info!("[6/7] Condition Generation");
  build_conditions(state, audit_id).await?;

  tracing::info!("[7/7] Threat Generation");
  build_threats(state, audit_id).await?;

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

/// Extract behaviors from source code, batched along the project-wide
/// call DAG. Earlier batches contain callees; their behaviors are
/// stored in `DataContext` before the next layer runs, so each batch
/// can render `called_function_behaviors` from prior layers' output.
/// See `crates/o11a-analyze/docs/build-plans/pipeline-dag.md`.
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn build_behaviors(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use crate::collaborator::agent::{context, function_dag};

  // Clear any prior behaviors before re-running. We do this up front so
  // re-runs don't accidentally feed last run's behaviors as callee
  // context for this run's earliest batches.
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
    audit_data
      .topic_metadata
      .retain(|_, m| !matches!(m, domain::TopicMetadata::BehaviorTopic { .. }));
    domain::rebuild_feature_context(audit_data);
  }

  // Build batches once. The DAG is a function of analyzer state, which
  // doesn't change during this step.
  let batches = {
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
    function_dag::build_batches(audit_data)
  };

  if batches.is_empty() {
    tracing::info!("No in-scope functions found, skipping behavior extraction");
    return Ok(());
  }

  // Group batches into DAG layers so callees finish before callers.
  // Within a layer, batches run concurrently; layers run sequentially.
  // Layer assignment is implicit: `build_batches` already orders batches
  // such that any prefix is a valid completion order. We accumulate
  // results in DAG order by running batches in the order returned and
  // committing after each batch — simpler than reconstructing layers.
  let total_batches = batches.len();
  tracing::info!(
    "Extracting behaviors from {} batches (DAG-ordered)",
    total_batches
  );

  let mut total_behaviors: usize = 0;
  for (idx, batch) in batches.into_iter().enumerate() {
    // Render the batch with current callee behaviors (already committed).
    let rendered = {
      let ctx = state
        .data_context
        .lock()
        .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
      let audit_data = ctx.get_audit(audit_id).ok_or_else(|| {
        PipelineError::AuditNotFound {
          audit_id: audit_id.to_string(),
        }
      })?;
      context::render_batch_for_extraction(&batch.members, audit_data)
    };

    let Some(rendered) = rendered else {
      tracing::debug!(
        "Batch {}/{} has no renderable members, skipping",
        idx + 1,
        total_batches
      );
      continue;
    };

    let parsed =
      match task::extract_behaviors_from_batch(&rendered.json, &rendered.label)
        .await
      {
        Ok(p) => p,
        Err(e) => {
          tracing::error!(
            "extract_behaviors_from_batch failed for batch {}/{} ({}): {}",
            idx + 1,
            total_batches,
            rendered.label,
            e
          );
          continue;
        }
      };

    // Commit this batch's behaviors before rendering the next batch so
    // downstream batches see them in `called_function_behaviors`.
    let added = parsed.behaviors.len();
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
      for (member_topic, description) in parsed.behaviors {
        let beh_topic = topic::new_behavior_topic(ids::allocate_behavior_id());
        audit_data.topic_metadata.insert(
          beh_topic,
          domain::TopicMetadata::BehaviorTopic {
            topic: beh_topic,
            description,
            member_topic,
            author: Author::System,
            created_at: None,
          },
        );
      }
      domain::rebuild_feature_context(audit_data);
    }
    total_behaviors += added;
    tracing::debug!(
      "Batch {}/{} ({}): {} behaviors",
      idx + 1,
      total_batches,
      rendered.label,
      added
    );
  }

  tracing::info!(
    "Completed behavior extraction: {} behaviors across {} batches",
    total_behaviors,
    total_batches
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

      // Step 4a wants pure signatures (bodies stripped) since step 5 will
      // see the bodies. The sentinel target_topic ensures
      // `omit_function_and_modifier_bodies` actually applies — using
      // `*member_topic` as target would re-expand the body via the
      // per-member override.
      let signature_ctx = context::ASTRenderContext {
        target_topic: topic::new_node_topic(&-1),
        omit_function_and_modifier_bodies: true,
        include_untrusted_comments: true,
      };
      let signatures_source: String = member_topics
        .iter()
        .filter_map(|mt| {
          context::render_member_for_agent(mt, &signature_ctx, audit_data)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

      let st = *section_topic;
      let fallback_dt = *section_topic;
      step4_handles.push(tokio::spawn(async move {
        task::link_step(
          SemanticLinkStep::MemberSignaturesFunctions,
          &st,
          &section_text,
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
      // Step 4b renders contract-level non-function declarations only —
      // `omit_function_and_modifier_bodies` is moot here (functions are
      // filtered out anyway) but we still want NatSpec/inline comments
      // visible for the LLM.
      let contract_level_ctx = context::ASTRenderContext {
        target_topic: topic::new_node_topic(&-1),
        omit_function_and_modifier_bodies: true,
        include_untrusted_comments: true,
      };
      let signatures_source: String = contract_topics
        .iter()
        .map(|ct| {
          context::render_contract_non_function_members_for_agent(
            ct,
            &contract_level_ctx,
            audit_data,
          )
        })
        .filter(|s| s != "[]" && !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
      let st = *section_topic;
      let fallback_dt = *section_topic;
      step4_handles.push(tokio::spawn(async move {
        task::link_step(
          SemanticLinkStep::MemberSignaturesContractLevel,
          &st,
          &section_text,
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

      // Step 5 needs full bodies — locals only make sense in their
      // executing context. `target_topic = *mt` is fine here because
      // `omit_function_and_modifier_bodies` is already false; the
      // is_target override is moot when we're not stripping bodies.
      let body_source: String = member_topics
        .iter()
        .filter_map(|mt| {
          let body_ctx = context::ASTRenderContext {
            target_topic: *mt,
            omit_function_and_modifier_bodies: false,
            include_untrusted_comments: true,
          };
          context::render_member_for_agent(mt, &body_ctx, audit_data)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

      let st = *section_topic;
      let fallback_dt = *section_topic;
      step5_handles.push(tokio::spawn(async move {
        task::link_step(
          SemanticLinkStep::MemberBodies,
          &st,
          &section_text,
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
        ids::allocate_functional_property_id(),
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

/// Generate functional purpose and placement rationale for every non-pure
/// subject in every in-scope function/modifier with a feature link. Reuses
/// the same DAG batches as behavior extraction so callee context (already-
/// extracted behaviors) is available to the LLM. Members without a feature
/// link or without any non-pure subjects are skipped — featureless members
/// are logged as a reconciliation gap. See pipeline-dag.md step 5.
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn build_functional_properties(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use crate::collaborator::agent::{context, function_dag};

  // Clear any prior FunctionalPurposeTopic / PlacementRationaleTopic
  // entries so re-runs don't accumulate stale generations. Sibling
  // FunctionalSemanticTopic entries are preserved — they're outputs of a
  // different pipeline step.
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
      !matches!(
        m,
        domain::TopicMetadata::FunctionalPurposeTopic { .. }
          | domain::TopicMetadata::PlacementRationaleTopic { .. }
      )
    });
    domain::rebuild_feature_context(audit_data);
  }

  // Reuse the DAG batches from behavior extraction to enumerate members
  // in DAG-respecting order, then iterate each batch's members flat —
  // step 5 generates per-subject output, so the LLM call granularity is
  // per-function, not per-batch. Affinity batching is bypassed; layer
  // ordering is preserved (callees still appear in earlier batches than
  // callers, so by the time we render any caller, callee behaviors are
  // already committed by step 3).
  let batches = {
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
    function_dag::build_batches(audit_data)
  };

  if batches.is_empty() {
    tracing::info!(
      "No in-scope functions found, skipping functional property generation"
    );
    return Ok(());
  }

  // Render every eligible member up front under a single lock acquisition,
  // counting featureless members for the reconciliation-gap report.
  // Members without eligible subjects (no feature link, or no non-pure
  // subjects) are dropped here so they don't take a parallelism slot.
  let mut rendered_members: Vec<context::BatchForExtraction> = Vec::new();
  let mut total_skipped_no_feature: usize = 0;
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
    for batch in &batches {
      for member in &batch.members {
        if !context::member_has_feature_link(member, audit_data) {
          total_skipped_no_feature += 1;
          continue;
        }
        if let Some(rendered) =
          context::render_batch_for_extraction(&[*member], audit_data)
        {
          if rendered.non_pure_subjects.is_empty() {
            // Pure-only function: nothing to ask the LLM about. The
            // unified renderer is step-agnostic and renders pure-only
            // members for step 3 (behaviors); step 5 filters them here.
            continue;
          }
          rendered_members.push(rendered);
        }
      }
    }
  }

  let total_members = rendered_members.len();
  tracing::info!(
    "Generating functional properties for {} member(s) (per-function, in parallel)",
    total_members
  );

  // Per-member calls have no inter-member dependencies — each generates
  // its own subjects from already-committed feature and behavior context.
  // Spawn all LLM calls concurrently.
  let mut handles = Vec::new();
  for rendered in rendered_members {
    handles.push(tokio::spawn(async move {
      let result = task::extract_functional_properties_from_batch(
        &rendered.json,
        &rendered.label,
      )
      .await;
      (rendered.label, result)
    }));
  }

  let mut all_entries: Vec<task::ParsedSubjectFunctionalProperties> =
    Vec::new();
  for handle in handles {
    match handle.await {
      Ok((_label, Ok(parsed))) => all_entries.extend(parsed.entries),
      Ok((label, Err(e))) => tracing::error!(
        "extract_functional_properties_from_batch failed for {}: {}",
        label,
        e
      ),
      Err(e) => tracing::error!(
        "extract_functional_properties_from_batch panicked: {}",
        e
      ),
    }
  }

  let total_subjects = all_entries.len();
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
    for entry in all_entries {
      let purpose_topic = topic::new_functional_property_topic(
        ids::allocate_functional_property_id(),
      );
      audit_data.topic_metadata.insert(
        purpose_topic,
        domain::TopicMetadata::FunctionalPurposeTopic {
          topic: purpose_topic,
          description: entry.functional_purpose,
          subject_topic: entry.subject_topic,
          author: Author::System,
          created_at: None,
        },
      );
      let placement_topic = topic::new_functional_property_topic(
        ids::allocate_functional_property_id(),
      );
      audit_data.topic_metadata.insert(
        placement_topic,
        domain::TopicMetadata::PlacementRationaleTopic {
          topic: placement_topic,
          description: entry.placement_rationale,
          subject_topic: entry.subject_topic,
          author: Author::System,
          created_at: None,
        },
      );
    }
    domain::rebuild_feature_context(audit_data);
  }

  if total_skipped_no_feature > 0 {
    tracing::warn!(
      "Skipped {} member(s) with no feature link \u{2014} reconciliation gap",
      total_skipped_no_feature
    );
  }
  tracing::info!(
    "Completed functional property generation: {} subjects across {} member(s)",
    total_subjects,
    total_members
  );

  Ok(())
}

/// For every non-pure subject in every in-scope, feature-linked function or
/// modifier, generate a list of **conditions** — assertions that must hold
/// for the subject's functional purpose and placement rationale to be
/// fulfilled. Step 7 (threats) generates adversarial scenarios that
/// falsify these assertions. One LLM call per function (mirrors step 5's
/// per-function granularity); one A-prefixed `ConditionTopic` per
/// assertion (subjects typically produce 1–8). Requires step 5 output:
/// the renderer inlines `functional_purpose` and `placement_rationale` on
/// each non-pure subject so the LLM grounds conditions in purpose +
/// placement. If step 5 produced nothing, this step skips cleanly. See
/// SPEC's "Conditions vs. Invariants" for the role distinction with
/// invariants (which are scope-organized defenses derived from threats).
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn build_conditions(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use crate::collaborator::agent::{context, function_dag};

  // Clear any prior ConditionTopic entries so re-runs don't accumulate
  // stale generations. Sibling FunctionalPurposeTopic /
  // PlacementRationaleTopic entries are preserved — they're outputs of
  // step 5, which this step depends on.
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
      !matches!(m, domain::TopicMetadata::ConditionTopic { .. })
    });
    domain::rebuild_feature_context(audit_data);

    // Conditions are downstream of step 5: every condition is grounded in
    // a non-pure subject's functional purpose. If step 5 produced nothing
    // there is nothing to ground against — exit cleanly without spawning
    // any LLM calls.
    if audit_data.subject_purposes.is_empty() {
      tracing::info!(
        "No functional purposes found, skipping condition generation"
      );
      return Ok(());
    }
  }

  // Reuse the DAG batches from behavior extraction to enumerate members
  // in DAG-respecting order, then iterate each batch's members flat —
  // step 6 is per-function, like step 5.
  let batches = {
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
    function_dag::build_batches(audit_data)
  };

  if batches.is_empty() {
    tracing::info!(
      "No in-scope functions found, skipping condition generation"
    );
    return Ok(());
  }

  // Render every eligible member up front under a single lock acquisition,
  // counting featureless members for the reconciliation-gap report.
  // Members without eligible subjects (no feature link, or no non-pure
  // subjects) are dropped here so they don't take a parallelism slot.
  let mut rendered_members: Vec<context::BatchForExtraction> = Vec::new();
  let mut total_skipped_no_feature: usize = 0;
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
    for batch in &batches {
      for member in &batch.members {
        if !context::member_has_feature_link(member, audit_data) {
          total_skipped_no_feature += 1;
          continue;
        }
        if let Some(rendered) =
          context::render_batch_for_extraction(&[*member], audit_data)
        {
          if rendered.non_pure_subjects.is_empty() {
            continue;
          }
          rendered_members.push(rendered);
        }
      }
    }
  }

  let total_members = rendered_members.len();
  tracing::info!(
    "Generating conditions for {} member(s) (per-function, in parallel)",
    total_members
  );

  // Per-member calls have no inter-member dependencies — each generates
  // its own conditions from already-committed feature, behavior, and
  // purpose+placement context. Spawn all LLM calls concurrently.
  let mut handles = Vec::new();
  for rendered in rendered_members {
    handles.push(tokio::spawn(async move {
      let result =
        task::extract_conditions_from_batch(&rendered.json, &rendered.label)
          .await;
      (rendered.label, result)
    }));
  }

  let mut all_entries: Vec<task::ParsedSubjectConditions> = Vec::new();
  for handle in handles {
    match handle.await {
      Ok((_label, Ok(parsed))) => all_entries.extend(parsed.entries),
      Ok((label, Err(e))) => tracing::error!(
        "extract_conditions_from_batch failed for {}: {}",
        label,
        e
      ),
      Err(e) => {
        tracing::error!("extract_conditions_from_batch panicked: {}", e)
      }
    }
  }

  let total_subjects = all_entries.len();
  let total_conditions: usize =
    all_entries.iter().map(|e| e.conditions.len()).sum();
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
    for entry in all_entries {
      for cond in entry.conditions {
        let cond_topic = topic::new_adversarial_property_topic(
          ids::allocate_adversarial_property_id(),
        );
        audit_data.topic_metadata.insert(
          cond_topic,
          domain::TopicMetadata::ConditionTopic {
            topic: cond_topic,
            description: cond.description,
            subject_topic: entry.subject_topic,
            kind: cond.kind,
            evidence_topics: cond.evidence_topics,
            author: Author::System,
            created_at: None,
          },
        );
      }
    }
    domain::rebuild_feature_context(audit_data);
  }

  if total_skipped_no_feature > 0 {
    tracing::warn!(
      "Skipped {} member(s) with no feature link \u{2014} reconciliation gap",
      total_skipped_no_feature
    );
  }
  tracing::info!(
    "Completed condition generation: {} conditions across {} subject(s) in \
     {} member(s)",
    total_conditions,
    total_subjects,
    total_members
  );

  Ok(())
}

/// For every condition on every non-pure subject in every in-scope, feature-
/// linked function or modifier, generate a list of **threats** — adversarial
/// scenarios in which the named assertion fails to hold. Each threat is its
/// own A-prefixed `ThreatTopic` that names exactly one `falsifies_condition`
/// and one `controlled_by` actor; one condition can be the target of many
/// threats. One LLM call per function (mirrors step 6's per-function
/// granularity). Requires step 6 output: the renderer inlines `conditions`
/// on each non-pure subject so the LLM grounds threats in concrete
/// assertions. If step 6 produced nothing, this step skips cleanly.
///
/// Reruns proactively clear downstream invariant data: `InvariantTopic`
/// entries are dropped from `topic_metadata` and `audit_data.invariants`
/// is emptied — a deleted threat orphans its invariants, and the audit
/// data must be internally consistent at step boundaries. Orphaned
/// `threat_feature_links` (impact-analysis entries whose `threat_topic`
/// no longer exists) are pruned; surviving non-orphaned links are kept,
/// though in practice A-prefix reallocation on rerun makes most links
/// orphaned. `no_threat_rationale` entries (the LLM's explicit "no
/// falsifier exists" response) are posted as agent-authored comments on
/// the condition topic so the audit signal persists in the auditor-
/// visible discussion thread. See SPEC's "Conditions vs. Invariants"
/// for the role distinction with invariants (step 8).
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn build_threats(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use crate::collaborator::agent::{context, function_dag};
  use crate::collaborator::synthetic;
  use crate::domain::CommentType;

  // Clear any prior ThreatTopic entries so re-runs don't accumulate stale
  // generations. Also proactively clear InvariantTopic entries and the
  // `audit_data.invariants` denormalization: a deleted threat orphans its
  // invariants, and the audit data must be internally consistent at step
  // boundaries (step 8 will repopulate). Prune `threat_feature_links` of
  // any entry whose `threat_topic` no longer exists in `topic_metadata`
  // after the clear — non-orphaned links survive so impact-analysis state
  // re-attaches across re-runs that preserve threat topic IDs. Also pull
  // the security_notes string so the LLM call can include it as system
  // context. Early-return if step 6 produced nothing: threats are
  // downstream of conditions, so without conditions there is nothing to
  // invert.
  let security_notes: Option<String> = {
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
      !matches!(
        m,
        domain::TopicMetadata::ThreatTopic { .. }
          | domain::TopicMetadata::InvariantTopic { .. }
      )
    });
    audit_data.invariants.clear();
    // Split the borrow: `mem::take` swaps the vec out so the
    // `topic_metadata` lookup inside `retain` doesn't conflict with the
    // mutable borrow on `threat_feature_links` that retain requires.
    let mut links = std::mem::take(&mut audit_data.threat_feature_links);
    links
      .retain(|link| audit_data.topic_metadata.contains_key(&link.threat_topic));
    audit_data.threat_feature_links = links;
    domain::rebuild_feature_context(audit_data);

    if audit_data.subject_conditions.is_empty() {
      tracing::info!("No conditions found, skipping threat generation");
      return Ok(());
    }

    audit_data.security_notes.clone()
  };

  // Reuse the DAG batches from behavior extraction to enumerate members
  // in DAG-respecting order, then iterate each batch's members flat —
  // step 7 is per-function, like step 5/6.
  let batches = {
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
    function_dag::build_batches(audit_data)
  };

  if batches.is_empty() {
    tracing::info!("No in-scope functions found, skipping threat generation");
    return Ok(());
  }

  // Render every eligible member up front under a single lock acquisition.
  // Members without eligible subjects (no feature link, no non-pure
  // subjects, or no conditions across any non-pure subject) are dropped
  // here so they don't take a parallelism slot — there is nothing for the
  // LLM to invert against.
  let mut rendered_members: Vec<context::BatchForExtraction> = Vec::new();
  let mut total_skipped_no_feature: usize = 0;
  let mut total_skipped_no_conditions: usize = 0;
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
    for batch in &batches {
      for member in &batch.members {
        if !context::member_has_feature_link(member, audit_data) {
          total_skipped_no_feature += 1;
          continue;
        }
        if let Some(rendered) =
          context::render_batch_for_extraction(&[*member], audit_data)
        {
          if rendered.non_pure_subjects.is_empty() {
            continue;
          }
          // Skip when no subject on this function has any conditions —
          // step 6 left nothing for step 7 to invert. Without this gate
          // a function with only pure-purpose subjects would burn an
          // LLM call producing an empty response.
          let any_subject_has_conditions =
            rendered.non_pure_subjects.iter().any(|st| {
              audit_data
                .subject_conditions
                .get(st)
                .is_some_and(|v| !v.is_empty())
            });
          if !any_subject_has_conditions {
            total_skipped_no_conditions += 1;
            continue;
          }
          rendered_members.push(rendered);
        }
      }
    }
  }

  let total_members = rendered_members.len();
  tracing::info!(
    "Generating threats for {} member(s) (per-function, in parallel)",
    total_members
  );

  // Per-member calls have no inter-member dependencies — each generates
  // threats from the inline conditions already stamped on every non-pure
  // subject by step 6's renderer hook. Spawn all LLM calls concurrently.
  // `security_notes` is cloned per call so each spawned future owns its
  // copy without cross-task borrowing.
  let mut handles = Vec::new();
  for rendered in rendered_members {
    let notes = security_notes.clone();
    handles.push(tokio::spawn(async move {
      let result = task::extract_threats_from_batch(
        &rendered.json,
        &rendered.label,
        notes.as_deref(),
      )
      .await;
      (rendered.label, result)
    }));
  }

  let mut all_entries: Vec<task::ParsedSubjectThreats> = Vec::new();
  for handle in handles {
    match handle.await {
      Ok((_label, Ok(parsed))) => all_entries.extend(parsed.entries),
      Ok((label, Err(e))) => tracing::error!(
        "extract_threats_from_batch failed for {}: {}",
        label,
        e
      ),
      Err(e) => tracing::error!("extract_threats_from_batch panicked: {}", e),
    }
  }

  let total_subjects = all_entries.len();
  let total_conditions_processed: usize =
    all_entries.iter().map(|e| e.conditions.len()).sum();
  let mut total_threats: usize = 0;
  let mut total_no_threat_comments: usize = 0;
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
    for entry in all_entries {
      for cond_threats in entry.conditions {
        // Allocate one A-topic per threat. A condition with three threats
        // consumes three A-IDs; each threat is independently addressable,
        // approvable, and (in step 8) linkable to invariants.
        for threat in cond_threats.threats {
          let threat_topic = topic::new_adversarial_property_topic(
            ids::allocate_adversarial_property_id(),
          );
          audit_data.topic_metadata.insert(
            threat_topic,
            domain::TopicMetadata::ThreatTopic {
              topic: threat_topic,
              description: threat.description,
              subject_topic: entry.subject_topic,
              falsifies_condition: cond_threats.falsifies_condition,
              controlled_by: threat.controlled_by,
              evidence_topics: threat.evidence_topics,
              author: Author::System,
              created_at: None,
              severity: None,
            },
          );
          total_threats += 1;
        }

        // `no_threat_rationale` posts as a pipeline-authored Note on the
        // condition topic. The `[step-7 / no-threat]` prefix lets the UI
        // filter pipeline-emitted rationale comments from human
        // discussion if filtering is added later. Author follows the
        // step 5/6 convention (`Author::System` for pipeline-authored
        // topics). Comments are not cleared by this step's re-run
        // retain, so the rationale persists across reruns and the
        // auditor can reply in-thread.
        if let Some(rationale) = cond_threats.no_threat_rationale {
          let body = format!("[step-7 / no-threat] {}", rationale);
          synthetic::create_synthetic_dev_comment(
            &cond_threats.falsifies_condition,
            &body,
            CommentType::Note,
            Author::System,
            audit_data,
          );
          total_no_threat_comments += 1;
        }
      }
    }
    domain::rebuild_feature_context(audit_data);
  }

  if total_skipped_no_feature > 0 {
    tracing::warn!(
      "Skipped {} member(s) with no feature link \u{2014} reconciliation gap",
      total_skipped_no_feature
    );
  }
  if total_skipped_no_conditions > 0 {
    tracing::debug!(
      "Skipped {} member(s) whose non-pure subjects had no conditions",
      total_skipped_no_conditions
    );
  }
  tracing::info!(
    "Completed threat generation: {} threats and {} no-threat rationale \
     comments across {} condition group(s) in {} subject(s) across {} \
     member(s)",
    total_threats,
    total_no_threat_comments,
    total_conditions_processed,
    total_subjects,
    total_members
  );

  Ok(())
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

#[cfg(test)]
mod build_threats_tests {
  use super::*;
  use crate::collaborator::models::Author;
  use crate::domain::{
    Invariant, ThreatActor, ThreatFeatureLink, ThreatFeatureRelation,
    ThreatSeverity, TopicMetadata, new_data_context,
  };
  use std::collections::HashSet;

  /// Exercises the proactive clear-on-rerun branch of `build_threats`.
  /// Because `subject_conditions` is empty the function early-returns
  /// before any LLM call — but only after running the clear block. So
  /// every pre-seeded `ThreatTopic`/`InvariantTopic` topic, every
  /// `audit_data.invariants` entry, and every now-orphaned
  /// `threat_feature_links` entry must be gone afterward.
  #[tokio::test]
  async fn build_threats_clears_stale_threat_and_invariant_state_on_rerun() {
    let mut ctx = new_data_context();
    let audit_id = "test_audit";
    assert!(ctx.create_audit(
      audit_id.to_string(),
      "Test".to_string(),
      HashSet::new(),
      None,
    ));

    let pre_threat_topic = topic::new_adversarial_property_topic(100);
    let pre_invariant_topic = topic::new_adversarial_property_topic(200);
    let subject_topic = topic::new_node_topic(&42);
    let condition_topic = topic::new_adversarial_property_topic(50);

    {
      let audit_data = ctx.get_audit_mut(audit_id).unwrap();
      audit_data.topic_metadata.insert(
        pre_threat_topic,
        TopicMetadata::ThreatTopic {
          topic: pre_threat_topic,
          description: "stale scenario from prior run".to_string(),
          subject_topic,
          falsifies_condition: condition_topic,
          controlled_by: ThreatActor::Caller,
          evidence_topics: vec![],
          author: Author::AgentLarge,
          created_at: None,
          severity: None,
        },
      );
      audit_data.topic_metadata.insert(
        pre_invariant_topic,
        TopicMetadata::InvariantTopic {
          topic: pre_invariant_topic,
          description: "stale invariant from prior run".to_string(),
          threat_topic: pre_threat_topic,
          author: Author::AgentLarge,
          created_at: String::new(),
          severity: None,
        },
      );
      audit_data.invariants.insert(
        pre_invariant_topic,
        Invariant { source_topics: vec![] },
      );
      // Two impact-analysis links — one referring to the existing threat
      // (will be orphaned once the clear runs), one already orphaned by
      // a previous deletion that didn't prune. Both must be gone.
      audit_data.threat_feature_links.push(ThreatFeatureLink {
        threat_topic: pre_threat_topic,
        feature_topic: topic::new_feature_topic(1),
        relation: ThreatFeatureRelation::IsVulnerableTo,
        severity: ThreatSeverity::Medium,
      });
      audit_data.threat_feature_links.push(ThreatFeatureLink {
        threat_topic: topic::new_adversarial_property_topic(999),
        feature_topic: topic::new_feature_topic(2),
        relation: ThreatFeatureRelation::DefendsAgainst,
        severity: ThreatSeverity::Low,
      });
      domain::rebuild_feature_context(audit_data);
      // Sanity: indexes are populated before the call.
      assert!(audit_data.subject_threats.contains_key(&subject_topic));
      assert!(
        audit_data.condition_threats.contains_key(&condition_topic),
        "condition_threats must be populated before the rerun"
      );
      // No conditions exist — so build_threats will early-return after
      // the clear runs.
      assert!(audit_data.subject_conditions.is_empty());
    }

    let state = PipelineState::new(Arc::new(Mutex::new(ctx)));
    build_threats(&state, audit_id)
      .await
      .expect("build_threats must early-return cleanly when no conditions");

    let ctx = state.data_context.lock().unwrap();
    let audit_data = ctx.get_audit(audit_id).unwrap();

    // Every pre-seeded ThreatTopic/InvariantTopic entry is gone.
    assert!(
      !audit_data.topic_metadata.contains_key(&pre_threat_topic),
      "stale ThreatTopic must be dropped on rerun"
    );
    assert!(
      !audit_data.topic_metadata.contains_key(&pre_invariant_topic),
      "stale InvariantTopic must be dropped on rerun"
    );

    // The `Invariant` denormalization is wiped.
    assert!(
      audit_data.invariants.is_empty(),
      "audit_data.invariants must be cleared on rerun"
    );

    // Both impact-analysis links are gone — the first because its threat
    // was just cleared, the second because it was already orphaned.
    assert!(
      audit_data.threat_feature_links.is_empty(),
      "all orphaned threat_feature_links must be pruned"
    );

    // Reverse indexes must reflect the cleared state (the post-clear
    // rebuild_feature_context inside build_threats handles this).
    assert!(
      audit_data.subject_threats.is_empty(),
      "subject_threats must be empty after clearing all ThreatTopics"
    );
    assert!(
      audit_data.condition_threats.is_empty(),
      "condition_threats must be empty after clearing all ThreatTopics"
    );
  }

  /// Two consecutive `build_threats` runs, each re-seeded with stale
  /// threat/invariant state between calls, must leave the audit clean
  /// without duplicate `ThreatTopic` or `InvariantTopic` entries. The
  /// spec calls this out explicitly under Phase 5 ("the second run does
  /// not produce duplicated `InvariantTopic` entries"); re-seeding
  /// between calls is the unit-test equivalent of running the full
  /// pipeline twice (which would itself repopulate via LLM output).
  #[tokio::test]
  async fn build_threats_clear_runs_on_every_call() {
    let mut ctx = new_data_context();
    let audit_id = "test_audit";
    ctx.create_audit(
      audit_id.to_string(),
      "Test".to_string(),
      HashSet::new(),
      None,
    );

    let state = PipelineState::new(Arc::new(Mutex::new(ctx)));

    for run in 0..2 {
      // Re-seed stale state on each iteration to simulate "rerun after
      // prior step-7 produced output, then re-run again." Re-using the
      // same topic IDs is the worst-case for duplicate accumulation.
      {
        let mut c = state.data_context.lock().unwrap();
        let audit_data = c.get_audit_mut(audit_id).unwrap();
        let pre_threat = topic::new_adversarial_property_topic(100 + run);
        let pre_invariant = topic::new_adversarial_property_topic(200 + run);
        audit_data.topic_metadata.insert(
          pre_threat,
          TopicMetadata::ThreatTopic {
            topic: pre_threat,
            description: "stale".to_string(),
            subject_topic: topic::new_node_topic(&1),
            falsifies_condition: topic::new_adversarial_property_topic(50),
            controlled_by: ThreatActor::Caller,
            evidence_topics: vec![],
            author: Author::AgentLarge,
            created_at: None,
            severity: None,
          },
        );
        audit_data.topic_metadata.insert(
          pre_invariant,
          TopicMetadata::InvariantTopic {
            topic: pre_invariant,
            description: "stale".to_string(),
            threat_topic: pre_threat,
            author: Author::AgentLarge,
            created_at: String::new(),
            severity: None,
          },
        );
        audit_data
          .invariants
          .insert(pre_invariant, Invariant { source_topics: vec![] });
      }

      build_threats(&state, audit_id).await.unwrap();

      let c = state.data_context.lock().unwrap();
      let audit_data = c.get_audit(audit_id).unwrap();
      let threat_count = audit_data
        .topic_metadata
        .values()
        .filter(|m| matches!(m, TopicMetadata::ThreatTopic { .. }))
        .count();
      let invariant_count = audit_data
        .topic_metadata
        .values()
        .filter(|m| matches!(m, TopicMetadata::InvariantTopic { .. }))
        .count();
      assert_eq!(
        threat_count, 0,
        "run {}: stale ThreatTopic must be cleared",
        run
      );
      assert_eq!(
        invariant_count, 0,
        "run {}: stale InvariantTopic must be cleared",
        run
      );
      assert!(
        audit_data.invariants.is_empty(),
        "run {}: audit_data.invariants must be cleared",
        run
      );
    }
  }

  /// The spec is explicit that comments are *not* cleared by step 7's
  /// retain — the no-threat rationale comments posted on condition
  /// topics must persist across reruns so the auditor can reply in-
  /// thread. A future refactor that adds `CommentTopic` to the retain
  /// filter would silently break this guarantee; this test pins the
  /// behavior.
  #[tokio::test]
  async fn build_threats_preserves_comments_on_clear() {
    use crate::collaborator::synthetic::create_synthetic_dev_comment;
    use crate::domain::CommentType;

    let mut ctx = new_data_context();
    let audit_id = "test_audit";
    ctx.create_audit(
      audit_id.to_string(),
      "Test".to_string(),
      HashSet::new(),
      None,
    );

    let condition_topic = topic::new_adversarial_property_topic(50);
    // Insert a synthetic comment on the condition before handing the
    // context to the pipeline. Capture the allocated comment topic
    // (negative-id range) so we can assert it survives the clear.
    let comment_topic_before: topic::Topic = {
      let audit_data = ctx.get_audit_mut(audit_id).unwrap();
      create_synthetic_dev_comment(
        &condition_topic,
        "[step-7 / no-threat] enforced by Solidity's checked arithmetic",
        CommentType::Note,
        Author::System,
        audit_data,
      );
      *audit_data
        .comment_index
        .get(&condition_topic)
        .and_then(|v| v.first())
        .expect("synthetic comment must be indexed")
    };

    let state = PipelineState::new(Arc::new(Mutex::new(ctx)));
    build_threats(&state, audit_id).await.unwrap();

    let c = state.data_context.lock().unwrap();
    let audit_data = c.get_audit(audit_id).unwrap();
    assert!(
      audit_data.topic_metadata.contains_key(&comment_topic_before),
      "comment topic must survive build_threats clear"
    );
    assert!(
      matches!(
        audit_data.topic_metadata.get(&comment_topic_before),
        Some(TopicMetadata::CommentTopic { .. })
      ),
      "preserved entry must still be a CommentTopic"
    );
    assert_eq!(
      audit_data
        .comment_index
        .get(&condition_topic)
        .map(|v| v.len()),
      Some(1),
      "comment_index entry must survive the clear"
    );
  }

  /// `AuditNotFound` surfaces as a typed error rather than a panic when
  /// the audit_id is unknown. Mirrors the error-path contract of every
  /// other pipeline step; small but worth pinning because lock-acquire
  /// + audit-lookup is repeated three times in `build_threats` and a
  /// regression in any one of them would shape this error differently.
  #[tokio::test]
  async fn build_threats_returns_audit_not_found_for_unknown_audit() {
    let state = PipelineState::new(Arc::new(Mutex::new(new_data_context())));
    let err = build_threats(&state, "missing").await.unwrap_err();
    match err {
      PipelineError::AuditNotFound { audit_id } => {
        assert_eq!(audit_id, "missing");
      }
      other => panic!("expected AuditNotFound, got {:?}", other),
    }
  }
}

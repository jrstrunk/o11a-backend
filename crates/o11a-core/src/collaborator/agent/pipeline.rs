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

/// Run the full analysis pipeline in ten steps:
///
/// 1. **Semantic Linking** — establish functional semantics on declarations.
/// 2. **Requirement Extraction** — pull documentation requirements *and*
///    system characteristics with semantics in context. Characteristics
///    here are "raw extracted"; step 5 replaces them with a synthesized
///    set.
/// 3. **Behavior Extraction** — DAG-batched per-function behavior generation
///    with callee context.
/// 4. **Feature Synthesis** — reconcile requirements and behaviors. Feature
///    synthesis is intentionally blind to characteristics; the boundary is
///    enforced by what the renderers emit.
/// 5. **Characteristic Synthesis** — consolidate and refine the raw
///    characteristics extracted in step 2 against the audit's
///    `security.md` notes. The threats step (8) consumes the
///    `Security`-kind subset of the output as a rendered text block in
///    place of the old raw `security_notes` blob.
/// 6. **Functional Purpose & Placement** — for every non-pure subject in
///    every in-scope function with a feature link, generate purpose and
///    placement rationale (per-function).
/// 7. **Condition Generation** — for every non-pure subject with a purpose
///    and placement, generate the assertions that must hold for that
///    purpose and placement to be fulfilled (per-function). Each condition
///    is its own A-prefixed topic.
/// 8. **Threat Generation** — for every condition on every non-pure subject,
///    generate adversarial scenarios that falsify the assertion (per-
///    function). Each threat is its own A-prefixed topic that names exactly
///    one `falsifies_condition` and one `controlled_by` actor; one condition
///    can be the target of many threats.
/// 9. **Invariant Generation** — for every threat on every non-pure subject,
///    generate codebase-level defensive properties phrased as "X must Y" /
///    "every Z does W" that the threat scenario violates (per-function).
///    Each invariant is its own A-prefixed topic that names exactly one
///    parent `threat_topic` and inherits `subject_topic` and `severity`
///    from that threat at write time; one threat can be defended by many
///    invariants.
/// 10. **Invariant Validation** — for every invariant on every non-pure
///    subject, generate a verdict on whether the invariant's property
///    actually holds in the code at the validated subject (per-function).
///    Each validation is its own A-prefixed `ValidationTopic` that names
///    exactly one parent `invariant_topic` and carries a verdict, a
///    one-sentence rationale, and the evidence topics inside the subject's
///    function that back the verdict. Cross-site propagation (one
///    invariant validated at many subjects) is deferred to a later step.
///
/// Semantic linking runs first so functional semantics are available when
/// rendering documentation for requirement extraction — inline code
/// references like `pID` get annotated with their project-specific meaning.
/// Characteristic synthesis (step 5) runs *after* feature synthesis (step 4)
/// so the two sets stay in disjoint contexts: feature synthesis sees only
/// requirements and behaviors, characteristic synthesis sees only the
/// extracted characteristics plus `security.md`. Step 6 runs after step 5
/// only because the pipeline keeps spec-family steps contiguous; functional
/// property generation reads features and behaviors, not characteristics.
/// Step 7 runs after step 6 because every condition is grounded in a
/// subject's functional purpose and placement rationale; step 8 runs after
/// step 7 because every threat is the adversarial inversion of a specific
/// condition; step 9 runs after step 8 because every invariant defends
/// against a specific threat; step 10 runs after step 9 because every
/// validation verdicts on a specific invariant.
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn run_full_pipeline(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use std::time::Instant;

  let pipeline_start = Instant::now();
  tracing::info!("Starting full analysis pipeline for audit {}", audit_id);

  // Each step logs elapsed time, artifact counts, and saves a
  // checkpoint so a late-stage failure doesn't lose prior expensive
  // LLM output. The checkpoint is written to
  // `<output_dir>/audit.checkpoint.json` using the same report
  // format as `audit.json` — the server can load it by pointing
  // `AUDIT_REPORT` at the checkpoint file.
  macro_rules! run_step {
    ($label:expr, $fn:expr) => {{
      tracing::info!($label);
      let step_start = Instant::now();
      $fn(state, audit_id).await?;
      let elapsed = step_start.elapsed();
      log_artifact_counts(state, audit_id);
      tracing::info!("{} — done in {:.1}s", $label, elapsed.as_secs_f64());
      if let Some(ref output_dir) = state.output_dir {
        if let Err(e) = save_checkpoint(output_dir, audit_id, state) {
          tracing::warn!("{} — checkpoint save failed: {}", $label, e);
        } else {
          tracing::info!("{} — checkpoint saved", $label);
        }
      }
    }};
  }

  run_step!("[1/10] Semantic Linking", build_semantic_links);
  run_step!("[2/10] Requirement Extraction", build_requirements);
  run_step!("[3/10] Behavior Extraction", build_behaviors);
  run_step!("[4/10] Feature Synthesis", synthesize_features);
  run_step!(
    "[5/10] Characteristic Synthesis",
    synthesize_characteristics
  );
  run_step!(
    "[6/10] Functional Purpose & Placement Generation",
    build_functional_properties
  );
  run_step!("[7/10] Condition Generation", build_conditions);
  run_step!("[8/10] Threat Generation", build_threats);
  run_step!("[9/10] Invariant Generation", build_invariants);
  run_step!("[10/10] Invariant Validation", build_validations);

  tracing::info!(
    "Pipeline complete for audit {} — {:.1}s total",
    audit_id,
    pipeline_start.elapsed().as_secs_f64()
  );
  Ok(())
}

/// Log a summary of every artifact kind currently in `topic_metadata`.
/// Gives the operator a running view of pipeline progress and surfaces
/// unexpected zeros early (e.g. a step that produced nothing because
/// the renderer emitted an empty payload).
fn log_artifact_counts(state: &PipelineState, audit_id: &str) {
  let ctx = match state.data_context.lock() {
    Ok(guard) => guard,
    Err(e) => {
      tracing::warn!("artifact counts: lock poisoned: {}", e);
      return;
    }
  };
  let Some(audit_data) = ctx.get_audit(audit_id) else {
    return;
  };
  let mut features = 0usize;
  let mut requirements = 0usize;
  let mut behaviors = 0usize;
  let mut characteristics = 0usize;
  let mut semantics = 0usize;
  let mut purposes = 0usize;
  let mut placements = 0usize;
  let mut conditions = 0usize;
  let mut threats = 0usize;
  let mut invariants = 0usize;
  let mut validations = 0usize;
  for m in audit_data.topic_metadata.values() {
    match m {
      domain::TopicMetadata::FeatureTopic { .. } => features += 1,
      domain::TopicMetadata::RequirementTopic { .. } => requirements += 1,
      domain::TopicMetadata::BehaviorTopic { .. } => behaviors += 1,
      domain::TopicMetadata::CharacteristicTopic { .. } => characteristics += 1,
      domain::TopicMetadata::FunctionalSemanticTopic { .. } => semantics += 1,
      domain::TopicMetadata::FunctionalPurposeTopic { .. } => purposes += 1,
      domain::TopicMetadata::PlacementRationaleTopic { .. } => placements += 1,
      domain::TopicMetadata::ConditionTopic { .. } => conditions += 1,
      domain::TopicMetadata::ThreatTopic { .. } => threats += 1,
      domain::TopicMetadata::InvariantTopic { .. } => invariants += 1,
      domain::TopicMetadata::ValidationTopic { .. } => validations += 1,
      _ => {}
    }
  }
  tracing::info!(
    "Artifact counts: {} features, {} requirements, {} behaviors, \
     {} characteristics, {} semantics, {} purposes, {} placements, \
     {} conditions, {} threats, {} invariants, {} validations",
    features,
    requirements,
    behaviors,
    characteristics,
    semantics,
    purposes,
    placements,
    conditions,
    threats,
    invariants,
    validations,
  );
}

/// Save a checkpoint of the current pipeline state to
/// `<output_dir>/audit.checkpoint.json`. The checkpoint uses the same
/// report format as `audit.json` so the server can load it directly
/// (by pointing `AUDIT_REPORT` at the checkpoint file).
fn save_checkpoint(
  output_dir: &std::path::Path,
  audit_id: &str,
  state: &PipelineState,
) -> Result<(), PipelineError> {
  let generated_at = ids::now_iso8601();

  // Rebuild reverse indexes for a consistent snapshot.
  {
    let mut ctx = state
      .data_context
      .lock()
      .map_err(|e| PipelineError::LockPoisoned(e.to_string()))?;
    if let Some(audit_data) = ctx.get_audit_mut(audit_id) {
      domain::rebuild_feature_context(audit_data);
    }
  }

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

  let report = crate::report::build_report(audit_id, audit_data, generated_at);
  let json = serde_json::to_string_pretty(&report).map_err(|e| {
    PipelineError::Other(format!("checkpoint serialize: {}", e))
  })?;

  let path = output_dir.join("audit.checkpoint.json");
  // Write atomically: tmp + rename.
  let tmp_path = path.with_extension("json.tmp");
  std::fs::write(&tmp_path, &json)
    .map_err(|e| PipelineError::Other(format!("checkpoint write: {}", e)))?;
  std::fs::rename(&tmp_path, &path)
    .map_err(|e| PipelineError::Other(format!("checkpoint rename: {}", e)))?;

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
  let section_count = parsed
    .section_requirements
    .keys()
    .chain(parsed.section_characteristics.keys())
    .collect::<std::collections::BTreeSet<_>>()
    .len();
  tracing::info!(
    "Extracted {} requirements and {} characteristics across {} sections",
    parsed.requirements.len(),
    parsed.characteristics.len(),
    section_count
  );

  // Re-key parsed entities with allocated IDs from the atomic counter.
  // The parser assigns local S-topic IDs starting from 1 (shared counter
  // across requirements and characteristics, so the kinds never collide
  // on a `Topic::Spec(_)` key); replace each with a process-wide allocated
  // ID so pipeline runs don't clash with already-allocated IDs in the
  // counter's range. Requirements and characteristics share one counter
  // (`allocate_spec_id`) by design — both live in the unified `S` topic
  // family alongside features and behaviors.
  //
  // Iteration order: `parsed_requirements` is a `BTreeMap` keyed by local
  // `Topic::Spec`, and the parser allocates those local IDs in
  // section-then-position order, so iterating `requirements` then
  // `characteristics` reproduces the document-natural allocation order
  // (requirements get the lower process-wide IDs, then characteristics).
  let task::ParsedRequirements {
    requirements: parsed_requirements,
    topic_metadata,
    section_requirements: _,
    characteristics: parsed_characteristics,
    section_characteristics: _,
  } = parsed;

  let mut id_remap: std::collections::HashMap<topic::Topic, topic::Topic> =
    std::collections::HashMap::with_capacity(
      parsed_requirements.len() + parsed_characteristics.len(),
    );
  let mut new_requirements: std::collections::BTreeMap<
    topic::Topic,
    domain::Requirement,
  > = std::collections::BTreeMap::new();
  let mut new_characteristics: std::collections::BTreeMap<
    topic::Topic,
    domain::Characteristic,
  > = std::collections::BTreeMap::new();

  for (old_req_topic, requirement) in parsed_requirements {
    let new_req_topic = topic::new_spec_topic(ids::allocate_spec_id());
    id_remap.insert(old_req_topic, new_req_topic);
    new_requirements.insert(new_req_topic, requirement);
  }

  for (old_char_topic, characteristic) in parsed_characteristics {
    let new_char_topic = topic::new_spec_topic(ids::allocate_spec_id());
    id_remap.insert(old_char_topic, new_char_topic);
    new_characteristics.insert(new_char_topic, characteristic);
  }

  // Re-key the parser's topic_metadata. The parser already set the right
  // `author` (`Author::System`) and `created_at` (`None`) — preserve them
  // verbatim instead of rewriting authorship in a second pass. Only the
  // `topic` field needs updating to the process-wide allocated ID.
  let mut new_topic_metadata: std::collections::BTreeMap<
    topic::Topic,
    domain::TopicMetadata,
  > = std::collections::BTreeMap::new();
  for (old_topic, metadata) in topic_metadata {
    let new_topic = match id_remap.get(&old_topic) {
      Some(t) => *t,
      None => continue,
    };
    match metadata {
      domain::TopicMetadata::RequirementTopic {
        description,
        section_topic,
        author,
        created_at,
        ..
      } => {
        new_topic_metadata.insert(
          new_topic,
          domain::TopicMetadata::RequirementTopic {
            topic: new_topic,
            description,
            section_topic,
            author,
            created_at,
          },
        );
      }
      domain::TopicMetadata::CharacteristicTopic {
        description,
        kind,
        section_topic,
        author,
        created_at,
        ..
      } => {
        new_topic_metadata.insert(
          new_topic,
          domain::TopicMetadata::CharacteristicTopic {
            topic: new_topic,
            description,
            kind,
            section_topic,
            author,
            created_at,
          },
        );
      }
      _ => continue,
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

  // Clear old feature/requirement/characteristic metadata — requirements
  // and characteristics are being replaced wholesale, and features will be
  // re-synthesized against the new requirement set in the next pipeline
  // step. Phase 4's characteristic synthesis will replace the characteristic
  // set again; the clear here covers reruns of step 2 in isolation. The
  // section_* reverse indexes are rebuilt from `topic_metadata` by
  // `rebuild_feature_context` at the end of this function, so we don't
  // mutate them directly.
  audit_data.topic_metadata.retain(|_, m| {
    !matches!(
      m,
      domain::TopicMetadata::FeatureTopic { .. }
        | domain::TopicMetadata::RequirementTopic { .. }
        | domain::TopicMetadata::CharacteristicTopic { .. }
    )
  });

  let req_count = new_requirements.len();
  let char_count = new_characteristics.len();
  audit_data.requirements = new_requirements;
  audit_data.characteristics = new_characteristics;
  audit_data.topic_metadata.extend(new_topic_metadata);
  audit_data.feature_requirement_links.clear();
  audit_data.feature_behavior_links.clear();
  domain::rebuild_feature_context(audit_data);

  tracing::info!(
    "Stored {} requirements and {} characteristics in DataContext",
    req_count,
    char_count
  );
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
      .or_insert_with(|| topic::new_spec_topic(ids::allocate_spec_id()));
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
        let t = topic::new_spec_topic(ids::allocate_spec_id());
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
        let t = topic::new_spec_topic(ids::allocate_spec_id());
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

/// Synthesize the refined characteristic set from the raw `security.md`
/// notes and the characteristics already extracted in step 2. Runs as
/// step 5 of 10 — after feature synthesis (so the renderer for this step
/// cannot accidentally leak feature context into the prompt) and before
/// functional property generation (so the threats step's eventual
/// consumer sees only the synthesized set).
///
/// **Boundary discipline.** The renderer (`render_characteristic_synthesis_context`)
/// reads only `audit_data.security_notes` plus the existing
/// `CharacteristicTopic` entries — never features, behaviors, requirements,
/// purposes, placements, conditions, threats, or invariants. The
/// permanent renderer-leak drift-guard test in
/// `pipeline::characteristic_synthesis_tests` asserts that the renderers
/// for steps 4 (features), 6 (functional properties), and 7 (conditions)
/// do not emit `CharacteristicTopic` IDs into their prompts.
///
/// **Idempotent rerun.** Replaces the entire `CharacteristicTopic` set
/// and the `audit_data.characteristics` map; the `section_characteristics`
/// reverse index is rebuilt by `rebuild_feature_context` at the end. No
/// other state is touched, so this step can rerun in isolation.
///
/// **Early skip.** Both inputs empty (no extracted characteristics *and*
/// no `security.md`) is the only case where this step is a no-op. When
/// only one side is empty we still run: cross-section consolidation is
/// valuable without a `security.md`, and a `security.md` with no
/// extracted set still needs its claims promoted to first-class topics.
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn synthesize_characteristics(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  // Snapshot the renderer inputs under a read lock. The renderer returns
  // owned strings so the lock is dropped before the LLM call.
  let (security_notes, extracted_json, prior_count) = {
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
    let prior_count = audit_data
      .topic_metadata
      .values()
      .filter(|m| {
        matches!(m, domain::TopicMetadata::CharacteristicTopic { .. })
      })
      .count();
    let (notes, extracted) =
      task::render_characteristic_synthesis_context(audit_data);
    (notes, extracted, prior_count)
  };

  // Skip only when both inputs are empty. `prior_count` counts
  // `CharacteristicTopic` entries already in `topic_metadata`; the
  // renderer emits exactly those, so `prior_count == 0` is the
  // structural equivalent of "extracted set is empty" without
  // resorting to a string sentinel on the rendered JSON.
  if security_notes.trim().is_empty() && prior_count == 0 {
    tracing::info!("No characteristic input, skipping synthesis");
    return Ok(());
  }

  tracing::info!(
    "Synthesizing characteristics from {} extracted item(s) and {} byte(s) of security notes",
    prior_count,
    security_notes.len()
  );
  let synthesized =
    task::synthesize_characteristics(&security_notes, &extracted_json).await?;

  // Re-key synthesized characteristics with allocated process-wide S IDs.
  // The task assigns local S-topic IDs starting from 1 (parallel to
  // requirement extraction); each is reallocated here so subsequent
  // pipeline runs of this step never collide with already-allocated IDs.
  let task::SynthesizedCharacteristics {
    topic_metadata: parsed_topic_metadata,
    characteristics: parsed_characteristics,
  } = synthesized;

  let mut id_remap: std::collections::HashMap<topic::Topic, topic::Topic> =
    std::collections::HashMap::with_capacity(parsed_characteristics.len());
  let mut new_characteristics: std::collections::BTreeMap<
    topic::Topic,
    domain::Characteristic,
  > = std::collections::BTreeMap::new();

  for (old_topic, characteristic) in parsed_characteristics {
    let new_topic = topic::new_spec_topic(ids::allocate_spec_id());
    id_remap.insert(old_topic, new_topic);
    new_characteristics.insert(new_topic, characteristic);
  }

  // Re-key parsed topic_metadata. The task already set the right `author`
  // (`Author::AgentLarge`) and `created_at` (`None`) — preserve them
  // verbatim instead of rewriting authorship in a second pass.
  let mut new_topic_metadata: std::collections::BTreeMap<
    topic::Topic,
    domain::TopicMetadata,
  > = std::collections::BTreeMap::new();
  for (old_topic, metadata) in parsed_topic_metadata {
    let new_topic = match id_remap.get(&old_topic) {
      Some(t) => *t,
      None => continue,
    };
    if let domain::TopicMetadata::CharacteristicTopic {
      description,
      kind,
      section_topic,
      author,
      created_at,
      ..
    } = metadata
    {
      new_topic_metadata.insert(
        new_topic,
        domain::TopicMetadata::CharacteristicTopic {
          topic: new_topic,
          description,
          kind,
          section_topic,
          author,
          created_at,
        },
      );
    }
  }

  let new_count = new_characteristics.len();

  // Clear prior `CharacteristicTopic` metadata and the
  // `audit_data.characteristics` map wholesale, then install the
  // synthesized set. The `section_characteristics` reverse index is
  // rebuilt from `topic_metadata` by `rebuild_feature_context`.
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
    !matches!(m, domain::TopicMetadata::CharacteristicTopic { .. })
  });
  audit_data.characteristics = new_characteristics;
  audit_data.topic_metadata.extend(new_topic_metadata);
  domain::rebuild_feature_context(audit_data);

  tracing::info!(
    "Synthesized {} characteristic(s) (from {} prior extracted item(s))",
    new_count,
    prior_count
  );

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
        let beh_topic = topic::new_spec_topic(ids::allocate_spec_id());
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
/// Format a member topic as `name (N12345)` for log readability.
fn member_display(
  member: &topic::Topic,
  audit_data: &domain::AuditData,
) -> String {
  let name = audit_data
    .topic_metadata
    .get(member)
    .and_then(|m| m.name())
    .unwrap_or("?");
  format!("{} ({})", name, member.id())
}

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
        let original_texts: Vec<&str> =
          originals.iter().map(|l| l.description.as_str()).collect();
        tracing::error!(
          "{}: condense_semantics failed for {}: {}, keeping {} \
           originals: {:?}",
          step_label,
          decl_topic.id(),
          e,
          originals.len(),
          original_texts,
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
  // step 6 generates per-subject output, so the LLM call granularity is
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
          tracing::debug!(
            "Skipping member with no feature link: {} ({})",
            member_display(member, audit_data),
            member.id(),
          );
          total_skipped_no_feature += 1;
          continue;
        }
        if let Some(rendered) =
          context::render_batch_for_extraction(&[*member], audit_data)
        {
          if rendered.non_pure_subjects.is_empty() {
            // Pure-only function: nothing to ask the LLM about. The
            // unified renderer is step-agnostic and renders pure-only
            // members for step 3 (behaviors); step 6 filters them here.
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
/// fulfilled. Step 8 (threats) generates adversarial scenarios that
/// falsify these assertions. One LLM call per function (mirrors step 6's
/// per-function granularity); one A-prefixed `ConditionTopic` per
/// assertion (subjects typically produce 1–8). Requires step 6 output:
/// the renderer inlines `functional_purpose` and `placement_rationale` on
/// each non-pure subject so the LLM grounds conditions in purpose +
/// placement. If step 6 produced nothing, this step skips cleanly. See
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
  // step 6, which this step depends on.
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

    // Conditions are downstream of step 6: every condition is grounded in
    // a non-pure subject's functional purpose. If step 6 produced nothing
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
  // step 7 is per-function, like step 6.
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
          tracing::debug!(
            "Skipping member with no feature link: {} ({})",
            member_display(member, audit_data),
            member.id(),
          );
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

/// Render every `CharacteristicTopic { kind: Security, .. }` as a single
/// `- description` text block, sorted by numeric topic ID for deterministic
/// output. Returns `None` when the audit has no Security characteristics —
/// that signals the caller to omit the `Security context:` block from the
/// threats prompt entirely (the `None` path of `extract_threats_from_batch`).
///
/// Shared consumer used by every adversarial-layer step that needs the
/// audit-wide security context: threats (step 8), invariants (step 9),
/// and validations (step 10). The renderer reads only `topic_metadata`;
/// no `audit_data.security_notes` read, since the raw `security.md`
/// blob's role is fully absorbed by characteristic synthesis (step 5
/// reads it as one of its two inputs, then emits `CharacteristicTopic`
/// entries that this function consumes).
fn render_security_characteristics(
  audit_data: &domain::AuditData,
) -> Option<String> {
  let mut items: Vec<(i32, String)> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      domain::TopicMetadata::CharacteristicTopic {
        topic,
        description,
        kind: domain::SystemCharacteristicKind::Security,
        ..
      } => Some((topic.numeric_id(), description.clone())),
      _ => None,
    })
    .collect();
  if items.is_empty() {
    return None;
  }
  items.sort_by_key(|(id, _)| *id);
  Some(
    items
      .into_iter()
      .map(|(_, d)| format!("- {}", d))
      .collect::<Vec<_>>()
      .join("\n"),
  )
}

/// For every condition on every non-pure subject in every in-scope, feature-
/// linked function or modifier, generate a list of **threats** — adversarial
/// scenarios in which the named assertion fails to hold. Each threat is its
/// own A-prefixed `ThreatTopic` that names exactly one `falsifies_condition`
/// and one `controlled_by` actor; one condition can be the target of many
/// threats. One LLM call per function (mirrors step 7's per-function
/// granularity). Requires step 7 output: the renderer inlines `conditions`
/// on each non-pure subject so the LLM grounds threats in concrete
/// assertions. If step 7 produced nothing, this step skips cleanly.
///
/// Reruns proactively clear downstream invariant and validation data:
/// `InvariantTopic` and `ValidationTopic` entries are dropped from
/// `topic_metadata` — a deleted threat orphans its invariants, and a
/// deleted invariant transitively orphans its validations, and the audit
/// data must be internally consistent at step boundaries. Orphaned
/// `threat_feature_links` (impact-analysis entries whose `threat_topic`
/// no longer exists) are pruned; surviving non-orphaned links are kept,
/// though in practice A-prefix reallocation on rerun makes most links
/// orphaned. `no_threat_rationale` entries (the LLM's explicit "no
/// falsifier exists" response) are posted as agent-authored comments on
/// the condition topic so the audit signal persists in the auditor-
/// visible discussion thread. See SPEC's "Conditions vs. Invariants"
/// for the role distinction with invariants (step 9).
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn build_threats(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use crate::collaborator::agent::{context, function_dag};
  use crate::collaborator::synthetic;
  use crate::domain::CommentType;

  // Clear any prior ThreatTopic entries so re-runs don't accumulate stale
  // generations. Also proactively clear InvariantTopic and ValidationTopic
  // entries: a deleted threat orphans its invariants, and a deleted
  // invariant transitively orphans its validations. The audit data must
  // be internally consistent at step boundaries (step 9 and step 10 will
  // repopulate). Prune
  // `threat_feature_links` of
  // any entry whose `threat_topic` no longer exists in `topic_metadata`
  // after the clear — non-orphaned links survive so impact-analysis state
  // re-attaches across re-runs that preserve threat topic IDs. Also render
  // the audit's Security characteristics so the LLM call can include them
  // as system context. Early-return if step 7 produced nothing: threats
  // are downstream of conditions, so without conditions there is nothing
  // to invert.
  let security_context: Option<String> = {
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
          | domain::TopicMetadata::ValidationTopic { .. }
      )
    });
    // Split the borrow: `mem::take` swaps the vec out so the
    // `topic_metadata` lookup inside `retain` doesn't conflict with the
    // mutable borrow on `threat_feature_links` that retain requires.
    let mut links = std::mem::take(&mut audit_data.threat_feature_links);
    links.retain(|link| {
      audit_data.topic_metadata.contains_key(&link.threat_topic)
    });
    audit_data.threat_feature_links = links;
    domain::rebuild_feature_context(audit_data);

    if audit_data.subject_conditions.is_empty() {
      tracing::info!("No conditions found, skipping threat generation");
      return Ok(());
    }

    render_security_characteristics(audit_data)
  };

  // Reuse the DAG batches from behavior extraction to enumerate members
  // in DAG-respecting order, then iterate each batch's members flat —
  // step 8 is per-function, like step 6/7.
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
          tracing::debug!(
            "Skipping member with no feature link: {} ({})",
            member_display(member, audit_data),
            member.id(),
          );
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
          // step 7 left nothing for step 8 to invert. Without this gate
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
  // subject by step 7's renderer hook. Spawn all LLM calls concurrently.
  // The rendered security-characteristics block is cloned per call so
  // each spawned future owns its copy without cross-task borrowing.
  let mut handles = Vec::new();
  for rendered in rendered_members {
    let context_block = security_context.clone();
    handles.push(tokio::spawn(async move {
      let result = task::extract_threats_from_batch(
        &rendered.json,
        &rendered.label,
        context_block.as_deref(),
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
        // approvable, and (in step 9) linkable to invariants.
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
        // condition topic. The `[step-7 / no-threat]` prefix is a stable
        // wire-format identifier (UI filters and tests pin to this
        // literal); the embedded "step-7" reflects the step number when
        // the identifier shape was introduced, not the current step
        // number. Author follows the step 6/7 convention (`Author::System`
        // for pipeline-authored topics). Comments are not cleared by this
        // step's re-run retain, so the rationale persists across reruns
        // and the auditor can reply in-thread.
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

/// For every threat on every non-pure subject in every in-scope, feature-
/// linked function or modifier, generate a list of **invariants** — codebase-
/// level defensive properties phrased as "X must Y" / "every Z does W" that
/// each parent threat scenario would falsify. Each invariant is its own A-
/// prefixed `InvariantTopic` that names exactly one parent `threat_topic`; one
/// threat can be defended by many invariants. One LLM call per function
/// (mirrors step 8's per-function granularity). Requires step 8 output: the
/// renderer inlines `threats` on each non-pure subject so the LLM grounds
/// invariants in concrete scenarios. If step 8 produced nothing, this step
/// skips cleanly.
///
/// Reruns proactively clear downstream validation data: `InvariantTopic`
/// AND `ValidationTopic` entries are dropped from `topic_metadata` — a
/// deleted invariant orphans its validations, and the audit data must be
/// internally consistent at step boundaries (step 10 will repopulate
/// validations). This mirrors the cascade pattern in step 8 (threats),
/// which clears both its own outputs and the invariants/validations they
/// transitively own.
/// `no_invariant_rationale` entries (the LLM's explicit "no defendable
/// property identified" response — e.g. mitigation deferred to user
/// discretion, economic incentives, or an external trust assumption) are
/// posted as agent-authored comments on the parent threat topic so the audit
/// signal persists in the auditor-visible discussion thread. See SPEC's
/// "Conditions vs. Invariants" for the role distinction with `ThreatTopic`.
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn build_invariants(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use crate::collaborator::agent::{context, function_dag};
  use crate::collaborator::synthetic;
  use crate::domain::CommentType;

  // Clear any prior `InvariantTopic` entries so re-runs don't accumulate
  // stale generations, rebuild reverse indexes so the post-clear state is
  // internally consistent, render the audit's Security characteristics for
  // use as system context, and early-return if step 8 produced nothing —
  // invariants are downstream of threats, so without threats there is
  // nothing to defend against. Step 10 (validations) owns its own clear,
  // so this step's clear is scoped to its own variant.
  let security_context: Option<String> = {
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
        domain::TopicMetadata::InvariantTopic { .. }
          | domain::TopicMetadata::ValidationTopic { .. }
      )
    });
    domain::rebuild_feature_context(audit_data);

    if audit_data.subject_threats.is_empty() {
      tracing::info!("No threats found, skipping invariant generation");
      return Ok(());
    }

    render_security_characteristics(audit_data)
  };

  // Reuse the DAG batches from behavior extraction to enumerate members in
  // DAG-respecting order, then iterate each batch's members flat — step 9 is
  // per-function, like steps 6/7/8.
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
      "No in-scope functions found, skipping invariant generation"
    );
    return Ok(());
  }

  // Render every eligible member up front under a single lock acquisition.
  // Members without eligible subjects (no feature link, no non-pure subjects,
  // or no threats across any non-pure subject) are dropped here so they don't
  // take a parallelism slot — there is nothing for the LLM to defend against.
  let mut rendered_members: Vec<context::BatchForExtraction> = Vec::new();
  let mut total_skipped_no_feature: usize = 0;
  let mut total_skipped_no_threats: usize = 0;
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
          tracing::debug!(
            "Skipping member with no feature link: {} ({})",
            member_display(member, audit_data),
            member.id(),
          );
          total_skipped_no_feature += 1;
          continue;
        }
        if let Some(rendered) =
          context::render_batch_for_extraction(&[*member], audit_data)
        {
          if rendered.non_pure_subjects.is_empty() {
            continue;
          }
          // Skip when no subject on this function has any threats — step 8
          // left nothing for step 9 to defend against. Without this gate a
          // function with only zero-threat subjects would burn an LLM call
          // producing an empty response.
          let any_subject_has_threats =
            rendered.non_pure_subjects.iter().any(|st| {
              audit_data
                .subject_threats
                .get(st)
                .is_some_and(|v| !v.is_empty())
            });
          if !any_subject_has_threats {
            total_skipped_no_threats += 1;
            continue;
          }
          rendered_members.push(rendered);
        }
      }
    }
  }

  let total_members = rendered_members.len();
  tracing::info!(
    "Generating invariants for {} member(s) (per-function, in parallel)",
    total_members
  );

  // Per-member calls have no inter-member dependencies — each generates
  // invariants from the inline threats already stamped on every non-pure
  // subject by step 8's renderer hook. Spawn all LLM calls concurrently. The
  // rendered security-characteristics block is cloned per call so each
  // spawned future owns its copy without cross-task borrowing.
  let mut handles = Vec::new();
  for rendered in rendered_members {
    let context_block = security_context.clone();
    handles.push(tokio::spawn(async move {
      let result = task::extract_invariants_from_batch(
        &rendered.json,
        &rendered.label,
        context_block.as_deref(),
      )
      .await;
      (rendered.label, result)
    }));
  }

  let mut all_entries: Vec<task::ParsedSubjectInvariants> = Vec::new();
  for handle in handles {
    match handle.await {
      Ok((_label, Ok(parsed))) => all_entries.extend(parsed.entries),
      Ok((label, Err(e))) => tracing::error!(
        "extract_invariants_from_batch failed for {}: {}",
        label,
        e
      ),
      Err(e) => {
        tracing::error!("extract_invariants_from_batch panicked: {}", e)
      }
    }
  }

  let total_subjects = all_entries.len();
  let total_threats_processed: usize =
    all_entries.iter().map(|e| e.threats.len()).sum();
  let mut total_invariants: usize = 0;
  let mut total_no_invariant_comments: usize = 0;
  let mut total_dropped_unknown_parent: usize = 0;
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
      for threat_invariants in entry.threats {
        // The parent threat exists by construction — it's the input — but
        // the lookup is fallible (the rendered batch could have referenced a
        // topic that was cleared concurrently, or the parser could have
        // accepted a topic whose variant drifted). On lookup failure or
        // wrong-variant metadata, warn and skip the entire entry (both the
        // invariants and any rationale); do not fall back to defaults.
        let (parent_subject_topic, parent_severity) = match audit_data
          .topic_metadata
          .get(&threat_invariants.threat_topic)
        {
          Some(domain::TopicMetadata::ThreatTopic {
            subject_topic,
            severity,
            ..
          }) => (*subject_topic, *severity),
          Some(_) => {
            tracing::warn!(
              "invariants: parent {:?} resolved but is not a \
               ThreatTopic; skipping its invariants and rationale",
              threat_invariants.threat_topic
            );
            tracing::debug!(
              "Dropped invariant descriptions: {:?}",
              threat_invariants
                .invariants
                .iter()
                .map(|inv| &inv.description)
                .collect::<Vec<_>>()
            );
            total_dropped_unknown_parent += 1;
            continue;
          }
          None => {
            tracing::warn!(
              "invariants: parent {:?} missing from topic_metadata; \
               skipping its invariants and rationale",
              threat_invariants.threat_topic
            );
            tracing::debug!(
              "Dropped invariant descriptions: {:?}",
              threat_invariants
                .invariants
                .iter()
                .map(|inv| &inv.description)
                .collect::<Vec<_>>()
            );
            total_dropped_unknown_parent += 1;
            continue;
          }
        };

        // Allocate one A-topic per invariant. A threat with three invariants
        // consumes three A-IDs; each invariant is independently addressable,
        // independently approvable, and independently re-verifiable when the
        // deferred re-check step lands. The per-threat entry is a grouping
        // construct only; it receives no topic ID. `subject_topic` and
        // `severity` are denormalized from the parent threat at write time;
        // `severity` is a write-time snapshot, not a live mirror.
        for invariant in threat_invariants.invariants {
          let invariant_topic = topic::new_adversarial_property_topic(
            ids::allocate_adversarial_property_id(),
          );
          audit_data.topic_metadata.insert(
            invariant_topic,
            domain::TopicMetadata::InvariantTopic {
              topic: invariant_topic,
              description: invariant.description,
              threat_topic: threat_invariants.threat_topic,
              subject_topic: parent_subject_topic,
              kind: invariant.kind,
              anchors: invariant.anchors,
              author: Author::System,
              created_at: None,
              severity: parent_severity,
            },
          );
          total_invariants += 1;
        }

        // `no_invariant_rationale` posts as a pipeline-authored Note on the
        // parent threat topic. The `[step-8 / no-invariant]` prefix is a
        // stable wire-format identifier (UI filters and tests pin to this
        // literal); the embedded "step-8" reflects the step number when the
        // identifier shape was introduced, not the current step number.
        // Author follows the step 6/7/8 convention (`Author::System` for
        // pipeline-authored topics). Comments are not cleared by this
        // step's re-run retain, so the rationale persists across reruns and
        // the auditor can reply in-thread.
        if let Some(rationale) = threat_invariants.no_invariant_rationale {
          let body = format!("[step-8 / no-invariant] {}", rationale);
          synthetic::create_synthetic_dev_comment(
            &threat_invariants.threat_topic,
            &body,
            CommentType::Note,
            Author::System,
            audit_data,
          );
          total_no_invariant_comments += 1;
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
  if total_skipped_no_threats > 0 {
    tracing::debug!(
      "Skipped {} member(s) whose non-pure subjects had no threats",
      total_skipped_no_threats
    );
  }
  if total_dropped_unknown_parent > 0 {
    tracing::warn!(
      "Dropped {} threat-invariant group(s) referencing missing or non-\
       ThreatTopic parents",
      total_dropped_unknown_parent
    );
  }
  tracing::info!(
    "Completed invariant generation: {} invariants and {} no-invariant \
     rationale comments across {} threat group(s) in {} subject(s) across \
     {} member(s)",
    total_invariants,
    total_no_invariant_comments,
    total_threats_processed,
    total_subjects,
    total_members
  );

  Ok(())
}

/// For every invariant on every non-pure subject in every in-scope,
/// feature-linked function or modifier, generate a `ValidationTopic`
/// carrying a verdict on whether the invariant's property actually
/// holds in the code at the validated subject. One LLM call per
/// function (mirrors step 9's per-function granularity). Requires
/// step 9 output: the renderer inlines `invariants` on each non-pure
/// subject so the LLM grounds verdicts in concrete properties. If
/// step 9 produced nothing, this step skips cleanly.
///
/// Reruns proactively clear prior `ValidationTopic` entries from
/// `topic_metadata` so re-runs do not accumulate stale verdicts. Step
/// 10 is the last step in the pipeline at this writing — there is no
/// downstream artifact to also clear. Step 11/12 (entry-boundary
/// absence check; cross-site pattern analysis) will own their own
/// clears when they land.
#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]
pub async fn build_validations(
  state: &PipelineState,
  audit_id: &str,
) -> Result<(), PipelineError> {
  use crate::collaborator::agent::{context, function_dag};

  // Clear any prior `ValidationTopic` entries so re-runs don't
  // accumulate stale verdicts, rebuild reverse indexes so the
  // post-clear state is internally consistent, and early-return if
  // step 9 produced nothing — validations are downstream of invariants.
  // Unlike step 8 (threat generation), step 10 does not render the
  // audit's `Security`-kind characteristics: the invariant + its
  // anchors already names the property to verify, so audit-wide
  // framing would be noise here.
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
      !matches!(m, domain::TopicMetadata::ValidationTopic { .. })
    });
    domain::rebuild_feature_context(audit_data);

    if audit_data.subject_invariants.is_empty() {
      tracing::info!("No invariants found, skipping validation generation");
      return Ok(());
    }
  }

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
      "No in-scope functions found, skipping validation generation"
    );
    return Ok(());
  }

  // Render every eligible member up front under a single lock
  // acquisition. Members without invariants are dropped here so they
  // don't take a parallelism slot — there's nothing for the LLM to
  // verdict.
  let mut rendered_members: Vec<(
    context::BatchForExtraction,
    Vec<topic::Topic>,
  )> = Vec::new();
  let mut total_skipped_no_feature: usize = 0;
  let mut total_skipped_no_invariants: usize = 0;
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
          tracing::debug!(
            "Skipping member with no feature link: {} ({})",
            member_display(member, audit_data),
            member.id(),
          );
          total_skipped_no_feature += 1;
          continue;
        }
        if let Some(rendered) =
          context::render_batch_for_extraction(&[*member], audit_data)
        {
          if rendered.non_pure_subjects.is_empty() {
            continue;
          }
          // Collect every invariant topic across the rendered
          // members' non-pure subjects. This is the
          // `invariants_to_validate` list the prompt enumerates.
          let mut invariant_topics: Vec<topic::Topic> = Vec::new();
          for st in &rendered.non_pure_subjects {
            if let Some(invs) = audit_data.subject_invariants.get(st) {
              invariant_topics.extend(invs.iter().copied());
            }
          }
          if invariant_topics.is_empty() {
            total_skipped_no_invariants += 1;
            continue;
          }
          rendered_members.push((rendered, invariant_topics));
        }
      }
    }
  }

  let total_members = rendered_members.len();
  tracing::info!(
    "Generating validations for {} member(s) (per-function, in parallel)",
    total_members
  );

  // Per-member calls have no inter-member dependencies. Spawn all LLM
  // calls concurrently.
  let mut handles = Vec::new();
  for (rendered, invariant_topics) in rendered_members {
    // Patch the rendered envelope to add the
    // `invariants_to_validate` top-level array. This is the
    // step-10-specific input; the unified renderer doesn't know
    // about it, so we stamp it in the pipeline-step layer.
    let augmented_json =
      augment_with_invariants_to_validate(&rendered.json, &invariant_topics);
    handles.push(tokio::spawn(async move {
      let result =
        task::extract_validations_from_batch(&augmented_json, &rendered.label)
          .await;
      (rendered.label, result)
    }));
  }

  let mut all_entries: Vec<task::ParsedValidation> = Vec::new();
  for handle in handles {
    match handle.await {
      Ok((_label, Ok(parsed))) => all_entries.extend(parsed.entries),
      Ok((label, Err(e))) => tracing::error!(
        "extract_validations_from_batch failed for {}: {}",
        label,
        e
      ),
      Err(e) => {
        tracing::error!("extract_validations_from_batch panicked: {}", e)
      }
    }
  }

  let total_validations = all_entries.len();
  let mut total_dropped_unknown_invariant: usize = 0;
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
    for v in all_entries {
      // Resolve the parent invariant to derive subject_topic. The
      // lookup is fallible (concurrent edits, stale parser output) —
      // on failure, warn and skip.
      let parent_subject_topic =
        match audit_data.topic_metadata.get(&v.invariant_topic) {
          Some(domain::TopicMetadata::InvariantTopic {
            subject_topic, ..
          }) => *subject_topic,
          _ => {
            tracing::warn!(
              "validations: parent {:?} missing from topic_metadata or \
               not an InvariantTopic; skipping",
              v.invariant_topic
            );
            tracing::debug!(
              "Dropped validation: verdict={:?} rationale={:?}",
              v.verdict,
              v.rationale,
            );
            total_dropped_unknown_invariant += 1;
            continue;
          }
        };

      let val_topic = topic::new_adversarial_property_topic(
        ids::allocate_adversarial_property_id(),
      );
      audit_data.topic_metadata.insert(
        val_topic,
        domain::TopicMetadata::ValidationTopic {
          topic: val_topic,
          invariant_topic: v.invariant_topic,
          subject_topic: parent_subject_topic,
          verdict: v.verdict,
          rationale: v.rationale,
          evidence_topics: v.evidence_topics,
          author: Author::System,
          created_at: None,
        },
      );
    }
    domain::rebuild_feature_context(audit_data);
  }

  if total_skipped_no_feature > 0 {
    tracing::warn!(
      "Skipped {} member(s) with no feature link \u{2014} reconciliation \
       gap",
      total_skipped_no_feature
    );
  }
  if total_skipped_no_invariants > 0 {
    tracing::debug!(
      "Skipped {} member(s) with no invariants on any non-pure subject",
      total_skipped_no_invariants
    );
  }
  if total_dropped_unknown_invariant > 0 {
    tracing::warn!(
      "Dropped {} validation(s) referencing missing or non-InvariantTopic \
       parents",
      total_dropped_unknown_invariant
    );
  }
  tracing::info!(
    "Completed validation generation: {} validations across {} member(s)",
    total_validations,
    total_members
  );

  Ok(())
}

/// Augment the unified-renderer's envelope with a top-level
/// `invariants_to_validate` array. The unified renderer is
/// step-agnostic; this addition is step-10-specific so it lives in
/// the pipeline step rather than in the renderer. Returns the
/// augmented JSON as a `String`.
fn augment_with_invariants_to_validate(
  base_json: &str,
  invariant_topics: &[topic::Topic],
) -> String {
  let Ok(mut value) = serde_json::from_str::<serde_json::Value>(base_json)
  else {
    // Defensive default: pass the JSON through unchanged. The
    // validator will report missing `invariants_to_validate` and
    // emit zero verdicts, but won't panic.
    return base_json.to_string();
  };
  if let Some(obj) = value.as_object_mut() {
    obj.insert(
      "invariants_to_validate".to_string(),
      serde_json::Value::Array(
        invariant_topics
          .iter()
          .map(|t| serde_json::Value::String(t.id()))
          .collect(),
      ),
    );
  }
  serde_json::to_string(&value).unwrap_or_else(|_| base_json.to_string())
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
    InvariantKind, SystemCharacteristicKind, ThreatActor, ThreatFeatureLink,
    ThreatFeatureRelation, ThreatSeverity, TopicMetadata, new_data_context,
  };
  use std::collections::HashSet;

  /// Exercises the proactive clear-on-rerun branch of `build_threats`.
  /// Because `subject_conditions` is empty the function early-returns
  /// before any LLM call — but only after running the clear block. So
  /// every pre-seeded `ThreatTopic` / `InvariantTopic` / `ValidationTopic`
  /// topic and every now-orphaned `threat_feature_links` entry must be
  /// gone afterward.
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
    let pre_validation_topic = topic::new_adversarial_property_topic(300);
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
          subject_topic,
          kind: InvariantKind::AccessGate,
          anchors: vec![],
          author: Author::AgentLarge,
          created_at: None,
          severity: None,
        },
      );
      audit_data.topic_metadata.insert(
        pre_validation_topic,
        TopicMetadata::ValidationTopic {
          topic: pre_validation_topic,
          invariant_topic: pre_invariant_topic,
          subject_topic,
          verdict: domain::ValidationVerdict::Enforced,
          rationale: "stale verdict from prior run".to_string(),
          evidence_topics: vec![],
          author: Author::AgentLarge,
          created_at: None,
        },
      );
      // Two impact-analysis links — one referring to the existing threat
      // (will be orphaned once the clear runs), one already orphaned by
      // a previous deletion that didn't prune. Both must be gone.
      audit_data.threat_feature_links.push(ThreatFeatureLink {
        threat_topic: pre_threat_topic,
        feature_topic: topic::new_spec_topic(1),
        relation: ThreatFeatureRelation::IsVulnerableTo,
        severity: ThreatSeverity::Medium,
      });
      audit_data.threat_feature_links.push(ThreatFeatureLink {
        threat_topic: topic::new_adversarial_property_topic(999),
        feature_topic: topic::new_spec_topic(2),
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
      assert!(
        audit_data
          .invariant_validations
          .contains_key(&pre_invariant_topic),
        "invariant_validations must be populated before the rerun"
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

    // Every pre-seeded ThreatTopic/InvariantTopic/ValidationTopic entry
    // is gone — the cascade clear covers the full threat→invariant→
    // validation chain to keep the audit data internally consistent.
    assert!(
      !audit_data.topic_metadata.contains_key(&pre_threat_topic),
      "stale ThreatTopic must be dropped on rerun"
    );
    assert!(
      !audit_data.topic_metadata.contains_key(&pre_invariant_topic),
      "stale InvariantTopic must be dropped on rerun"
    );
    assert!(
      !audit_data
        .topic_metadata
        .contains_key(&pre_validation_topic),
      "stale ValidationTopic must be dropped on rerun via cascade"
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
    assert!(
      audit_data.invariant_validations.is_empty(),
      "invariant_validations must be empty after cascade-clearing ValidationTopics"
    );
    assert!(
      audit_data.subject_validations.is_empty(),
      "subject_validations must be empty after cascade-clearing ValidationTopics"
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
            subject_topic: topic::new_node_topic(&1),
            kind: InvariantKind::AccessGate,
            anchors: vec![],
            author: Author::AgentLarge,
            created_at: None,
            severity: None,
          },
        );
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
    }
  }

  /// The spec is explicit that comments are *not* cleared by step 8's
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
      audit_data
        .topic_metadata
        .contains_key(&comment_topic_before),
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

  /// `render_security_characteristics` returns `None` when no Security
  /// characteristics exist. This is the structural switch that drives the
  /// `_ => format!("{}Batch:\n{}", EXTRACT_THREATS_PROMPT, batch_json)`
  /// arm of `extract_threats_from_batch` — the prompt is built without a
  /// `Security context:` block so the LLM doesn't see an empty header
  /// that could be mistaken for "no security considerations apply."
  #[test]
  fn render_security_characteristics_returns_none_when_no_characteristics() {
    let mut ctx = new_data_context();
    let audit_id = "no_chars_audit";
    assert!(
      ctx.create_audit(
        audit_id.to_string(),
        "Audit".to_string(),
        HashSet::new(),
        Some(
          "Raw security.md kept on AuditData for diagnostic purposes only."
            .to_string()
        ),
      )
    );
    let audit_data = ctx.get_audit(audit_id).unwrap();
    assert!(
      render_security_characteristics(audit_data).is_none(),
      "no Security characteristics → None (drives the no-`Security context:` \
       prompt branch in extract_threats_from_batch)"
    );
  }

  /// One Security characteristic renders as a single `- description`
  /// line. The verbatim-description property is what makes the threats
  /// LLM call work — the LLM reads the bullet, treats it as a system-
  /// wide claim, and uses it to pick realistic actors.
  #[test]
  fn render_security_characteristics_renders_single_characteristic_verbatim() {
    let mut ctx = new_data_context();
    let audit_id = "single_char_audit";
    assert!(ctx.create_audit(
      audit_id.to_string(),
      "Audit".to_string(),
      HashSet::new(),
      None,
    ));
    let char_topic = topic::new_spec_topic(101);
    let description = "The relayer key is trusted within the system \
                       trust boundary.";
    {
      let audit_data = ctx.get_audit_mut(audit_id).unwrap();
      audit_data.topic_metadata.insert(
        char_topic,
        TopicMetadata::CharacteristicTopic {
          topic: char_topic,
          description: description.to_string(),
          kind: SystemCharacteristicKind::Security,
          section_topic: None,
          author: Author::AgentLarge,
          created_at: None,
        },
      );
    }
    let audit_data = ctx.get_audit(audit_id).unwrap();
    let rendered = render_security_characteristics(audit_data)
      .expect("one Security characteristic → Some");
    assert_eq!(rendered, format!("- {}", description));
  }

  /// Multiple Security characteristics are sorted by numeric topic ID
  /// and joined with newlines. Determinism matters: the rendered block
  /// is interpolated into every per-function threats prompt in the
  /// audit, and non-deterministic ordering would invalidate prompt
  /// caching at the LLM router level.
  #[test]
  fn render_security_characteristics_sorts_by_numeric_id() {
    let mut ctx = new_data_context();
    let audit_id = "sorted_audit";
    assert!(ctx.create_audit(
      audit_id.to_string(),
      "Audit".to_string(),
      HashSet::new(),
      None,
    ));
    // Insert out of order so the sort step has to do real work — a
    // BTreeMap iterator would already produce sorted output, so testing
    // with already-sorted insertion would not actually exercise the
    // sort_by_key path.
    let high = topic::new_spec_topic(900);
    let low = topic::new_spec_topic(100);
    let mid = topic::new_spec_topic(500);
    {
      let audit_data = ctx.get_audit_mut(audit_id).unwrap();
      for (t, d) in [
        (high, "third claim"),
        (low, "first claim"),
        (mid, "second claim"),
      ] {
        audit_data.topic_metadata.insert(
          t,
          TopicMetadata::CharacteristicTopic {
            topic: t,
            description: d.to_string(),
            kind: SystemCharacteristicKind::Security,
            section_topic: None,
            author: Author::AgentLarge,
            created_at: None,
          },
        );
      }
    }
    let audit_data = ctx.get_audit(audit_id).unwrap();
    let rendered = render_security_characteristics(audit_data)
      .expect("non-empty Security set → Some");
    assert_eq!(rendered, "- first claim\n- second claim\n- third claim");
  }

  /// Non-`CharacteristicTopic` entries are skipped, and the rendered
  /// output never includes their topic IDs. This is the "don't mix the
  /// data sources" guarantee — the threats prompt's `Security context:`
  /// block contains only Security characteristic descriptions, never
  /// requirements/behaviors/features/conditions/comments.
  #[test]
  fn render_security_characteristics_ignores_non_characteristic_topics() {
    let mut ctx = new_data_context();
    let audit_id = "mixed_audit";
    assert!(ctx.create_audit(
      audit_id.to_string(),
      "Audit".to_string(),
      HashSet::new(),
      None,
    ));
    let char_topic = topic::new_spec_topic(1);
    let req_topic = topic::new_spec_topic(2);
    let beh_topic = topic::new_spec_topic(3);
    {
      let audit_data = ctx.get_audit_mut(audit_id).unwrap();
      audit_data.topic_metadata.insert(
        char_topic,
        TopicMetadata::CharacteristicTopic {
          topic: char_topic,
          description: "characteristic description".to_string(),
          kind: SystemCharacteristicKind::Security,
          section_topic: None,
          author: Author::AgentLarge,
          created_at: None,
        },
      );
      audit_data.topic_metadata.insert(
        req_topic,
        TopicMetadata::RequirementTopic {
          topic: req_topic,
          description: "requirement description".to_string(),
          section_topic: topic::Topic::Documentation(1),
          author: Author::System,
          created_at: None,
        },
      );
      audit_data.topic_metadata.insert(
        beh_topic,
        TopicMetadata::BehaviorTopic {
          topic: beh_topic,
          description: "behavior description".to_string(),
          member_topic: topic::new_node_topic(&1),
          author: Author::System,
          created_at: None,
        },
      );
    }
    let audit_data = ctx.get_audit(audit_id).unwrap();
    let rendered = render_security_characteristics(audit_data)
      .expect("a Security characteristic exists → Some");
    assert_eq!(rendered, "- characteristic description");
    assert!(
      !rendered.contains("requirement description"),
      "requirement bodies must not leak into the threats `Security \
       context:` block"
    );
    assert!(
      !rendered.contains("behavior description"),
      "behavior bodies must not leak into the threats `Security \
       context:` block"
    );
  }
}

#[cfg(test)]
mod build_invariants_tests {
  use super::*;
  use crate::collaborator::models::Author;
  use crate::domain::{
    InvariantKind, ThreatSeverity, TopicMetadata, new_data_context,
  };
  use std::collections::HashSet;

  /// Exercises the proactive clear-on-rerun branch of `build_invariants`.
  /// Because `subject_threats` is empty the function early-returns before any
  /// LLM call — but only after running the clear block. So every pre-seeded
  /// `InvariantTopic` and `ValidationTopic` must be gone afterward and the
  /// `subject_invariants` / `threat_invariants` / `invariant_validations` /
  /// `subject_validations` reverse indexes must reflect the cleared state.
  ///
  /// We intentionally do *not* pre-seed a `ThreatTopic` here: doing so would
  /// trip `rebuild_feature_context` into populating `subject_threats`, which
  /// would in turn defeat the early-return path and push the test toward an
  /// LLM call. The "step 9's clear leaves `ThreatTopic` entries alone"
  /// invariant is covered by the implementation pattern itself (the retain
  /// filters out `InvariantTopic` and `ValidationTopic` — the downstream
  /// cascade for consistency with `build_threats` — and nothing upstream)
  /// and is statically obvious: adding a `ThreatTopic` to the retain set
  /// would require an explicit edit to the function and would also break
  /// the `build_threats` tests above.
  #[tokio::test]
  async fn build_invariants_clears_stale_invariant_state_on_rerun() {
    let mut ctx = new_data_context();
    let audit_id = "test_audit";
    assert!(ctx.create_audit(
      audit_id.to_string(),
      "Test".to_string(),
      HashSet::new(),
      None,
    ));

    let pre_invariant_topic = topic::new_adversarial_property_topic(200);
    let pre_threat_topic = topic::new_adversarial_property_topic(100);
    let pre_validation_topic = topic::new_adversarial_property_topic(300);
    let subject_topic = topic::new_node_topic(&42);

    {
      let audit_data = ctx.get_audit_mut(audit_id).unwrap();
      audit_data.topic_metadata.insert(
        pre_invariant_topic,
        TopicMetadata::InvariantTopic {
          topic: pre_invariant_topic,
          description: "stale invariant from prior run".to_string(),
          threat_topic: pre_threat_topic,
          subject_topic,
          kind: InvariantKind::AccessGate,
          anchors: vec![],
          author: Author::AgentLarge,
          created_at: None,
          severity: Some(ThreatSeverity::High),
        },
      );
      audit_data.topic_metadata.insert(
        pre_validation_topic,
        TopicMetadata::ValidationTopic {
          topic: pre_validation_topic,
          invariant_topic: pre_invariant_topic,
          subject_topic,
          verdict: domain::ValidationVerdict::Enforced,
          rationale: "stale verdict from prior run".to_string(),
          evidence_topics: vec![],
          author: Author::AgentLarge,
          created_at: None,
        },
      );
      domain::rebuild_feature_context(audit_data);
      // Sanity: indexes are populated before the call.
      assert!(audit_data.threat_invariants.contains_key(&pre_threat_topic));
      assert!(audit_data.subject_invariants.contains_key(&subject_topic));
      assert!(
        audit_data
          .invariant_validations
          .contains_key(&pre_invariant_topic)
      );
      assert!(audit_data.subject_validations.contains_key(&subject_topic));
      // No threats — `subject_threats` is empty, so build_invariants takes
      // the early-return path after the clear runs.
      assert!(audit_data.subject_threats.is_empty());
    }

    let state = PipelineState::new(Arc::new(Mutex::new(ctx)));
    build_invariants(&state, audit_id)
      .await
      .expect("build_invariants must early-return cleanly when no threats");

    let ctx = state.data_context.lock().unwrap();
    let audit_data = ctx.get_audit(audit_id).unwrap();

    // The InvariantTopic is gone.
    assert!(
      !audit_data.topic_metadata.contains_key(&pre_invariant_topic),
      "stale InvariantTopic must be dropped on rerun"
    );
    // The downstream ValidationTopic is also gone (cascade clear).
    assert!(
      !audit_data
        .topic_metadata
        .contains_key(&pre_validation_topic),
      "stale ValidationTopic must be dropped on rerun via cascade"
    );

    // Reverse indexes must reflect the cleared state (the post-clear
    // rebuild_feature_context inside build_invariants handles this).
    assert!(
      audit_data.threat_invariants.is_empty(),
      "threat_invariants must be empty after clearing all InvariantTopics"
    );
    assert!(
      audit_data.subject_invariants.is_empty(),
      "subject_invariants must be empty after clearing all InvariantTopics"
    );
    assert!(
      audit_data.invariant_validations.is_empty(),
      "invariant_validations must be empty after clearing all ValidationTopics"
    );
    assert!(
      audit_data.subject_validations.is_empty(),
      "subject_validations must be empty after clearing all ValidationTopics"
    );
  }

  /// Two consecutive `build_invariants` runs, each re-seeded with stale
  /// invariant state between calls, must leave the audit clean without
  /// duplicate `InvariantTopic` entries. The spec calls this out explicitly
  /// under Phase 5 Final Verification ("rerunning the pipeline twice
  /// produces no duplicated `InvariantTopic` entries"); re-seeding between
  /// calls is the unit-test equivalent of running the full pipeline twice
  /// (which would itself repopulate via LLM output).
  #[tokio::test]
  async fn build_invariants_clear_runs_on_every_call() {
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
      // prior step-8 produced output, then re-run again." Re-using nearby
      // topic IDs is the worst-case for duplicate accumulation.
      {
        let mut c = state.data_context.lock().unwrap();
        let audit_data = c.get_audit_mut(audit_id).unwrap();
        let pre_invariant = topic::new_adversarial_property_topic(200 + run);
        audit_data.topic_metadata.insert(
          pre_invariant,
          TopicMetadata::InvariantTopic {
            topic: pre_invariant,
            description: "stale".to_string(),
            threat_topic: topic::new_adversarial_property_topic(100 + run),
            subject_topic: topic::new_node_topic(&1),
            kind: InvariantKind::AccessGate,
            anchors: vec![],
            author: Author::AgentLarge,
            created_at: None,
            severity: Some(ThreatSeverity::Medium),
          },
        );
      }

      build_invariants(&state, audit_id).await.unwrap();

      let c = state.data_context.lock().unwrap();
      let audit_data = c.get_audit(audit_id).unwrap();
      let invariant_count = audit_data
        .topic_metadata
        .values()
        .filter(|m| matches!(m, TopicMetadata::InvariantTopic { .. }))
        .count();
      assert_eq!(
        invariant_count, 0,
        "run {}: stale InvariantTopic must be cleared",
        run
      );
    }
  }

  /// The spec is explicit that comments are *not* cleared by step 9's
  /// retain — the no-invariant rationale comments posted on threat topics
  /// must persist across reruns so the auditor can reply in-thread. A
  /// future refactor that adds `CommentTopic` to the retain filter would
  /// silently break this guarantee; this test pins the behavior.
  #[tokio::test]
  async fn build_invariants_preserves_comments_on_clear() {
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

    let threat_topic = topic::new_adversarial_property_topic(100);
    // Insert a synthetic comment on the threat before handing the context
    // to the pipeline. Capture the allocated comment topic (negative-id
    // range) so we can assert it survives the clear.
    let comment_topic_before: topic::Topic = {
      let audit_data = ctx.get_audit_mut(audit_id).unwrap();
      create_synthetic_dev_comment(
        &threat_topic,
        "[step-8 / no-invariant] mitigated by economic incentives",
        CommentType::Note,
        Author::System,
        audit_data,
      );
      *audit_data
        .comment_index
        .get(&threat_topic)
        .and_then(|v| v.first())
        .expect("synthetic comment must be indexed")
    };

    let state = PipelineState::new(Arc::new(Mutex::new(ctx)));
    build_invariants(&state, audit_id).await.unwrap();

    let c = state.data_context.lock().unwrap();
    let audit_data = c.get_audit(audit_id).unwrap();
    assert!(
      audit_data
        .topic_metadata
        .contains_key(&comment_topic_before),
      "comment topic must survive build_invariants clear"
    );
    assert!(
      matches!(
        audit_data.topic_metadata.get(&comment_topic_before),
        Some(TopicMetadata::CommentTopic { .. })
      ),
      "preserved entry must still be a CommentTopic"
    );
    assert_eq!(
      audit_data.comment_index.get(&threat_topic).map(|v| v.len()),
      Some(1),
      "comment_index entry must survive the clear"
    );
  }

  /// `AuditNotFound` surfaces as a typed error rather than a panic when the
  /// audit_id is unknown. Mirrors the error-path contract of every other
  /// pipeline step; small but worth pinning because lock-acquire + audit-
  /// lookup is repeated three times in `build_invariants` and a regression
  /// in any one of them would shape this error differently.
  #[tokio::test]
  async fn build_invariants_returns_audit_not_found_for_unknown_audit() {
    let state = PipelineState::new(Arc::new(Mutex::new(new_data_context())));
    let err = build_invariants(&state, "missing").await.unwrap_err();
    match err {
      PipelineError::AuditNotFound { audit_id } => {
        assert_eq!(audit_id, "missing");
      }
      other => panic!("expected AuditNotFound, got {:?}", other),
    }
  }
}

/// Permanent drift guard: no renderer other than characteristic
/// synthesis itself may leak `CharacteristicTopic` IDs into a prompt.
///
/// The Phase 4 contract separates feature synthesis (sees requirements +
/// behaviors only) from characteristic synthesis (sees the extracted
/// characteristics + raw `security.md` only). Phase 5 added step 8
/// (threats) as a *partial* consumer: its `Security context:` block
/// receives Security characteristic *descriptions* via
/// `render_security_characteristics`, but never their `S`-prefixed topic
/// IDs — the bullet output is deliberately opaque so the LLM cannot
/// treat characteristic topics as addressable cross-function anchors in
/// `evidence_topics`. Steps 9 (invariants) and 10 (validations) inherit
/// the same opaque-descriptions-only consumption pattern. The contract
/// is enforced not by prose in the prompts but by what each step's
/// renderer actually emits — if a renderer for steps 4 (features), 6
/// (functional properties), 7 (conditions), 8 (threats), 9 (invariants),
/// or 10 (validations) ever started including a `CharacteristicTopic`
/// ID in its rendered context, the boundary would silently leak.
///
/// This module covers the two renderers that operate on the whole
/// `AuditData` and could plausibly leak via a `topic_metadata` walk:
/// step 4's `render_reconciliation_context` (negative case — must not
/// leak) and step 5's `render_characteristic_synthesis_context`
/// (positive case — *must* surface characteristics, so a regression
/// that silently stops rendering them is also caught).
///
/// Steps 6 and 7 render through `context::render_batch_for_extraction`,
/// which is member-scoped and does not take a path through
/// `audit_data.characteristics`. Steps 8 (threats), 9 (invariants), and
/// 10 (validations) also use `render_batch_for_extraction` for the
/// per-function batch, plus the audit-wide
/// `render_security_characteristics` for the `Security context:` block —
/// covered by the `build_threats_tests::render_security_characteristics_*`
/// tests that assert no topic IDs and no non-characteristic descriptions
/// leak into the block. The
/// realistic prompt-text regression vector is a prompt edit that asks
/// the LLM to consider characteristics — that vector is guarded by
/// `task::characteristic_synthesis_tests::
/// other_pipeline_prompts_do_not_mention_characteristics`, which asserts
/// the prompt constants for steps 4/6/7/8/9/10 never reference the word
/// "characteristic".
///
/// Per the build plan, these drift-guard tests "stay in the suite
/// permanently." Do not delete or weaken them without replacing them
/// with a stricter mechanical guarantee (e.g., a typed renderer
/// interface that statically excludes `CharacteristicTopic`).
#[cfg(test)]
mod characteristic_synthesis_tests {
  use super::*;
  use crate::collaborator::models::Author;
  use crate::domain::{
    SystemCharacteristicKind, TopicMetadata, new_data_context,
  };
  use std::collections::HashSet;

  fn seed_audit_with_characteristics()
  -> (PipelineState, String, Vec<topic::Topic>) {
    let mut ctx = new_data_context();
    let audit_id = "drift_guard_audit";
    assert!(ctx.create_audit(
      audit_id.to_string(),
      "Drift Guard".to_string(),
      HashSet::new(),
      Some("Raw security.md notes for synthesis only.".to_string()),
    ));

    // Pick IDs in a high range so they can't accidentally collide with
    // requirement/behavior IDs allocated by other tests in the same
    // process. The shared `S` counter is reseeded by other tests; we use
    // explicit literals here to keep the assertion list stable across
    // test ordering.
    let char_topics = vec![
      topic::new_spec_topic(7001),
      topic::new_spec_topic(7002),
      topic::new_spec_topic(7003),
    ];
    let req_topic = topic::new_spec_topic(8001);
    let beh_topic = topic::new_spec_topic(8002);
    let section_topic = topic::Topic::Documentation(42);

    let audit_data = ctx.get_audit_mut(audit_id).unwrap();
    for (i, t) in char_topics.iter().enumerate() {
      audit_data.topic_metadata.insert(
        *t,
        TopicMetadata::CharacteristicTopic {
          topic: *t,
          description: format!("Characteristic claim #{}", i + 1),
          kind: SystemCharacteristicKind::Security,
          section_topic: if i == 0 { Some(section_topic) } else { None },
          author: Author::AgentLarge,
          created_at: None,
        },
      );
      audit_data.characteristics.insert(
        *t,
        domain::Characteristic {
          documentation_topics: vec![section_topic],
        },
      );
    }

    // One requirement and one behavior so the reconciliation renderer has
    // a non-empty payload. Without these, the renderer returns `[]` and
    // the absence assertion below would be vacuously satisfied.
    audit_data.topic_metadata.insert(
      req_topic,
      TopicMetadata::RequirementTopic {
        topic: req_topic,
        description: "A documented requirement.".to_string(),
        section_topic,
        author: Author::System,
        created_at: None,
      },
    );
    audit_data.requirements.insert(
      req_topic,
      domain::Requirement {
        documentation_topics: vec![section_topic],
      },
    );

    audit_data.topic_metadata.insert(
      beh_topic,
      TopicMetadata::BehaviorTopic {
        topic: beh_topic,
        description: "An observed behavior.".to_string(),
        member_topic: topic::new_node_topic(&1),
        author: Author::System,
        created_at: None,
      },
    );

    domain::rebuild_feature_context(audit_data);

    let state = PipelineState::new(Arc::new(Mutex::new(ctx)));
    (state, audit_id.to_string(), char_topics)
  }

  fn assert_no_characteristic_ids_in(
    rendered: &str,
    char_topics: &[topic::Topic],
    renderer_name: &str,
  ) {
    for t in char_topics {
      let id = t.id();
      assert!(
        !rendered.contains(&id),
        "{} must not emit CharacteristicTopic {} into its prompt context — \
         renderer leak would break the Phase 4 boundary contract. \
         Rendered: {}",
        renderer_name,
        id,
        rendered
      );
    }
  }

  /// Step 4 (feature synthesis) renderer must not leak characteristic
  /// IDs. `render_reconciliation_context` walks `requirements` and
  /// `member_behaviors`; if it ever started iterating `characteristics`
  /// or scanning all `topic_metadata` for spec-family entries, every
  /// `CharacteristicTopic` ID would leak into the feature prompt.
  #[test]
  fn step_4_reconciliation_renderer_excludes_characteristic_topics() {
    let (state, audit_id, char_topics) = seed_audit_with_characteristics();
    let ctx = state.data_context.lock().unwrap();
    let audit_data = ctx.get_audit(&audit_id).unwrap();

    let (requirements_json, behaviors_json) =
      task::render_reconciliation_context(audit_data);

    assert_no_characteristic_ids_in(
      &requirements_json,
      &char_topics,
      "render_reconciliation_context::requirements_json",
    );
    assert_no_characteristic_ids_in(
      &behaviors_json,
      &char_topics,
      "render_reconciliation_context::behaviors_json",
    );

    // Sanity-check the rendered payload is not vacuously empty — the
    // negative assertion above means nothing if the renderer returned
    // `[]` for unrelated reasons.
    assert!(
      requirements_json.contains("S8001"),
      "seeded requirement S8001 must appear in the rendered context"
    );
  }

  /// The characteristic synthesis renderer (step 5) is the *only*
  /// renderer that's allowed to emit `CharacteristicTopic` IDs. This is
  /// the positive half of the boundary contract — without it, a
  /// regression could pass the negative checks above by silently
  /// stopping rendering characteristics altogether.
  #[test]
  fn step_5_characteristic_synthesis_renderer_does_emit_characteristic_topics()
  {
    let (state, audit_id, char_topics) = seed_audit_with_characteristics();
    let ctx = state.data_context.lock().unwrap();
    let audit_data = ctx.get_audit(&audit_id).unwrap();

    let (security_notes, extracted_json) =
      task::render_characteristic_synthesis_context(audit_data);

    assert!(
      security_notes.contains("security.md notes"),
      "render_characteristic_synthesis_context must surface security_notes"
    );
    for t in &char_topics {
      let id = t.id();
      assert!(
        extracted_json.contains(&id),
        "render_characteristic_synthesis_context must emit characteristic \
         topic {} into the extracted_json payload",
        id
      );
    }
  }
}

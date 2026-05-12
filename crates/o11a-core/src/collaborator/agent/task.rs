//! Agent tasks for building an audit's security model from documentation and
//! source code.
//!
//! # Design principles
//!
//! **Documentation is untrusted.** It represents the developer's *claimed*
//! behavior, not verified truth.
//!
//! **Requirements capture documented claims.** Each requirement states what
//! the documentation says the system does, grouped by the documentation
//! section it was extracted from.
//!
//! **Features are synthesized from reconciliation.** Features are not created
//! upfront — they emerge from reconciling documentation-derived requirements
//! with code-derived behaviors.
//!
//! **Functional semantics bridge docs and code.** Semantic linking connects
//! documentation sections to code declarations, defining what each declaration
//! represents in project context. This is done before behavior extraction so
//! behaviors carry business-level meaning.
//!
//! # Pipeline
//!
//! 1. **Normalize** (`normalize_documentation`): Strip emojis, HTML, etc.
//! 2. **Extract requirements** (`extract_requirements_from_documentation`):
//!    Per-document extraction grouped by section, then consolidation.
//!    Emits feature requirements *and* raw system characteristics as
//!    parallel arrays per section.
//! 3. **Semantic linking** (LLM steps 2/4/5 — steps 1 and 3 are mechanical /
//!    BM25): Connect doc sections to code declarations across three LLM
//!    synthesis steps that build on each other (`link_contracts` →
//!    `link_member_signatures` → `link_member_bodies`), producing functional
//!    semantics with provenance.
//! 4. **Extract behaviors** (`extract_behaviors_from_batch`): DAG-batched
//!    extraction with semantics + callee behaviors in context.
//! 5. **Synthesize features** (`synthesize_features`): Single-pass LLM
//!    reconciliation of all requirements and behaviors into features.
//!    Characteristics are intentionally excluded from this prompt; see
//!    Phase 4 boundary contract in `pipeline::synthesize_characteristics`.
//! 6. **Synthesize characteristics** (`synthesize_characteristics`):
//!    Consolidate the raw characteristics from step 2 with the audit's
//!    `security.md` notes. The threats step consumes the
//!    `Security`-kind subset.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::json;

use crate::collaborator::agent::context;
use crate::collaborator::agent::router::{self, JsonSchema, TaskSize};
use crate::collaborator::models::Author;
use crate::domain::{self, AST, AuditData, Requirement, topic};

/// Errors produced by LLM-driven agent tasks.
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
  #[error("HTTP error: {0}")]
  HttpError(#[from] reqwest::Error),
  #[error("JSON parse error in {label}: {source}")]
  JsonParse {
    label: String,
    #[source]
    source: serde_json::Error,
  },
  #[error("missing env var: {0}")]
  MissingEnv(String),
  #[error("missing field: {0}")]
  MissingField(&'static str),
  #[error("I/O error: {0}")]
  Io(#[from] std::io::Error),
  #[error("{0}")]
  Other(String),
}

/// Raw requirement as returned by the LLM (no topic ID yet).
#[derive(Deserialize)]
struct LLMRequirement {
  description: String,
  documentation_topics: Vec<String>,
}

/// Render all documentation ASTs as separate per-file JSON strings for iterative processing.
pub fn render_documentation_files(audit_data: &AuditData) -> Vec<String> {
  let mut files = Vec::new();

  for (path, ast) in &audit_data.asts {
    let doc_ast = match ast {
      AST::Documentation(doc_ast) => doc_ast,
      _ => continue,
    };

    let rendered: Vec<serde_json::Value> = doc_ast
      .nodes
      .iter()
      .map(|node| {
        context::render_documentation_ast_snippet(node, audit_data, None)
      })
      .collect();

    let file_json = serde_json::json!({
      "file": path.file_path,
      "content": rendered,
    });

    files.push(serde_json::to_string(&file_json).unwrap_or_default());
  }

  files
}

/// Prompt for extracting feature requirements and system characteristics from
/// a single documentation file, grouped by section.
const EXTRACT_REQUIREMENTS_PROMPT: &str = "Below is a documentation file for a smart contract project, \
rendered as structured JSON with topic IDs (D-prefixed, like \"D42\") \
on each section, paragraph, list, and code block.\n\n\
Your task is to extract two parallel sets of claims from this document:\n\
1. **Feature requirements** — what the documentation claims the system *does* \
in the course of fulfilling a feature. Each requirement captures a documented \
behavior, constraint, or behavioral guarantee that an auditor must verify is \
correctly implemented for a particular code feature.\n\
2. **System characteristics** — system-wide claims that an auditor must take \
as developer-asserted ground truth when reasoning about adversarial scenarios. \
Examples: trust assumptions, role definitions, threat-model statements, and \
system-wide security properties that hold across the entire codebase rather \
than scoped to one feature. Each characteristic has a `kind`; only \
`\"security\"` is supported now.\n\n\
Both feature requirements and system characteristics will be used by \
independent security auditors organizing their review. The documentation is \
developer-provided and **not trusted** — it represents claimed behavior, not \
verified truth.\n\n\
Feature requirements define the **scope of what an auditor must verify**. \
State them broadly enough that the auditor is not anchored to a developer's \
stated implementation — the auditor should think critically and consider \
attack vectors beyond what the documentation explicitly addresses. For \
example, prefer \"withdrawals must be safe from reentrancy\" over \"balance \
must be zeroed before the external call\", because the latter assumes a \
specific implementation strategy.\n\n\
**Do not use code declaration names** (function names, variable names, contract \
names) in either array. Describe capabilities in behavioral terms. For \
example, instead of \"invalidateParticipations() must only be callable by the \
authorized relayer\", write \"Only the authorized relayer is allowed to \
invalidate participations.\" The original declaration names are preserved in \
the linked documentation topics for traceability.\n\n\
**Each item must describe exactly one claim.** If a documentation passage \
describes two distinct things (e.g., access control AND batching support), \
split them into separate items. For example, \"Only the authorized relayer \
can invalidate participations\" and \"Invalidating multiple participations \
in a single call must be supported\" are two items, not one joined with \
\"and.\"\n\n\
**Overlap rule.** A single documented claim that is both a feature-level \
requirement *and* a system-wide security characteristic must be emitted twice \
— once in `feature_requirements` (framed as feature behavior, e.g., \"The \
withdraw flow must reject reentrant calls\") and once in \
`system_characteristics` (framed as a system-wide guarantee or threat-model \
statement, e.g., \"The protocol assumes no external call can re-enter a \
state-mutating function before its caller frame completes\"). The same \
`documentation_topics` may appear in both arrays.\n\n\
Group both arrays under the documentation **section** they were extracted \
from, using the section's D-prefixed topic ID.\n\n\
Return a JSON object with a `sections` key whose value is an array of \
section groups. Each section group has:\n\
- `section_topic`: the D-prefixed topic ID of the section header (e.g., \"D5\")\n\
- `feature_requirements`: an array of feature-level requirement objects, each with:\n\
  - `description`: a single, specific, testable statement of what the system must do or prevent\n\
  - `documentation_topics`: an array of D-prefixed topic IDs for every paragraph, \
list, or code block within this section that informed this specific requirement\n\
- `system_characteristics`: an array of system-wide characteristic objects, each with:\n\
  - `description`: a single, specific statement of the system-wide guarantee, \
trust assumption, role definition, or threat-model claim (framed as a \
system-wide property, not as feature behavior)\n\
  - `kind`: the characteristic kind (only `\"security\"` is supported now — \
omit any characteristic whose kind would fall outside this set rather than \
emitting a sentinel value)\n\
  - `documentation_topics`: an array of D-prefixed topic IDs for every paragraph, \
list, or code block within this section that informed this characteristic\n\n\
Rules:\n\
- Every documentation topic ID that describes system behavior, requirements, \
constraints, security concerns, or invariants should appear in at least one \
item across the two arrays. Exclude boilerplate like tables of contents, \
version history, author credits, and headings.\n\
- A documentation topic may appear in multiple items if relevant to more than one.\n\
- Do not invent topic IDs. Only use IDs present in the documentation.\n\
- Preserve the developer's specific terminology and phrasing nuances. \
Subtle differences in how the documentation describes constraints often \
reflect important design distinctions.\n\
- Include both **happy-path** items (what the system should do) and \
**non-happy-path** items (what the system must prevent).\n\
- If the documentation describes security threats, attack vectors, access \
control rules, or invariants that apply system-wide, capture those as system \
characteristics; if they apply to a specific feature, capture them as feature \
requirements; if both, emit twice per the overlap rule.\n\
- Either array may be empty for a section that has no items of that kind, \
but a section group should not be emitted unless at least one of its arrays \
is non-empty.\n\
- Sections with no behavioral content (boilerplate, navigation, etc.) should be omitted.\n\
- If the document contains no behavioral content at all, return `{\"sections\": []}`.\n\n\
Documentation:\n";

/// Prompt for consolidating feature requirements extracted from multiple
/// documents. System characteristics bypass this consolidation step entirely
/// — they accumulate across per-document responses and are consolidated later
/// during characteristic synthesis (Phase 4), which has additional context
/// (the raw `security.md`) that the LLM needs to merge characteristics well.
/// Centralising characteristic consolidation in one step keeps the data model
/// simple and avoids the LLM reasoning about characteristic merging twice.
const CONSOLIDATE_REQUIREMENTS_PROMPT: &str = "Below are feature requirements extracted independently \
from multiple documentation files for a smart contract project, grouped by \
documentation section. Because each file was processed separately, some \
requirements may overlap or describe the same claim.\n\n\
Your task is to consolidate these into a single, deduplicated list of \
feature requirements grouped by section. For each group of similar \
requirements across different sections, merge them into the most specific \
section and combine their documentation_topics.\n\n\
Return a JSON object with a `sections` key whose value is an array of section \
groups. Each section group has:\n\
- `section_topic`: the D-prefixed topic ID\n\
- `feature_requirements`: array of feature-requirement objects with \
`description` and `documentation_topics`\n\
- `system_characteristics`: an empty array (characteristics are consolidated \
in a later pipeline step — emit `[]` here)\n\n\
Rules:\n\
- Merge duplicate requirements that describe the same claim, keeping the more \
specific wording and combining documentation_topics. Preserve the developer's \
specific terminology and phrasing nuances — if merging requirements that describe \
the same area with meaningfully different emphasis or detail, make sure to \
preserve the details and nuances of both perspectives.\n\
- Do not drop any unique requirements.\n\
- Do not modify documentation_topics arrays, just combine them when merging.\n\
- If a requirement appears in multiple sections, keep it in the most specific section.\n\
- Do not use code declaration names in requirements — describe capabilities \
in behavioral terms.\n\
- Each requirement must describe exactly one claim. If a merged requirement \
covers two distinct things, split it back into separate requirements.\n\
- Every section group must include both `feature_requirements` and \
`system_characteristics` keys; the latter is always `[]` at this stage.\n\
- If no requirements remain after deduplication, return `{\"sections\": []}`.\n\n\
Requirements to consolidate:\n";

/// Raw characteristic as returned by the LLM (no topic ID yet).
#[derive(Deserialize)]
struct LLMCharacteristic {
  description: String,
  documentation_topics: Vec<String>,
  /// Characteristic kind, parsed into `SystemCharacteristicKind` at materialise
  /// time. Currently only `"security"` is supported; the JSON Schema enum
  /// rejects other strings at the router boundary, but we re-validate here
  /// so a malformed response surfaces as a `TaskError` rather than a panic.
  kind: String,
}

/// Raw section group as returned by the requirement extraction LLM. Each
/// section carries two parallel arrays: feature-level requirements (consumed
/// by feature synthesis) and system-wide characteristics (consumed by
/// characteristic synthesis in Phase 4, then by threats).
#[derive(Deserialize)]
struct LLMSectionGroup {
  section_topic: String,
  feature_requirements: Vec<LLMRequirement>,
  system_characteristics: Vec<LLMCharacteristic>,
}

#[derive(Deserialize)]
struct LLMRequirementsResponse {
  sections: Vec<LLMSectionGroup>,
}

static REQUIREMENTS_SCHEMA: LazyLock<JsonSchema> =
  LazyLock::new(|| JsonSchema {
    name: "requirements",
    schema: json!({
      "type": "object",
      "additionalProperties": false,
      "required": ["sections"],
      "properties": {
        "sections": {
          "type": "array",
          "items": {
            "type": "object",
            "additionalProperties": false,
            "required": [
              "section_topic",
              "feature_requirements",
              "system_characteristics"
            ],
            "properties": {
              "section_topic": { "type": "string" },
              "feature_requirements": {
                "type": "array",
                "items": {
                  "type": "object",
                  "additionalProperties": false,
                  "required": ["description", "documentation_topics"],
                  "properties": {
                    "description": { "type": "string" },
                    "documentation_topics": {
                      "type": "array",
                      "items": { "type": "string" }
                    }
                  }
                }
              },
              "system_characteristics": {
                "type": "array",
                "items": {
                  "type": "object",
                  "additionalProperties": false,
                  "required": ["description", "kind", "documentation_topics"],
                  "properties": {
                    "description": { "type": "string" },
                    "kind": {
                      "type": "string",
                      "enum": ["security"]
                    },
                    "documentation_topics": {
                      "type": "array",
                      "items": { "type": "string" }
                    }
                  }
                }
              }
            }
          }
        }
      }
    }),
    empty_response: r#"{"sections":[]}"#,
  });

/// Result of parsing an extraction LLM response: feature requirements and
/// system characteristics, both grouped by documentation section. Feature
/// synthesis (step 4) consumes the requirement half; characteristic
/// synthesis (step 5, Phase 4) consumes the characteristic half. Topic
/// metadata for both kinds shares a single map — local Topic IDs are
/// disjoint within one parse, so a merged `topic_metadata` is safe.
pub struct ParsedRequirements {
  pub requirements: BTreeMap<topic::Topic, Requirement>,
  pub topic_metadata: BTreeMap<topic::Topic, domain::TopicMetadata>,
  /// Section D-topic → S-topic list, preserving document structure.
  pub section_requirements: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Characteristics keyed by their (locally-allocated) S-prefixed topic.
  /// Always carries `section_topic = Some(_)` at this stage; characteristic
  /// synthesis (Phase 4) is the only step that introduces `None` entries
  /// (claims originating from the raw `security.md` with no section anchor).
  pub characteristics: BTreeMap<topic::Topic, domain::Characteristic>,
  /// Section D-topic → S-topic list for characteristics.
  pub section_characteristics: BTreeMap<topic::Topic, Vec<topic::Topic>>,
}

/// Parse the LLM response for section-grouped requirements and
/// characteristics.
///
/// Feature requirements and system characteristics both materialise as
/// `Topic::Spec(_)` entries in `topic_metadata`, distinguished by variant
/// (`RequirementTopic` vs `CharacteristicTopic`). A single shared counter
/// allocates local IDs across both so the merged metadata map cannot
/// collide on a `Topic::Spec(_)` key.
///
/// Defensive parsing:
/// - A malformed `section_topic` makes the whole section unusable (we'd have
///   no way to anchor its requirements/characteristics back to the doc),
///   so the section is logged and skipped.
/// - A malformed `documentation_topic` on an otherwise-valid item is logged
///   and dropped; the item itself is preserved (its description still
///   captures the claim — traceability is partially lost but the security
///   model isn't).
/// - An unknown `SystemCharacteristicKind` fails the whole parse loudly
///   per the build-plan decision: silent dropping is the wrong failure
///   mode for security-relevant artifacts.
///
/// `Author::System` is set on extracted entities directly so the pipeline
/// can preserve parser-produced metadata verbatim instead of rewriting
/// authorship in a second pass. Characteristic synthesis (Phase 4) writes
/// its own author (`AgentLarge` for `TaskSize::Large`) and is therefore
/// the only step that needs to override.
fn parse_requirements_response(
  response: &str,
  prompt: &str,
) -> Result<ParsedRequirements, TaskError> {
  let wrapper: LLMRequirementsResponse =
    router::parse_response(response, "requirements", prompt)?;
  let raw_sections = wrapper.sections;

  let mut requirements = BTreeMap::new();
  let mut topic_metadata = BTreeMap::new();
  let mut section_requirements = BTreeMap::new();
  let mut characteristics = BTreeMap::new();
  let mut section_characteristics = BTreeMap::new();
  let mut next_local_id = 0i32;

  for section in raw_sections {
    let section_topic =
      match topic::parse_documentation_topic(&section.section_topic) {
        Ok(t) => t,
        Err(e) => {
          tracing::warn!(
            "skipping section with malformed section_topic '{}': {}",
            section.section_topic,
            e
          );
          continue;
        }
      };

    let mut section_req_topics = Vec::new();
    for raw_req in section.feature_requirements {
      next_local_id += 1;
      let req_topic = topic::new_spec_topic(next_local_id);
      section_req_topics.push(req_topic);

      let doc_topics = parse_documentation_topic_list(
        raw_req.documentation_topics,
        "feature_requirement",
        &section.section_topic,
      );

      topic_metadata.insert(
        req_topic,
        domain::TopicMetadata::RequirementTopic {
          topic: req_topic,
          description: raw_req.description,
          section_topic,
          author: Author::System,
          created_at: None,
        },
      );

      requirements.insert(
        req_topic,
        Requirement {
          documentation_topics: doc_topics,
        },
      );
    }
    if !section_req_topics.is_empty() {
      section_requirements.insert(section_topic, section_req_topics);
    }

    let mut section_char_topics = Vec::new();
    for raw_char in section.system_characteristics {
      let kind = domain::SystemCharacteristicKind::parse_str(&raw_char.kind)
        .ok_or_else(|| {
          TaskError::Other(format!(
            "unknown SystemCharacteristicKind '{}' in section {}",
            raw_char.kind, section.section_topic
          ))
        })?;

      next_local_id += 1;
      let char_topic = topic::new_spec_topic(next_local_id);
      section_char_topics.push(char_topic);

      let doc_topics = parse_documentation_topic_list(
        raw_char.documentation_topics,
        "system_characteristic",
        &section.section_topic,
      );

      topic_metadata.insert(
        char_topic,
        domain::TopicMetadata::CharacteristicTopic {
          topic: char_topic,
          description: raw_char.description,
          kind,
          section_topic: Some(section_topic),
          author: Author::System,
          created_at: None,
        },
      );

      characteristics.insert(
        char_topic,
        domain::Characteristic {
          documentation_topics: doc_topics,
        },
      );
    }
    if !section_char_topics.is_empty() {
      section_characteristics.insert(section_topic, section_char_topics);
    }
  }

  Ok(ParsedRequirements {
    requirements,
    topic_metadata,
    section_requirements,
    characteristics,
    section_characteristics,
  })
}

/// Parse a list of D-prefixed documentation topic IDs returned by the
/// extraction LLM. Malformed entries are dropped with a warning rather
/// than aborting the parse — the schema only constrains entries to be
/// strings, not to be well-formed topic IDs, so a hallucinated value
/// must not take down the whole extraction pass.
fn parse_documentation_topic_list(
  ids: Vec<String>,
  item_kind: &str,
  section_id: &str,
) -> Vec<topic::Topic> {
  let mut topics = Vec::with_capacity(ids.len());
  for id in ids {
    match topic::parse_documentation_topic(&id) {
      Ok(t) => topics.push(t),
      Err(e) => {
        tracing::warn!(
          "ignoring malformed documentation_topic '{}' on {} in section {}: {}",
          id,
          item_kind,
          section_id,
          e
        );
      }
    }
  }
  topics
}

/// Extract requirements and system characteristics from documentation files
/// via LLM, grouped by section. Multi-document runs send only the requirement
/// half through the consolidation LLM; characteristics accumulate verbatim
/// across documents and are consolidated later in characteristic synthesis
/// (Phase 4), which has the raw `security.md` as additional input.
pub async fn extract_requirements_from_documentation(
  documentation_files: &[String],
) -> Result<ParsedRequirements, TaskError> {
  if documentation_files.is_empty() {
    return Err(TaskError::Other(
      "No documentation found in audit".to_string(),
    ));
  }

  if documentation_files.len() == 1 {
    let prompt =
      format!("{}{}", EXTRACT_REQUIREMENTS_PROMPT, &documentation_files[0]);
    let response = router::chat_completion(
      TaskSize::Large,
      router::SYSTEM_MESSAGE_DOCUMENTATION,
      &prompt,
      None,
      Some(&REQUIREMENTS_SCHEMA),
    )
    .await?;
    return parse_requirements_response(&response, &prompt);
  }

  let mut handles = Vec::new();
  for (i, doc_json) in documentation_files.iter().enumerate() {
    let prompt = format!("{}{}", EXTRACT_REQUIREMENTS_PROMPT, doc_json);
    let label = format!("requirements_{}", i);
    handles.push(tokio::spawn(async move {
      router::chat_completion(
        TaskSize::Large,
        router::SYSTEM_MESSAGE_DOCUMENTATION,
        &prompt,
        Some(&label),
        Some(&REQUIREMENTS_SCHEMA),
      )
      .await
    }));
  }

  let mut per_doc_results: Vec<String> = Vec::new();
  for (i, handle) in handles.into_iter().enumerate() {
    match handle.await {
      Ok(Ok(response)) => per_doc_results.push(response),
      Ok(Err(e)) => {
        tracing::error!(
          "extract_requirements failed for document {}: {}",
          i,
          e
        );
      }
      Err(e) => {
        tracing::error!(
          "extract_requirements task panicked for document {}: {}",
          i,
          e
        );
      }
    }
  }

  if per_doc_results.is_empty() {
    return Err(TaskError::Other(
      "All document requirement extractions failed".to_string(),
    ));
  }

  if per_doc_results.len() == 1 {
    return parse_requirements_response(
      &per_doc_results[0],
      EXTRACT_REQUIREMENTS_PROMPT,
    );
  }

  // Parse every per-doc response first so we can split the two arrays apart:
  // feature requirements feed the consolidation LLM (existing behavior);
  // characteristics bypass it and are merged in afterwards. This avoids the
  // LLM having to reason about characteristic merging at two stages — Phase
  // 4's `synthesize_characteristics` is the single canonical consolidation
  // point and has access to the raw `security.md` that this step does not.
  let mut parsed_per_doc: Vec<ParsedRequirements> =
    Vec::with_capacity(per_doc_results.len());
  for (i, raw) in per_doc_results.iter().enumerate() {
    match parse_requirements_response(raw, EXTRACT_REQUIREMENTS_PROMPT) {
      Ok(p) => parsed_per_doc.push(p),
      Err(e) => {
        tracing::error!(
          "per-doc requirement response {} failed to parse: {}",
          i,
          e
        );
      }
    }
  }

  if parsed_per_doc.is_empty() {
    return Err(TaskError::Other(
      "All document requirement responses failed to parse".to_string(),
    ));
  }

  // If only one per-doc response actually parsed, skip the consolidation LLM
  // — there's nothing to consolidate, and an LLM call with one input wastes
  // tokens and risks the model dropping requirements it would otherwise
  // emit verbatim.
  if parsed_per_doc.len() == 1 {
    return Ok(parsed_per_doc.into_iter().next().unwrap());
  }

  let consolidation_input = render_consolidation_input(&parsed_per_doc);
  let prompt =
    format!("{}{}", CONSOLIDATE_REQUIREMENTS_PROMPT, consolidation_input);
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some("requirements_consolidate"),
    Some(&REQUIREMENTS_SCHEMA),
  )
  .await?;

  let mut consolidated = parse_requirements_response(&response, &prompt)?;

  // The consolidation prompt instructs the LLM to emit
  // `system_characteristics: []` because characteristics are consolidated
  // later in Phase 4 with the raw `security.md` as additional context.
  // Defensively drop any characteristics the LLM might still emit so the
  // per-doc accumulation below is the single source of truth — otherwise
  // a noncompliant LLM response would double-count characteristics.
  consolidated.characteristics.clear();
  consolidated.section_characteristics.clear();
  consolidated.topic_metadata.retain(|_, m| {
    !matches!(m, domain::TopicMetadata::CharacteristicTopic { .. })
  });

  // Merge characteristics from every per-doc parse into the consolidated
  // result with fresh local IDs that don't collide with the consolidation's
  // requirement IDs. The pipeline-level remap will reallocate process-wide
  // IDs for everything regardless, so these local IDs need only be unique
  // within the returned `ParsedRequirements`.
  let mut next_local_id = consolidated
    .topic_metadata
    .keys()
    .map(|t| t.numeric_id())
    .max()
    .unwrap_or(0);

  for per_doc in parsed_per_doc {
    let ParsedRequirements {
      characteristics,
      topic_metadata,
      ..
    } = per_doc;

    for (old_topic, characteristic) in characteristics {
      let (description, kind, section_topic) =
        match topic_metadata.get(&old_topic) {
          Some(domain::TopicMetadata::CharacteristicTopic {
            description,
            kind,
            section_topic,
            ..
          }) => (description.clone(), *kind, *section_topic),
          _ => continue,
        };

      next_local_id += 1;
      let new_topic = topic::new_spec_topic(next_local_id);

      consolidated.topic_metadata.insert(
        new_topic,
        domain::TopicMetadata::CharacteristicTopic {
          topic: new_topic,
          description,
          kind,
          section_topic,
          author: Author::System,
          created_at: None,
        },
      );
      consolidated
        .characteristics
        .insert(new_topic, characteristic);

      if let Some(st) = section_topic {
        consolidated
          .section_characteristics
          .entry(st)
          .or_default()
          .push(new_topic);
      }
    }
  }

  Ok(consolidated)
}

#[cfg(test)]
mod requirements_parser_tests {
  use super::*;

  #[test]
  fn parses_feature_requirements_only_section() {
    let response = r#"{
      "sections": [
        {
          "section_topic": "D5",
          "feature_requirements": [
            {
              "description": "Withdrawals must be safe from reentrancy.",
              "documentation_topics": ["D6", "D7"]
            }
          ],
          "system_characteristics": []
        }
      ]
    }"#;

    let parsed =
      parse_requirements_response(response, "<prompt>").expect("parses");

    assert_eq!(parsed.requirements.len(), 1);
    assert_eq!(parsed.characteristics.len(), 0);
    assert_eq!(parsed.section_requirements.len(), 1);
    assert_eq!(parsed.section_characteristics.len(), 0);

    // Parser sets `Author::System` directly so the pipeline can preserve
    // parser metadata verbatim. If this ever flips back to `AgentLarge`,
    // the pipeline's preserve-author assumption breaks silently.
    match parsed.topic_metadata.values().next() {
      Some(domain::TopicMetadata::RequirementTopic { author, .. }) => {
        assert_eq!(*author, Author::System);
      }
      other => panic!("expected RequirementTopic, got {:?}", other),
    }
  }

  #[test]
  fn parses_system_characteristics_only_section() {
    let response = r#"{
      "sections": [
        {
          "section_topic": "D5",
          "feature_requirements": [],
          "system_characteristics": [
            {
              "description": "The protocol assumes no external call can re-enter a state-mutating function before its caller frame completes.",
              "kind": "security",
              "documentation_topics": ["D9"]
            }
          ]
        }
      ]
    }"#;

    let parsed =
      parse_requirements_response(response, "<prompt>").expect("parses");

    assert_eq!(parsed.requirements.len(), 0);
    assert_eq!(parsed.characteristics.len(), 1);

    let (_, characteristic) = parsed.characteristics.iter().next().unwrap();
    assert_eq!(
      characteristic.documentation_topics,
      vec![topic::Topic::Documentation(9)]
    );

    let metadata = parsed.topic_metadata.values().next().unwrap();
    match metadata {
      domain::TopicMetadata::CharacteristicTopic {
        kind,
        section_topic,
        ..
      } => {
        assert_eq!(*kind, domain::SystemCharacteristicKind::Security);
        assert_eq!(*section_topic, Some(topic::Topic::Documentation(5)));
      }
      other => panic!("expected CharacteristicTopic, got {:?}", other),
    }

    // section_characteristics index points to the same characteristic.
    let chars = parsed
      .section_characteristics
      .get(&topic::Topic::Documentation(5));
    assert_eq!(chars.map(|v| v.len()), Some(1));
  }

  #[test]
  fn parses_both_arrays_with_disjoint_local_ids() {
    let response = r#"{
      "sections": [
        {
          "section_topic": "D1",
          "feature_requirements": [
            {
              "description": "Only the authorized relayer is allowed to invalidate participations.",
              "documentation_topics": ["D2"]
            }
          ],
          "system_characteristics": [
            {
              "description": "The relayer key is treated as trusted within the system trust boundary.",
              "kind": "security",
              "documentation_topics": ["D2"]
            }
          ]
        }
      ]
    }"#;

    let parsed =
      parse_requirements_response(response, "<prompt>").expect("parses");

    // Both kinds produced one entry each.
    assert_eq!(parsed.requirements.len(), 1);
    assert_eq!(parsed.characteristics.len(), 1);

    // Local topic IDs are disjoint within the shared `topic_metadata` map —
    // the single shared counter ensures no `Topic::Spec(_)` collisions
    // between the requirement and characteristic halves. If they ever
    // collide, the BTreeMap insert silently overwrites and the metadata
    // count drops below the sum of the two kinds.
    assert_eq!(
      parsed.topic_metadata.len(),
      parsed.requirements.len() + parsed.characteristics.len(),
      "topic_metadata must contain every requirement and every characteristic without collision"
    );

    // The requirement's metadata key must match the requirement entry's
    // own key — same for the characteristic.
    let req_topic = *parsed.requirements.keys().next().unwrap();
    let char_topic = *parsed.characteristics.keys().next().unwrap();
    assert_ne!(req_topic, char_topic, "kinds must have distinct topics");
    assert!(matches!(
      parsed.topic_metadata.get(&req_topic),
      Some(domain::TopicMetadata::RequirementTopic { .. })
    ));
    assert!(matches!(
      parsed.topic_metadata.get(&char_topic),
      Some(domain::TopicMetadata::CharacteristicTopic { .. })
    ));

    // The same documentation topic can appear on both sides — that's an
    // explicitly supported overlap pattern.
    let req_doc_topics = parsed
      .requirements
      .values()
      .next()
      .unwrap()
      .documentation_topics
      .clone();
    let char_doc_topics = parsed
      .characteristics
      .values()
      .next()
      .unwrap()
      .documentation_topics
      .clone();
    assert_eq!(req_doc_topics, vec![topic::Topic::Documentation(2)]);
    assert_eq!(char_doc_topics, vec![topic::Topic::Documentation(2)]);
  }

  #[test]
  fn rejects_unknown_characteristic_kind() {
    let response = r#"{
      "sections": [
        {
          "section_topic": "D1",
          "feature_requirements": [],
          "system_characteristics": [
            {
              "description": "An untyped characteristic.",
              "kind": "performance",
              "documentation_topics": ["D2"]
            }
          ]
        }
      ]
    }"#;

    let err = match parse_requirements_response(response, "<prompt>") {
      Ok(_) => panic!("unknown kind should fail to parse"),
      Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
      msg.contains("performance"),
      "error should name the offending kind, got: {}",
      msg
    );
  }

  #[test]
  fn parses_empty_sections() {
    let response = r#"{"sections": []}"#;
    let parsed =
      parse_requirements_response(response, "<prompt>").expect("parses");
    assert!(parsed.requirements.is_empty());
    assert!(parsed.characteristics.is_empty());
    assert!(parsed.topic_metadata.is_empty());
    assert!(parsed.section_requirements.is_empty());
    assert!(parsed.section_characteristics.is_empty());
  }

  #[test]
  fn skips_section_with_malformed_section_topic() {
    // The schema only constrains `section_topic` to be a string; an LLM
    // hallucination that emits a non-topic string must not panic the
    // pipeline. The whole section is skipped (its requirements/
    // characteristics would be unanchored), but other valid sections in
    // the same response still parse.
    let response = r#"{
      "sections": [
        {
          "section_topic": "not-a-topic",
          "feature_requirements": [
            {
              "description": "Should be dropped along with its section.",
              "documentation_topics": ["D2"]
            }
          ],
          "system_characteristics": []
        },
        {
          "section_topic": "D5",
          "feature_requirements": [
            {
              "description": "Valid section keeps its requirements.",
              "documentation_topics": ["D6"]
            }
          ],
          "system_characteristics": []
        }
      ]
    }"#;

    let parsed =
      parse_requirements_response(response, "<prompt>").expect("parses");

    assert_eq!(parsed.requirements.len(), 1);
    assert_eq!(parsed.section_requirements.len(), 1);
    let (only_section, _) = parsed.section_requirements.iter().next().unwrap();
    assert_eq!(*only_section, topic::Topic::Documentation(5));
  }

  #[test]
  fn rejects_section_topic_with_wrong_prefix() {
    // A topic that parses as a valid `Topic` but isn't `Documentation`
    // (e.g. `S5`, `N5`) is also skipped — the parser is strict about
    // documentation-topic anchors because cross-prefix confusion would
    // corrupt the `section_requirements` reverse index.
    let response = r#"{
      "sections": [
        {
          "section_topic": "S5",
          "feature_requirements": [
            {
              "description": "Belongs to a misclassified section.",
              "documentation_topics": ["D2"]
            }
          ],
          "system_characteristics": []
        }
      ]
    }"#;

    let parsed =
      parse_requirements_response(response, "<prompt>").expect("parses");
    assert!(parsed.requirements.is_empty());
    assert!(parsed.section_requirements.is_empty());
  }

  #[test]
  fn drops_malformed_documentation_topics_keeping_requirement() {
    // A bad `documentation_topic` entry on a valid requirement loses
    // traceability for that entry but the requirement itself survives —
    // its description still captures the documented claim. Other valid
    // documentation_topics in the same list are preserved.
    let response = r#"{
      "sections": [
        {
          "section_topic": "D1",
          "feature_requirements": [
            {
              "description": "Requirement with a partially bad doc list.",
              "documentation_topics": ["D2", "not-a-topic", "S5", "D7"]
            }
          ],
          "system_characteristics": []
        }
      ]
    }"#;

    let parsed =
      parse_requirements_response(response, "<prompt>").expect("parses");

    assert_eq!(parsed.requirements.len(), 1);
    let req = parsed.requirements.values().next().unwrap();
    assert_eq!(
      req.documentation_topics,
      vec![
        topic::Topic::Documentation(2),
        topic::Topic::Documentation(7)
      ],
      "only well-formed D-prefixed topics should survive"
    );
  }

  #[test]
  fn drops_malformed_documentation_topics_on_characteristic() {
    // Same defensive parsing for characteristic documentation_topics —
    // the entry is kept, malformed IDs are silently dropped.
    let response = r#"{
      "sections": [
        {
          "section_topic": "D1",
          "feature_requirements": [],
          "system_characteristics": [
            {
              "description": "Characteristic with a bad doc list.",
              "kind": "security",
              "documentation_topics": ["D9", "garbage", "D11"]
            }
          ]
        }
      ]
    }"#;

    let parsed =
      parse_requirements_response(response, "<prompt>").expect("parses");

    assert_eq!(parsed.characteristics.len(), 1);
    let ch = parsed.characteristics.values().next().unwrap();
    assert_eq!(
      ch.documentation_topics,
      vec![
        topic::Topic::Documentation(9),
        topic::Topic::Documentation(11)
      ]
    );
  }
}

/// Render the per-doc parsed requirements as a feature-requirements-only
/// JSON payload for the consolidation LLM. System characteristics are
/// omitted from the input entirely — they consolidate later in Phase 4.
/// Each document becomes one object with its own `sections` array; the LLM
/// merges across documents into a single output stream. Documents whose
/// requirement extraction yielded nothing are dropped so the LLM doesn't
/// waste reasoning on noise.
///
/// The input shape is *not* the response shape: the LLM receives an array
/// of per-document section groups and must produce a single flat `sections`
/// array per `REQUIREMENTS_SCHEMA`. The output preserves the
/// `system_characteristics: []` field because the schema requires it.
fn render_consolidation_input(per_doc: &[ParsedRequirements]) -> String {
  let mut docs: Vec<serde_json::Value> = Vec::with_capacity(per_doc.len());

  for (i, parsed) in per_doc.iter().enumerate() {
    let mut sections: Vec<serde_json::Value> = Vec::new();

    for (section_topic, req_topics) in &parsed.section_requirements {
      let mut reqs: Vec<serde_json::Value> =
        Vec::with_capacity(req_topics.len());
      for req_topic in req_topics {
        let description = match parsed.topic_metadata.get(req_topic) {
          Some(domain::TopicMetadata::RequirementTopic {
            description, ..
          }) => description.clone(),
          _ => continue,
        };
        let doc_topics: Vec<String> = parsed
          .requirements
          .get(req_topic)
          .map(|r| {
            r.documentation_topics
              .iter()
              .map(|t| t.to_string())
              .collect()
          })
          .unwrap_or_default();
        reqs.push(json!({
          "description": description,
          "documentation_topics": doc_topics,
        }));
      }

      if reqs.is_empty() {
        continue;
      }
      sections.push(json!({
        "section_topic": section_topic.to_string(),
        "feature_requirements": reqs,
        "system_characteristics": [],
      }));
    }

    // Skip documents that contributed no feature requirements — they'd be
    // pure noise to the consolidation LLM (per-doc characteristics still
    // flow through the accumulation path regardless).
    if sections.is_empty() {
      continue;
    }

    docs.push(json!({
      "document_index": i,
      "sections": sections,
    }));
  }

  serde_json::to_string(&docs).unwrap_or_default()
}

// ============================================================================
// Semantic Linking LLM Tasks (steps 2, 4, 5)
// ============================================================================

/// Step 2 — generate semantics for *contract entities* matched to a
/// documentation section. The "Declarations needing semantics" payload is
/// the contract list itself; the "Source code" payload is each contract's
/// name + NatSpec + public-member-name list.
const LINK_CONTRACTS_PROMPT: &str = "Below is a documentation section from a smart contract \
project, followed by a list of contract entities that need a semantic \
meaning assigned, followed by per-contract NatSpec and public-member \
summaries for reference.\n\n\
Your task is to assign **semantic meaning** to each contract based on what \
the **documentation says** the contract represents in the project. The \
semantic should reflect the developer's documented intent for the contract \
as a whole — its role in the system — not a mechanical description of its \
member surface.\n\n\
The contract summaries (NatSpec + member names) are provided ONLY to help \
you identify which contract the documentation is describing — for example, \
to confirm that \"the rewards pool\" in the documentation refers to \
`RewardsVault`. Do NOT derive meaning from the member list. If the \
documentation says a contract is \"the canonical reward source\" but its \
implementation is delegated to another contract, the semantic should still \
be \"the canonical reward source.\"\n\n\
Return a JSON object with a `links` key whose value is an array of objects. \
For each contract with documented semantics, include one object with ALL \
of these fields (all are required):\n\
- `declaration_topic` (required): the N-prefixed topic ID of the contract\n\
- `semantic_text` (required): a concise description of what the documentation \
says this contract represents (e.g., \"the canonical staking pool\", \"a \
fee-collecting wrapper around the underlying market\")\n\
- `documentation_topics` (required): array of D-prefixed topic IDs for the \
specific paragraphs, lists, or subsections in the documentation that this \
semantic was derived from\n\n\
Rules:\n\
- Derive semantics from the documentation section, not from member names.\n\
- If the documentation does not describe a contract's role, omit it from \
the output — do not invent a semantic from the member list.\n\
- The semantic text should be project-specific meaning, not a generic \
type description. \"a Solidity contract\" is not a semantic — \"the \
canonical staking pool that mints reward shares\" is.\n\
- Preserve the developer's specific terminology and phrasing nuances.\n\
- Each `documentation_topics` entry must be a D-prefixed ID that appears \
in the provided documentation section.\n\
- If the documentation does not describe any of the provided contracts, \
return `{\"links\": []}`.\n\
- Every link object MUST include all three fields.\n\n";

/// Step 4 — generate semantics for *member-level declarations* (functions
/// and modifiers + their parameters and return values, OR non-function
/// component-scoped declarations like state variables, events, errors,
/// and struct/enum definitions). The "Source code" payload is the member
/// signature(s) or contract-level signature snippets. Step 2's contract
/// semantics are injected as prior-context.
const LINK_MEMBER_SIGNATURES_PROMPT: &str = "Below is a documentation section from a smart contract \
project, followed by previously-derived semantics for the containing \
contracts (for context), followed by a list of code declarations that need \
semantic meaning assigned, followed by the source code for reference.\n\n\
The declarations may come from multiple functions, modifiers, or \
contract-level definitions. Each declaration has a topic ID and name. Use \
the contract semantics to interpret what each member represents *within \
that contract's role*.\n\n\
Your task is to assign **semantic meaning** to each declaration based on \
what the **documentation says** it represents in the project. The semantic \
should reflect the developer's documented intent, NOT what the code does \
with the declaration.\n\n\
The source code is provided ONLY to help you identify which declarations \
the documentation is describing — for example, to confirm that `pID` in \
the documentation refers to the `participationId` parameter. Do NOT \
derive meaning from how the code uses a variable. If the documentation \
says a variable is a \"proportional reward factor\" but the code uses it \
as a divisor, the semantic should still be \"proportional reward factor\" \
— that mismatch is valuable information for auditors.\n\n\
Return a JSON object with a `links` key whose value is an array of \
objects. For each declaration with documented semantics, include one \
object with ALL of these fields (all are required):\n\
- `declaration_topic` (required): the N-prefixed topic ID of the declaration\n\
- `semantic_text` (required): a concise description of what the documentation \
says this declaration represents in project context (e.g., \"proportional \
reward multiplier\", \"user's staked token balance\", \"reward distribution \
mechanism\")\n\
- `documentation_topics` (required): array of D-prefixed topic IDs for the \
specific paragraphs, lists, or subsections in the documentation that this \
semantic was derived from\n\n\
Rules:\n\
- Derive semantics from the documentation section, not from code behavior.\n\
- If the documentation does not describe a declaration's meaning, omit it \
from the output.\n\
- The semantic text should be project-specific meaning, not a generic type \
description.\n\
- Preserve the developer's specific terminology and phrasing nuances.\n\
- Functions and modifiers can receive semantics too — their semantic is \
what they represent (e.g., \"reward distribution mechanism\", \"access \
control check for admin role\").\n\
- Each `documentation_topics` entry must be a D-prefixed ID that appears \
in the provided documentation section.\n\
- If the documentation does not describe any of the provided declarations, \
return `{\"links\": []}`.\n\
- Every link object MUST include all three fields.\n\n";

/// Step 5 — generate semantics for *body locals* declared inside member
/// bodies. Step 2 contract semantics and step 4 member/signature semantics
/// are both injected as prior-context, so a statement like
/// `let ret = Contract.transfer(input, to)` can be interpreted with
/// `Contract`, `transfer`, `input`, and `to` already meaningful.
const LINK_MEMBER_BODIES_PROMPT: &str = "Below is a documentation section from a smart contract \
project, followed by a list of body-local \
declarations that need semantic meaning assigned, followed by the full \
member source code for reference. Prior-derived semantics for contract and \
member declarations appear inline on their respective AST nodes as \
`semantics` fields.\n\n\
The declarations are locals declared inside function/modifier bodies. Use \
the contract and member semantics already visible in the source code to \
interpret each local in light of the values flowing into it. For example, in a body \
statement like `let ret = Contract.transfer(input, to)`, knowing what \
`Contract`, `transfer`, `input`, and `to` already represent lets you give \
`ret` a meaningful semantic.\n\n\
Your task is to assign **semantic meaning** to each local based on what \
the **documentation says** the surrounding behavior represents. The \
semantic should reflect the developer's documented intent for the value \
the local holds, NOT a mechanical description of the expression that \
produced it.\n\n\
The source code is provided ONLY to help you identify which locals the \
documentation is describing and to see how they relate to the already-known \
contract and member semantics. Do NOT derive meaning from raw operator use.\n\n\
Return a JSON object with a `links` key whose value is an array of objects. \
For each local with documented semantics, include one object with ALL of \
these fields (all are required):\n\
- `declaration_topic` (required): the N-prefixed topic ID of the local\n\
- `semantic_text` (required): a concise description of what the documentation \
says this local represents in project context\n\
- `documentation_topics` (required): array of D-prefixed topic IDs for the \
specific paragraphs, lists, or subsections in the documentation that this \
semantic was derived from\n\n\
Rules:\n\
- Derive semantics from the documentation section in light of the prior \
contract/member semantics visible in the source code, not from raw code mechanics.\n\
- If the documentation does not describe a local's meaning, omit it from \
the output.\n\
- The semantic text should be project-specific meaning, not a generic type \
description.\n\
- Preserve the developer's specific terminology and phrasing nuances.\n\
- Each `documentation_topics` entry must be a D-prefixed ID that appears \
in the provided documentation section.\n\
- If the documentation does not describe any of the provided locals, \
return `{\"links\": []}`.\n\
- Every link object MUST include all three fields.\n\n";

/// A single semantic link returned by any of the three synthesis steps.
#[derive(Deserialize)]
struct LLMSemanticLink {
  declaration_topic: String,
  semantic_text: String,
  documentation_topics: Vec<String>,
}

#[derive(Deserialize)]
struct LLMSemanticLinkResponse {
  links: Vec<LLMSemanticLink>,
}

static SEMANTIC_LINK_SCHEMA: LazyLock<JsonSchema> = LazyLock::new(|| {
  JsonSchema {
    name: "semantic_link",
    schema: json!({
      "type": "object",
      "additionalProperties": false,
      "required": ["links"],
      "properties": {
        "links": {
          "type": "array",
          "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["declaration_topic", "semantic_text", "documentation_topics"],
            "properties": {
              "declaration_topic": { "type": "string" },
              "semantic_text": { "type": "string" },
              "documentation_topics": {
                "type": "array",
                "items": { "type": "string" }
              }
            }
          }
        }
      }
    }),
    empty_response: r#"{"links":[]}"#,
  }
});

/// Result of one synthesis-step LLM call: the parsed semantic links.
pub struct SemanticLinkResult {
  pub links: Vec<domain::SemanticLink>,
}

/// Which synthesis-step batch is being run. Picks the prompt template, the
/// call label prefix, and the human-readable name used in log / warning
/// messages.
///
/// Step 4 has two batch variants per section — `MemberSignaturesFunctions`
/// (function/modifier topics + their params/returns) and
/// `MemberSignaturesContractLevel` (non-function component-scoped
/// declarations). They share the same prompt and JSON schema, but use
/// distinct labels so log lines and saved-prompt files don't collide
/// between the two batches per section.
#[derive(Debug, Clone, Copy)]
pub enum SemanticLinkStep {
  /// Step 2 — semantics for contract entities.
  Contracts,
  /// Step 4(a) — semantics for function/modifier members and their
  /// parameters/return values.
  MemberSignaturesFunctions,
  /// Step 4(b) — semantics for non-function component-scoped declarations
  /// (state vars, events, errors, struct/enum defs, struct fields, enum
  /// members).
  MemberSignaturesContractLevel,
  /// Step 5 — semantics for body locals inside member bodies.
  MemberBodies,
}

impl SemanticLinkStep {
  fn prompt_prefix(self) -> &'static str {
    match self {
      SemanticLinkStep::Contracts => LINK_CONTRACTS_PROMPT,
      SemanticLinkStep::MemberSignaturesFunctions
      | SemanticLinkStep::MemberSignaturesContractLevel => {
        LINK_MEMBER_SIGNATURES_PROMPT
      }
      SemanticLinkStep::MemberBodies => LINK_MEMBER_BODIES_PROMPT,
    }
  }

  fn label(self) -> &'static str {
    match self {
      SemanticLinkStep::Contracts => "step2_contracts",
      SemanticLinkStep::MemberSignaturesFunctions => "step4a_member_signatures",
      SemanticLinkStep::MemberSignaturesContractLevel => {
        "step4b_contract_level"
      }
      SemanticLinkStep::MemberBodies => "step5_member_bodies",
    }
  }

  fn diagnostic_name(self) -> &'static str {
    match self {
      SemanticLinkStep::Contracts => "link_contracts",
      SemanticLinkStep::MemberSignaturesFunctions => {
        "link_member_signatures (functions)"
      }
      SemanticLinkStep::MemberSignaturesContractLevel => {
        "link_member_signatures (contract-level)"
      }
      SemanticLinkStep::MemberBodies => "link_member_bodies",
    }
  }
}

/// Run one synthesis-step LLM call. Steps 2, 4, and 5 share the same JSON
/// schema and parsing path; only the prompt template differs.
///
/// `fallback_doc_topic` is used when the LLM omits `documentation_topics`
/// from a returned link. `match_source` is the provenance tag stamped on
/// every returned link.
#[allow(clippy::too_many_arguments)]
pub async fn link_step(
  step: SemanticLinkStep,
  section_topic: &topic::Topic,
  section_text: &str,
  declarations_json: &str,
  source_code: &str,
  fallback_doc_topic: &topic::Topic,
  match_source: domain::MatchSource,
) -> Result<SemanticLinkResult, TaskError> {
  let prompt = format!(
    "{}Documentation section:\n{}\n\nDeclarations needing semantics:\n{}\n\nSource code (for disambiguation only):\n{}",
    step.prompt_prefix(),
    section_text,
    declarations_json,
    source_code
  );

  let label = format!("{}_{}", step.label(), section_topic.id());
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some(&label),
    Some(&SEMANTIC_LINK_SCHEMA),
  )
  .await?;

  let wrapper: LLMSemanticLinkResponse =
    router::parse_response(&response, step.diagnostic_name(), &prompt)?;

  let links = wrapper
    .links
    .into_iter()
    .filter_map(|l| {
      let declaration_topic = match topic::parse_topic(&l.declaration_topic) {
        Ok(t) => t,
        Err(e) => {
          tracing::warn!(
            "{} dropping link with malformed declaration_topic '{}': {}",
            step.diagnostic_name(),
            l.declaration_topic,
            e
          );
          return None;
        }
      };
      let doc_topics: Vec<topic::Topic> = l
        .documentation_topics
        .iter()
        .filter(|d| d.starts_with('D'))
        .filter_map(|d| match topic::parse_topic(d) {
          Ok(t) => Some(t),
          Err(e) => {
            tracing::warn!(
              "{} dropping malformed documentation_topic '{}': {}",
              step.diagnostic_name(),
              d,
              e
            );
            None
          }
        })
        .collect();

      Some(domain::SemanticLink {
        documentation_topics: if doc_topics.is_empty() {
          vec![*fallback_doc_topic]
        } else {
          doc_topics
        },
        declaration_topic,
        description: l.semantic_text,
        match_source,
      })
    })
    .collect();

  Ok(SemanticLinkResult { links })
}

/// Collect all documentation section topics that have content (TitledTopic entries).
/// Collect top-level documentation sections (direct children of document roots).
/// These are sections at the Container scope level — typically H1 sections.
/// Using top-level sections as the unit reduces LLM calls while providing
/// enough context per call for meaningful matching.
pub fn collect_documentation_sections(
  audit_data: &AuditData,
) -> Vec<topic::Topic> {
  audit_data
    .topic_metadata
    .iter()
    .filter_map(|(t, m)| {
      if let domain::TopicMetadata::TitledTopic {
        kind: domain::TitledTopicKind::DocumentationSection,
        scope,
        ..
      } = m
      {
        // Only top-level sections (Container scope = document root children)
        if matches!(scope, domain::Scope::Container { .. }) {
          return Some(*t);
        }
      }
      None
    })
    .collect()
}

// ============================================================================
// Semantic Condensation
// ============================================================================

const CONDENSE_SEMANTICS_PROMPT: &str = "Below are multiple semantic descriptions \
for the same code declaration, each numbered and generated from a different \
documentation section. Many are near-duplicates saying the same thing in \
different words.\n\n\
Your task is to aggressively condense them into the **fewest possible entries** \
that together capture every genuinely distinct facet. Prefer fewer, denser \
entries over many similar ones.\n\n\
Merge entries that describe the same concept — even if the wording differs — \
into a single, precise description. When merging, combine the most specific \
details from all sources into one comprehensive entry. Only keep entries as \
separate if they describe **fundamentally different aspects** (e.g., one \
describes purpose, another describes access control, another describes an \
error condition).\n\n\
Return a JSON object with an `entries` key whose value is an array of \
objects. Each object has:\n\
- `text`: the condensed semantic description\n\
- `sources`: array of 1-based indices of ALL original entries that were \
merged into this entry (for provenance tracking)\n\n\
Example: `{\"entries\": [{\"text\": \"event emitted when a user joins a \
campaign\", \"sources\": [1, 3, 5, 8]}, {\"text\": \"enables offchain \
tracking of participation details\", \"sources\": [2, 4]}]}`\n\n\
Rules:\n\
- Preserve nuances BY MERGING them into fewer entries, not by keeping \
redundant entries. A single dense description that captures all facets is \
better than multiple overlapping ones.\n\
- When merging, include the most specific details from all sources \
(e.g., access control restrictions, specific parameters, business context).\n\
- Each output entry should be a concise, self-contained semantic description.\n\
- Every original entry index must appear in exactly one `sources` array.\n\n";

#[derive(Deserialize)]
struct LLMCondensedSemantic {
  text: String,
  sources: Vec<usize>,
}

#[derive(Deserialize)]
struct LLMCondenseResponse {
  entries: Vec<LLMCondensedSemantic>,
}

static CONDENSE_SCHEMA: LazyLock<JsonSchema> = LazyLock::new(|| JsonSchema {
  name: "condense_semantics",
  schema: json!({
    "type": "object",
    "additionalProperties": false,
    "required": ["entries"],
    "properties": {
      "entries": {
        "type": "array",
        "items": {
          "type": "object",
          "additionalProperties": false,
          "required": ["text", "sources"],
          "properties": {
            "text": { "type": "string" },
            "sources": {
              "type": "array",
              "items": { "type": "integer" }
            }
          }
        }
      }
    }
  }),
  empty_response: r#"{"entries":[]}"#,
});

/// A condensed semantic entry with the text and indices of the original
/// entries (0-based) that were merged into it.
pub struct CondensedSemantic {
  pub text: String,
  pub source_indices: Vec<usize>,
}

/// Condense a group of repetitive semantics for a single declaration,
/// preserving all nuances and returning source indices for provenance.
pub async fn condense_semantics(
  declaration_topic: &str,
  semantics: &[String],
) -> Result<Vec<CondensedSemantic>, TaskError> {
  let semantics_list = semantics
    .iter()
    .enumerate()
    .map(|(i, s)| format!("{}. {}", i + 1, s))
    .collect::<Vec<_>>()
    .join("\n");

  let prompt = format!(
    "{}Declaration: {}\n\nSemantic descriptions ({} total):\n{}",
    CONDENSE_SEMANTICS_PROMPT,
    declaration_topic,
    semantics.len(),
    semantics_list
  );

  let label = format!("condense_{}", declaration_topic);
  let response = router::chat_completion(
    TaskSize::Small,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some(&label),
    Some(&CONDENSE_SCHEMA),
  )
  .await?;

  let wrapper: LLMCondenseResponse =
    router::parse_response(&response, "condensed semantics", &prompt)?;

  Ok(
    wrapper
      .entries
      .into_iter()
      .map(|entry| CondensedSemantic {
        text: entry.text,
        // Convert from 1-based (LLM) to 0-based indices.
        source_indices: entry
          .sources
          .into_iter()
          .map(|i| i.saturating_sub(1))
          .collect(),
      })
      .collect(),
  )
}

// ============================================================================
// Feature Synthesis via Reconciliation
// ============================================================================

/// Prompt for synthesizing features from requirements and behaviors.
const SYNTHESIZE_FEATURES_PROMPT: &str = "Below are two lists from a smart contract audit:\n\n\
1. **Requirements** — what the documentation claims the system does, grouped \
by documentation section. Each requirement has an S-prefixed topic ID.\n\
2. **Behaviors** — what the source code actually does, grouped by code member. \
Each behavior has an S-prefixed topic ID.\n\n\
Your task is to **synthesize features** that represent the system's \
capabilities. Each feature connects the documented intent (requirements) \
with the implemented reality (behaviors) for a coherent area of \
functionality.\n\n\
Return a JSON object with a `features` key whose value is an array of \
feature objects. Each feature has:\n\
- `name`: a short, descriptive feature name (behavioral, not technical)\n\
- `description`: a summary synthesized from both the documented intent and \
the implemented reality\n\
- `requirement_topics`: array of S-prefixed topic IDs that apply to this feature\n\
- `behavior_topics`: array of S-prefixed topic IDs that apply to this feature\n\n\
Rules:\n\
- Link each requirement and behavior to every feature it genuinely \
constrains or participates in. Cross-cutting concerns like access control, \
pausing, fee calculations, and validation often apply to multiple features — \
include them in each relevant feature.\n\
- A feature should represent a coherent capability. If a requirement applies \
to the feature's functionality, include it — even if it also appears in \
other features.\n\
- Every requirement and behavior must appear in at least one feature. \
If a requirement has no matching behaviors, it still belongs to a feature \
(unimplemented specification). If a behavior has no matching requirements, \
it still belongs to a feature (undocumented implementation).\n\
- Do not force weak matches. It is better to leave a requirement or behavior \
in a single feature than to artificially spread it across features where \
the connection is tenuous.\n\
- If no features can be synthesized, return `{\"features\": []}`.\n\n";

/// Raw feature from reconciliation LLM.
#[derive(Deserialize)]
struct LLMSynthesizedFeature {
  name: String,
  description: String,
  requirement_topics: Vec<String>,
  behavior_topics: Vec<String>,
}

#[derive(Deserialize)]
struct LLMFeaturesResponse {
  features: Vec<LLMSynthesizedFeature>,
}

static FEATURES_SCHEMA: LazyLock<JsonSchema> = LazyLock::new(|| JsonSchema {
  name: "synthesize_features",
  schema: json!({
    "type": "object",
    "additionalProperties": false,
    "required": ["features"],
    "properties": {
      "features": {
        "type": "array",
        "items": {
          "type": "object",
          "additionalProperties": false,
          "required": ["name", "description", "requirement_topics", "behavior_topics"],
          "properties": {
            "name": { "type": "string" },
            "description": { "type": "string" },
            "requirement_topics": {
              "type": "array",
              "items": { "type": "string" }
            },
            "behavior_topics": {
              "type": "array",
              "items": { "type": "string" }
            }
          }
        }
      }
    }
  }),
  empty_response: r#"{"features":[]}"#,
});

/// Result of feature synthesis.
pub struct SynthesizedFeatures {
  pub topic_metadata: BTreeMap<topic::Topic, domain::TopicMetadata>,
  /// Feature → requirement links (S-topic → [S-topics])
  pub feature_requirement_links: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Feature → behavior links (S-topic → [S-topics])
  pub feature_behavior_links: BTreeMap<topic::Topic, Vec<topic::Topic>>,
}

/// Render all requirements grouped by section for the reconciliation prompt.
fn render_requirements_for_reconciliation(audit_data: &AuditData) -> String {
  let mut sections: Vec<serde_json::Value> = Vec::new();

  for (section_topic, req_topics) in &audit_data.section_requirements {
    let section_title = audit_data
      .topic_metadata
      .get(section_topic)
      .and_then(|m| m.name())
      .unwrap_or("")
      .to_string();

    let reqs: Vec<serde_json::Value> = req_topics
      .iter()
      .filter_map(|rt| {
        if let Some(domain::TopicMetadata::RequirementTopic {
          description,
          ..
        }) = audit_data.topic_metadata.get(rt)
        {
          Some(serde_json::json!({
            "topic": rt.id(),
            "description": description,
          }))
        } else {
          None
        }
      })
      .collect();

    if !reqs.is_empty() {
      sections.push(serde_json::json!({
        "section": section_title,
        "section_topic": section_topic.id(),
        "requirements": reqs,
      }));
    }
  }

  // Also include requirements not in any section
  let in_sections: std::collections::HashSet<&topic::Topic> = audit_data
    .section_requirements
    .values()
    .flat_map(|v| v.iter())
    .collect();

  let orphan_reqs: Vec<serde_json::Value> = audit_data
    .requirements
    .keys()
    .filter(|rt| !in_sections.contains(rt))
    .filter_map(|rt| {
      if let Some(domain::TopicMetadata::RequirementTopic {
        description, ..
      }) = audit_data.topic_metadata.get(rt)
      {
        Some(serde_json::json!({
          "topic": rt.id(),
          "description": description,
        }))
      } else {
        None
      }
    })
    .collect();

  if !orphan_reqs.is_empty() {
    sections.push(serde_json::json!({
      "section": "(ungrouped)",
      "requirements": orphan_reqs,
    }));
  }

  serde_json::to_string(&sections).unwrap_or_else(|_| "[]".to_string())
}

/// Render all behaviors grouped by member for the reconciliation prompt.
fn render_behaviors_for_reconciliation(audit_data: &AuditData) -> String {
  let mut members: Vec<serde_json::Value> = Vec::new();

  for (member_topic, beh_topics) in &audit_data.member_behaviors {
    let member_name = audit_data
      .topic_metadata
      .get(member_topic)
      .and_then(|m| m.name())
      .unwrap_or("")
      .to_string();

    let behs: Vec<serde_json::Value> = beh_topics
      .iter()
      .filter_map(|bt| {
        if let Some(domain::TopicMetadata::BehaviorTopic {
          description, ..
        }) = audit_data.topic_metadata.get(bt)
        {
          Some(serde_json::json!({
            "topic": bt.id(),
            "description": description,
          }))
        } else {
          None
        }
      })
      .collect();

    if !behs.is_empty() {
      members.push(serde_json::json!({
        "member": member_name,
        "member_topic": member_topic.id(),
        "behaviors": behs,
      }));
    }
  }

  serde_json::to_string(&members).unwrap_or_else(|_| "[]".to_string())
}

/// Render requirements and behaviors for the reconciliation prompt.
/// Called while holding the lock, returns owned strings.
pub fn render_reconciliation_context(
  audit_data: &AuditData,
) -> (String, String) {
  let requirements_json = render_requirements_for_reconciliation(audit_data);
  let behaviors_json = render_behaviors_for_reconciliation(audit_data);
  (requirements_json, behaviors_json)
}

/// Synthesize features by reconciling all requirements with all behaviors
/// in a single LLM pass.
pub async fn synthesize_features(
  requirements_json: &str,
  behaviors_json: &str,
) -> Result<SynthesizedFeatures, TaskError> {
  let prompt = format!(
    "{}Requirements:\n{}\n\nBehaviors:\n{}",
    SYNTHESIZE_FEATURES_PROMPT, requirements_json, behaviors_json
  );

  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some("synthesize_features"),
    Some(&FEATURES_SCHEMA),
  )
  .await?;

  let wrapper: LLMFeaturesResponse =
    router::parse_response(&response, "synthesized features", &prompt)?;
  let raw_features = wrapper.features;

  let mut topic_metadata = BTreeMap::new();
  let mut feature_requirement_links = BTreeMap::new();
  let mut feature_behavior_links = BTreeMap::new();

  for (i, raw) in raw_features.into_iter().enumerate() {
    let feature_topic = topic::new_spec_topic((i + 1) as i32);

    let requirement_topics: Vec<topic::Topic> = raw
      .requirement_topics
      .into_iter()
      .map(|id| topic::new_topic(&id))
      .collect();

    let behavior_topics: Vec<topic::Topic> = raw
      .behavior_topics
      .into_iter()
      .map(|id| topic::new_topic(&id))
      .collect();

    feature_requirement_links.insert(feature_topic, requirement_topics);
    feature_behavior_links.insert(feature_topic, behavior_topics);

    topic_metadata.insert(
      feature_topic,
      domain::TopicMetadata::FeatureTopic {
        topic: feature_topic,
        name: raw.name,
        description: raw.description,
        author: Author::AgentLarge,
        created_at: None,
      },
    );
  }

  Ok(SynthesizedFeatures {
    topic_metadata,
    feature_requirement_links,
    feature_behavior_links,
  })
}

// ============================================================================
// Characteristic Synthesis (Phase 4)
// ============================================================================

/// Prompt for consolidating and refining the system characteristics extracted
/// in step 2 against the raw `security.md` notes. This is the single
/// canonical consolidation point for characteristics — per-doc extraction
/// emits raw items and the multi-doc consolidation pass deliberately bypasses
/// them, so any cross-section dedup, cross-document merging, or promotion of
/// `security.md`-only claims happens here.
///
/// Inputs (formatted into the prompt below):
/// - `Extracted characteristics:` — JSON list of every `CharacteristicTopic`
///   currently in `topic_metadata`. Each item has its current S-topic ID for
///   reference; the synthesizer is free to merge, drop, or refine, and the
///   pipeline reallocates fresh S-IDs from the output regardless.
/// - `Security notes:` — verbatim content of `security.md` (may be empty
///   when the audit didn't ship one). Claims here that don't appear in the
///   extracted set get promoted to first-class characteristics with
///   `section_topic: null`.
///
/// The output preserves `documentation_topics` for characteristics that
/// trace to a documentation section and emits `section_topic: null` for any
/// item whose only source is `security.md`.
const SYNTHESIZE_CHARACTERISTICS_PROMPT: &str = "Below are two inputs from a smart contract audit:\n\n\
1. **Extracted characteristics** — system-wide claims (security \
properties, trust assumptions, role definitions, threat-model statements) \
already extracted from the project's documentation, each carrying an \
S-prefixed topic ID, an optional D-prefixed section anchor, and a list of \
D-prefixed documentation topics that informed it.\n\
2. **Security notes** — the raw, free-form contents of the project's \
`security.md` (or equivalent). May be empty.\n\n\
Your task is to produce a **refined, consolidated** set of system \
characteristics that captures every distinct security-relevant claim across \
both inputs.\n\n\
Rules:\n\
- **Merge overlapping claims** from the extracted set and `security.md` \
into a single characteristic. When merging, preserve the \
`documentation_topics` from the extracted side and prefer the most specific \
wording across the inputs. If the merged characteristic still anchors to a \
documentation section, set `section_topic` to that section's D-prefixed ID; \
if the claim originated only in `security.md`, set `section_topic` to \
`null`.\n\
- **Promote any claim present only in `security.md`** to a first-class \
characteristic with `kind: \"security\"`, `section_topic: null`, and an \
empty `documentation_topics` array. Do not invent doc-topic IDs.\n\
- **Refine descriptions for clarity**. Each item must describe exactly one \
system-wide claim. If an extracted item conflates two claims, split it. \
Aim for concise, specific, auditor-actionable language; do not pad and do \
not summarize away meaningful detail.\n\
- **Preserve every distinct claim**. If you are unsure whether two items \
describe the same claim, keep them separate. It is better to leave a \
near-duplicate than to drop a unique constraint, trust assumption, or \
threat-model statement.\n\
- **Do not use code declaration names** (function names, contract names, \
variable names) in descriptions. State the system-wide property in \
behavioral terms; the linked `documentation_topics` carry the traceability \
back to specific declarations.\n\
- **Kind**: only `\"security\"` is supported. Omit any claim that does not \
fit this kind rather than emitting a sentinel value.\n\
- **Documentation topics**: only use D-prefixed IDs that appear on at \
least one extracted characteristic in the input. Never invent IDs. The \
`documentation_topics` array on a refined item is the union of the \
documentation topics from every extracted characteristic merged into it.\n\
- **Section topic**: if an item is derived from one or more documentation \
sections, set `section_topic` to the single most-specific D-prefixed \
section ID it anchors to. If a claim spans several sections, pick the \
section that best matches the claim and include the rest as \
`documentation_topics`. Use `null` only for `security.md`-only claims.\n\
- If both inputs are empty (no extracted characteristics and no security \
notes), return `{\"characteristics\": []}`.\n\n\
Return a JSON object with a `characteristics` key whose value is an array \
of characteristic objects. Each characteristic has:\n\
- `description`: a single, specific statement of the system-wide claim\n\
- `kind`: always `\"security\"` for the current pipeline\n\
- `section_topic`: a D-prefixed topic ID, or `null` for `security.md`-only \
claims\n\
- `documentation_topics`: array of D-prefixed topic IDs that informed this \
characteristic (empty for `security.md`-only claims)\n\n";

/// Raw characteristic as returned by the synthesis LLM. Shares the
/// `LLMCharacteristic` shape from extraction with the addition of an
/// optional `section_topic` (extraction always anchors to a section; the
/// synthesizer may emit `null` for `security.md`-only claims).
#[derive(Deserialize)]
struct LLMSynthesizedCharacteristic {
  description: String,
  kind: String,
  section_topic: Option<String>,
  documentation_topics: Vec<String>,
}

#[derive(Deserialize)]
struct LLMCharacteristicsResponse {
  characteristics: Vec<LLMSynthesizedCharacteristic>,
}

static CHARACTERISTICS_SCHEMA: LazyLock<JsonSchema> =
  LazyLock::new(|| JsonSchema {
    name: "synthesize_characteristics",
    schema: json!({
      "type": "object",
      "additionalProperties": false,
      "required": ["characteristics"],
      "properties": {
        "characteristics": {
          "type": "array",
          "items": {
            "type": "object",
            "additionalProperties": false,
            "required": [
              "description",
              "kind",
              "section_topic",
              "documentation_topics"
            ],
            "properties": {
              "description": { "type": "string" },
              "kind": {
                "type": "string",
                "enum": ["security"]
              },
              "section_topic": {
                "type": ["string", "null"]
              },
              "documentation_topics": {
                "type": "array",
                "items": { "type": "string" }
              }
            }
          }
        }
      }
    }),
    empty_response: r#"{"characteristics":[]}"#,
  });

/// Result of characteristic synthesis.
///
/// `topic_metadata` carries `CharacteristicTopic` variants keyed by
/// locally-allocated `Topic::Spec(_)` IDs; the pipeline reallocates
/// process-wide IDs before inserting into `audit_data`. The matching
/// `Characteristic` entries (with their `documentation_topics`) live in
/// `characteristics`, keyed by the same local IDs.
pub struct SynthesizedCharacteristics {
  pub topic_metadata: BTreeMap<topic::Topic, domain::TopicMetadata>,
  pub characteristics: BTreeMap<topic::Topic, domain::Characteristic>,
}

/// Render the synthesis inputs from `AuditData` as `(security_notes,
/// extracted_json)`. Called while holding the data-context lock; returns
/// owned strings so the lock can be dropped before the LLM call.
///
/// `security_notes` is `audit_data.security_notes` verbatim (or the empty
/// string when absent). `extracted_json` is a JSON array of every
/// `CharacteristicTopic` in `topic_metadata`, sorted by topic ID for
/// determinism (snapshot/diff stability across runs of the same input).
pub fn render_characteristic_synthesis_context(
  audit_data: &AuditData,
) -> (String, String) {
  let security_notes = audit_data.security_notes.clone().unwrap_or_default();

  let mut entries: Vec<(topic::Topic, serde_json::Value)> = Vec::new();
  for (topic, metadata) in &audit_data.topic_metadata {
    if let domain::TopicMetadata::CharacteristicTopic {
      description,
      kind,
      section_topic,
      ..
    } = metadata
    {
      let doc_topics: Vec<String> = audit_data
        .characteristics
        .get(topic)
        .map(|c| c.documentation_topics.iter().map(|t| t.id()).collect())
        .unwrap_or_default();

      entries.push((
        *topic,
        json!({
          "topic": topic.id(),
          "description": description,
          // Lowercase to match the synthesizer's output schema
          // (`enum: ["security"]`). If we sent the capitalized
          // display form, the LLM could mirror it in output and the
          // OpenRouter-side schema validation would reject the
          // response. See `SystemCharacteristicKind::wire_name`.
          "kind": kind.wire_name(),
          "section_topic": section_topic.map(|t| t.id()),
          "documentation_topics": doc_topics,
        }),
      ));
    }
  }

  entries.sort_by_key(|(t, _)| t.numeric_id());
  let extracted: Vec<serde_json::Value> =
    entries.into_iter().map(|(_, v)| v).collect();
  let extracted_json =
    serde_json::to_string(&extracted).unwrap_or_else(|_| "[]".to_string());

  (security_notes, extracted_json)
}

/// Synthesize the refined characteristic set from the raw `security.md`
/// notes plus the characteristics already extracted in step 2. A single
/// LLM call consolidates across both inputs and produces the final
/// `CharacteristicTopic` set the pipeline persists.
///
/// The caller is expected to skip this step when both inputs are empty;
/// when only one side is empty the step still runs (cross-section
/// consolidation is valuable even without a `security.md`, and a
/// `security.md` with no documentation-extracted characteristics still
/// needs its claims promoted to first-class topics).
///
/// Author resolution: every synthesized entry carries
/// `Author::AgentLarge` because this function uses `TaskSize::Large`,
/// matching `extract_requirements_from_documentation`. If the task size
/// ever changes, also update the `Author` literal here so the on-wire
/// authorship stays consistent with the model that produced the text.
pub async fn synthesize_characteristics(
  security_notes: &str,
  extracted_json: &str,
) -> Result<SynthesizedCharacteristics, TaskError> {
  let prompt = format!(
    "{}Extracted characteristics:\n{}\n\nSecurity notes:\n{}",
    SYNTHESIZE_CHARACTERISTICS_PROMPT, extracted_json, security_notes
  );

  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some("synthesize_characteristics"),
    Some(&CHARACTERISTICS_SCHEMA),
  )
  .await?;

  let wrapper: LLMCharacteristicsResponse =
    router::parse_response(&response, "synthesized characteristics", &prompt)?;

  let mut topic_metadata = BTreeMap::new();
  let mut characteristics = BTreeMap::new();

  for (i, raw) in wrapper.characteristics.into_iter().enumerate() {
    // An unknown kind is a loud failure here per the build-plan decision:
    // silent dropping is the wrong failure mode for a security-relevant
    // artifact. The JSON Schema enum already restricts the value space at
    // the router boundary, so this is a defense-in-depth check that
    // surfaces drift between the schema and the parsed type.
    let kind = domain::SystemCharacteristicKind::parse_str(&raw.kind)
      .ok_or_else(|| {
        TaskError::Other(format!(
          "unknown SystemCharacteristicKind '{}' in synthesized characteristic {}",
          raw.kind, i
        ))
      })?;

    // Optional section anchor: only well-formed D-prefixed IDs survive.
    // A malformed value drops the anchor but keeps the characteristic —
    // its description still captures the claim.
    let section_topic = match raw.section_topic.as_deref() {
      None | Some("") => None,
      Some(id) => match topic::parse_documentation_topic(id) {
        Ok(t) => Some(t),
        Err(e) => {
          tracing::warn!(
            "ignoring malformed section_topic '{}' on synthesized characteristic {}: {}",
            id,
            i,
            e
          );
          None
        }
      },
    };

    let doc_topics = parse_documentation_topic_list(
      raw.documentation_topics,
      "synthesized_characteristic",
      &section_topic
        .map(|t| t.id())
        .unwrap_or_else(|| "<none>".to_string()),
    );

    // Local topic IDs are 1-based and dense; the pipeline remap reallocates
    // process-wide IDs before insertion, so local-id collisions across
    // pipeline runs aren't possible.
    let char_topic = topic::new_spec_topic((i + 1) as i32);

    topic_metadata.insert(
      char_topic,
      domain::TopicMetadata::CharacteristicTopic {
        topic: char_topic,
        description: raw.description,
        kind,
        section_topic,
        author: Author::AgentLarge,
        created_at: None,
      },
    );

    characteristics.insert(
      char_topic,
      domain::Characteristic {
        documentation_topics: doc_topics,
      },
    );
  }

  Ok(SynthesizedCharacteristics {
    topic_metadata,
    characteristics,
  })
}

#[cfg(test)]
mod characteristic_synthesis_tests {
  use super::*;

  #[test]
  fn renders_empty_when_no_characteristics_or_notes() {
    use crate::domain::ProjectPath;
    use std::collections::HashSet;

    let audit_data = domain::new_audit_data(
      "test".to_string(),
      HashSet::<ProjectPath>::new(),
      None,
    );

    let (notes, extracted) =
      render_characteristic_synthesis_context(&audit_data);
    assert_eq!(notes, "");
    assert_eq!(extracted, "[]");
  }

  #[test]
  fn renders_security_notes_and_extracted_in_topic_order() {
    use crate::domain::ProjectPath;
    use std::collections::HashSet;

    let mut audit_data = domain::new_audit_data(
      "test".to_string(),
      HashSet::<ProjectPath>::new(),
      Some("# Roles\nThe relayer key is trusted.".to_string()),
    );

    // Insert two characteristics with non-monotonic insertion order to
    // confirm the renderer sorts by numeric topic ID.
    let s2 = topic::new_spec_topic(2);
    let s1 = topic::new_spec_topic(1);

    audit_data.topic_metadata.insert(
      s2,
      domain::TopicMetadata::CharacteristicTopic {
        topic: s2,
        description: "Second claim.".to_string(),
        kind: domain::SystemCharacteristicKind::Security,
        section_topic: Some(topic::Topic::Documentation(7)),
        author: Author::System,
        created_at: None,
      },
    );
    audit_data.characteristics.insert(
      s2,
      domain::Characteristic {
        documentation_topics: vec![topic::Topic::Documentation(7)],
      },
    );

    audit_data.topic_metadata.insert(
      s1,
      domain::TopicMetadata::CharacteristicTopic {
        topic: s1,
        description: "First claim.".to_string(),
        kind: domain::SystemCharacteristicKind::Security,
        section_topic: None,
        author: Author::System,
        created_at: None,
      },
    );
    audit_data.characteristics.insert(
      s1,
      domain::Characteristic {
        documentation_topics: vec![],
      },
    );

    let (notes, extracted_json) =
      render_characteristic_synthesis_context(&audit_data);
    assert!(notes.contains("relayer key is trusted"));

    let parsed: serde_json::Value =
      serde_json::from_str(&extracted_json).expect("valid JSON");
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    // Sorted by numeric ID: S1 first, S2 second.
    assert_eq!(arr[0]["topic"], "S1");
    assert_eq!(arr[1]["topic"], "S2");
    assert_eq!(arr[0]["section_topic"], serde_json::Value::Null);
    assert_eq!(arr[1]["section_topic"], "D7");
    // Lowercase wire form — matches CHARACTERISTICS_SCHEMA's
    // `enum: ["security"]`. If this ever flips back to capitalized
    // `"Security"`, the synthesizer LLM may mirror the case and emit
    // a response that fails OpenRouter's strict-schema validation.
    assert_eq!(arr[0]["kind"], "security");
    assert_eq!(arr[1]["kind"], "security");
  }

  #[test]
  fn parses_merged_characteristics_with_and_without_section() {
    // A minimal end-to-end exercise of the response parser used by
    // `synthesize_characteristics`. We bypass the LLM call by invoking the
    // schema's response-shape directly through the same deserialization
    // path — this is the post-LLM parse the function executes.
    let response = r#"{
      "characteristics": [
        {
          "description": "The relayer key is treated as trusted within the system trust boundary.",
          "kind": "security",
          "section_topic": "D5",
          "documentation_topics": ["D5", "D7"]
        },
        {
          "description": "The protocol assumes no external call can re-enter a state-mutating function before its caller frame completes.",
          "kind": "security",
          "section_topic": null,
          "documentation_topics": []
        }
      ]
    }"#;

    let wrapper: LLMCharacteristicsResponse = router::parse_response(
      response,
      "synthesized characteristics",
      "<prompt>",
    )
    .expect("parses");
    assert_eq!(wrapper.characteristics.len(), 2);
    assert_eq!(
      wrapper.characteristics[0].section_topic.as_deref(),
      Some("D5")
    );
    assert_eq!(wrapper.characteristics[1].section_topic, None);
    assert_eq!(wrapper.characteristics[1].documentation_topics.len(), 0);
  }

  /// Permanent drift guard for the Phase 4 boundary contract.
  ///
  /// Characteristic synthesis is the *only* pipeline step whose prompt
  /// is allowed to mention characteristics. Feature synthesis (step 4),
  /// functional property generation (step 6), condition generation
  /// (step 7), and threat generation (step 8) consume their inputs from
  /// disjoint sources (requirements + behaviors for step 4; member-scoped
  /// AST + per-step prior outputs for steps 6/7/8). If any of those
  /// prompts ever started saying "and these system characteristics: …",
  /// the prompt author would also need to wire a renderer change to
  /// populate the JSON field — but the prompt change alone is enough to
  /// invalidate the boundary contract.
  ///
  /// This is the strongest mechanical guard available at the unit level:
  /// it doesn't require constructing real AST nodes (which would be
  /// needed to exercise `render_batch_for_extraction` end-to-end), and it
  /// catches the realistic regression vector — a prompt edit that
  /// surfaces characteristics into a downstream step.
  ///
  /// Per the build-plan decision: "This test stays in the suite
  /// permanently as the drift guard." Strengthen it if a stricter
  /// mechanical guarantee becomes available; do not weaken or delete.
  #[test]
  fn other_pipeline_prompts_do_not_mention_characteristics() {
    let prompts: &[(&str, &str)] = &[
      (
        "SYNTHESIZE_FEATURES_PROMPT (step 4)",
        SYNTHESIZE_FEATURES_PROMPT,
      ),
      (
        "EXTRACT_FUNCTIONAL_PROPERTIES_PROMPT (step 6)",
        EXTRACT_FUNCTIONAL_PROPERTIES_PROMPT,
      ),
      (
        "EXTRACT_CONDITIONS_PROMPT (step 7)",
        EXTRACT_CONDITIONS_PROMPT,
      ),
      ("EXTRACT_THREATS_PROMPT (step 8)", EXTRACT_THREATS_PROMPT),
    ];

    // Case-insensitive substring search. Lowercasing both sides
    // catches the obvious vectors ("Characteristic", "characteristic",
    // "characteristics", "CHARACTERISTICS").
    for (name, prompt) in prompts {
      let lc = prompt.to_lowercase();
      assert!(
        !lc.contains("characteristic"),
        "{} must not mention 'characteristic' — Phase 4 boundary \
         contract requires that only the characteristic synthesis step \
         renders characteristics into its prompt. If you're adding a \
         legitimate cross-step reference, update the build-plan first \
         and write a renderer-side test that proves no CharacteristicTopic \
         IDs leak into the rendered JSON.",
        name,
      );
    }
  }
}

// ============================================================================
// Behavior Extraction LLM Tasks
// ============================================================================

/// Prompt for extracting behaviors from a batch of dependency-ordered
/// functions/modifiers. The batch JSON is the output of
/// `render_batch_for_extraction`. See pipeline-dag.md.
const EXTRACT_BEHAVIORS_BATCH_PROMPT: &str = "Below are one or more functions/modifiers from \
an in-scope smart contract project. The input is wrapped in either a \
`subject` field (single member) or a `batch` array (multiple members). \
Each member object includes:\n\
- `topic`: the N-prefixed topic ID of the function or modifier.\n\
- `name`, `kind`, `visibility`, and a `modifiers` array of `{topic, name}` \
entries listing every modifier applied to the function signature.\n\
- `state_reads` and `state_writes`: arrays of state-variable topic IDs \
this function reads from or mutates. (`state_reads` may be empty even \
when reads occur; treat the body AST as the source of truth for reads.)\n\
- `features`: an array of features this member contributes to. Each \
feature has `topic`, `name`, `description`, and `requirements`. A member \
may belong to more than one feature; reasoning that draws on multiple \
features is welcome.\n\
- `definition`: the function's signature and body as an AST. Reference \
nodes (Identifier, IdentifierPath, MemberAccess) carry an inline \
`semantic` field when the referenced declaration has a project-specific \
meaning. Function call nodes carry an inline `callee_behaviors` array \
when the callee is in-scope and already extracted.\n\
- `semantics`: a top-level map keyed by declaration topic — the same \
project-specific meanings as the inline annotations, deduped for \
enumeration.\n\
- `called_function_behaviors`: a top-level map keyed by callee topic — \
the same callee behaviors as the inline call-site annotations, deduped \
for enumeration. Out-of-scope callees appear with an empty `behaviors` \
array.\n\n\
Your task is to extract **behaviors** for each function — what it actually \
does, described in business-level terms.\n\n\
- Use the semantics to describe behaviors at a business level rather than \
mechanically. For example, if `propFactor` has the semantic \"proportional \
reward multiplier\" and `stakerBalance` has \"user's staked token balance\", \
describe the behavior as \"calculates proportional reward share for the \
staker\" rather than \"multiplies propFactor by stakerBalance\".\n\
- Use the called_function_behaviors and inline `callee_behaviors` to \
understand what internal calls do without re-describing them. Describe \
the composite effect of this function in terms of what its callees do, \
not how they do it.\n\
- Preserve the developer's specific terminology from the semantics — \
subtle naming differences often reflect important design distinctions.\n\
- Include both normal execution paths and edge case behaviors (reverts, \
access control checks, state mutations).\n\
- Do not use identifer names, rely on the semantics to describe subjects and behavior.
- Do not describe implementation details like \"calls _transfer internally\" — \
describe the observable effect.\n\n\
Return a JSON object with a `members` key whose value is an array of member \
groups. Each group has:\n\
- `member_topic`: the N-prefixed topic ID of the function/modifier\n\
- `behaviors`: an array of behavior description strings\n\n\
Every function and modifier in the input must appear in the output with at \
least one behavior. If the input is empty, return `{\"members\": []}`.\n\n";

/// Raw member behavior group from LLM.
#[derive(Deserialize)]
struct LLMMemberBehaviors {
  member_topic: String,
  behaviors: Vec<String>,
}

#[derive(Deserialize)]
struct LLMBehaviorsResponse {
  members: Vec<LLMMemberBehaviors>,
}

static BEHAVIORS_SCHEMA: LazyLock<JsonSchema> = LazyLock::new(|| JsonSchema {
  name: "extract_behaviors",
  schema: json!({
    "type": "object",
    "additionalProperties": false,
    "required": ["members"],
    "properties": {
      "members": {
        "type": "array",
        "items": {
          "type": "object",
          "additionalProperties": false,
          "required": ["member_topic", "behaviors"],
          "properties": {
            "member_topic": { "type": "string" },
            "behaviors": {
              "type": "array",
              "items": { "type": "string" }
            }
          }
        }
      }
    }
  }),
  empty_response: r#"{"members":[]}"#,
});

/// Result of behavior extraction for a contract.
pub struct ParsedBehaviors {
  pub behaviors: Vec<(topic::Topic, String)>, // (member_topic, description)
}

/// Extract behaviors from a DAG-ordered batch of in-scope functions and
/// modifiers via LLM. `batch_json` is the output of
/// `context::render_batch_for_extraction`. `label` identifies the batch
/// for logs and LLM-call telemetry (use the `BatchForExtraction.label`
/// field).
pub async fn extract_behaviors_from_batch(
  batch_json: &str,
  label: &str,
) -> Result<ParsedBehaviors, TaskError> {
  let prompt =
    format!("{}Batch:\n{}", EXTRACT_BEHAVIORS_BATCH_PROMPT, batch_json);

  let log_label = format!("behaviors_{}", label);
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_CODE,
    &prompt,
    Some(&log_label),
    Some(&BEHAVIORS_SCHEMA),
  )
  .await?;

  let wrapper: LLMBehaviorsResponse =
    router::parse_response(&response, "behaviors", &prompt)?;

  let mut behaviors = Vec::new();
  for group in wrapper.members {
    let member_topic = match topic::parse_topic(&group.member_topic) {
      Ok(t @ topic::Topic::Node(_)) => t,
      Ok(other) => {
        tracing::warn!(
          batch = %label,
          "behavior extraction: member_topic {:?} is not an N-prefixed topic; \
           skipping {} behavior(s)",
          other,
          group.behaviors.len()
        );
        continue;
      }
      Err(e) => {
        tracing::warn!(
          batch = %label,
          "behavior extraction: failed to parse member_topic {:?}: {}; \
           skipping {} behavior(s)",
          group.member_topic,
          e,
          group.behaviors.len()
        );
        continue;
      }
    };
    for desc in group.behaviors {
      behaviors.push((member_topic, desc));
    }
  }

  Ok(ParsedBehaviors { behaviors })
}

// ============================================================================
// Functional Purpose & Placement Rationale (Pipeline Step 5)
// ============================================================================

/// Prompt for generating functional purpose and placement rationale for
/// every non-pure subject in a single in-scope function/modifier. The
/// input JSON is the output of `render_batch_for_extraction` called with
/// a single-member slice; the envelope uses the `subject` shape (not
/// `batch`). See pipeline-dag.md step 5.
const EXTRACT_FUNCTIONAL_PROPERTIES_PROMPT: &str = "Below is one in-scope \
function or modifier from a smart contract project. The function appears \
under the `subject` field with:\n\
- `topic`, `name`, `kind`, `visibility`, and a `modifiers` array of \
`{topic, name}` entries.\n\
- `state_reads` and `state_writes`: state-variable topic IDs the function \
reads or mutates.\n\
- `features`: an array of features this function contributes to. Each \
feature has `topic`, `name`, `description`, and `requirements`. The \
function may belong to more than one feature; the purpose and placement \
of subjects inside it are constrained by all of them.\n\
- `behaviors`: what the function as a whole does (already extracted).\n\
- `definition`: the function's signature and body as an AST. **Non-pure \
subjects in the body have `purity: \"non_pure\"`; function calls include \
`purity: \"pure\"` or `purity: \"non_pure\"`.** Reference nodes carry an \
inline `semantic` when the referenced declaration has a project-specific \
meaning. Call sites carry an inline `callee_behaviors` when the callee \
is in-scope.\n\
- `semantics`: top-level map keyed by declaration topic, deduped \
enumeration of the inline annotations.\n\
- `called_function_behaviors`: top-level map keyed by callee topic, \
deduped enumeration of the inline call-site annotations.\n\n\
The top-level **`non_pure_subjects`** array lists every non-pure subject \
in this function. For **each** topic in that list, produce two \
properties:\n\n\
- **`functional_purpose`** — the business-logic reason this subject exists, \
expressed in terms of the function's feature(s) and the value the subject \
contributes to them. Avoid restating what the operation mechanically does; \
explain the impact on users or the system if it were absent.\n\
- **`placement_rationale`** — the ordering reason this subject is at this \
point in its function rather than earlier or later. Refer to specific \
neighboring operations in the function body when relevant: what state \
must already exist before this subject runs, what state this subject \
must commit before subsequent operations, what would change if this \
subject moved.\n\n\
Use the function's behaviors and feature context to ground both answers. \
Use called_function_behaviors to understand what internal calls do without \
re-describing them. Use semantics to describe values at a business level \
rather than mechanically.\n\n\
Return a JSON object with a `subjects` key whose value is an array. Each \
entry has:\n\
- `subject_topic`: the topic ID from the `non_pure_subjects` list\n\
- `functional_purpose`: one or two sentences\n\
- `placement_rationale`: one or two sentences\n\n\
Every topic in `non_pure_subjects` must appear exactly once in the output. \
If the function contains no non-pure subjects, return `{\"subjects\": []}`.\n\n";

#[derive(Deserialize)]
struct LLMSubjectFunctionalProperties {
  subject_topic: String,
  functional_purpose: String,
  placement_rationale: String,
}

#[derive(Deserialize)]
struct LLMFunctionalPropertiesResponse {
  subjects: Vec<LLMSubjectFunctionalProperties>,
}

static FUNCTIONAL_PROPERTIES_SCHEMA: LazyLock<JsonSchema> =
  LazyLock::new(|| JsonSchema {
    name: "extract_functional_properties",
    schema: json!({
      "type": "object",
      "additionalProperties": false,
      "required": ["subjects"],
      "properties": {
        "subjects": {
          "type": "array",
          "items": {
            "type": "object",
            "additionalProperties": false,
            "required": [
              "subject_topic",
              "functional_purpose",
              "placement_rationale"
            ],
            "properties": {
              "subject_topic": { "type": "string" },
              "functional_purpose": { "type": "string" },
              "placement_rationale": { "type": "string" }
            }
          }
        }
      }
    }),
    empty_response: r#"{"subjects":[]}"#,
  });

/// Result of functional-purpose / placement-rationale extraction for one
/// batch. Each entry pairs a non-pure subject topic with its two
/// generated properties.
pub struct ParsedFunctionalProperties {
  pub entries: Vec<ParsedSubjectFunctionalProperties>,
}

pub struct ParsedSubjectFunctionalProperties {
  pub subject_topic: topic::Topic,
  pub functional_purpose: String,
  pub placement_rationale: String,
}

/// Run the functional-purpose / placement-rationale LLM task against a
/// single function rendered by `context::render_batch_for_extraction`
/// in `subject` shape. `label` identifies the function for logs.
/// Validates that every topic in the input's `non_pure_subjects` list
/// appears exactly once in the LLM's response; missing or extra topics
/// surface as `tracing::warn` events but do not fail the task — the
/// auditor sees the partial result and can prompt for missing entries
/// during review.
pub async fn extract_functional_properties_from_batch(
  batch_json: &str,
  label: &str,
) -> Result<ParsedFunctionalProperties, TaskError> {
  let prompt = format!(
    "{}Batch:\n{}",
    EXTRACT_FUNCTIONAL_PROPERTIES_PROMPT, batch_json
  );

  let log_label = format!("functional_properties_{}", label);
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_CODE,
    &prompt,
    Some(&log_label),
    Some(&FUNCTIONAL_PROPERTIES_SCHEMA),
  )
  .await?;

  let wrapper: LLMFunctionalPropertiesResponse =
    router::parse_response(&response, "functional_properties", &prompt)?;

  let expected = parse_non_pure_subjects(batch_json);
  validate_functional_property_coverage(&expected, &wrapper.subjects, label);

  // Parse each subject_topic safely, dedupe, and reject any topic that
  // wasn't in the batch's `non_pure_subjects` list. Both guards prevent
  // metadata pollution: dedup avoids orphan entries (the reverse index
  // would keep only the last write while topic_metadata accumulates),
  // and the strict filter prevents the LLM from inserting properties
  // for hallucinated or out-of-batch subjects.
  let mut entries = Vec::with_capacity(wrapper.subjects.len());
  let mut seen_subjects: std::collections::HashSet<topic::Topic> =
    std::collections::HashSet::new();
  let mut duplicates: Vec<String> = Vec::new();
  let mut rejected_unexpected: Vec<String> = Vec::new();
  for s in wrapper.subjects {
    let subject_topic = match topic::parse_topic(&s.subject_topic) {
      Ok(t @ topic::Topic::Node(_)) => t,
      Ok(other) => {
        tracing::warn!(
          batch = %label,
          "functional properties: subject_topic {:?} is not an N-prefixed \
           topic; skipping",
          other
        );
        continue;
      }
      Err(e) => {
        tracing::warn!(
          batch = %label,
          "functional properties: failed to parse subject_topic {:?}: {}; \
           skipping",
          s.subject_topic,
          e
        );
        continue;
      }
    };
    // Strict membership check: only accept subjects that were in the
    // input list. The validate step already warned about extras; this
    // guard prevents them from being persisted. When the input list is
    // empty (parse failure or absent field), accept all valid topics
    // so legacy callers without a `non_pure_subjects` field still work.
    if !expected.is_empty() && !expected.contains(&s.subject_topic) {
      rejected_unexpected.push(s.subject_topic);
      continue;
    }
    if !seen_subjects.insert(subject_topic) {
      duplicates.push(s.subject_topic);
      continue;
    }
    entries.push(ParsedSubjectFunctionalProperties {
      subject_topic,
      functional_purpose: s.functional_purpose,
      placement_rationale: s.placement_rationale,
    });
  }
  if !duplicates.is_empty() {
    tracing::warn!(
      batch = %label,
      "functional properties: LLM returned {} duplicate subject_topic(s) \
       (kept first, dropped subsequent): {:?}",
      duplicates.len(),
      duplicates
    );
  }
  if !rejected_unexpected.is_empty() {
    tracing::warn!(
      batch = %label,
      "functional properties: rejected {} subject_topic(s) outside the \
       batch's non_pure_subjects list: {:?}",
      rejected_unexpected.len(),
      rejected_unexpected
    );
  }

  Ok(ParsedFunctionalProperties { entries })
}

/// Extract the `non_pure_subjects` array from a batch JSON payload as a
/// set of topic-id strings. Returns an empty set if the field is absent
/// or malformed — coverage validation just becomes a no-op in that case.
fn parse_non_pure_subjects(
  batch_json: &str,
) -> std::collections::HashSet<String> {
  let Ok(value) = serde_json::from_str::<serde_json::Value>(batch_json) else {
    return std::collections::HashSet::new();
  };
  value
    .get("non_pure_subjects")
    .and_then(|v| v.as_array())
    .map(|arr| {
      arr
        .iter()
        .filter_map(|item| item.as_str().map(String::from))
        .collect()
    })
    .unwrap_or_default()
}

/// Log warnings for any input topic missing from the LLM output and for
/// any output topic not in the input list. Both are ambiguity signals the
/// auditor should see during review.
fn validate_functional_property_coverage(
  expected: &std::collections::HashSet<String>,
  got: &[LLMSubjectFunctionalProperties],
  label: &str,
) {
  if expected.is_empty() {
    return;
  }
  let received: std::collections::HashSet<String> =
    got.iter().map(|s| s.subject_topic.clone()).collect();
  let missing: Vec<&String> = expected.difference(&received).collect();
  let extra: Vec<&String> = received.difference(expected).collect();
  if !missing.is_empty() {
    tracing::warn!(
      batch = %label,
      "functional properties: {} subject(s) in batch were not addressed by the LLM: {:?}",
      missing.len(),
      missing
    );
  }
  if !extra.is_empty() {
    tracing::warn!(
      batch = %label,
      "functional properties: {} subject(s) in LLM output were not in the batch's non_pure_subjects list: {:?}",
      extra.len(),
      extra
    );
  }
}

// ============================================================================
// Conditions (Pipeline Step 6)
// ============================================================================

/// Prompt for generating conditions — assertions that must hold for a
/// non-pure subject's functional purpose and placement rationale to be
/// fulfilled — for every non-pure subject in a single in-scope
/// function/modifier. The input JSON is the output of
/// `render_batch_for_extraction` called with a single-member slice; the
/// envelope uses the `subject` shape (not `batch`). The renderer inlines
/// `functional_purpose` and `placement_rationale` (from step 5) on each
/// non-pure subject node, so the LLM can reason from purpose+placement
/// without re-deriving them. See pipeline-dag.md step 6 and SPEC's
/// "Managing Conditions" / "Conditions vs. Invariants" sections.
const EXTRACT_CONDITIONS_PROMPT: &str = "Below is one in-scope function or \
modifier from a smart contract project. The function appears under the \
`subject` field with:\n\
- `topic`, `name`, `kind`, `visibility`, and a `modifiers` array of \
`{topic, name}` entries.\n\
- `state_reads` and `state_writes`: state-variable topic IDs the function \
reads or mutates.\n\
- `features`: an array of features this function contributes to. Each \
feature has `topic`, `name`, `description`, and `requirements`.\n\
- `behaviors`: what the function as a whole does (already extracted).\n\
- `definition`: the function's signature and body as an AST. Non-pure \
subjects in the body have `purity: \"non_pure\"`; function calls include \
`purity: \"pure\"` or `purity: \"non_pure\"`. Reference nodes carry an \
inline `semantic` when the referenced declaration has a project-specific \
meaning. Call sites carry an inline `callee_behaviors` when the callee \
is in-scope. **Each non-pure subject node carries inline \
`functional_purpose` and `placement_rationale` describing why that \
subject exists at that point in the function.**\n\
- `semantics`: top-level map keyed by declaration topic, deduped \
enumeration of the inline annotations.\n\
- `called_function_behaviors`: top-level map keyed by callee topic, \
deduped enumeration of the inline call-site annotations.\n\n\
The top-level **`non_pure_subjects`** array lists every non-pure subject \
in this function. For **each** topic in that list, produce one or more \
**conditions**. A condition is a single **assertion that must hold** for \
the subject's `functional_purpose` and `placement_rationale` to be \
fulfilled — what the subject's purpose presumes about its environment, \
inputs, callees, or surrounding state. Phrase every condition \
affirmatively: \"X holds,\" \"the caller is …\", \"the value reflects \
…\", \"no other operation observes …\". Each condition is one thing the \
auditor can independently agree or disagree with; do not bundle multiple \
assertions into one entry.\n\n\
**Do not describe failure modes, attack scenarios, or what could go \
wrong.** Those belong to the next pipeline step (threats), which \
generates adversarial inversions of these assertions. If you find \
yourself writing \"could fail,\" \"may be stale,\" \"an attacker can \
…\", or any phrase about something going wrong, stop and re-state as \
the assumption being violated by that scenario. Distinguishing tests:\n\
- If a condition reads \"the code must enforce X,\" you have written a \
small invariant — rewrite as \"the purpose presumes X.\"\n\
- If a condition names a specific failure scenario, you have written a \
threat — rewrite as the assumption that scenario would violate.\n\n\
For each condition, choose a `kind` that names the **category of \
assertion** being expressed. The kinds are:\n\
- `RestrictedReachability` — Triggering of this interaction is \
constrained to expected runtime contexts.\n\
- `AuthorizedAccess` — The caller carries the privilege the subject's \
purpose presumes.\n\
- `ErrorRecoverability` — On failure, the system is in a recoverable \
state.\n\
- `InputIntegrity` — Inputs and read state are not attacker-controlled \
in a way that defeats the purpose.\n\
- `ValueFreshness` — The value being read reflects the latest committed \
state relevant to the purpose.\n\
- `AtomicConsistency` — No interleaving operation observes inconsistent \
state across this point.\n\
- `ResourceAvailability` — Shared resources remain available under \
expected use.\n\
- `Other` — Genuinely novel assertion; description carries the \
structure. Use `Other` when no kind above fits, rather than \
force-fitting to a near-match.\n\n\
For `evidence_topics`, cite topic IDs that are **visible in the rendered \
input** and that justify the assertion: state-variable topics (from \
`state_reads`/`state_writes`), parameter topics, callee topics (from \
inline `callee_behaviors` or `called_function_behaviors`), declaration \
topics from `semantics`, sibling non-pure subject topics, or \
documentation topics from `features.requirements`. Do not invent topic \
IDs. An empty `evidence_topics` array is acceptable when the assertion \
is about an absence of code (e.g. \"the caller is constrained to the \
contract's owner\") that has no positive code anchor.\n\n\
Ground each condition in the subject's `functional_purpose` (what value \
this subject contributes to its feature) and `placement_rationale` (why \
this subject is at this point in the function). The condition states \
what the purpose presumes — not what the subject mechanically does, and \
not how the purpose might fail.\n\n\
Return a JSON object with a `subjects` key whose value is an array. Each \
entry has:\n\
- `subject_topic`: the topic ID from the `non_pure_subjects` list\n\
- `conditions`: an array of `{description, kind, evidence_topics}` \
entries (length >= 1). Each `description` is one or two sentences \
phrased affirmatively; `kind` is one of the eight values above; \
`evidence_topics` is an array of topic ID strings as described.\n\n\
Every topic in `non_pure_subjects` must appear exactly once in the \
output, and each must have at least one condition. If a subject has no \
purpose-relevant assertion that seems load-bearing, that itself is \
signal — emit a single condition with kind `Other` and a description \
naming the degenerate state (e.g. \"this subject's purpose makes no \
presumption about its environment beyond mechanical correctness\"). If \
the function contains no non-pure subjects, return \
`{\"subjects\": []}`.\n\n";

#[derive(Deserialize)]
struct LLMCondition {
  description: String,
  kind: domain::ConditionKind,
  evidence_topics: Vec<String>,
}

#[derive(Deserialize)]
struct LLMSubjectConditions {
  subject_topic: String,
  conditions: Vec<LLMCondition>,
}

#[derive(Deserialize)]
struct LLMConditionsResponse {
  subjects: Vec<LLMSubjectConditions>,
}

static CONDITIONS_SCHEMA: LazyLock<JsonSchema> = LazyLock::new(|| JsonSchema {
  name: "extract_conditions",
  schema: json!({
    "type": "object",
    "additionalProperties": false,
    "required": ["subjects"],
    "properties": {
      "subjects": {
        "type": "array",
        "items": {
          "type": "object",
          "additionalProperties": false,
          "required": ["subject_topic", "conditions"],
          "properties": {
            "subject_topic": { "type": "string" },
            "conditions": {
              "type": "array",
              "items": {
                "type": "object",
                "additionalProperties": false,
                "required": ["description", "kind", "evidence_topics"],
                "properties": {
                  "description": { "type": "string" },
                  "kind": {
                    "type": "string",
                    "enum": [
                      "RestrictedReachability",
                      "AuthorizedAccess",
                      "ErrorRecoverability",
                      "InputIntegrity",
                      "ValueFreshness",
                      "AtomicConsistency",
                      "ResourceAvailability",
                      "Other"
                    ]
                  },
                  "evidence_topics": {
                    "type": "array",
                    "items": { "type": "string" }
                  }
                }
              }
            }
          }
        }
      }
    }
  }),
  empty_response: r#"{"subjects":[]}"#,
});

/// Result of conditions extraction for one batch (one function in
/// per-function mode). Each entry pairs a non-pure subject topic with
/// the list of conditions the LLM produced for it.
pub struct ParsedConditions {
  pub entries: Vec<ParsedSubjectConditions>,
}

pub struct ParsedSubjectConditions {
  pub subject_topic: topic::Topic,
  pub conditions: Vec<ParsedCondition>,
}

pub struct ParsedCondition {
  pub description: String,
  pub kind: domain::ConditionKind,
  pub evidence_topics: Vec<topic::Topic>,
}

/// Run the conditions LLM task against a single function rendered by
/// `context::render_batch_for_extraction` in `subject` shape. `label`
/// identifies the function for logs. Validates that every topic in the
/// input's `non_pure_subjects` list appears exactly once in the LLM's
/// response; missing or extra topics surface as `tracing::warn` events
/// but do not fail the task. Subjects with zero conditions are dropped
/// with a warning (step 7 will not see them). Malformed evidence topics
/// are dropped from their condition; the condition itself is kept with
/// the remaining valid evidence topics.
pub async fn extract_conditions_from_batch(
  batch_json: &str,
  label: &str,
) -> Result<ParsedConditions, TaskError> {
  let prompt = format!("{}Batch:\n{}", EXTRACT_CONDITIONS_PROMPT, batch_json);

  let log_label = format!("conditions_{}", label);
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_CODE,
    &prompt,
    Some(&log_label),
    Some(&CONDITIONS_SCHEMA),
  )
  .await?;

  let wrapper: LLMConditionsResponse =
    router::parse_response(&response, "conditions", &prompt)?;

  let expected = parse_non_pure_subjects(batch_json);
  validate_conditions_coverage(&expected, &wrapper.subjects, label);

  // Parse each subject_topic safely, dedupe, reject any topic that wasn't
  // in the batch's `non_pure_subjects` list, drop subjects with zero
  // conditions, and parse evidence topics per condition. Same shape as
  // step 5's strict-filter / dedupe block — see that function's comment
  // for the rationale.
  let mut entries = Vec::with_capacity(wrapper.subjects.len());
  let mut seen_subjects: std::collections::HashSet<topic::Topic> =
    std::collections::HashSet::new();
  let mut duplicates: Vec<String> = Vec::new();
  let mut rejected_unexpected: Vec<String> = Vec::new();
  let mut empty_conditions: Vec<String> = Vec::new();
  let mut malformed_evidence: Vec<String> = Vec::new();
  for s in wrapper.subjects {
    let subject_topic = match topic::parse_topic(&s.subject_topic) {
      Ok(t @ topic::Topic::Node(_)) => t,
      Ok(other) => {
        tracing::warn!(
          batch = %label,
          "conditions: subject_topic {:?} is not an N-prefixed topic; \
           skipping",
          other
        );
        continue;
      }
      Err(e) => {
        tracing::warn!(
          batch = %label,
          "conditions: failed to parse subject_topic {:?}: {}; skipping",
          s.subject_topic,
          e
        );
        continue;
      }
    };
    // Strict membership check matches step 5: only accept subjects from
    // the batch's `non_pure_subjects` list. When the input list is empty
    // (parse failure or absent field), accept all valid topics so the
    // caller still gets results.
    if !expected.is_empty() && !expected.contains(&s.subject_topic) {
      rejected_unexpected.push(s.subject_topic);
      continue;
    }
    if !seen_subjects.insert(subject_topic) {
      duplicates.push(s.subject_topic);
      continue;
    }

    let mut conditions = Vec::with_capacity(s.conditions.len());
    for c in s.conditions {
      let mut evidence_topics = Vec::with_capacity(c.evidence_topics.len());
      for ev in c.evidence_topics {
        match topic::parse_topic(&ev) {
          Ok(t) => evidence_topics.push(t),
          Err(_) => malformed_evidence.push(ev),
        }
      }
      conditions.push(ParsedCondition {
        description: c.description,
        kind: c.kind,
        evidence_topics,
      });
    }

    if conditions.is_empty() {
      // OpenRouter's strict JSON-schema mode disallows `minItems`, so
      // the schema cannot enforce conditions.len() >= 1. The prompt
      // tells the LLM to emit at least one condition per subject; a
      // zero-condition subject is signal that the LLM gave up on this
      // subject. Drop the entry (step 7 won't see it) and keep the
      // seen-marker — a duplicate empty followed by non-empty for the
      // same subject is still treated as "first wins, dropped".
      empty_conditions.push(s.subject_topic);
      continue;
    }

    entries.push(ParsedSubjectConditions {
      subject_topic,
      conditions,
    });
  }
  if !duplicates.is_empty() {
    tracing::warn!(
      batch = %label,
      "conditions: LLM returned {} duplicate subject_topic(s) (kept \
       first, dropped subsequent): {:?}",
      duplicates.len(),
      duplicates
    );
  }
  if !rejected_unexpected.is_empty() {
    tracing::warn!(
      batch = %label,
      "conditions: rejected {} subject_topic(s) outside the batch's \
       non_pure_subjects list: {:?}",
      rejected_unexpected.len(),
      rejected_unexpected
    );
  }
  if !empty_conditions.is_empty() {
    tracing::warn!(
      batch = %label,
      "conditions: dropped {} subject(s) with zero conditions: {:?}",
      empty_conditions.len(),
      empty_conditions
    );
  }
  if !malformed_evidence.is_empty() {
    tracing::warn!(
      batch = %label,
      "conditions: dropped {} malformed evidence_topic(s) across all \
       subjects: {:?}",
      malformed_evidence.len(),
      malformed_evidence
    );
  }

  Ok(ParsedConditions { entries })
}

/// Log warnings for any input topic missing from the LLM output and for
/// any output topic not in the input list. Same shape as step 5's
/// `validate_functional_property_coverage`.
fn validate_conditions_coverage(
  expected: &std::collections::HashSet<String>,
  got: &[LLMSubjectConditions],
  label: &str,
) {
  if expected.is_empty() {
    return;
  }
  let received: std::collections::HashSet<String> =
    got.iter().map(|s| s.subject_topic.clone()).collect();
  let missing: Vec<&String> = expected.difference(&received).collect();
  let extra: Vec<&String> = received.difference(expected).collect();
  if !missing.is_empty() {
    tracing::warn!(
      batch = %label,
      "conditions: {} subject(s) in batch were not addressed by the LLM: {:?}",
      missing.len(),
      missing
    );
  }
  if !extra.is_empty() {
    tracing::warn!(
      batch = %label,
      "conditions: {} subject(s) in LLM output were not in the batch's \
       non_pure_subjects list: {:?}",
      extra.len(),
      extra
    );
  }
}

// ============================================================================
// Threats (Pipeline Step 7)
// ============================================================================

/// Prompt for generating threats — concrete adversarial scenarios that
/// falsify a specific condition — for every condition on every non-pure
/// subject in a single in-scope function/modifier. The input JSON is the
/// output of `render_batch_for_extraction` called with a single-member
/// slice; the envelope uses the `subject` shape. The unified renderer
/// inlines a `conditions` array on each non-pure subject (step 6 phase 3
/// wired this) — that array is the load-bearing input. Threats are 1:1
/// to the condition they falsify; one condition can be the target of
/// many threats. See SPEC's "Conditions vs. Invariants" and
/// `threats-step-7.md`.
const EXTRACT_THREATS_PROMPT: &str = "Below is one in-scope function or \
modifier from a smart contract project. The function appears under the \
`subject` field with:\n\
- `topic`, `name`, `kind`, `visibility`, and a `modifiers` array of \
`{topic, name}` entries.\n\
- `state_reads`, `state_writes`, `events_emitted`, `reverts` and their \
transitive counterparts (state and effects this function reads, writes, \
emits, or reverts with — directly and through the call graph).\n\
- `features`: an array of features this function contributes to. Each \
feature has `topic`, `name`, `description`, and `requirements`.\n\
- `behaviors`: what the function as a whole does (already extracted).\n\
- `definition`: the function's signature and body as an AST. Non-pure \
subjects in the body have `purity: \"non_pure\"`; function calls include \
`purity: \"pure\"` or `purity: \"non_pure\"`. Reference nodes carry an \
inline `semantic` when the referenced declaration has a project-specific \
meaning. Call sites carry an inline `callee_behaviors` when the callee \
is in-scope. **Each non-pure subject node carries inline \
`functional_purpose`, `placement_rationale`, AND a `conditions` array of \
`{topic, description, kind, evidence_topics}` entries describing the \
assertions that must hold for that subject's purpose to be fulfilled. \
The conditions array is the load-bearing input for this task.**\n\
- `semantics`: top-level map keyed by declaration topic, deduped \
enumeration of the inline annotations.\n\
- `called_function_behaviors`: top-level map keyed by callee topic, \
deduped enumeration of the inline call-site annotations.\n\n\
The top-level **`non_pure_subjects`** array lists every non-pure subject \
in this function. For **each subject** in that list, and for **each \
condition** on that subject, produce zero or more **threats** — \
concrete adversarial scenarios in which the named condition fails to \
hold. A threat is the adversarial inversion of one condition: each \
threat names exactly one condition it falsifies via \
`falsifies_condition`; one condition can be the target of many threats. \
The reasoning chain is purpose → conditions → threats → invariants \
(invariants are a later pipeline step).\n\n\
**Phrase each threat as a concrete scenario, not as a guard \
recommendation.** Good: \"the deterministic token address can be \
pre-computed and `createPair` called first, bricking deployment.\" Bad: \
\"the function should use a commit-reveal scheme.\" If you find \
yourself writing \"the code should …\" or \"the function must …\", \
stop and re-state as the scenario being enabled.\n\n\
**Description prose stays actor-agnostic.** The structured \
`controlled_by` field is the canonical home for actor identity. The \
description must NOT name the actor — no \"an attacker,\" \"a miner,\" \
\"the admin,\" \"the caller,\" \"a user,\" \"a validator,\" \"a \
sequencer,\" \"the owner,\" \"the operator,\" \"the counterparty.\" \
Phrase scenarios in the passive or in terms of the mechanism: \"the \
value can be reordered before the dependent read commits\" — not \"a \
miner reorders the value before the dependent read commits.\" This \
keeps the actor classification independently approvable: an auditor can \
agree with the scenario and disagree with the actor (or vice versa) \
without the prose forcing a paired interpretation. **Distinguishing \
test:** if your description starts with \"an attacker,\" \"a miner,\" \
\"the caller,\" or any other noun naming a party, restate the scenario \
without naming the party.\n\n\
**Bound the evidence scope strictly.** `evidence_topics` may reference \
only topics inside the subject's containing function: the subject node \
itself, its descendants in the AST, sibling statements in the same \
semantic block, the function's signature, and the function's modifiers \
and parameters. Cross-function topics — other functions, state-variable \
declarations, documentation topics, called-function topics — are \
**invalid for threats** and will be rejected. Those are invariant-layer \
anchors (step 8 will produce invariants that point outside the subject \
to the codebase-level defenses). Rationale: threats describe the \
vulnerable surface; invariants describe the protections.\n\n\
**Frame absence as in-subject evidence.** If the threat is enabled by \
the absence of something (no reentrancy guard, no slippage check, no \
access control modifier, no staleness check), point `evidence_topics` \
at the subject node or the function's modifier list to anchor the \
absence — do not point at the missing element, since by definition it \
is not in the codebase. An empty `evidence_topics` array is also \
acceptable for pure-absence threats; the post-processor will populate \
the subject node as a fallback anchor.\n\n\
For each threat, choose a `controlled_by` actor — the party whose \
action drives the scenario. Pick the primary actor; multi-actor \
coordination scenarios go in the description. The actors are:\n\
- `Caller` — An unauthenticated external caller of a public/external \
entry point.\n\
- `PrivilegedRole` — A role-gated party (admin, owner, governor, \
operator). The specific role lives in the description, not in this \
token.\n\
- `External` — A third-party contract: callee in an external call, an \
oracle the subject reads from, or a token the subject interacts with.\n\
- `BlockProducer` — Miner, sequencer, validator, or other party with \
control over transaction ordering or inclusion.\n\
- `Counterparty` — A peer in the protocol's economic model (LP, \
borrower, counterparty to a trade) whose interests differ from the \
subject's purpose.\n\
- `Self` — The contract itself reentering through an external call.\n\
- `AnyParty` — No constraint on who triggers the scenario; \
permissionless.\n\
- `Other` — Genuinely novel actor classification; description carries \
the structure. Use `Other` only when no actor above fits, rather than \
force-fitting to a near-match.\n\n\
**Empty-threats handling.** If a condition has no plausible falsifying \
scenario you can identify (because the assertion is enforced by \
Solidity itself, by an upstream type constraint, or by a structural \
property of the codebase that makes the violation infeasible), emit an \
empty `threats` array AND a `no_threat_rationale` string explaining why \
the condition is discharged. Do not invent threats to fill the slot; \
the rationale is the audit signal that you considered the assertion and \
found no falsifier. When you produce one or more threats for a \
condition, set `no_threat_rationale` to `null`.\n\n\
**Use the audit-wide security context.** Any `Security context` block \
above this prompt names known threats, role definitions, and security \
considerations specific to this audit. Use it to pick realistic actors \
and to avoid restating defenses the auditor has already documented.\n\n\
Return a JSON object with a `subjects` key whose value is an array. \
Each entry has:\n\
- `subject_topic`: a topic ID from the `non_pure_subjects` list\n\
- `conditions`: an array of entries — one per condition on that \
subject — each with:\n\
  - `falsifies_condition`: the condition's A-prefixed topic ID, taken \
from the subject's inline `conditions` array. Citing a condition topic \
that is not on this subject is invalid and will be dropped.\n\
  - `threats`: an array of `{description, controlled_by, \
evidence_topics}` entries (may be empty).\n\
  - `no_threat_rationale`: a string explaining the empty-threats \
decision, or `null` when `threats` is non-empty.\n\n\
If a subject has no conditions in the inline `conditions` array, omit \
the subject from the response entirely. If the function has no \
non-pure subjects, return `{\"subjects\": []}`.\n\n";

#[derive(Deserialize)]
struct LLMThreat {
  description: String,
  controlled_by: domain::ThreatActor,
  evidence_topics: Vec<String>,
}

#[derive(Deserialize)]
struct LLMConditionThreats {
  falsifies_condition: String,
  threats: Vec<LLMThreat>,
  /// `None` when the LLM omits the field or sets it to `null`.
  /// Validation enforces the mutually-exclusive shape:
  /// `threats` non-empty ↔ `no_threat_rationale` is `None`.
  #[serde(default)]
  no_threat_rationale: Option<String>,
}

#[derive(Deserialize)]
struct LLMSubjectThreats {
  subject_topic: String,
  conditions: Vec<LLMConditionThreats>,
}

#[derive(Deserialize)]
struct LLMThreatsResponse {
  subjects: Vec<LLMSubjectThreats>,
}

static THREATS_SCHEMA: LazyLock<JsonSchema> = LazyLock::new(|| JsonSchema {
  name: "extract_threats",
  schema: json!({
    "type": "object",
    "additionalProperties": false,
    "required": ["subjects"],
    "properties": {
      "subjects": {
        "type": "array",
        "items": {
          "type": "object",
          "additionalProperties": false,
          "required": ["subject_topic", "conditions"],
          "properties": {
            "subject_topic": { "type": "string" },
            "conditions": {
              "type": "array",
              "items": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                  "falsifies_condition",
                  "threats",
                  "no_threat_rationale"
                ],
                "properties": {
                  "falsifies_condition": { "type": "string" },
                  "threats": {
                    "type": "array",
                    "items": {
                      "type": "object",
                      "additionalProperties": false,
                      "required": [
                        "description",
                        "controlled_by",
                        "evidence_topics"
                      ],
                      "properties": {
                        "description": { "type": "string" },
                        "controlled_by": {
                          "type": "string",
                          "enum": [
                            "Caller",
                            "PrivilegedRole",
                            "External",
                            "BlockProducer",
                            "Counterparty",
                            "Self",
                            "AnyParty",
                            "Other"
                          ]
                        },
                        "evidence_topics": {
                          "type": "array",
                          "items": { "type": "string" }
                        }
                      }
                    }
                  },
                  "no_threat_rationale": {
                    "type": ["string", "null"]
                  }
                }
              }
            }
          }
        }
      }
    }
  }),
  empty_response: r#"{"subjects":[]}"#,
});

/// Result of threats extraction for one batch (one function in
/// per-function mode). Each entry pairs a non-pure subject topic with
/// its per-condition threat groupings.
pub struct ParsedThreats {
  pub entries: Vec<ParsedSubjectThreats>,
}

pub struct ParsedSubjectThreats {
  pub subject_topic: topic::Topic,
  pub conditions: Vec<ParsedConditionThreats>,
}

pub struct ParsedConditionThreats {
  pub falsifies_condition: topic::Topic,
  pub threats: Vec<ParsedThreat>,
  /// Populated only when `threats` is empty; the LLM's explanation of
  /// why no falsifier was found. The pipeline step posts this as an
  /// agent comment on the condition topic so the rationale lands in
  /// the auditor-visible discussion thread (see `threats-step-7.md`
  /// Phase 5).
  pub no_threat_rationale: Option<String>,
}

pub struct ParsedThreat {
  pub description: String,
  pub controlled_by: domain::ThreatActor,
  pub evidence_topics: Vec<topic::Topic>,
}

/// Validation context derived from the rendered batch JSON. Carries the
/// expected subject set, the in-function topic scope (subject + function
/// topic + modifiers + all node-IDs reachable through the rendered AST
/// definition), and the per-subject inline-condition map used to validate
/// each `falsifies_condition` link.
struct ThreatsValidationContext {
  /// Subjects in the batch's top-level `non_pure_subjects` array.
  expected_subjects: std::collections::HashSet<String>,
  /// Every topic ID treated as "inside the containing function" for the
  /// purposes of evidence-scope validation. Conservative superset: the
  /// function's own topic, modifier topics, and every `id` field
  /// reachable through `subject.definition`. State-variable topics
  /// referenced via `referenced_declaration` (not `id`) are excluded by
  /// design — those are cross-function anchors and are an invariant-
  /// layer concern.
  in_function_topics: std::collections::HashSet<String>,
  /// Subject topic id → set of inline condition topic ids stamped on
  /// that subject by step 6's renderer hook. Used to reject
  /// `falsifies_condition` entries that don't actually belong to the
  /// claimed subject.
  subject_conditions:
    std::collections::HashMap<String, std::collections::HashSet<String>>,
}

/// Run the threats LLM task against a single function rendered by
/// `context::render_batch_for_extraction` in `subject` shape. `label`
/// identifies the function for logs. `security_notes` is the
/// audit-wide framing loaded from `security.md`; when `Some`, it is
/// prepended to the prompt as a `Security context:` block so the LLM
/// picks realistic actors. Validation drops bad entries but keeps the
/// good — a batch with one malformed evidence topic still produces
/// threats with the remaining valid topics. Same drop-and-warn shape
/// as steps 5 and 6.
pub async fn extract_threats_from_batch(
  batch_json: &str,
  label: &str,
  security_notes: Option<&str>,
) -> Result<ParsedThreats, TaskError> {
  let prompt = match security_notes {
    Some(notes) if !notes.trim().is_empty() => format!(
      "Security context:\n{}\n\n{}Batch:\n{}",
      notes.trim(),
      EXTRACT_THREATS_PROMPT,
      batch_json
    ),
    _ => format!("{}Batch:\n{}", EXTRACT_THREATS_PROMPT, batch_json),
  };

  let log_label = format!("threats_{}", label);
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_CODE,
    &prompt,
    Some(&log_label),
    Some(&THREATS_SCHEMA),
  )
  .await?;

  let wrapper: LLMThreatsResponse =
    router::parse_response(&response, "threats", &prompt)?;

  let ctx = build_threats_validation_context(batch_json);
  validate_threats_coverage(&ctx.expected_subjects, &wrapper.subjects, label);

  let entries = parse_threats_response(wrapper, &ctx, label);
  Ok(ParsedThreats { entries })
}

/// Parse a single-subject batch JSON envelope into the validation
/// context used by `parse_threats_response`. Returns an empty context
/// on malformed JSON — the LLM call's outputs are then accepted
/// permissively (same defensive default as `parse_non_pure_subjects`).
fn build_threats_validation_context(
  batch_json: &str,
) -> ThreatsValidationContext {
  let mut ctx = ThreatsValidationContext {
    expected_subjects: std::collections::HashSet::new(),
    in_function_topics: std::collections::HashSet::new(),
    subject_conditions: std::collections::HashMap::new(),
  };
  let Ok(value) = serde_json::from_str::<serde_json::Value>(batch_json) else {
    return ctx;
  };
  if let Some(arr) = value.get("non_pure_subjects").and_then(|v| v.as_array()) {
    ctx.expected_subjects = arr
      .iter()
      .filter_map(|item| item.as_str().map(String::from))
      .collect();
  }
  if let Some(subject) = value.get("subject") {
    if let Some(t) = subject.get("topic").and_then(|v| v.as_str()) {
      ctx.in_function_topics.insert(t.to_string());
    }
    if let Some(mods) = subject.get("modifiers").and_then(|v| v.as_array()) {
      for m in mods {
        if let Some(t) = m.get("topic").and_then(|v| v.as_str()) {
          ctx.in_function_topics.insert(t.to_string());
        }
      }
    }
    if let Some(def) = subject.get("definition") {
      collect_in_function_ids(def, &mut ctx.in_function_topics);
      collect_inline_conditions_per_subject(def, &mut ctx.subject_conditions);
    }
  }
  ctx
}

/// Walk the rendered AST `definition` subtree and collect every `id`
/// field value. These are the topic IDs of AST nodes that live *inside*
/// the function — body subjects, descendants, parameter declarations,
/// etc. By scoping the walk to `subject.definition` (and not
/// `state_reads`, `semantics`, etc.), we exclude cross-function topics
/// from the in-function scope by construction.
fn collect_in_function_ids(
  v: &serde_json::Value,
  out: &mut std::collections::HashSet<String>,
) {
  match v {
    serde_json::Value::Object(map) => {
      if let Some(s) = map.get("id").and_then(|v| v.as_str()) {
        out.insert(s.to_string());
      }
      for value in map.values() {
        collect_in_function_ids(value, out);
      }
    }
    serde_json::Value::Array(arr) => {
      for elem in arr {
        collect_in_function_ids(elem, out);
      }
    }
    _ => {}
  }
}

/// Walk the rendered AST `definition` subtree and, for each node that
/// carries both an `id` (the subject's topic) and a `conditions` array
/// (stamped by the step-6 renderer hook), map the subject topic to the
/// set of condition topic IDs on that subject. Used by
/// `parse_threats_response` to enforce that each `falsifies_condition`
/// link refers to a condition actually attached to the claimed subject.
fn collect_inline_conditions_per_subject(
  v: &serde_json::Value,
  out: &mut std::collections::HashMap<
    String,
    std::collections::HashSet<String>,
  >,
) {
  match v {
    serde_json::Value::Object(map) => {
      if let (Some(id), Some(conds)) = (
        map.get("id").and_then(|v| v.as_str()),
        map.get("conditions").and_then(|v| v.as_array()),
      ) {
        let set: std::collections::HashSet<String> = conds
          .iter()
          .filter_map(|c| {
            c.get("topic").and_then(|t| t.as_str()).map(String::from)
          })
          .collect();
        if !set.is_empty() {
          out.entry(id.to_string()).or_default().extend(set);
        }
      }
      for value in map.values() {
        collect_inline_conditions_per_subject(value, out);
      }
    }
    serde_json::Value::Array(arr) => {
      for elem in arr {
        collect_inline_conditions_per_subject(elem, out);
      }
    }
    _ => {}
  }
}

/// Parse + dedupe + validate the raw LLM response against the batch's
/// rendered envelope. Same "drop-on-defect, warn-on-defect, keep what
/// remains" shape as `extract_conditions_from_batch`.
fn parse_threats_response(
  wrapper: LLMThreatsResponse,
  ctx: &ThreatsValidationContext,
  label: &str,
) -> Vec<ParsedSubjectThreats> {
  let mut entries: Vec<ParsedSubjectThreats> =
    Vec::with_capacity(wrapper.subjects.len());
  let mut seen_subjects: std::collections::HashSet<topic::Topic> =
    std::collections::HashSet::new();
  let mut duplicates: Vec<String> = Vec::new();
  let mut rejected_unexpected: Vec<String> = Vec::new();
  let mut malformed_evidence: Vec<String> = Vec::new();
  let mut out_of_scope_evidence: Vec<String> = Vec::new();
  let mut unknown_condition_links: Vec<String> = Vec::new();
  let mut bad_condition_topics: Vec<String> = Vec::new();
  let mut empty_threats_without_rationale: Vec<String> = Vec::new();
  let mut rationale_dropped_alongside_threats: Vec<String> = Vec::new();
  let mut empty_descriptions: Vec<String> = Vec::new();
  let mut party_named_descriptions: Vec<String> = Vec::new();

  for s in wrapper.subjects {
    let subject_topic = match topic::parse_topic(&s.subject_topic) {
      Ok(t @ topic::Topic::Node(_)) => t,
      Ok(other) => {
        tracing::warn!(
          batch = %label,
          "threats: subject_topic {:?} is not an N-prefixed topic; skipping",
          other
        );
        continue;
      }
      Err(e) => {
        tracing::warn!(
          batch = %label,
          "threats: failed to parse subject_topic {:?}: {}; skipping",
          s.subject_topic,
          e
        );
        continue;
      }
    };
    if !ctx.expected_subjects.is_empty()
      && !ctx.expected_subjects.contains(&s.subject_topic)
    {
      rejected_unexpected.push(s.subject_topic);
      continue;
    }
    if !seen_subjects.insert(subject_topic) {
      duplicates.push(s.subject_topic);
      continue;
    }

    // Subject-scoped condition set from the renderer's inline stamp.
    // `None` here means the subject node had no `conditions` array in the
    // rendered batch JSON (either step 6 produced nothing for this
    // subject, or the renderer omitted it). Under the spec's
    // "falsifies_condition must appear in the subject's inline
    // conditions array" rule, no link can be valid when the array is
    // absent. We gate the strict cross-check on a populated
    // `expected_subjects` set so a fully-malformed batch JSON (where
    // every map ends up empty) still produces results — the dedupe and
    // topic-prefix checks above remain the safety net in that case.
    let allowed_conditions = ctx.subject_conditions.get(&s.subject_topic);
    let cross_check_active = !ctx.expected_subjects.is_empty();

    let mut conditions: Vec<ParsedConditionThreats> =
      Vec::with_capacity(s.conditions.len());
    for ct in s.conditions {
      let falsifies_condition =
        match topic::parse_topic(&ct.falsifies_condition) {
          Ok(t @ topic::Topic::AdversarialProperty(_)) => t,
          _ => {
            bad_condition_topics.push(ct.falsifies_condition);
            continue;
          }
        };
      if cross_check_active {
        let valid_link = allowed_conditions
          .map(|allowed| allowed.contains(&ct.falsifies_condition))
          .unwrap_or(false);
        if !valid_link {
          unknown_condition_links.push(ct.falsifies_condition);
          continue;
        }
      }

      let mut threats: Vec<ParsedThreat> = Vec::with_capacity(ct.threats.len());
      for t in ct.threats {
        let description = t.description.trim().to_string();
        if description.is_empty() {
          empty_descriptions
            .push(format!("{}::{}", s.subject_topic, ct.falsifies_condition));
          continue;
        }
        if description_starts_with_party_noun(&description) {
          // Warn but keep — descriptions with party nouns still carry
          // useful scenario content. If the warn rate spikes, tighten
          // the prompt rather than dropping output.
          party_named_descriptions.push(description.clone());
        }
        let mut evidence_topics: Vec<topic::Topic> =
          Vec::with_capacity(t.evidence_topics.len());
        for ev in t.evidence_topics {
          let parsed = match topic::parse_topic(&ev) {
            Ok(p) => p,
            Err(_) => {
              malformed_evidence.push(ev);
              continue;
            }
          };
          // In-function scope check. The set is empty only when the
          // batch JSON was unparseable; in that case skip the scope
          // check (we already accepted topic-string parsing as the
          // safety net).
          if !ctx.in_function_topics.is_empty()
            && !ctx.in_function_topics.contains(&ev)
          {
            out_of_scope_evidence.push(ev);
            continue;
          }
          evidence_topics.push(parsed);
        }
        // Post-processor fallback: every threat carries at least its
        // own anchor. If the LLM emits zero topics or all topics fail
        // validation, point evidence at the subject node so the
        // auditor can still navigate from threat → site.
        if evidence_topics.is_empty() {
          evidence_topics.push(subject_topic);
        }
        threats.push(ParsedThreat {
          description,
          controlled_by: t.controlled_by,
          evidence_topics,
        });
      }

      // Enforce the mutually-exclusive shape between `threats` and
      // `no_threat_rationale`. Same logic as the spec's validation
      // rules: empty + None drops the entry; non-empty + Some drops
      // the rationale and keeps the threats.
      let no_threat_rationale = match (
        threats.is_empty(),
        ct.no_threat_rationale.as_ref().map(|s| s.trim()),
      ) {
        (true, Some(r)) if !r.is_empty() => Some(r.to_string()),
        (true, _) => {
          empty_threats_without_rationale.push(ct.falsifies_condition.clone());
          continue;
        }
        (false, Some(r)) if !r.is_empty() => {
          rationale_dropped_alongside_threats
            .push(ct.falsifies_condition.clone());
          // Drop the rationale; keep the threats.
          None
        }
        (false, _) => None,
      };

      conditions.push(ParsedConditionThreats {
        falsifies_condition,
        threats,
        no_threat_rationale,
      });
    }

    if conditions.is_empty() {
      // A subject with zero kept conditions is a no-signal entry —
      // drop it from the output (the seen-marker still blocks
      // duplicates, matching step 6's "first wins" semantics).
      continue;
    }

    entries.push(ParsedSubjectThreats {
      subject_topic,
      conditions,
    });
  }

  if !duplicates.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: LLM returned {} duplicate subject_topic(s) (kept first, \
       dropped subsequent): {:?}",
      duplicates.len(),
      duplicates
    );
  }
  if !rejected_unexpected.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: rejected {} subject_topic(s) outside the batch's \
       non_pure_subjects list: {:?}",
      rejected_unexpected.len(),
      rejected_unexpected
    );
  }
  if !bad_condition_topics.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: dropped {} entry(ies) with malformed or non-A-prefixed \
       falsifies_condition: {:?}",
      bad_condition_topics.len(),
      bad_condition_topics
    );
  }
  if !unknown_condition_links.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: dropped {} entry(ies) whose falsifies_condition is not \
       on the claimed subject: {:?}",
      unknown_condition_links.len(),
      unknown_condition_links
    );
  }
  if !malformed_evidence.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: dropped {} malformed evidence_topic(s) across all \
       threats: {:?}",
      malformed_evidence.len(),
      malformed_evidence
    );
  }
  if !out_of_scope_evidence.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: dropped {} evidence_topic(s) outside the subject's \
       containing function: {:?}",
      out_of_scope_evidence.len(),
      out_of_scope_evidence
    );
  }
  if !empty_threats_without_rationale.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: dropped {} condition(s) with empty threats and no \
       no_threat_rationale: {:?}",
      empty_threats_without_rationale.len(),
      empty_threats_without_rationale
    );
  }
  if !rationale_dropped_alongside_threats.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: dropped no_threat_rationale on {} condition(s) that \
       also produced threats (kept the threats, discarded the \
       contradictory rationale): {:?}",
      rationale_dropped_alongside_threats.len(),
      rationale_dropped_alongside_threats
    );
  }
  if !empty_descriptions.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: dropped {} threat(s) with empty descriptions: {:?}",
      empty_descriptions.len(),
      empty_descriptions
    );
  }
  if !party_named_descriptions.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: {} threat description(s) begin with a party-naming noun \
       (actor identity should live in controlled_by; kept the threat \
       — tighten the prompt if this rate spikes)",
      party_named_descriptions.len()
    );
  }

  entries
}

/// Log warnings for any input subject missing from the LLM output and
/// for any output subject not in the input list. Same shape as step
/// 5/6 coverage validators.
fn validate_threats_coverage(
  expected: &std::collections::HashSet<String>,
  got: &[LLMSubjectThreats],
  label: &str,
) {
  if expected.is_empty() {
    return;
  }
  let received: std::collections::HashSet<String> =
    got.iter().map(|s| s.subject_topic.clone()).collect();
  let missing: Vec<&String> = expected.difference(&received).collect();
  let extra: Vec<&String> = received.difference(expected).collect();
  if !missing.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: {} subject(s) in batch were not addressed by the LLM: {:?}",
      missing.len(),
      missing
    );
  }
  if !extra.is_empty() {
    tracing::warn!(
      batch = %label,
      "threats: {} subject(s) in LLM output were not in the batch's \
       non_pure_subjects list: {:?}",
      extra.len(),
      extra
    );
  }
}

/// Lightweight heuristic: does the description's first phrase name a
/// party? Used as a prompt-quality signal — flagged threats are still
/// kept (the spec calls this out as the "one exception" to the drop-on-
/// defect rule), since dropping a description with useful scenario
/// content over a stylistic violation is worse than logging a warning
/// and letting the auditor decide.
fn description_starts_with_party_noun(desc: &str) -> bool {
  let trimmed = desc.trim_start().to_lowercase();
  const PARTY_PREFIXES: &[&str] = &[
    "an attacker",
    "a attacker",
    "the attacker",
    "an user",
    "a user",
    "the user",
    "a caller",
    "an caller",
    "the caller",
    "a miner",
    "the miner",
    "a validator",
    "the validator",
    "a sequencer",
    "the sequencer",
    "an admin",
    "a admin",
    "the admin",
    "the owner",
    "an owner",
    "a owner",
    "the operator",
    "an operator",
    "a operator",
    "the contract",
    "a contract",
    "an external party",
    "a counterparty",
    "the counterparty",
    "an actor",
    "a malicious",
    "the malicious",
  ];
  for prefix in PARTY_PREFIXES {
    if let Some(rest) = trimmed.strip_prefix(prefix) {
      // Word boundary: end of string, or the next char isn't a letter,
      // digit, or underscore. Prevents `the operator` matching
      // `the operators` only as a non-issue (it's still a party plural)
      // while rejecting `the contracted` from matching `the contract`.
      if rest.is_empty()
        || !rest
          .chars()
          .next()
          .map(|c| c.is_alphanumeric() || c == '_')
          .unwrap_or(false)
      {
        return true;
      }
    }
  }
  false
}

/// Prompt for normalizing a documentation file for plain text readability.
const NORMALIZE_DOCUMENTATION_PROMPT: &str = "Below is a documentation file from a smart contract \
project. Your task is to normalize it for optimal plain text readability \
by both LLMs and human readers.\n\n\
Apply the following transformations:\n\
- **Emojis**: Remove emojis entirely, or replace them with a plain text \
  symbol or word equivalent where the emoji carries semantic meaning \
  (e.g., ⚠️ -> WARNING, ✅ -> [OK]).\n\
- **Images and videos**: Replace inline images/videos with their alt text \
  or title on its own line, followed by a markdown link to the resource. \
  For example: `![Architecture diagram](url)` becomes:\n  \
  Architecture diagram\n  Link: url\n\
- **HTML tags**: Convert any inline HTML to its plain text or markdown \
  equivalent. Remove purely presentational HTML (e.g., `<br>`, `<hr>`, \
  `<div>`, `<span>` wrappers) while preserving semantic content. Convert \
  `<a href=\"url\">text</a>` to `[text](url)`, `<strong>` to `**`, \
  `<em>` to `*`, `<code>` to backticks, and HTML tables to markdown tables.\n\
- **Badges and shields**: Remove CI/CD badges, status shields, and \
  similar decorative image links entirely.\n\
- **Decorative formatting**: Remove decorative horizontal rules, excessive \
  blank lines (collapse to at most two), and ornamental characters used \
  for visual separation (e.g., lines of `---`, `===`, `***` used purely \
  for decoration, not as markdown thematic breaks between sections).\n\
- **Anchor links and fragments**: Remove HTML anchor tags (`<a name=\"...\">`) \
  used only for in-page navigation. Keep the heading or text content.\n\
- **Internal navigation**: Remove documentation navigation elements such as \
  \"next section\", \"previous section\", \"back to top\", breadcrumb trails, \
  tables of contents, and similar inter-page or intra-page navigation links \
  that only serve a browsing purpose and carry no informational content.\n\
- **Markdown structure**: Preserve headings, lists, code blocks, tables, \
  blockquotes, and links. These carry semantic value.\n\
- **Section headers**: If the document lacks markdown section headers and \
  reads as flat prose, add appropriate markdown headers to \
  organize the content into logical sections based on topic changes. \
  Do not change the content — only add structure to unstructured documents. \
  Documents that already have headers should keep their existing structure.\n\
- **Content**: Do NOT alter, summarize, rephrase, or remove any textual \
  content. Every sentence, paragraph, and data point must be preserved \
  verbatim. Only formatting and presentation should change.\n\n\
Return ONLY the normalized document text, with no wrapper, no explanation, \
and no code fences around the output.\n\n\
Document:\n";

/// A single documentation file to be normalized: its project-relative path
/// and raw source content.
pub struct DocumentationFile {
  pub file_path: String,
  pub source_content: String,
}

/// Collect documentation files from the audit data for normalization.
pub fn collect_documentation_files(
  audit_data: &AuditData,
) -> Vec<DocumentationFile> {
  let mut files = Vec::new();
  for (path, ast) in &audit_data.asts {
    let doc_ast = match ast {
      AST::Documentation(doc_ast) => doc_ast,
      _ => continue,
    };
    files.push(DocumentationFile {
      file_path: path.file_path.clone(),
      source_content: doc_ast.source_content.clone(),
    });
  }
  files
}

/// Result of normalizing documentation files: maps file paths to their
/// normalized content.
pub struct NormalizedDocumentation {
  pub files: BTreeMap<String, String>,
}

/// Normalize all documentation files for plain text readability via LLM.
///
/// Each file is processed independently in parallel. The caller collects
/// documentation files while holding the lock, then passes them to this
/// function after releasing it. Returns a map of file paths to normalized
/// content that the caller can write back to disk.
pub async fn normalize_documentation(
  documentation_files: &[DocumentationFile],
) -> Result<NormalizedDocumentation, TaskError> {
  if documentation_files.is_empty() {
    return Err(TaskError::Other(
      "No documentation files to normalize".to_string(),
    ));
  }

  let mut handles = Vec::new();
  for doc in documentation_files {
    let prompt =
      format!("{}{}", NORMALIZE_DOCUMENTATION_PROMPT, doc.source_content);
    let file_path = doc.file_path.clone();
    handles.push(tokio::spawn(async move {
      let result = router::chat_completion(
        TaskSize::Small,
        router::SYSTEM_MESSAGE_DOCUMENTATION,
        &prompt,
        Some(&file_path),
        None,
      )
      .await;
      (file_path, result)
    }));
  }

  let mut files = BTreeMap::new();
  for handle in handles {
    match handle.await {
      Ok((path, Ok(response))) => {
        files.insert(path, response);
      }
      Ok((path, Err(e))) => {
        tracing::error!("normalize_documentation failed for {}: {}", path, e);
      }
      Err(e) => {
        tracing::error!("normalize_documentation task panicked: {}", e);
      }
    }
  }

  if files.is_empty() {
    return Err(TaskError::Other(
      "All documentation normalizations failed".to_string(),
    ));
  }

  Ok(NormalizedDocumentation { files })
}

#[cfg(test)]
mod functional_properties_tests {
  use super::*;

  fn subject(
    id: &str,
    purpose: &str,
    placement: &str,
  ) -> LLMSubjectFunctionalProperties {
    LLMSubjectFunctionalProperties {
      subject_topic: id.to_string(),
      functional_purpose: purpose.to_string(),
      placement_rationale: placement.to_string(),
    }
  }

  fn expected_set(items: &[&str]) -> std::collections::HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
  }

  #[test]
  fn parse_non_pure_subjects_extracts_array() {
    let json = r#"{"non_pure_subjects":["N10","N20","N30"],"batch":[]}"#;
    let got = parse_non_pure_subjects(json);
    assert_eq!(got, expected_set(&["N10", "N20", "N30"]));
  }

  #[test]
  fn parse_non_pure_subjects_handles_missing_field() {
    let json = r#"{"batch":[]}"#;
    let got = parse_non_pure_subjects(json);
    assert!(got.is_empty());
  }

  #[test]
  fn parse_non_pure_subjects_handles_malformed_json() {
    let got = parse_non_pure_subjects("not json");
    assert!(got.is_empty());
  }

  #[test]
  fn validate_coverage_no_op_when_expected_empty() {
    // Should not panic and should not warn (we can't easily assert no-warn,
    // but we can at least confirm the path runs).
    let expected = expected_set(&[]);
    let got = vec![subject("N10", "p", "r")];
    validate_functional_property_coverage(&expected, &got, "test");
  }

  #[test]
  fn validate_coverage_full_match_passes() {
    let expected = expected_set(&["N10", "N20"]);
    let got = vec![subject("N10", "p1", "r1"), subject("N20", "p2", "r2")];
    validate_functional_property_coverage(&expected, &got, "test");
  }

  #[test]
  fn validate_coverage_handles_missing_and_extra() {
    // Just exercises the warn paths — the function never panics or
    // returns an error.
    let expected = expected_set(&["N10", "N20", "N30"]);
    let got = vec![subject("N10", "p1", "r1"), subject("N99", "p99", "r99")];
    validate_functional_property_coverage(&expected, &got, "test");
  }

  // --------- end-to-end response parsing ---------

  /// Mirror of the parse + dedupe + strict-filter block from
  /// `extract_functional_properties_from_batch` — runs without an LLM
  /// call so the parsing logic can be unit-tested deterministically.
  /// `expected` matches the batch's `non_pure_subjects` list; an empty
  /// set means "accept all" (legacy behavior).
  fn run_parse_with_expected(
    json_response: &str,
    expected: std::collections::HashSet<String>,
  ) -> Vec<ParsedSubjectFunctionalProperties> {
    let wrapper: LLMFunctionalPropertiesResponse =
      serde_json::from_str(json_response).expect("malformed test JSON");
    let mut entries = Vec::new();
    let mut seen: std::collections::HashSet<topic::Topic> =
      std::collections::HashSet::new();
    for s in wrapper.subjects {
      let Ok(t @ topic::Topic::Node(_)) = topic::parse_topic(&s.subject_topic)
      else {
        continue;
      };
      if !expected.is_empty() && !expected.contains(&s.subject_topic) {
        continue;
      }
      if !seen.insert(t) {
        continue;
      }
      entries.push(ParsedSubjectFunctionalProperties {
        subject_topic: t,
        functional_purpose: s.functional_purpose,
        placement_rationale: s.placement_rationale,
      });
    }
    entries
  }

  fn run_parse(json_response: &str) -> Vec<ParsedSubjectFunctionalProperties> {
    run_parse_with_expected(json_response, std::collections::HashSet::new())
  }

  #[test]
  fn extract_response_parser_rejects_malformed_topic() {
    let json = r#"{"subjects":[
      {"subject_topic":"N10","functional_purpose":"p","placement_rationale":"r"},
      {"subject_topic":"NOT_A_TOPIC","functional_purpose":"p2","placement_rationale":"r2"}
    ]}"#;
    let entries = run_parse(json);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].subject_topic, topic::new_node_topic(&10));
  }

  #[test]
  fn extract_response_parser_rejects_non_node_topic() {
    let json = r#"{"subjects":[
      {"subject_topic":"S5","functional_purpose":"p","placement_rationale":"r"}
    ]}"#;
    let entries = run_parse(json);
    assert!(entries.is_empty(), "S-prefixed topic must not be accepted");
  }

  #[test]
  fn extract_response_parser_dedupes_repeated_subject() {
    let json = r#"{"subjects":[
      {"subject_topic":"N10","functional_purpose":"first","placement_rationale":"first"},
      {"subject_topic":"N10","functional_purpose":"second","placement_rationale":"second"}
    ]}"#;
    let entries = run_parse(json);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].functional_purpose, "first");
  }

  #[test]
  fn extract_response_parser_rejects_unexpected_subject() {
    // The LLM returned a topic that wasn't in the batch's
    // non_pure_subjects list. Must be dropped, not persisted.
    let json = r#"{"subjects":[
      {"subject_topic":"N10","functional_purpose":"ok","placement_rationale":"ok"},
      {"subject_topic":"N999","functional_purpose":"hallucinated","placement_rationale":"!"}
    ]}"#;
    let expected = expected_set(&["N10"]);
    let entries = run_parse_with_expected(json, expected);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].subject_topic, topic::new_node_topic(&10));
  }

  #[test]
  fn extract_response_parser_accepts_all_when_expected_empty() {
    // No expected list means "accept all valid topics" — preserves the
    // legacy behavior so callers without a non_pure_subjects field
    // (currently none, defensive) continue to work.
    let json = r#"{"subjects":[
      {"subject_topic":"N10","functional_purpose":"a","placement_rationale":"b"},
      {"subject_topic":"N20","functional_purpose":"c","placement_rationale":"d"}
    ]}"#;
    let entries =
      run_parse_with_expected(json, std::collections::HashSet::new());
    assert_eq!(entries.len(), 2);
  }

  #[test]
  fn extract_response_parser_handles_empty_subjects() {
    let json = r#"{"subjects":[]}"#;
    let entries = run_parse(json);
    assert!(entries.is_empty());
  }
}

#[cfg(test)]
mod behaviors_parser_tests {
  use super::*;

  fn run_parse(json_response: &str) -> Vec<(topic::Topic, String)> {
    let wrapper: LLMBehaviorsResponse =
      serde_json::from_str(json_response).expect("malformed test JSON");
    let mut behaviors = Vec::new();
    for group in wrapper.members {
      let Ok(member_topic @ topic::Topic::Node(_)) =
        topic::parse_topic(&group.member_topic)
      else {
        continue;
      };
      for desc in group.behaviors {
        behaviors.push((member_topic, desc));
      }
    }
    behaviors
  }

  #[test]
  fn behaviors_parser_skips_malformed_topic() {
    let json = r#"{"members":[
      {"member_topic":"N10","behaviors":["does X"]},
      {"member_topic":"BAD","behaviors":["does Y"]}
    ]}"#;
    let got = run_parse(json);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].1, "does X");
  }

  #[test]
  fn behaviors_parser_skips_non_node_topic() {
    let json = r#"{"members":[
      {"member_topic":"S5","behaviors":["does X"]}
    ]}"#;
    let got = run_parse(json);
    assert!(got.is_empty());
  }
}

#[cfg(test)]
mod conditions_parser_tests {
  use super::*;

  fn expected_set(items: &[&str]) -> std::collections::HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
  }

  /// Mirror of the parse + dedupe + strict-filter block from
  /// `extract_conditions_from_batch` — runs without an LLM call so the
  /// parsing logic can be unit-tested deterministically. `expected`
  /// matches the batch's `non_pure_subjects` list; an empty set means
  /// "accept all" (legacy behavior).
  fn run_parse_with_expected(
    json_response: &str,
    expected: std::collections::HashSet<String>,
  ) -> Vec<ParsedSubjectConditions> {
    let wrapper: LLMConditionsResponse =
      serde_json::from_str(json_response).expect("malformed test JSON");
    let mut entries = Vec::new();
    let mut seen: std::collections::HashSet<topic::Topic> =
      std::collections::HashSet::new();
    for s in wrapper.subjects {
      let Ok(t @ topic::Topic::Node(_)) = topic::parse_topic(&s.subject_topic)
      else {
        continue;
      };
      if !expected.is_empty() && !expected.contains(&s.subject_topic) {
        continue;
      }
      if !seen.insert(t) {
        continue;
      }
      let mut conditions = Vec::with_capacity(s.conditions.len());
      for c in s.conditions {
        let mut evidence_topics = Vec::with_capacity(c.evidence_topics.len());
        for ev in c.evidence_topics {
          if let Ok(et) = topic::parse_topic(&ev) {
            evidence_topics.push(et);
          }
        }
        conditions.push(ParsedCondition {
          description: c.description,
          kind: c.kind,
          evidence_topics,
        });
      }
      if conditions.is_empty() {
        continue;
      }
      entries.push(ParsedSubjectConditions {
        subject_topic: t,
        conditions,
      });
    }
    entries
  }

  fn run_parse(json_response: &str) -> Vec<ParsedSubjectConditions> {
    run_parse_with_expected(json_response, std::collections::HashSet::new())
  }

  // --------- coverage validation ---------

  fn subject(id: &str, conditions: Vec<LLMCondition>) -> LLMSubjectConditions {
    LLMSubjectConditions {
      subject_topic: id.to_string(),
      conditions,
    }
  }

  fn cond(
    description: &str,
    kind: domain::ConditionKind,
    evidence: &[&str],
  ) -> LLMCondition {
    LLMCondition {
      description: description.to_string(),
      kind,
      evidence_topics: evidence.iter().map(|s| s.to_string()).collect(),
    }
  }

  #[test]
  fn validate_coverage_no_op_when_expected_empty() {
    let expected = expected_set(&[]);
    let got = vec![subject(
      "N10",
      vec![cond(
        "c",
        domain::ConditionKind::RestrictedReachability,
        &[],
      )],
    )];
    validate_conditions_coverage(&expected, &got, "test");
  }

  #[test]
  fn validate_coverage_full_match_passes() {
    let expected = expected_set(&["N10", "N20"]);
    let got = vec![
      subject(
        "N10",
        vec![cond(
          "c1",
          domain::ConditionKind::RestrictedReachability,
          &[],
        )],
      ),
      subject(
        "N20",
        vec![cond("c2", domain::ConditionKind::AuthorizedAccess, &[])],
      ),
    ];
    validate_conditions_coverage(&expected, &got, "test");
  }

  #[test]
  fn validate_coverage_handles_missing_and_extra() {
    let expected = expected_set(&["N10", "N20", "N30"]);
    let got = vec![
      subject(
        "N10",
        vec![cond(
          "c",
          domain::ConditionKind::RestrictedReachability,
          &[],
        )],
      ),
      subject(
        "N99",
        vec![cond("c", domain::ConditionKind::AuthorizedAccess, &[])],
      ),
    ];
    validate_conditions_coverage(&expected, &got, "test");
  }

  // --------- end-to-end response parsing ---------

  #[test]
  fn parser_round_trips_well_formed_response() {
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"description":"the caller is the privileged owner","kind":"AuthorizedAccess","evidence_topics":["N5"]},
        {"description":"the value reflects the latest committed price","kind":"ValueFreshness","evidence_topics":[]}
      ]},
      {"subject_topic":"N20","conditions":[
        {"description":"shared liquidity remains available under expected use","kind":"ResourceAvailability","evidence_topics":["N7","N8"]}
      ]}
    ]}"#;
    let entries = run_parse(json);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].subject_topic, topic::new_node_topic(&10));
    assert_eq!(entries[0].conditions.len(), 2);
    assert_eq!(
      entries[0].conditions[0].kind,
      domain::ConditionKind::AuthorizedAccess
    );
    assert_eq!(
      entries[0].conditions[0].evidence_topics,
      vec![topic::new_node_topic(&5)]
    );
    assert_eq!(
      entries[0].conditions[1].kind,
      domain::ConditionKind::ValueFreshness
    );
    assert!(entries[0].conditions[1].evidence_topics.is_empty());
    assert_eq!(entries[1].subject_topic, topic::new_node_topic(&20));
    assert_eq!(
      entries[1].conditions[0].evidence_topics,
      vec![topic::new_node_topic(&7), topic::new_node_topic(&8)]
    );
  }

  #[test]
  fn parser_subject_missing_from_response_yields_no_entry() {
    // Subject N20 was expected but not returned. The validator warns
    // (no panic); the parser produces no entry for it. validation runs
    // separately from parsing — here we just assert the parser produces
    // only the entries that were in the response.
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"description":"c","kind":"RestrictedReachability","evidence_topics":[]}
      ]}
    ]}"#;
    let expected = expected_set(&["N10", "N20"]);
    let entries = run_parse_with_expected(json, expected);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].subject_topic, topic::new_node_topic(&10));
  }

  #[test]
  fn parser_rejects_subject_not_in_non_pure_subjects() {
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"description":"ok","kind":"RestrictedReachability","evidence_topics":[]}
      ]},
      {"subject_topic":"N999","conditions":[
        {"description":"hallucinated","kind":"RestrictedReachability","evidence_topics":[]}
      ]}
    ]}"#;
    let expected = expected_set(&["N10"]);
    let entries = run_parse_with_expected(json, expected);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].subject_topic, topic::new_node_topic(&10));
  }

  #[test]
  fn parser_dedupes_repeated_subject() {
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"description":"first","kind":"RestrictedReachability","evidence_topics":[]}
      ]},
      {"subject_topic":"N10","conditions":[
        {"description":"second","kind":"AuthorizedAccess","evidence_topics":[]}
      ]}
    ]}"#;
    let entries = run_parse(json);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].conditions.len(), 1);
    assert_eq!(entries[0].conditions[0].description, "first");
  }

  #[test]
  fn parser_skips_malformed_subject_topic() {
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"description":"ok","kind":"RestrictedReachability","evidence_topics":[]}
      ]},
      {"subject_topic":"NOT_A_TOPIC","conditions":[
        {"description":"bad","kind":"RestrictedReachability","evidence_topics":[]}
      ]}
    ]}"#;
    let entries = run_parse(json);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].subject_topic, topic::new_node_topic(&10));
  }

  #[test]
  fn parser_skips_non_node_subject_topic() {
    // Subjects must be N-prefixed; an A-prefixed topic must be skipped.
    let json = r#"{"subjects":[
      {"subject_topic":"A5","conditions":[
        {"description":"x","kind":"RestrictedReachability","evidence_topics":[]}
      ]}
    ]}"#;
    let entries = run_parse(json);
    assert!(entries.is_empty(), "non-Node subject_topic must be skipped");
  }

  #[test]
  fn parser_drops_malformed_evidence_topic_keeps_condition() {
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"description":"c","kind":"RestrictedReachability","evidence_topics":["N5","BAD","N7"]}
      ]}
    ]}"#;
    let entries = run_parse(json);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].conditions.len(), 1);
    assert_eq!(
      entries[0].conditions[0].evidence_topics,
      vec![topic::new_node_topic(&5), topic::new_node_topic(&7)]
    );
  }

  #[test]
  fn parser_drops_subject_with_zero_conditions() {
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[]},
      {"subject_topic":"N20","conditions":[
        {"description":"c","kind":"RestrictedReachability","evidence_topics":[]}
      ]}
    ]}"#;
    let entries = run_parse(json);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].subject_topic, topic::new_node_topic(&20));
  }

  #[test]
  fn parser_first_wins_even_when_first_has_empty_conditions() {
    // Strict "first occurrence wins" semantics — matches step 5.
    // If the LLM emits the same subject twice and the first copy has
    // zero conditions, the subject is dropped and the second copy is
    // discarded as a duplicate. Otherwise the dedup rule would silently
    // become "first non-empty wins", which is hard to reason about.
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[]},
      {"subject_topic":"N10","conditions":[
        {"description":"second","kind":"RestrictedReachability","evidence_topics":[]}
      ]}
    ]}"#;
    let entries = run_parse(json);
    assert!(
      entries.is_empty(),
      "first-empty + second-non-empty must yield no entry under \
       'first wins' semantics"
    );
  }

  #[test]
  fn parser_accepts_each_condition_kind() {
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"description":"a","kind":"RestrictedReachability","evidence_topics":[]},
        {"description":"b","kind":"AuthorizedAccess","evidence_topics":[]},
        {"description":"c","kind":"ErrorRecoverability","evidence_topics":[]},
        {"description":"d","kind":"InputIntegrity","evidence_topics":[]},
        {"description":"e","kind":"ValueFreshness","evidence_topics":[]},
        {"description":"f","kind":"AtomicConsistency","evidence_topics":[]},
        {"description":"g","kind":"ResourceAvailability","evidence_topics":[]},
        {"description":"h","kind":"Other","evidence_topics":[]}
      ]}
    ]}"#;
    let entries = run_parse(json);
    assert_eq!(entries.len(), 1);
    let kinds: Vec<domain::ConditionKind> =
      entries[0].conditions.iter().map(|c| c.kind).collect();
    assert_eq!(
      kinds,
      vec![
        domain::ConditionKind::RestrictedReachability,
        domain::ConditionKind::AuthorizedAccess,
        domain::ConditionKind::ErrorRecoverability,
        domain::ConditionKind::InputIntegrity,
        domain::ConditionKind::ValueFreshness,
        domain::ConditionKind::AtomicConsistency,
        domain::ConditionKind::ResourceAvailability,
        domain::ConditionKind::Other,
      ]
    );
  }

  #[test]
  fn parser_rejects_off_list_kind() {
    // An off-list kind must fail deserialization. The schema enforces
    // this at the LLM layer; serde enforces it at the response-parse
    // layer. This is the type-system's safety net for that contract.
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"description":"x","kind":"NotARealKind","evidence_topics":[]}
      ]}
    ]}"#;
    let result: Result<LLMConditionsResponse, _> = serde_json::from_str(json);
    assert!(
      result.is_err(),
      "off-list ConditionKind value must fail deserialization"
    );
  }

  #[test]
  fn parser_handles_empty_subjects_array() {
    let json = r#"{"subjects":[]}"#;
    let entries = run_parse(json);
    assert!(entries.is_empty());
  }

  #[test]
  fn parser_accepts_all_when_expected_empty() {
    // No expected list → "accept all valid topics" (legacy behavior,
    // matching step 5).
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"description":"a","kind":"RestrictedReachability","evidence_topics":[]}
      ]},
      {"subject_topic":"N20","conditions":[
        {"description":"b","kind":"AuthorizedAccess","evidence_topics":[]}
      ]}
    ]}"#;
    let entries =
      run_parse_with_expected(json, std::collections::HashSet::new());
    assert_eq!(entries.len(), 2);
  }

  /// Drift guard: the JSON schema's `kind.enum` array (which the LLM is
  /// constrained against) must exactly match the Rust `ConditionKind`
  /// variants by serialized name, in declaration order. If a future
  /// contributor adds, removes, renames, or reorders a Rust variant
  /// without updating `CONDITIONS_SCHEMA`, the schema and the parser
  /// would silently disagree about the legal value set — the schema
  /// might accept a value serde rejects (or vice versa), surfacing as
  /// confusing test or production failures rather than as a single
  /// clear drift error. Failing here means **update both sides
  /// together.**
  #[test]
  fn schema_kind_enum_matches_rust_variants_exactly() {
    use domain::ConditionKind;

    // Source of truth: every variant of the Rust enum, serialized
    // through serde to its wire-name, in declaration order. Adding a
    // new variant requires extending this array — exhaustiveness on
    // ConditionKind is checked at the bottom.
    let rust_variants: Vec<String> = [
      ConditionKind::RestrictedReachability,
      ConditionKind::AuthorizedAccess,
      ConditionKind::ErrorRecoverability,
      ConditionKind::InputIntegrity,
      ConditionKind::ValueFreshness,
      ConditionKind::AtomicConsistency,
      ConditionKind::ResourceAvailability,
      ConditionKind::Other,
    ]
    .into_iter()
    .map(|k| {
      serde_json::to_value(k)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
    })
    .collect();

    // Enforce that the local list above is exhaustive over the enum.
    // If a new variant is added without extending the array, this match
    // fails to compile (which is the error we want — exhaustiveness on
    // a domain enum is a compile-time guarantee, not a runtime one).
    fn _exhaustiveness_guard(k: ConditionKind) {
      match k {
        ConditionKind::RestrictedReachability
        | ConditionKind::AuthorizedAccess
        | ConditionKind::ErrorRecoverability
        | ConditionKind::InputIntegrity
        | ConditionKind::ValueFreshness
        | ConditionKind::AtomicConsistency
        | ConditionKind::ResourceAvailability
        | ConditionKind::Other => (),
      }
    }

    // Walk the schema JSON to the kind.enum array.
    let kind_enum = CONDITIONS_SCHEMA
      .schema
      .pointer(
        "/properties/subjects/items/properties/conditions/items/properties/kind/enum",
      )
      .expect(
        "CONDITIONS_SCHEMA shape changed; update the JSON pointer in this \
         test to match",
      )
      .as_array()
      .expect("kind.enum must be a JSON array");

    let schema_strings: Vec<String> = kind_enum
      .iter()
      .map(|v| {
        v.as_str()
          .expect("kind.enum entries must be strings")
          .to_string()
      })
      .collect();

    assert_eq!(
      schema_strings, rust_variants,
      "CONDITIONS_SCHEMA's kind.enum drifted from the Rust ConditionKind \
       variants. Update both together — the schema is what the LLM is \
       constrained against; the Rust enum is what serde deserializes the \
       response into."
    );
  }
}

#[cfg(test)]
mod threats_parser_tests {
  use super::*;

  /// Construct a single-subject batch envelope with one non-pure subject
  /// (`subject_id`) carrying an inline `conditions` array (per the step-6
  /// renderer hook) and a small synthetic body. `descendant_ids` are
  /// AST-node topic IDs the renderer would have stamped inside the
  /// function body (e.g. literal nodes); they become valid evidence
  /// anchors. `member_id` is the containing function's topic. The
  /// resulting JSON satisfies `build_threats_validation_context`.
  fn build_batch_envelope(
    member_id: &str,
    subject_id: &str,
    condition_ids: &[&str],
    descendant_ids: &[&str],
    modifier_ids: &[&str],
  ) -> String {
    let conditions: Vec<serde_json::Value> = condition_ids
      .iter()
      .map(|c| {
        json!({
          "topic": c,
          "description": "an assertion",
          "kind": "RestrictedReachability",
          "evidence_topics": [],
        })
      })
      .collect();
    let body_descendants: Vec<serde_json::Value> = descendant_ids
      .iter()
      .map(|d| json!({ "type": "literal", "id": d, "kind": "number", "value": "1" }))
      .collect();
    let modifiers: Vec<serde_json::Value> = modifier_ids
      .iter()
      .map(|m| json!({ "topic": m, "name": "mod" }))
      .collect();

    // Subject AST node carries `id` (its topic) and the inline `conditions`
    // array. When descendant_ids are supplied, the first one nests inside
    // the subject (so it's a true subtree-descendant in the AST walker's
    // sense) and the rest are siblings in the function body. Zero
    // descendants is supported so tests can exercise the bare-subject
    // case without indexing into an empty vec.
    let mut body_descendants = body_descendants.into_iter();
    let first_descendant = body_descendants.next();
    let mut subject_node = json!({
      "type": "assignment",
      "id": subject_id,
      "conditions": conditions,
    });
    if let Some(first) = first_descendant {
      subject_node["right_hand_side"] = first;
    }
    let mut body_statements = vec![subject_node];
    body_statements.extend(body_descendants);

    let envelope = json!({
      "non_pure_subjects": [subject_id],
      "subject": {
        "topic": member_id,
        "name": "f",
        "kind": "function",
        "modifiers": modifiers,
        "definition": {
          "type": "function_definition",
          "id": member_id,
          "body": { "type": "block", "statements": body_statements },
        },
        "semantics": {},
        "called_function_behaviors": {},
        // External anchors that must NOT be in-function scope.
        "state_reads": ["N9001"],
        "state_writes": ["N9002"],
      }
    });
    serde_json::to_string(&envelope).unwrap()
  }

  /// End-to-end parse-and-validate: parses the LLM response then runs it
  /// through `parse_threats_response`. Mirrors the production path
  /// without the LLM call.
  fn run_parse(
    json_response: &str,
    batch_json: &str,
  ) -> Vec<ParsedSubjectThreats> {
    let wrapper: LLMThreatsResponse =
      serde_json::from_str(json_response).expect("malformed test JSON");
    let ctx = build_threats_validation_context(batch_json);
    parse_threats_response(wrapper, &ctx, "test")
  }

  // --------- end-to-end response parsing ---------

  #[test]
  fn parser_round_trips_well_formed_response() {
    let batch =
      build_batch_envelope("N100", "N10", &["A5", "A6"], &["N11", "N12"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"the value can be reordered before the dependent read commits","controlled_by":"BlockProducer","evidence_topics":["N11"]},
          {"description":"the deterministic address can be pre-computed, bricking deployment","controlled_by":"AnyParty","evidence_topics":["N10","N12"]}
        ],"no_threat_rationale":null},
        {"falsifies_condition":"A6","threats":[],"no_threat_rationale":"the assertion is enforced by Solidity's type system"}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].subject_topic, topic::new_node_topic(&10));
    assert_eq!(entries[0].conditions.len(), 2);

    let c0 = &entries[0].conditions[0];
    assert_eq!(
      c0.falsifies_condition,
      topic::new_adversarial_property_topic(5)
    );
    assert_eq!(c0.threats.len(), 2);
    assert!(c0.no_threat_rationale.is_none());
    assert_eq!(
      c0.threats[0].controlled_by,
      domain::ThreatActor::BlockProducer
    );
    assert_eq!(
      c0.threats[0].evidence_topics,
      vec![topic::new_node_topic(&11)]
    );
    assert_eq!(c0.threats[1].controlled_by, domain::ThreatActor::AnyParty);
    assert_eq!(
      c0.threats[1].evidence_topics,
      vec![topic::new_node_topic(&10), topic::new_node_topic(&12)]
    );

    let c1 = &entries[0].conditions[1];
    assert_eq!(
      c1.falsifies_condition,
      topic::new_adversarial_property_topic(6)
    );
    assert!(c1.threats.is_empty());
    assert_eq!(
      c1.no_threat_rationale.as_deref(),
      Some("the assertion is enforced by Solidity's type system")
    );
  }

  #[test]
  fn parser_multiple_threats_targeting_same_condition_kept() {
    // 1:N — one condition can be the target of many threats. All kept.
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"scenario A","controlled_by":"Caller","evidence_topics":[]},
          {"description":"scenario B","controlled_by":"External","evidence_topics":[]},
          {"description":"scenario C","controlled_by":"Self","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].conditions.len(), 1);
    assert_eq!(entries[0].conditions[0].threats.len(), 3);
  }

  #[test]
  fn parser_rejects_subject_not_in_non_pure_subjects() {
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"ok","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]},
      {"subject_topic":"N999","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"hallucinated subject","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].subject_topic, topic::new_node_topic(&10));
  }

  #[test]
  fn parser_dedupes_repeated_subject_first_wins() {
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"first","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]},
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"second","controlled_by":"External","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].conditions.len(), 1);
    assert_eq!(entries[0].conditions[0].threats.len(), 1);
    assert_eq!(entries[0].conditions[0].threats[0].description, "first");
  }

  #[test]
  fn parser_skips_malformed_subject_topic() {
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"NOT_A_TOPIC","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"ok","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert!(entries.is_empty());
  }

  #[test]
  fn parser_skips_non_node_subject_topic() {
    // An A-prefixed topic in subject_topic must be rejected (subjects
    // are AST nodes only).
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"A5","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"ok","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert!(entries.is_empty());
  }

  #[test]
  fn parser_drops_falsifies_condition_not_on_subject() {
    // The subject's inline conditions are {A5}. A response that cites
    // A99 must be dropped (the LLM hallucinated a non-existent
    // condition link).
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A99","threats":[
          {"description":"orphan link","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null},
        {"falsifies_condition":"A5","threats":[
          {"description":"valid link","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].conditions.len(), 1);
    assert_eq!(
      entries[0].conditions[0].falsifies_condition,
      topic::new_adversarial_property_topic(5)
    );
  }

  #[test]
  fn parser_drops_non_a_prefixed_falsifies_condition() {
    // falsifies_condition must be A-prefixed. N-prefixed or malformed
    // values drop the condition entry.
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"N5","threats":[
          {"description":"wrong prefix","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null},
        {"falsifies_condition":"GARBAGE","threats":[
          {"description":"unparseable","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null},
        {"falsifies_condition":"A5","threats":[
          {"description":"valid","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].conditions.len(), 1);
    assert_eq!(
      entries[0].conditions[0].falsifies_condition,
      topic::new_adversarial_property_topic(5)
    );
  }

  #[test]
  fn parser_drops_empty_threats_without_rationale() {
    let batch =
      build_batch_envelope("N100", "N10", &["A5", "A6"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[],"no_threat_rationale":null},
        {"falsifies_condition":"A6","threats":[
          {"description":"kept","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].conditions.len(), 1);
    assert_eq!(
      entries[0].conditions[0].falsifies_condition,
      topic::new_adversarial_property_topic(6)
    );
  }

  #[test]
  fn parser_keeps_empty_threats_with_rationale() {
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[],
         "no_threat_rationale":"enforced by Solidity's type system"}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    let c = &entries[0].conditions[0];
    assert!(c.threats.is_empty());
    assert_eq!(
      c.no_threat_rationale.as_deref(),
      Some("enforced by Solidity's type system")
    );
  }

  #[test]
  fn parser_drops_rationale_when_threats_present() {
    // Mutually-exclusive shape: non-empty threats + Some(rationale)
    // means the LLM contradicted itself. Keep the threats, drop the
    // rationale.
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"scenario","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":"this should be discarded"}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    let c = &entries[0].conditions[0];
    assert_eq!(c.threats.len(), 1);
    assert!(
      c.no_threat_rationale.is_none(),
      "rationale must be dropped when threats are present"
    );
  }

  #[test]
  fn parser_accepts_each_threat_actor_value() {
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"a","controlled_by":"Caller","evidence_topics":[]},
          {"description":"b","controlled_by":"PrivilegedRole","evidence_topics":[]},
          {"description":"c","controlled_by":"External","evidence_topics":[]},
          {"description":"d","controlled_by":"BlockProducer","evidence_topics":[]},
          {"description":"e","controlled_by":"Counterparty","evidence_topics":[]},
          {"description":"f","controlled_by":"Self","evidence_topics":[]},
          {"description":"g","controlled_by":"AnyParty","evidence_topics":[]},
          {"description":"h","controlled_by":"Other","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    let actors: Vec<domain::ThreatActor> = entries[0].conditions[0]
      .threats
      .iter()
      .map(|t| t.controlled_by)
      .collect();
    assert_eq!(
      actors,
      vec![
        domain::ThreatActor::Caller,
        domain::ThreatActor::PrivilegedRole,
        domain::ThreatActor::External,
        domain::ThreatActor::BlockProducer,
        domain::ThreatActor::Counterparty,
        domain::ThreatActor::Self_,
        domain::ThreatActor::AnyParty,
        domain::ThreatActor::Other,
      ]
    );
  }

  #[test]
  fn parser_rejects_off_list_controlled_by() {
    // Schema enforces this at the LLM layer; serde is the safety net.
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"x","controlled_by":"NotARealActor","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let result: Result<LLMThreatsResponse, _> = serde_json::from_str(json);
    assert!(
      result.is_err(),
      "off-list controlled_by must fail deserialization"
    );
  }

  #[test]
  fn parser_drops_evidence_outside_containing_function() {
    // N9001 is in state_reads but NOT in the in-function scope (we
    // explicitly exclude state-reads from the topic walk). It must be
    // dropped; the threat itself is kept with the remaining valid
    // anchor.
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"scenario","controlled_by":"Caller","evidence_topics":["N9001","N11"]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    let t = &entries[0].conditions[0].threats[0];
    assert_eq!(t.evidence_topics, vec![topic::new_node_topic(&11)]);
  }

  #[test]
  fn parser_keeps_subject_topic_and_descendants_as_evidence() {
    let batch =
      build_batch_envelope("N100", "N10", &["A5"], &["N11", "N12"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"scenario","controlled_by":"Caller","evidence_topics":["N10","N11","N12"]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    let t = &entries[0].conditions[0].threats[0];
    assert_eq!(
      t.evidence_topics,
      vec![
        topic::new_node_topic(&10),
        topic::new_node_topic(&11),
        topic::new_node_topic(&12),
      ]
    );
  }

  #[test]
  fn parser_keeps_modifier_topic_as_evidence() {
    let batch =
      build_batch_envelope("N100", "N10", &["A5"], &["N11"], &["N200", "N201"]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"scenario","controlled_by":"Caller","evidence_topics":["N200"]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    let t = &entries[0].conditions[0].threats[0];
    assert_eq!(t.evidence_topics, vec![topic::new_node_topic(&200)]);
  }

  #[test]
  fn parser_keeps_function_topic_as_evidence() {
    // The containing function (the member) is in-scope.
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"scenario","controlled_by":"Caller","evidence_topics":["N100"]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    let t = &entries[0].conditions[0].threats[0];
    assert_eq!(t.evidence_topics, vec![topic::new_node_topic(&100)]);
  }

  #[test]
  fn parser_populates_subject_topic_when_evidence_empty() {
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"absence-anchored scenario","controlled_by":"Self","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    let t = &entries[0].conditions[0].threats[0];
    assert_eq!(
      t.evidence_topics,
      vec![topic::new_node_topic(&10)],
      "empty evidence must be backfilled with the subject topic"
    );
  }

  #[test]
  fn parser_populates_subject_topic_when_all_evidence_invalid() {
    // Every cited topic is out of scope. After dropping, evidence is
    // empty → backfilled with subject_topic.
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"scenario","controlled_by":"Caller","evidence_topics":["N9001","N9002"]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    let t = &entries[0].conditions[0].threats[0];
    assert_eq!(t.evidence_topics, vec![topic::new_node_topic(&10)]);
  }

  #[test]
  fn parser_drops_threat_with_empty_description() {
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"   ","controlled_by":"Caller","evidence_topics":[]},
          {"description":"good","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries[0].conditions[0].threats.len(), 1);
    assert_eq!(entries[0].conditions[0].threats[0].description, "good");
  }

  #[test]
  fn parser_keeps_threat_with_party_named_description_and_warns() {
    // Per spec: descriptions naming a party are kept (warning logged).
    // The warn-rate is the prompt-quality signal, not a drop condition.
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"an attacker can drain the pool","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    let t = &entries[0].conditions[0].threats[0];
    assert_eq!(t.description, "an attacker can drain the pool");
  }

  #[test]
  fn description_starts_with_party_noun_detects_common_phrases() {
    assert!(description_starts_with_party_noun("an attacker reorders X"));
    assert!(description_starts_with_party_noun("An Attacker reorders X"));
    assert!(description_starts_with_party_noun("the caller bypasses Y"));
    assert!(description_starts_with_party_noun("a miner reorders Z"));
    assert!(description_starts_with_party_noun(
      "the admin sets a malicious value"
    ));
    assert!(description_starts_with_party_noun(
      "the counterparty withdraws"
    ));
    assert!(description_starts_with_party_noun(
      "a malicious counterparty withdraws"
    ));
  }

  #[test]
  fn description_starts_with_party_noun_skips_innocuous_openings() {
    assert!(!description_starts_with_party_noun(
      "the value can be reordered before the dependent read commits"
    ));
    assert!(!description_starts_with_party_noun(
      "deterministic addresses can be pre-computed"
    ));
    assert!(!description_starts_with_party_noun(
      "the unguarded entry permits reentry"
    ));
    // Word-boundary check: "the contracted party" must not match "the contract".
    assert!(!description_starts_with_party_noun(
      "the contracted balance overflows"
    ));
    // "the caller's" must still match because punctuation breaks the
    // word-boundary check correctly.
    assert!(description_starts_with_party_noun(
      "the caller's allowance is bypassed"
    ));
  }

  #[test]
  fn parser_handles_empty_subjects_array() {
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[]}"#;
    let entries = run_parse(json, &batch);
    assert!(entries.is_empty());
  }

  #[test]
  fn parser_rejects_all_links_when_subject_has_no_inline_conditions() {
    // Batch is well-formed (expected_subjects populated) but the
    // subject node was rendered without a `conditions` array (step 6
    // produced no conditions for this subject, or the renderer omitted
    // it). Under the strict "must appear in the inline conditions
    // array" rule, no falsifies_condition can be valid — every entry
    // must be dropped. Same intent as conditions parser's strict
    // membership check.
    let envelope = serde_json::json!({
      "non_pure_subjects": ["N10"],
      "subject": {
        "topic": "N100",
        "name": "f",
        "kind": "function",
        "modifiers": [],
        // Subject node has `id` but no `conditions` field.
        "definition": {
          "type": "function_definition",
          "id": "N100",
          "body": {
            "type": "block",
            "statements": [
              { "type": "assignment", "id": "N10" }
            ]
          }
        }
      }
    });
    let batch = serde_json::to_string(&envelope).unwrap();
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"hallucinated link","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert!(
      entries.is_empty(),
      "subject without inline conditions must reject every \
       falsifies_condition link"
    );
  }

  #[test]
  fn parser_falls_back_permissive_when_batch_json_malformed() {
    // Malformed batch JSON yields an empty validation context — no
    // expected_subjects, no in_function_topics, no subject_conditions.
    // The parser falls back to permissive: only the structural checks
    // (N-prefixed subject, A-prefixed falsifies_condition, parseable
    // evidence topics) gate output. This is the same "accept all when
    // input list is empty" fallback the conditions parser uses for
    // robustness against renderer changes.
    let batch = "{ not valid json";
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"scenario","controlled_by":"Caller","evidence_topics":["N42"]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, batch);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].subject_topic, topic::new_node_topic(&10));
    assert_eq!(
      entries[0].conditions[0].falsifies_condition,
      topic::new_adversarial_property_topic(5)
    );
    // Permissive in-function check: N42 is accepted even though we
    // have no scope reference, because in_function_topics is empty.
    assert_eq!(
      entries[0].conditions[0].threats[0].evidence_topics,
      vec![topic::new_node_topic(&42)]
    );
  }

  #[test]
  fn parser_drops_unparseable_evidence_keeps_threat_with_valid_anchors() {
    // A threat with one malformed evidence topic keeps the threat
    // (with the valid topics) and surfaces the malformed one as a
    // warning. Mirrors the conditions parser's same-shape test.
    let batch =
      build_batch_envelope("N100", "N10", &["A5"], &["N11", "N12"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"scenario","controlled_by":"Caller","evidence_topics":["N11","GARBAGE","N12"]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    let t = &entries[0].conditions[0].threats[0];
    assert_eq!(
      t.evidence_topics,
      vec![topic::new_node_topic(&11), topic::new_node_topic(&12)]
    );
  }

  #[test]
  fn parser_drops_subject_when_every_condition_was_dropped() {
    // The condition's falsifies_condition is invalid (non-A-prefixed),
    // so the condition is dropped. With zero kept conditions, the
    // entire subject must be dropped from output — same "no signal"
    // policy as step 6's zero-conditions-subject rule.
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"N5","threats":[
          {"description":"wrong prefix","controlled_by":"Caller","evidence_topics":[]}
        ],"no_threat_rationale":null}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert!(entries.is_empty());
  }

  #[test]
  fn parser_no_threat_rationale_is_trimmed_and_empty_treated_as_absent() {
    // Whitespace-only rationale alongside empty threats is treated as
    // "no rationale" → condition dropped. The mutually-exclusive shape
    // requires *meaningful* content in the rationale slot.
    let batch =
      build_batch_envelope("N100", "N10", &["A5", "A6"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[],"no_threat_rationale":"   \n  "},
        {"falsifies_condition":"A6","threats":[],"no_threat_rationale":"   real reason   "}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].conditions.len(), 1);
    let c = &entries[0].conditions[0];
    assert_eq!(
      c.falsifies_condition,
      topic::new_adversarial_property_topic(6)
    );
    assert_eq!(c.no_threat_rationale.as_deref(), Some("real reason"));
  }

  #[test]
  fn parser_no_threat_rationale_field_absence_handled_gracefully() {
    // `#[serde(default)]` lets the parser handle field absence
    // gracefully. Schema strict mode requires the field, but the
    // serde-level guard is the safety net if the LLM (or a test
    // fixture) omits it. Field absent + threats present = no-op
    // rationale-wise.
    let batch = build_batch_envelope("N100", "N10", &["A5"], &["N11"], &[]);
    let json = r#"{"subjects":[
      {"subject_topic":"N10","conditions":[
        {"falsifies_condition":"A5","threats":[
          {"description":"scenario","controlled_by":"Caller","evidence_topics":[]}
        ]}
      ]}
    ]}"#;
    let entries = run_parse(json, &batch);
    assert_eq!(entries.len(), 1);
    assert!(entries[0].conditions[0].no_threat_rationale.is_none());
  }

  #[test]
  fn build_context_subject_with_empty_conditions_array_absent_from_map() {
    // The renderer hook omits the `conditions` field when the inline
    // array is empty (see context.rs:1590). The collector mirrors
    // that: a subject node with no `conditions` key — or with an empty
    // array — produces no entry in `subject_conditions`. This makes
    // the "subject not in subject_conditions" state in
    // `parse_threats_response` the canonical signal for "no inline
    // conditions on this subject."
    let envelope = serde_json::json!({
      "non_pure_subjects": ["N10"],
      "subject": {
        "topic": "N100",
        "definition": {
          "type": "function_definition",
          "id": "N100",
          "body": {
            "type": "block",
            "statements": [
              // No conditions key.
              { "type": "assignment", "id": "N10" },
              // Empty conditions array — also absent from map.
              { "type": "assignment", "id": "N11", "conditions": [] },
            ]
          }
        }
      }
    });
    let batch = serde_json::to_string(&envelope).unwrap();
    let ctx = build_threats_validation_context(&batch);
    assert!(ctx.subject_conditions.is_empty());
    assert!(ctx.in_function_topics.contains("N10"));
    assert!(ctx.in_function_topics.contains("N11"));
  }

  #[test]
  fn build_context_extracts_expected_subjects_and_topics() {
    let batch = build_batch_envelope(
      "N100",
      "N10",
      &["A5", "A6"],
      &["N11", "N12"],
      &["N200"],
    );
    let ctx = build_threats_validation_context(&batch);
    assert!(ctx.expected_subjects.contains("N10"));
    assert!(ctx.in_function_topics.contains("N100")); // member
    assert!(ctx.in_function_topics.contains("N200")); // modifier
    assert!(ctx.in_function_topics.contains("N10")); // subject
    assert!(ctx.in_function_topics.contains("N11")); // body descendant
    assert!(ctx.in_function_topics.contains("N12")); // body descendant
    // State-reads and state-writes are NOT in-function scope.
    assert!(!ctx.in_function_topics.contains("N9001"));
    assert!(!ctx.in_function_topics.contains("N9002"));

    let conds = ctx
      .subject_conditions
      .get("N10")
      .expect("subject N10 inline conditions present");
    assert!(conds.contains("A5"));
    assert!(conds.contains("A6"));
  }

  #[test]
  fn build_context_returns_empty_on_malformed_json() {
    let ctx = build_threats_validation_context("{ malformed");
    assert!(ctx.expected_subjects.is_empty());
    assert!(ctx.in_function_topics.is_empty());
    assert!(ctx.subject_conditions.is_empty());
  }

  #[test]
  fn validate_coverage_no_op_when_expected_empty() {
    let expected = std::collections::HashSet::new();
    let got = vec![LLMSubjectThreats {
      subject_topic: "N10".to_string(),
      conditions: vec![],
    }];
    validate_threats_coverage(&expected, &got, "test");
  }

  #[test]
  fn validate_coverage_handles_missing_and_extra() {
    // Logs warnings but does not panic.
    let mut expected = std::collections::HashSet::new();
    expected.insert("N10".to_string());
    expected.insert("N20".to_string());
    let got = vec![
      LLMSubjectThreats {
        subject_topic: "N10".to_string(),
        conditions: vec![],
      },
      LLMSubjectThreats {
        subject_topic: "N99".to_string(),
        conditions: vec![],
      },
    ];
    validate_threats_coverage(&expected, &got, "test");
  }

  // --------- schema drift guard ---------

  /// Drift guard: the JSON schema's `controlled_by.enum` array (which
  /// the LLM is constrained against) must exactly match the Rust
  /// `ThreatActor` variants by serialized name, in declaration order.
  /// Same shape as the matching `CONDITIONS_SCHEMA` test — see its
  /// docstring for why this guard is load-bearing.
  #[test]
  fn schema_controlled_by_enum_matches_rust_variants_exactly() {
    use domain::ThreatActor;

    let rust_variants: Vec<String> = [
      ThreatActor::Caller,
      ThreatActor::PrivilegedRole,
      ThreatActor::External,
      ThreatActor::BlockProducer,
      ThreatActor::Counterparty,
      ThreatActor::Self_,
      ThreatActor::AnyParty,
      ThreatActor::Other,
    ]
    .into_iter()
    .map(|a| {
      serde_json::to_value(a)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
    })
    .collect();

    fn _exhaustiveness_guard(a: ThreatActor) {
      match a {
        ThreatActor::Caller
        | ThreatActor::PrivilegedRole
        | ThreatActor::External
        | ThreatActor::BlockProducer
        | ThreatActor::Counterparty
        | ThreatActor::Self_
        | ThreatActor::AnyParty
        | ThreatActor::Other => (),
      }
    }

    let actor_enum = THREATS_SCHEMA
      .schema
      .pointer(
        "/properties/subjects/items/properties/conditions/items/properties/threats/items/properties/controlled_by/enum",
      )
      .expect(
        "THREATS_SCHEMA shape changed; update the JSON pointer in this \
         test to match",
      )
      .as_array()
      .expect("controlled_by.enum must be a JSON array");

    let schema_strings: Vec<String> = actor_enum
      .iter()
      .map(|v| {
        v.as_str()
          .expect("controlled_by.enum entries must be strings")
          .to_string()
      })
      .collect();

    assert_eq!(
      schema_strings, rust_variants,
      "THREATS_SCHEMA's controlled_by.enum drifted from the Rust \
       ThreatActor variants. Update both together — the schema is what \
       the LLM is constrained against; the Rust enum is what serde \
       deserializes the response into."
    );
  }
}

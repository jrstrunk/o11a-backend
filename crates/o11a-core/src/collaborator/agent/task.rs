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
//! 3. **Semantic linking** (LLM steps 2/4/5 — steps 1 and 3 are mechanical /
//!    BM25): Connect doc sections to code declarations across three LLM
//!    synthesis steps that build on each other (`link_contracts` →
//!    `link_member_signatures` → `link_member_bodies`), producing functional
//!    semantics with provenance.
//! 4. **Extract behaviors** (`extract_behaviors_from_batch`): DAG-batched
//!    extraction with semantics + callee behaviors in context.
//! 5. **Synthesize features** (`synthesize_features`): Single-pass LLM
//!    reconciliation of all requirements and behaviors into features.

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

/// Prompt for extracting requirements from a single documentation file, grouped by section.
const EXTRACT_REQUIREMENTS_PROMPT: &str = "Below is a documentation file for a smart contract project, \
rendered as structured JSON with topic IDs (D-prefixed, like \"D42\") \
on each section, paragraph, list, and code block.\n\n\
Your task is to extract **requirements** from this document. Requirements are \
what the documentation claims the system does — each one captures a documented \
behavior, constraint, access control rule, or security property.\n\n\
These requirements will be used by independent security auditors to organize \
their review of the codebase. The documentation is developer-provided and \
**not trusted** — it represents claimed behavior, not verified truth.\n\n\
Requirements define the **scope of what an auditor must verify**. State them \
broadly enough that the auditor is not anchored to a developer's stated \
implementation — the auditor should think critically and consider attack \
vectors beyond what the documentation explicitly addresses. For example, \
prefer \"withdrawals must be safe from reentrancy\" over \"balance must be \
zeroed before the external call\", because the latter assumes a specific \
implementation strategy.\n\n\
**Do not use code declaration names** (function names, variable names, contract \
names) in requirements. Describe capabilities in behavioral terms. For example, \
instead of \"invalidateParticipations() must only be callable by the authorized \
relayer\", write \"Only the authorized relayer is allowed to invalidate \
participations.\" The requirement should describe *what the system must do*, \
not *which function does it*. The original declaration names are preserved in \
the linked documentation topics for traceability.\n\n\
**Each requirement must describe exactly one claim.** If a documentation passage \
describes two distinct things (e.g., access control AND batching support), split \
them into separate requirements. For example, \"Only the authorized relayer can \
invalidate participations\" and \"Invalidating multiple participations in a \
single call must be supported\" are two requirements, not one joined with \
\"and.\"\n\n\
Group requirements under the documentation **section** they were extracted from, \
using the section's D-prefixed topic ID. Each section that contains behavioral \
content should produce one or more requirements.\n\n\
Return a JSON object with a `sections` key whose value is an array of section \
groups. Each section group has:\n\
- `section_topic`: the D-prefixed topic ID of the section header (e.g., \"D5\")\n\
- `requirements`: an array of requirement objects, each with:\n\
  - `description`: a single, specific, testable statement of what the system must do or prevent\n\
  - `documentation_topics`: an array of D-prefixed topic IDs for every paragraph, \
list, or code block within this section that informed this specific requirement\n\n\
Rules:\n\
- Every documentation topic ID that describes system behavior, requirements, \
constraints, security concerns, or invariants should appear in at least one \
requirement. Exclude boilerplate like tables of contents, version history, \
author credits, and headings.\n\
- A documentation topic may appear in multiple requirements if relevant to more than one.\n\
- Do not invent topic IDs. Only use IDs present in the documentation.\n\
- Preserve the developer's specific terminology and phrasing nuances in \
requirement descriptions. Subtle differences in how the documentation \
describes constraints often reflect important design distinctions.\n\
- Include both **happy-path** requirements (what the system should do) and \
**non-happy-path** requirements (what the system must prevent).\n\
- If the documentation describes security threats, attack vectors, access control \
rules, or invariants, capture those as requirements.\n\
- Each section group should have at least one requirement.\n\
- Sections with no behavioral content (boilerplate, navigation, etc.) should be omitted.\n\
- If the document contains no behavioral content at all, return `{\"sections\": []}`.\n\n\
Documentation:\n";

/// Prompt for consolidating requirements extracted from multiple documents.
const CONSOLIDATE_REQUIREMENTS_PROMPT: &str = "Below are requirements extracted independently \
from multiple documentation files for a smart contract project, grouped by \
documentation section. Because each file was processed separately, some \
requirements may overlap or describe the same claim.\n\n\
Your task is to consolidate these into a single, deduplicated list of \
requirements grouped by section. For each group of similar requirements \
across different sections, merge them into the most specific section and \
combine their documentation_topics.\n\n\
Return a JSON object with a `sections` key whose value is an array of section \
groups. Each section group has:\n\
- `section_topic`: the D-prefixed topic ID\n\
- `requirements`: array of requirement objects with `description` and `documentation_topics`\n\n\
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
- If no requirements remain after deduplication, return `{\"sections\": []}`.\n\n\
Requirements to consolidate:\n";

/// Raw section group as returned by the requirement extraction LLM.
#[derive(Deserialize)]
struct LLMSectionGroup {
  section_topic: String,
  requirements: Vec<LLMRequirement>,
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
            "required": ["section_topic", "requirements"],
            "properties": {
              "section_topic": { "type": "string" },
              "requirements": {
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
              }
            }
          }
        }
      }
    }),
    empty_response: r#"{"sections":[]}"#,
  });

/// Result of parsing LLM requirements: requirements grouped by section, no features.
pub struct ParsedRequirements {
  pub requirements: BTreeMap<topic::Topic, Requirement>,
  pub topic_metadata: BTreeMap<topic::Topic, domain::TopicMetadata>,
  /// Section D-topic → R-topic list, preserving document structure
  pub section_requirements: BTreeMap<topic::Topic, Vec<topic::Topic>>,
}

/// Parse the LLM response for section-grouped requirements.
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
  let mut req_counter = 0i32;

  for section in raw_sections {
    let section_topic = topic::new_topic(&section.section_topic);
    let mut section_req_topics = Vec::new();

    for raw_req in section.requirements {
      req_counter += 1;
      let req_topic = topic::new_requirement_topic(req_counter);
      section_req_topics.push(req_topic);

      let doc_topics: Vec<topic::Topic> = raw_req
        .documentation_topics
        .into_iter()
        .map(|id| topic::new_topic(&id))
        .collect();

      topic_metadata.insert(
        req_topic,
        domain::TopicMetadata::RequirementTopic {
          topic: req_topic,
          description: raw_req.description,
          section_topic,
          author: Author::AgentLarge,
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
  }

  Ok(ParsedRequirements {
    requirements,
    topic_metadata,
    section_requirements,
  })
}

/// Extract requirements from documentation files via LLM, grouped by section.
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

  let combined = per_doc_results.join("\n");
  let prompt = format!("{}{}", CONSOLIDATE_REQUIREMENTS_PROMPT, combined);
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some("requirements_consolidate"),
    Some(&REQUIREMENTS_SCHEMA),
  )
  .await?;

  parse_requirements_response(&response, &prompt)
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
by documentation section. Each requirement has an R-prefixed topic ID.\n\
2. **Behaviors** — what the source code actually does, grouped by code member. \
Each behavior has a B-prefixed topic ID.\n\n\
Your task is to **synthesize features** that represent the system's \
capabilities. Each feature connects the documented intent (requirements) \
with the implemented reality (behaviors) for a coherent area of \
functionality.\n\n\
Return a JSON object with a `features` key whose value is an array of \
feature objects. Each feature has:\n\
- `name`: a short, descriptive feature name (behavioral, not technical)\n\
- `description`: a summary synthesized from both the documented intent and \
the implemented reality\n\
- `requirement_topics`: array of R-prefixed topic IDs that apply to this feature\n\
- `behavior_topics`: array of B-prefixed topic IDs that apply to this feature\n\n\
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
  /// Feature → requirement links (F-topic → [R-topics])
  pub feature_requirement_links: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Feature → behavior links (F-topic → [B-topics])
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
    let feature_topic = topic::new_feature_topic((i + 1) as i32);

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
// Behavior Extraction LLM Tasks
// ============================================================================

/// Prompt for extracting behaviors from a batch of dependency-ordered
/// functions/modifiers. The batch JSON is the output of
/// `render_batch_for_behavior_extraction`. See pipeline-dag.md.
const EXTRACT_BEHAVIORS_BATCH_PROMPT: &str = "Below are one or more functions/modifiers from \
an in-scope smart contract project. Each function includes:\n\
- Its `definition` AST (signature and body).\n\
- A `semantics` map keyed by declaration topic — the project-specific \
meaning of each parameter, return value, local, and mutated state variable.\n\
- A `called_function_behaviors` map keyed by callee topic — the behaviors \
of every internal function this one calls (already extracted in earlier \
batches). Out-of-scope callees appear with an empty `behaviors` array.\n\n\
Your task is to extract **behaviors** for each function — what it actually \
does, described in business-level terms.\n\n\
- Use the semantics to describe behaviors at a business level rather than \
mechanically. For example, if `propFactor` has the semantic \"proportional \
reward multiplier\" and `stakerBalance` has \"user's staked token balance\", \
describe the behavior as \"calculates proportional reward share for the \
staker\" rather than \"multiplies propFactor by stakerBalance\".\n\
- Use the called_function_behaviors to understand what internal calls do \
without re-describing them. Describe the composite effect of this function \
in terms of what its callees do, not how they do it.\n\
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
Every function and modifier in the batch must appear in the output with at \
least one behavior. If the batch is empty, return `{\"members\": []}`.\n\n";

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
/// `context::render_batch_for_behavior_extraction`. `label` identifies
/// the batch for logs and LLM-call telemetry (use the
/// `BatchForExtraction.label` field).
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
/// every non-pure subject in a batch of in-scope functions/modifiers.
/// The batch JSON is the output of
/// `render_batch_for_functional_properties`. See pipeline-dag.md step 5.
const EXTRACT_FUNCTIONAL_PROPERTIES_PROMPT: &str = "Below are one or more in-scope \
functions/modifiers from a smart contract project. For each function:\n\
- `definition` is the function's signature and body as an AST. **Non-pure \
subjects in the body have `purity: \"non_pure\"`. Function calls include
`purity: \"pure\"` or `purity: \"non_pure\"`.**\n\
- `feature` is the linked feature: name, description, and requirements.\n\
- `behaviors` lists what the function as a whole does (already extracted).\n\
- `semantics` maps each declaration topic to its name and project-specific \
meaning.\n\
- `called_function_behaviors` maps each callee topic to what that callee does.\n\n\
The top-level **`non_pure_subjects`** array lists every non-pure subject \
across all functions in this batch. For **each** topic in that list, \
produce two properties:\n\n\
- **`functional_purpose`** — the business-logic reason this subject exists, \
expressed in terms of the function's feature and the value the subject \
contributes to that feature. Avoid restating what the operation \
mechanically does; explain the impact on users or the system if it \
were absent.\n\
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
If the batch contains no subjects, return `{\"subjects\": []}`.\n\n";

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
/// rendered batch. `batch_json` is the output of
/// `context::render_batch_for_functional_properties`. `label` identifies
/// the batch for logs. Validates that every topic in the batch's
/// `non_pure_subjects` list appears exactly once in the LLM's response;
/// missing or extra topics surface as `tracing::warn` events but do not
/// fail the task — the auditor sees the partial result and can prompt
/// for missing entries during review.
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
      {"subject_topic":"F5","functional_purpose":"p","placement_rationale":"r"}
    ]}"#;
    let entries = run_parse(json);
    assert!(entries.is_empty(), "F-prefixed topic must not be accepted");
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
      {"member_topic":"B5","behaviors":["does X"]}
    ]}"#;
    let got = run_parse(json);
    assert!(got.is_empty());
  }
}

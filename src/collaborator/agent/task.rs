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
//! 3. **Semantic linking** (`semantic_link_pass1`, `semantic_link_pass2`):
//!    Connect doc sections to code declarations via mechanical resolution
//!    + two LLM passes, producing functional semantics with provenance.
//! 4. **Extract behaviors** (`extract_behaviors_from_contract`): Per-contract
//!    extraction with functional semantics in context.
//! 5. **Synthesize features** (`synthesize_features`): Single-pass LLM
//!    reconciliation of all requirements and behaviors into features.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::collaborator::agent::context;
use crate::collaborator::agent::router::{self, TaskSize};
use crate::collaborator::models::AUTHOR_AGENT;
use crate::core::{self, AST, AuditData, Feature, Requirement, topic};

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
Return a JSON array of section groups, where each group has:\n\
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
- Include both **happy-path** requirements (what the system should do) and \
**non-happy-path** requirements (what the system must prevent).\n\
- If the documentation describes security threats, attack vectors, access control \
rules, or invariants, capture those as requirements.\n\
- Each section group should have at least one requirement.\n\
- Sections with no behavioral content (boilerplate, navigation, etc.) should be omitted.\n\
- Return ONLY a JSON array of section group objects, no other text.\n\n\
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
Return a JSON array of section groups, where each group has:\n\
- `section_topic`: the D-prefixed topic ID\n\
- `requirements`: array of requirement objects with `description` and `documentation_topics`\n\n\
Rules:\n\
- Merge duplicate requirements that describe the same claim, keeping the more \
specific wording and combining documentation_topics.\n\
- Do not drop any unique requirements.\n\
- Do not modify documentation_topics arrays, just combine them when merging.\n\
- If a requirement appears in multiple sections, keep it in the most specific section.\n\
- Do not use code declaration names in requirements — describe capabilities \
in behavioral terms.\n\
- Each requirement must describe exactly one claim. If a merged requirement \
covers two distinct things, split it back into separate requirements.\n\
- Return ONLY a JSON array of section group objects, no other text.\n\n\
Requirements to consolidate:\n";

/// Raw section group as returned by the requirement extraction LLM.
#[derive(Deserialize)]
struct LLMSectionGroup {
  section_topic: String,
  requirements: Vec<LLMRequirement>,
}

/// Result of parsing LLM requirements: requirements grouped by section, no features.
pub struct ParsedRequirements {
  pub requirements: BTreeMap<topic::Topic, Requirement>,
  pub topic_metadata: BTreeMap<topic::Topic, core::TopicMetadata>,
  /// Section D-topic → R-topic list, preserving document structure
  pub section_requirements: BTreeMap<topic::Topic, Vec<topic::Topic>>,
}

/// Parse the LLM response for section-grouped requirements.
fn parse_requirements_response(
  response: &str,
) -> Result<ParsedRequirements, String> {
  let json_str = response
    .trim()
    .strip_prefix("```json")
    .or_else(|| response.trim().strip_prefix("```"))
    .unwrap_or(response.trim());
  let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

  let raw_sections: Vec<LLMSectionGroup> = serde_json::from_str(json_str)
    .map_err(|e| {
      eprintln!(
        "Failed to parse requirements JSON: {}\nResponse:\n{}",
        e, json_str
      );
      format!("Failed to parse requirements JSON: {}", e)
    })?;

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
      section_req_topics.push(req_topic.clone());

      let doc_topics: Vec<topic::Topic> = raw_req
        .documentation_topics
        .into_iter()
        .map(|id| topic::new_topic(&id))
        .collect();

      topic_metadata.insert(
        req_topic.clone(),
        core::TopicMetadata::RequirementTopic {
          topic: req_topic.clone(),
          description: raw_req.description,
          feature_topic: topic::new_topic(""),
          section_topic: Some(section_topic.clone()),
          author_id: AUTHOR_AGENT,
          created_at: String::new(),
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
) -> Result<ParsedRequirements, String> {
  if documentation_files.is_empty() {
    return Err("No documentation found in audit".to_string());
  }

  if documentation_files.len() == 1 {
    let prompt =
      format!("{}{}", EXTRACT_REQUIREMENTS_PROMPT, &documentation_files[0]);
    let response = router::chat_completion(
      TaskSize::Large,
      router::SYSTEM_MESSAGE_DOCUMENTATION,
      &prompt,
      None,
    )
    .await?;
    return parse_requirements_response(&response);
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
      )
      .await
    }));
  }

  let mut per_doc_results: Vec<String> = Vec::new();
  for (i, handle) in handles.into_iter().enumerate() {
    match handle.await {
      Ok(Ok(response)) => per_doc_results.push(response),
      Ok(Err(e)) => {
        eprintln!("extract_requirements failed for document {}: {}", i, e);
      }
      Err(e) => {
        eprintln!(
          "extract_requirements task panicked for document {}: {}",
          i, e
        );
      }
    }
  }

  if per_doc_results.is_empty() {
    return Err("All document requirement extractions failed".to_string());
  }

  if per_doc_results.len() == 1 {
    return parse_requirements_response(&per_doc_results[0]);
  }

  let combined = per_doc_results.join("\n");
  let prompt = format!("{}{}", CONSOLIDATE_REQUIREMENTS_PROMPT, combined);
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some("requirements_consolidate"),
  )
  .await?;

  parse_requirements_response(&response)
}

// ============================================================================
// Semantic Linking LLM Tasks
// ============================================================================

/// LLM pass 1: Given a documentation section and a list of contract signatures,
/// identify which contracts are relevant to this section.
const SEMANTIC_LINK_PASS1_PROMPT: &str = "Below is a documentation section from a smart contract \
project and a list of contracts with their member signatures.\n\n\
Some contracts have been pre-identified as relevant through inline code \
references in the documentation (marked as \"confirmed\"). These are already \
matched — do not repeat them in your response.\n\n\
Your task is to identify any **additional** contracts that this documentation \
section is relevant to beyond the confirmed ones.\n\n\
A contract is relevant if the documentation section describes behavior, \
requirements, or properties that apply to that contract's functionality.\n\n\
Return a JSON array of N-prefixed contract topic ID strings for ONLY the \
newly identified contracts. If there are no additional contracts beyond the \
confirmed ones, return an empty array `[]`.\n\
Return ONLY the JSON array, no other text.\n\n";

/// LLM pass 2: Given a documentation section and a contract's member signatures,
/// identify which members this section is relevant to.
const SEMANTIC_LINK_PASS2_PROMPT: &str = "Below is a documentation section from a smart contract \
project and a contract's member signatures (functions, modifiers, state \
variables, events, etc.).\n\n\
Some members have been pre-identified as relevant through inline code \
references in the documentation (marked as \"confirmed\"). These are already \
matched — do not repeat them in your response.\n\n\
Your task is to identify any **additional** members that this documentation \
section is relevant to beyond the confirmed ones.\n\n\
A member is relevant if the documentation section describes behavior, \
requirements, or properties that apply to that member's functionality.\n\n\
Return a JSON array of N-prefixed member topic ID strings for ONLY the \
newly identified members. If there are no additional members beyond the \
confirmed ones, return an empty array `[]`.\n\
Return ONLY the JSON array, no other text.\n\n";

/// LLM pass 3: Given a documentation section, a list of declarations needing
/// semantics, and the member's source code for disambiguation.
const SEMANTIC_LINK_PASS3_PROMPT: &str = "Below is a documentation section from a smart contract \
project, followed by a list of declarations that need semantic meaning \
assigned, followed by the source code of the containing function/modifier \
for reference.\n\n\
Your task is to assign **semantic meaning** to each declaration based on \
what the **documentation says** it represents in the project. The semantic \
should reflect the developer's documented intent, NOT what the code does \
with the declaration.\n\n\
The source code is provided ONLY to help you identify which declarations \
the documentation is describing — for example, to confirm that `pID` in the \
documentation refers to the `participationId` parameter. Do NOT derive \
meaning from how the code uses a variable. If the documentation says a \
variable is a \"proportional reward factor\" but the code uses it as a \
divisor, the semantic should still be \"proportional reward factor\" — that \
mismatch is valuable information for auditors.\n\n\
For each declaration, provide:\n\
- `declaration_topic`: the N-prefixed topic ID\n\
- `semantic_text`: a concise description of what the documentation says this \
declaration represents in project context (e.g., \"proportional reward \
multiplier\", \"user's staked token balance\", \"reward distribution mechanism\")\n\n\
Rules:\n\
- Derive semantics from the documentation section, not from code behavior.\n\
- If the documentation does not describe a declaration's meaning, omit it \
from the output — do not invent a semantic from the code.\n\
- The semantic text should be project-specific meaning, not a generic type \
description. \"uint256 balance\" is not a semantic — \"user's total staked \
balance\" is.\n\
- Functions and modifiers can receive semantics too — their semantic is \
what they represent (e.g., \"reward distribution mechanism\", \"access \
control check for admin role\").\n\
- Return ONLY a JSON array of objects, no other text.\n\n";

/// Result of LLM pass 1: relevant contract topics for a section.
pub struct SemanticLinkPass1Result {
  pub section_topic: topic::Topic,
  pub contract_topics: Vec<topic::Topic>,
}

/// Result of LLM pass 2: relevant member topics for a (section, contract) pair.
pub struct SemanticLinkPass2Result {
  pub section_topic: topic::Topic,
  pub member_topics: Vec<topic::Topic>,
}

/// A single semantic link from LLM pass 3.
#[derive(Deserialize)]
struct LLMSemanticLink {
  declaration_topic: String,
  semantic_text: String,
}

/// Result of LLM pass 3: semantic links for a (section, member) pair.
pub struct SemanticLinkPass3Result {
  pub section_topic: topic::Topic,
  pub links: Vec<core::SemanticLink>,
}

/// LLM pass 1: For each section, identify relevant contracts.
/// Takes pre-rendered section text, contract list JSON, and confirmed associations.
pub async fn semantic_link_pass1(
  section_topic: &topic::Topic,
  section_text: &str,
  contracts_json: &str,
  confirmed_contracts: &[topic::Topic],
) -> Result<SemanticLinkPass1Result, String> {
  let confirmed_str = if confirmed_contracts.is_empty() {
    String::new()
  } else {
    let ids: Vec<&str> = confirmed_contracts.iter().map(|t| t.id()).collect();
    format!("\nConfirmed relevant contracts: {}\n", ids.join(", "))
  };

  let prompt = format!(
    "{}{}Section:\n{}\n\nContracts:\n{}",
    SEMANTIC_LINK_PASS1_PROMPT, confirmed_str, section_text, contracts_json
  );

  let label = format!("semantic_pass1_{}", section_topic.id());
  let response = router::chat_completion(
    TaskSize::Small,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some(&label),
  )
  .await?;

  let json_str = response
    .trim()
    .strip_prefix("```json")
    .or_else(|| response.trim().strip_prefix("```"))
    .unwrap_or(response.trim());
  let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

  let contract_ids: Vec<String> =
    serde_json::from_str(json_str).map_err(|e| {
      eprintln!(
        "Failed to parse semantic link pass1 JSON: {}\nResponse:\n{}",
        e, json_str
      );
      format!("Failed to parse semantic link pass1: {}", e)
    })?;

  Ok(SemanticLinkPass1Result {
    section_topic: section_topic.clone(),
    contract_topics: contract_ids
      .into_iter()
      .map(|id| topic::new_topic(&id))
      .collect(),
  })
}

/// LLM pass 2: For a (section, contract) pair, identify relevant members.
pub async fn semantic_link_pass2(
  section_topic: &topic::Topic,
  section_text: &str,
  contract_json: &str,
  confirmed_members: &[topic::Topic],
) -> Result<SemanticLinkPass2Result, String> {
  let confirmed_str = if confirmed_members.is_empty() {
    String::new()
  } else {
    let ids: Vec<&str> = confirmed_members.iter().map(|t| t.id()).collect();
    format!("\nConfirmed relevant members: {}\n", ids.join(", "))
  };

  let prompt = format!(
    "{}{}Section:\n{}\n\nContract members:\n{}",
    SEMANTIC_LINK_PASS2_PROMPT, confirmed_str, section_text, contract_json
  );

  let label = format!("semantic_pass2_{}", section_topic.id());
  let response = router::chat_completion(
    TaskSize::Small,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some(&label),
  )
  .await?;

  let json_str = response
    .trim()
    .strip_prefix("```json")
    .or_else(|| response.trim().strip_prefix("```"))
    .unwrap_or(response.trim());
  let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

  let member_ids: Vec<String> =
    serde_json::from_str(json_str).map_err(|e| {
      eprintln!(
        "Failed to parse semantic link pass2 JSON: {}\nResponse:\n{}",
        e, json_str
      );
      format!("Failed to parse semantic link pass2: {}", e)
    })?;

  Ok(SemanticLinkPass2Result {
    section_topic: section_topic.clone(),
    member_topics: member_ids
      .into_iter()
      .map(|id| topic::new_topic(&id))
      .collect(),
  })
}

/// LLM pass 3: For a (section, member) pair, assign semantic meanings to
/// declarations based on documentation. Source code is provided only for
/// disambiguation — semantics must reflect documented intent, not code behavior.
pub async fn semantic_link_pass3(
  section_topic: &topic::Topic,
  section_text: &str,
  declarations_json: &str,
  member_source: &str,
) -> Result<SemanticLinkPass3Result, String> {
  let prompt = format!(
    "{}Documentation section:\n{}\n\nDeclarations needing semantics:\n{}\n\nSource code (for disambiguation only):\n{}",
    SEMANTIC_LINK_PASS3_PROMPT, section_text, declarations_json, member_source
  );

  let label = format!("semantic_pass3_{}", section_topic.id());
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some(&label),
  )
  .await?;

  let json_str = response
    .trim()
    .strip_prefix("```json")
    .or_else(|| response.trim().strip_prefix("```"))
    .unwrap_or(response.trim());
  let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

  let raw_links: Vec<LLMSemanticLink> = serde_json::from_str(json_str)
    .map_err(|e| {
      eprintln!(
        "Failed to parse semantic link pass3 JSON: {}\nResponse:\n{}",
        e, json_str
      );
      format!("Failed to parse semantic link pass3: {}", e)
    })?;

  let links = raw_links
    .into_iter()
    .map(|l| core::SemanticLink {
      documentation_topic: section_topic.clone(),
      declaration_topic: topic::new_topic(&l.declaration_topic),
      semantic_text: l.semantic_text,
    })
    .collect();

  Ok(SemanticLinkPass3Result {
    section_topic: section_topic.clone(),
    links,
  })
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
      if let core::TopicMetadata::TitledTopic {
        kind: core::TitledTopicKind::DocumentationSection,
        scope,
        ..
      } = m
      {
        // Only top-level sections (Container scope = document root children)
        if matches!(scope, core::Scope::Container { .. }) {
          return Some(t.clone());
        }
      }
      None
    })
    .collect()
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
Your task is to **synthesize features** by grouping related requirements and \
behaviors together. A feature represents a coherent capability or area of the \
system where documented claims and implemented behaviors overlap.\n\n\
For each feature, provide:\n\
- `name`: a short, descriptive feature name\n\
- `description`: a summary synthesized from both the documented intent and \
the implemented reality\n\
- `requirement_topics`: array of R-prefixed topic IDs grouped into this feature\n\
- `behavior_topics`: array of B-prefixed topic IDs grouped into this feature\n\n\
Rules:\n\
- Group requirements and behaviors that describe the same capability together.\n\
- A requirement or behavior may belong to at most one feature.\n\
- Requirements with no matching behaviors should still form a feature — this \
represents **unimplemented specification** (the docs claim something the code \
doesn't do). Set behavior_topics to an empty array.\n\
- Behaviors with no matching requirements should still form a feature — this \
represents **undocumented implementation** (the code does something the docs \
don't describe). Set requirement_topics to an empty array.\n\
- Do not force weak matches. It's better to surface an orphan than to create \
a misleading grouping.\n\
- Feature names should be behavioral (what the system does), not technical.\n\
- Return ONLY a JSON array of feature objects, no other text.\n\n";

/// Raw feature from reconciliation LLM.
#[derive(Deserialize)]
struct LLMSynthesizedFeature {
  name: String,
  description: String,
  requirement_topics: Vec<String>,
  behavior_topics: Vec<String>,
}

/// Result of feature synthesis.
pub struct SynthesizedFeatures {
  pub features: BTreeMap<topic::Topic, Feature>,
  pub topic_metadata: BTreeMap<topic::Topic, core::TopicMetadata>,
  /// Which requirements belong to which feature (R-topic → F-topic)
  pub requirement_to_feature: BTreeMap<topic::Topic, topic::Topic>,
  /// Which behaviors belong to which feature (B-topic → F-topic)
  pub behavior_to_feature: BTreeMap<topic::Topic, topic::Topic>,
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
        if let Some(core::TopicMetadata::RequirementTopic {
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
      if let Some(core::TopicMetadata::RequirementTopic {
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
        if let Some(core::TopicMetadata::BehaviorTopic {
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
) -> Result<SynthesizedFeatures, String> {
  let prompt = format!(
    "{}Requirements:\n{}\n\nBehaviors:\n{}",
    SYNTHESIZE_FEATURES_PROMPT, requirements_json, behaviors_json
  );

  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some("synthesize_features"),
  )
  .await?;

  let json_str = response
    .trim()
    .strip_prefix("```json")
    .or_else(|| response.trim().strip_prefix("```"))
    .unwrap_or(response.trim());
  let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

  let raw_features: Vec<LLMSynthesizedFeature> = serde_json::from_str(json_str)
    .map_err(|e| {
      eprintln!(
        "Failed to parse synthesized features JSON: {}\nResponse:\n{}",
        e, json_str
      );
      format!("Failed to parse synthesized features: {}", e)
    })?;

  let mut features = BTreeMap::new();
  let mut topic_metadata = BTreeMap::new();
  let mut requirement_to_feature = BTreeMap::new();
  let mut behavior_to_feature = BTreeMap::new();

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

    for rt in &requirement_topics {
      requirement_to_feature.insert(rt.clone(), feature_topic.clone());
    }
    for bt in &behavior_topics {
      behavior_to_feature.insert(bt.clone(), feature_topic.clone());
    }

    topic_metadata.insert(
      feature_topic.clone(),
      core::TopicMetadata::FeatureTopic {
        topic: feature_topic.clone(),
        name: raw.name,
        description: raw.description,
        author_id: AUTHOR_AGENT,
        created_at: String::new(),
      },
    );

    features.insert(feature_topic, Feature { requirement_topics });
  }

  Ok(SynthesizedFeatures {
    features,
    topic_metadata,
    requirement_to_feature,
    behavior_to_feature,
  })
}

// ============================================================================
// Behavior Extraction LLM Tasks
// ============================================================================

/// Prompt for extracting behaviors from a contract's source code.
const EXTRACT_BEHAVIORS_PROMPT: &str = "Below is a smart contract with its functions and \
modifiers (state variables are excluded) and functional semantics \
(project-specific meanings for declarations).\n\n\
Your task is to extract **behaviors** — what each function/modifier in this \
contract actually does, described in business-level terms using the \
functional semantics provided.\n\n\
For each function or modifier, produce one or more behaviors that describe \
what it does. Use the functional semantics to describe behaviors at a \
business level rather than mechanically. For example, if `propFactor` has \
the semantic \"proportional reward multiplier\" and `stakerBalance` has \
\"user's staked token balance\", describe the behavior as \"calculates \
proportional reward share for the staker\" rather than \"multiplies \
propFactor by stakerBalance.\"\n\n\
Each behavior belongs to exactly one function or modifier. \
Each function/modifier may have multiple behaviors.\n\n\
Return a JSON array of member groups, where each group has:\n\
- `member_topic`: the N-prefixed topic ID of the function/modifier\n\
- `behaviors`: an array of behavior description strings\n\n\
Rules:\n\
- Every function and modifier must have at least one behavior.\n\
- Each behavior should be a concise, specific description of what the code does.\n\
- Use functional semantics to give business-level meaning when available.\n\
- Include both normal execution paths and edge case behaviors (reverts, \
access control checks, state mutations).\n\
- Do not describe implementation details like \"calls _transfer internally\" — \
describe the observable effect: \"transfers tokens from sender to recipient.\"\n\
- Return ONLY the JSON array, no other text.\n\n";

/// Raw member behavior group from LLM.
#[derive(Deserialize)]
struct LLMMemberBehaviors {
  member_topic: String,
  behaviors: Vec<String>,
}

/// Result of behavior extraction for a contract.
pub struct ParsedBehaviors {
  pub behaviors: Vec<(topic::Topic, String)>, // (member_topic, description)
}

/// Extract behaviors from a contract's source code via LLM.
pub async fn extract_behaviors_from_contract(
  contract_json: &str,
  contract_name: &str,
) -> Result<ParsedBehaviors, String> {
  let prompt =
    format!("{}Contract:\n{}", EXTRACT_BEHAVIORS_PROMPT, contract_json);

  let label = format!("behaviors_{}", contract_name);
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_CODE,
    &prompt,
    Some(&label),
  )
  .await?;

  let json_str = response
    .trim()
    .strip_prefix("```json")
    .or_else(|| response.trim().strip_prefix("```"))
    .unwrap_or(response.trim());
  let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

  let raw_groups: Vec<LLMMemberBehaviors> = serde_json::from_str(json_str)
    .map_err(|e| {
      eprintln!(
        "Failed to parse behaviors JSON: {}\nResponse:\n{}",
        e, json_str
      );
      format!("Failed to parse behaviors JSON: {}", e)
    })?;

  let mut behaviors = Vec::new();
  for group in raw_groups {
    let member_topic = topic::new_topic(&group.member_topic);
    for desc in group.behaviors {
      behaviors.push((member_topic.clone(), desc));
    }
  }

  Ok(ParsedBehaviors { behaviors })
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
) -> Result<NormalizedDocumentation, String> {
  if documentation_files.is_empty() {
    return Err("No documentation files to normalize".to_string());
  }

  let mut handles = Vec::new();
  for doc in documentation_files {
    let prompt =
      format!("{}{}", NORMALIZE_DOCUMENTATION_PROMPT, doc.source_content);
    let file_path = doc.file_path.clone();
    handles.push(tokio::spawn(async move {
      let result = router::chat_completion(
        TaskSize::Large,
        router::SYSTEM_MESSAGE_DOCUMENTATION,
        &prompt,
        Some(&file_path),
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
        eprintln!("normalize_documentation failed for {}: {}", path, e);
      }
      Err(e) => {
        eprintln!("normalize_documentation task panicked: {}", e);
      }
    }
  }

  if files.is_empty() {
    return Err("All documentation normalizations failed".to_string());
  }

  Ok(NormalizedDocumentation { files })
}

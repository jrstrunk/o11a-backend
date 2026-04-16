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
use crate::collaborator::models::AUTHOR_AGENT_LARGE;
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
- Preserve the developer's specific terminology and phrasing nuances in \
requirement descriptions. Subtle differences in how the documentation \
describes constraints often reflect important design distinctions.\n\
- Include both **happy-path** requirements (what the system should do) and \
**non-happy-path** requirements (what the system must prevent).\n\
- If the documentation describes security threats, attack vectors, access control \
rules, or invariants, capture those as requirements.\n\
- Each section group should have at least one requirement.\n\
- Sections with no behavioral content (boilerplate, navigation, etc.) should be omitted.\n\
- If the document contains no behavioral content at all, return an empty array `[]`.\n\
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
- If no requirements remain after deduplication, return an empty array `[]`.\n\
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
async fn parse_requirements_response(
  response: &str,
  prompt: &str,
) -> Result<ParsedRequirements, String> {
  let raw_sections: Vec<LLMSectionGroup> =
    router::extract_json(
      response,
      "requirements",
      r#"[{"section_topic": "D5", "requirements": [{"description": "...", "documentation_topics": ["D6", "D7"]}]}]"#,
      prompt,
    ).await?;

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
          section_topic: Some(section_topic.clone()),
          author_id: AUTHOR_AGENT_LARGE,
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
      true,
    )
    .await?;
    return parse_requirements_response(&response, &prompt).await;
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
        true,
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
    return parse_requirements_response(
      &per_doc_results[0],
      EXTRACT_REQUIREMENTS_PROMPT,
    )
    .await;
  }

  let combined = per_doc_results.join("\n");
  let prompt = format!("{}{}", CONSOLIDATE_REQUIREMENTS_PROMPT, combined);
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some("requirements_consolidate"),
    true,
  )
  .await?;

  parse_requirements_response(&response, &prompt).await
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
The documentation section contains D-prefixed topic IDs (like \"D42\") on \
each paragraph, list, code block, and subsection.\n\n\
Some members have been pre-identified as relevant through inline code \
references in the documentation (marked as \"confirmed\"). These are already \
matched — do not repeat them in your response.\n\n\
Your task is to identify any **additional** members that this documentation \
section is relevant to beyond the confirmed ones, and for each member, \
specify which specific D-prefixed child elements of the documentation \
describe it.\n\n\
A member is relevant if the documentation section describes behavior, \
requirements, or properties that apply to that member's functionality.\n\n\
Return a JSON array of objects, each with:\n\
- `member_topic`: the N-prefixed member topic ID\n\
- `doc_topics`: array of D-prefixed topic IDs for the specific paragraphs, \
lists, or subsections within the documentation that describe this member\n\n\
Example: `[{\"member_topic\": \"N-1234\", \"doc_topics\": [\"D42\", \"D43\"]}]`\n\n\
Rules:\n\
- Only include members not already in the confirmed list.\n\
- The `doc_topics` must be D-prefixed IDs that actually appear in the \
provided documentation section. Do not invent IDs.\n\
- If a member relates to the entire section rather than specific child \
elements, use the section's own D-prefixed ID.\n\
- If there are no additional members, return an empty array `[]`.\n\
- Return ONLY the JSON array, no other text.\n\n";

/// LLM pass 3: Given a documentation section, a list of declarations needing
/// semantics, and the member's source code for disambiguation.
const SEMANTIC_LINK_PASS3_PROMPT: &str = "Below is a documentation section from a smart contract \
project, followed by a list of code declarations that need semantic meaning \
assigned, followed by the source code for reference.\n\n\
The declarations may come from multiple functions, modifiers, or \
contract-level definitions. Each declaration has a topic ID and name.\n\n\
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
For each declaration, provide an object with ALL of these fields (all are required):\n\
- `declaration_topic` (required): the N-prefixed topic ID of the declaration\n\
- `semantic_text` (required): a concise description of what the documentation says this \
declaration represents in project context (e.g., \"proportional reward \
multiplier\", \"user's staked token balance\", \"reward distribution mechanism\")\n\
- `documentation_topics` (required): array of D-prefixed topic IDs for the \
specific paragraphs, lists, or subsections in the documentation that this \
semantic was derived from. Include all child elements that contributed to \
the semantic meaning.\n\n\
Rules:\n\
- Derive semantics from the documentation section, not from code behavior.\n\
- If the documentation does not describe a declaration's meaning, omit it \
from the output — do not invent a semantic from the code.\n\
- The semantic text should be project-specific meaning, not a generic type \
description. \"uint256 balance\" is not a semantic — \"user's total staked \
balance\" is.\n\
- Preserve the developer's specific terminology and phrasing nuances. \
Subtle differences in how the documentation describes something often \
reflect important design distinctions.\n\
- Functions and modifiers can receive semantics too — their semantic is \
what they represent (e.g., \"reward distribution mechanism\", \"access \
control check for admin role\").\n\
- Each `documentation_topics` entry must be a D-prefixed ID that appears in \
the provided documentation section.\n\
- If the documentation does not describe any of the provided declarations, \
return an empty array `[]`. Do NOT echo back the documentation input or \
return the documentation structure — only return semantic link objects \
or an empty array.\n\
- Every object MUST include all three fields.\n\
- Always return a JSON **array**, even for a single result: `[{...}]` not `{...}`.\n\
- Do NOT return an empty object `{}` — use an empty array `[]` instead.\n\
- Return ONLY a JSON array of objects, no other text.\n\n";

/// Result of LLM pass 1: relevant contract topics for a section.
pub struct SemanticLinkPass1Result {
  pub section_topic: topic::Topic,
  pub contract_topics: Vec<topic::Topic>,
}

/// A member matched by pass 2 with the specific doc child sections it relates to.
pub struct MemberDocMapping {
  pub member_topic: topic::Topic,
  pub doc_topics: Vec<topic::Topic>,
}

/// Result of LLM pass 2: members mapped to specific child doc sections.
pub struct SemanticLinkPass2Result {
  pub section_topic: topic::Topic,
  pub member_mappings: Vec<MemberDocMapping>,
}

#[derive(Deserialize)]
struct LLMPass2MemberMapping {
  member_topic: String,
  doc_topics: Vec<String>,
}

/// A single semantic link from LLM pass 3.
#[derive(Deserialize)]
struct LLMSemanticLink {
  declaration_topic: String,
  semantic_text: String,
  #[serde(default)]
  documentation_topics: Vec<String>,
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
    true,
  )
  .await?;

  let contract_ids: Vec<String> = router::extract_json(
    &response,
    "semantic link pass1",
    r#"["N-1234", "N-5678"]"#,
    &prompt,
  )
  .await?;

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
    TaskSize::Medium,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some(&label),
    true,
  )
  .await?;

  let raw_mappings: Vec<LLMPass2MemberMapping> = router::extract_json(
    &response,
    "semantic link pass2",
    r#"[{"member_topic": "N-1234", "doc_topics": ["D42", "D43"]}]"#,
    &prompt,
  )
  .await?;

  Ok(SemanticLinkPass2Result {
    section_topic: section_topic.clone(),
    member_mappings: raw_mappings
      .into_iter()
      .map(|m| MemberDocMapping {
        member_topic: topic::new_topic(&m.member_topic),
        doc_topics: m
          .doc_topics
          .into_iter()
          .map(|d| topic::new_topic(&d))
          .collect(),
      })
      .collect(),
  })
}

/// LLM pass 3: Assign semantic meanings to declarations based on documentation.
/// Accepts batched declarations from multiple members. Source code is provided
/// only for disambiguation — semantics must reflect documented intent.
///
/// `fallback_doc_topic` is used when the LLM omits `documentation_topic` from
/// a result object.
pub async fn semantic_link_pass3(
  section_topic: &topic::Topic,
  section_text: &str,
  declarations_json: &str,
  source_code: &str,
  fallback_doc_topic: &topic::Topic,
) -> Result<SemanticLinkPass3Result, String> {
  let prompt = format!(
    "{}Documentation section:\n{}\n\nDeclarations needing semantics:\n{}\n\nSource code (for disambiguation only):\n{}",
    SEMANTIC_LINK_PASS3_PROMPT, section_text, declarations_json, source_code
  );

  let label = format!("semantic_pass3_{}", section_topic.id());
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some(&label),
    true,
  )
  .await?;

  let raw_links: Vec<LLMSemanticLink> = router::extract_json(
    &response,
    "semantic link pass3",
    r#"[{"declaration_topic": "N-1234", "semantic_text": "...", "documentation_topics": ["D42", "D43"]}]"#,
    &prompt,
  )
  .await?;

  let links = raw_links
    .into_iter()
    .map(|l| {
      let doc_topics: Vec<topic::Topic> = l
        .documentation_topics
        .iter()
        .filter(|d| d.starts_with('D'))
        .map(|d| topic::new_topic(d))
        .collect();

      core::SemanticLink {
        documentation_topics: if doc_topics.is_empty() {
          vec![fallback_doc_topic.clone()]
        } else {
          doc_topics
        },
        declaration_topic: topic::new_topic(&l.declaration_topic),
        semantic_text: l.semantic_text,
      }
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
Return a JSON array of objects, where each object has:\n\
- `text`: the condensed semantic description\n\
- `sources`: array of 1-based indices of ALL original entries that were \
merged into this entry (for provenance tracking)\n\n\
Example: `[{\"text\": \"event emitted when a user joins a campaign\", \
\"sources\": [1, 3, 5, 8]}, {\"text\": \"enables offchain tracking of \
participation details\", \"sources\": [2, 4]}]`\n\n\
Rules:\n\
- Preserve nuances BY MERGING them into fewer entries, not by keeping \
redundant entries. A single dense description that captures all facets is \
better than multiple overlapping ones.\n\
- When merging, include the most specific details from all sources \
(e.g., access control restrictions, specific parameters, business context).\n\
- Each output entry should be a concise, self-contained semantic description.\n\
- Every original entry index must appear in exactly one `sources` array.\n\
- Return ONLY a JSON array of objects, no other text, reasoning, or thought process.\n\n";

#[derive(Deserialize)]
struct LLMCondensedSemantic {
  text: String,
  sources: Vec<usize>,
}

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
) -> Result<Vec<CondensedSemantic>, String> {
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
    true,
  )
  .await?;

  let raw: Vec<LLMCondensedSemantic> = router::extract_json(
    &response,
    "condensed semantics",
    r#"[{"text": "concise semantic", "sources": [1, 2, 5]}]"#,
    &prompt,
  )
  .await?;

  Ok(
    raw
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
For each feature, provide:\n\
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
- If no features can be synthesized, return an empty array `[]`.\n\
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
    true,
  )
  .await?;

  let raw_features: Vec<LLMSynthesizedFeature> =
    router::extract_json(
      &response,
      "synthesized features",
      r#"[{"name": "...", "description": "...", "requirement_topics": ["R1"], "behavior_topics": ["B1"]}]"#,
      &prompt,
    ).await?;

  let mut features = BTreeMap::new();
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

    feature_requirement_links
      .insert(feature_topic.clone(), requirement_topics.clone());
    feature_behavior_links.insert(feature_topic.clone(), behavior_topics);

    topic_metadata.insert(
      feature_topic.clone(),
      core::TopicMetadata::FeatureTopic {
        topic: feature_topic.clone(),
        name: raw.name,
        description: raw.description,
        author_id: AUTHOR_AGENT_LARGE,
        created_at: String::new(),
        expanded_context: Vec::new(),
      },
    );

    features.insert(feature_topic, Feature);
  }

  Ok(SynthesizedFeatures {
    features,
    topic_metadata,
    feature_requirement_links,
    feature_behavior_links,
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
- Use functional semantics to give business-level meaning when available. \
Preserve the developer's specific terminology from the functional semantics — \
subtle differences in naming often reflect important design distinctions.\n\
- Include both normal execution paths and edge case behaviors (reverts, \
access control checks, state mutations).\n\
- Do not describe implementation details like \"calls _transfer internally\" — \
describe the observable effect: \"transfers tokens from sender to recipient.\"\n\
- If the contract has no functions or modifiers, return an empty array `[]`.\n\
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
    true,
  )
  .await?;

  let raw_groups: Vec<LLMMemberBehaviors> = router::extract_json(
    &response,
    "behaviors",
    r#"[{"member_topic": "N-1234", "behaviors": ["...", "..."]}]"#,
    &prompt,
  )
  .await?;

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
- **Code identifiers**: Wrap references to code identifiers — such as \
  function names, variable names, contract names, role constants, event \
  names, error names, and similar code artifacts — in backtick \
  inline code blocks. For example, `NUDGE_ADMIN_ROLE` not NUDGE_ADMIN_ROLE, \
  `transferFrom()` not transferFrom().\n\
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
        TaskSize::Small,
        router::SYSTEM_MESSAGE_DOCUMENTATION,
        &prompt,
        Some(&file_path),
        false,
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

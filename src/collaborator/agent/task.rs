//! Agent tasks for building an audit's organizational model from documentation.
//!
//! This module drives LLM-based extraction of features, requirements, threats,
//! and invariants from developer-provided project documentation. The output is
//! an organizational index that independent security auditors use to structure
//! their review of the codebase — it is not a replacement for the documentation
//! itself.
//!
//! # Design principles
//!
//! **Documentation is untrusted.** It represents the developer's *claimed*
//! behavior, not verified truth. The structures built here frame what an
//! auditor must verify, without anchoring them to the developer's stated
//! implementation.
//!
//! **Features are behavioral.** Feature names and descriptions are written at
//! a user-visible or protocol-level abstraction, regardless of whether the
//! source document is a high-level overview or a technical deep-dive. This
//! ensures that features extracted from different document types merge cleanly
//! during consolidation.
//!
//! **Requirements define verification scope.** Each requirement states *what*
//! an auditor must verify for a feature, broadly enough that the auditor is
//! not constrained to the developer's stated approach. Linked documentation
//! topics provide the developer's claimed design as context, but the
//! requirement itself frames the verification goal. This encourages auditors
//! to think critically and consider attack vectors beyond what the
//! documentation explicitly addresses.
//!
//! **Threats and invariants are adversarial.** The threat model is built by
//! thinking offensively about each feature — what could an attacker exploit?
//! Invariants are concrete, verifiable properties that must hold to prevent
//! each threat. Security notes (developer-provided roles, known threats, and
//! invariants) are incorporated but not taken at face value.
//!
//! # Pipeline
//!
//! 1. **Normalize** (`normalize_documentation`): Rewrite raw documentation
//!    files for plain text readability (strip emojis, inline HTML, navigation,
//!    badges, etc.) without altering content.
//! 2. **Extract** (`build_features_from_documentation`): Process each
//!    documentation file independently to extract behavioral features and
//!    verification-scoped requirements, then consolidate across files —
//!    merging duplicates and dissolving broad features into more specific ones.
//! 3. **Threaten** (`build_threats_for_feature`): For each feature, generate
//!    threats and invariants. Security notes are included in context so the
//!    LLM can incorporate roles and known attack vectors.
//! 4. **Review** (`review_security_coverage`): A completeness pass that checks
//!    whether all security notes content is accounted for in the generated
//!    threat model, filling any gaps.
//! 5. **Link** (`link_requirements_to_source`): For each (contract, feature)
//!    pair, determine which requirements apply to which contract members.
//!    One LLM call per pair (Option D) using the large model for quality.
//!    Members linked to a requirement implicitly link their parent contract.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::collaborator::agent::context;
use crate::collaborator::agent::router::{self, TaskSize};
use crate::collaborator::models::AUTHOR_AGENT;
use crate::core::{
  self, AST, AuditData, Feature, Invariant, Requirement, Threat,
  ThreatSeverity, topic,
};

/// Raw requirement as returned by the LLM (no topic ID yet).
#[derive(Deserialize)]
struct LLMRequirement {
  description: String,
  documentation_topics: Vec<String>,
}

/// Raw feature as returned by the LLM (no topic ID yet).
#[derive(Deserialize)]
struct LLMFeature {
  name: String,
  description: String,
  requirements: Vec<LLMRequirement>,
}

/// Raw threat as returned by the LLM for threat analysis.
/// Invariants are plain description strings; severity is inherited from the threat.
#[derive(Deserialize, serde::Serialize)]
struct LLMThreat {
  description: String,
  severity: String,
  invariants: Vec<String>,
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
      .map(|node| context::render_documentation_ast_snippet(node, audit_data))
      .collect();

    let file_json = serde_json::json!({
      "file": path.file_path,
      "content": rendered,
    });

    files.push(serde_json::to_string(&file_json).unwrap_or_default());
  }

  files
}

/// Prompt for extracting features from a single documentation file.
const BUILD_FEATURES_PROMPT: &str = "Below is a documentation file for a smart contract project, \
rendered as structured JSON with topic IDs (D-prefixed, like \"D42\") \
on each section, paragraph, list, and code block.\n\n\
Your task is to identify all distinct smart contract **features** \
(or **goals**) described in this document. A feature is \
a discrete capability or behavior of the system. Be specific with the \
features, avoiding overlap where one \"feature\" encompasses multiple \
distinct implementations.\n\n\
These features and requirements will be used by independent security \
auditors to organize their review of the codebase. The documentation is \
developer-provided and **not trusted** — it represents claimed behavior, \
not verified truth.\n\n\
Feature names and descriptions must be written at a **behavioral** \
abstraction level — describe *what* the system does, not *how* it is \
implemented. Even if the documentation is highly technical (e.g., \
describing specific contract functions, modifiers, or storage layouts), \
extract the user-visible or protocol-level behavior based on the design \
intent of the document, not the specific implementation details. For example, \
if the document describes a `pause()` modifier with an `onlyAdmin` check, the \
feature should be \"Admins can pause the protocol\", not \"pause() modifier \
with onlyAdmin guard\".\n\n\
Requirements define the **scope of what an auditor must verify** for each \
feature. State them broadly enough that the auditor is not anchored to a \
developer's stated implementation — the auditor should think critically \
and consider attack vectors beyond what the documentation explicitly \
addresses. For example, prefer \"withdrawals must be safe from reentrancy\" \
over \"balance must be zeroed before the external call\", because the \
latter assumes a specific implementation strategy and may cause the \
auditor to overlook other reentrancy vectors. The linked documentation \
topics provide the developer's claimed design as context, but the \
requirement itself should frame the verification goal, not repeat the \
documentation since it is untrusted and unverified.\n\n\
For each feature, provide:\n\
- `name`: a short, descriptive name\n\
- `description`: a summary of the feature\n\
- `requirements`: an array of requirement objects, where each object has:\n\
  - `description`: a behavioral requirement that the implementation must satisfy. \
Each requirement should be a single, specific, testable statement. \
Include both **happy-path** requirements (what the system should do) and \
**non-happy-path** requirements (what the system must prevent). If the \
documentation describes security threats, attack vectors, access control \
rules, or invariants, capture those as requirements too.\n\
  - `documentation_topics`: an array of D-prefixed topic ID strings \
(e.g., [\"D12\", \"D34\"]) for every documentation section, paragraph, \
list, or code block that informed this specific requirement. Each \
requirement must have at least one documentation topic.\n\n\
Rules:\n\
- Every documentation topic ID that describes system behavior, \
requirements, constraints, security concerns, or invariants should appear \
in at least one requirement. Exclude boilerplate \
like tables of contents, version history, author credits, and headings.\n\
- A documentation topic may appear in multiple requirements if it is \
relevant to more than one.\n\
- Do not invent topic IDs. Only use IDs present in the documentation.\n\
- When in doubt whether something is one feature or two, prefer \
splitting into more specific features.\n\
- Each feature should have at least one requirement.\n\
- Return ONLY a JSON array of feature objects, no other text.\n\n\
Documentation:\n";

/// Prompt for consolidating features extracted from multiple documents.
const CONSOLIDATE_FEATURES_PROMPT: &str = "Below are features and requirements extracted \
independently from multiple documentation files for a smart contract project. \
Because each file was processed separately, some features may overlap or \
describe the same capability.\n\n\
Your task is to consolidate these into a single, deduplicated list of features. \
For each group of similar features, merge them into one feature that \
combines all their requirements. Preserve every requirement and its \
documentation_topics — do not drop any.\n\n\
For each consolidated feature, provide:\n\
- `name`: a short, descriptive name\n\
- `description`: a summary of the feature\n\
- `requirements`: the combined array of requirement objects from all merged \
features. Each requirement object has:\n\
  - `description`: the requirement text\n\
  - `documentation_topics`: the D-prefixed topic ID strings\n\n\
Rules:\n\
- Merge features that describe the same capability or system behavior. Do not \
merge unrelated features just because they share a theme, keep each \
feature as distinct as possible, merging only duplicate features.\n\
- Do not drop any requirements.\n\
- Do not modify documentation_topics arrays, just combine them.\n\
- You may rewrite feature names and descriptions to better reflect the \
merged content.\n\
- If a broad feature encompasses multiple more specific features that \
were extracted separately, dissolve the broad feature and distribute its \
requirements into the matching specific features. For example, if one \
document produced a broad \"Staking and Rewards\" feature and another \
produced separate \"Token Staking\", \"Stake Withdrawal\", and \"Reward \
Distribution\" features, remove \"Staking and Rewards\" and assign each \
of its requirements to whichever specific feature it belongs to. If a \
requirement applies to multiple specific features, add it to each.\n\
- If a feature was created for a documented invariant, remove it as a feature and \
apply it as a requirement to each feature it applies to. For example, \
\"Unauthorized users should not be able to perform admin actions\" is an invariant, not a feature, so it should be applied as a requirement to the features that it applies to.\n\
- Return ONLY a JSON array of feature objects, no other text.\n\n\
Features to consolidate:\n";

/// Result of parsing LLM features: features and their requirements as separate maps.
pub struct ParsedFeatures {
  pub features: BTreeMap<topic::Topic, Feature>,
  pub requirements: BTreeMap<topic::Topic, Requirement>,
  pub topic_metadata: BTreeMap<topic::Topic, core::TopicMetadata>,
}

/// Parse the LLM response into features and requirements,
/// assigning sequential F-prefixed and R-prefixed topic IDs.
fn parse_features_response(response: &str) -> Result<ParsedFeatures, String> {
  // Strip markdown code fences if present
  let json_str = response
    .trim()
    .strip_prefix("```json")
    .or_else(|| response.trim().strip_prefix("```"))
    .unwrap_or(response.trim());
  let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

  let raw_features: Vec<LLMFeature> =
    serde_json::from_str(json_str).map_err(|e| {
      eprintln!(
        "Failed to parse features JSON: {}\nResponse:\n{}",
        e, json_str
      );
      format!("Failed to parse features JSON: {}", e)
    })?;

  let mut features = BTreeMap::new();
  let mut requirements = BTreeMap::new();
  let mut topic_metadata = BTreeMap::new();
  let mut req_counter = 0i32;

  for (i, raw) in raw_features.into_iter().enumerate() {
    let feature_topic = topic::new_feature_topic((i + 1) as i32);

    let mut requirement_topics = Vec::new();
    for raw_req in raw.requirements {
      req_counter += 1;
      let req_topic = topic::new_requirement_topic(req_counter);
      requirement_topics.push(req_topic.clone());
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
          feature_topic: feature_topic.clone(),
          author_id: AUTHOR_AGENT,
          created_at: String::new(),
        },
      );
      requirements.insert(
        req_topic,
        Requirement {
          documentation_topics: doc_topics,
          source_topics: Vec::new(),
        },
      );
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

    features.insert(
      feature_topic,
      Feature {
        requirement_topics,
        threat_topics: Vec::new(),
      },
    );
  }

  Ok(ParsedFeatures {
    features,
    requirements,
    topic_metadata,
  })
}

/// Extract project features and requirements from documentation files via LLM.
///
/// Each documentation file is processed independently in parallel, then a
/// consolidation pass merges similar features. If there is only one file,
/// the consolidation pass is skipped.
///
/// The caller renders documentation files while holding the lock, then passes
/// the JSON strings to this function after releasing it.
pub async fn build_features_from_documentation(
  documentation_files: &[String],
) -> Result<ParsedFeatures, String> {
  if documentation_files.is_empty() {
    return Err("No documentation found in audit".to_string());
  }

  // Single document: extract and return directly
  if documentation_files.len() == 1 {
    let prompt =
      format!("{}{}", BUILD_FEATURES_PROMPT, &documentation_files[0]);
    let response = router::chat_completion(
      TaskSize::Large,
      router::SYSTEM_MESSAGE_DOCUMENTATION,
      &prompt,
      None,
    )
    .await?;
    return parse_features_response(&response);
  }

  // Multiple documents: extract in parallel, then consolidate
  let mut handles = Vec::new();
  for (i, doc_json) in documentation_files.iter().enumerate() {
    let prompt = format!("{}{}", BUILD_FEATURES_PROMPT, doc_json);
    let label = format!("features_{}", i);
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

  // Collect per-document results
  let mut per_doc_features: Vec<String> = Vec::new();
  for (i, handle) in handles.into_iter().enumerate() {
    match handle.await {
      Ok(Ok(response)) => per_doc_features.push(response),
      Ok(Err(e)) => {
        eprintln!("build_features failed for document {}: {}", i, e);
      }
      Err(e) => {
        eprintln!("build_features task panicked for document {}: {}", i, e);
      }
    }
  }

  if per_doc_features.is_empty() {
    return Err("All document feature extractions failed".to_string());
  }

  // If only one document succeeded, skip consolidation
  if per_doc_features.len() == 1 {
    return parse_features_response(&per_doc_features[0]);
  }

  // Consolidation pass: merge similar features across documents
  let combined = per_doc_features.join("\n");
  let prompt = format!("{}{}", CONSOLIDATE_FEATURES_PROMPT, combined);
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some("features_consolidate"),
  )
  .await?;

  parse_features_response(&response)
}

/// Build the prompt for threat and invariant extraction for a single feature.
const BUILD_THREATS_PROMPT: &str = "Below is a smart contract feature and its \
requirements, extracted from project documentation.\n\n\
Your task is to think adversarially about this feature and produce a list \
of **threats** (attack vectors that an attacker could use to compromise \
or abuse the feature). For each threat, produce a list of at least one **invariant** — \
properties that must always hold in the implementation to prevent that \
specific attack.\n\n\
For each threat, provide:\n\
- `description`: a concise description of the attack vector — how an attacker \
could exploit the feature (e.g., \"An attacker re-enters the withdraw function \
before the balance is updated to drain the contract\")\n\
- `severity`: one of \"medium\", \"high\", or \"critical\"\n\
- `invariants`: an array of strings, where each string is a specific, testable \
property that must hold to prevent this attack (e.g., \"The contract's token \
- Consider all common smart contract attack classes: reentrancy, replay, access control \
bypass, front-running, oracle manipulation, flash loan attacks, price manipulation, \
shared resource consumption, griefing, denial of service, storage \
collision, and privilege escalation.\n\
- Each threat should be specific to the feature, not generic.\n\
- Each threat must have at least one invariant.\n\
- Each invariant should be a concrete, verifiable property — not a vague \
statement like \"the system should be secure\".\n\
- The feature should have at least one threat\n\
- If a requirement for a feature is describing an invariant, make sure it gets \
added as an actual invariant to a threat on the feature.\n\
- Return ONLY a JSON array of threat objects, no other text.\n\n";

/// Appended to the threat prompt when a security notes document is present.
const SECURITY_NOTES_PREAMBLE: &str = "\
The project developers have provided a **Security Notes** document \
containing roles, known threats, invariants, and other security \
considerations. You MUST incorporate this information:\n\n\
- If the document defines **roles** (named actors in the access control \
model), those are the ONLY roles that exist. When writing access-control \
invariants, reference only these roles by name. The enforcement of these \
roles (e.g., \"only the `guardian` role can pause the protocol\") is a \
valid and encouraged invariant. Do not invent roles that are not listed, \
and do not create access control invariants that are not based on these roles.\n\
- If the document describes **threats or attack vectors** that are \
relevant to this feature, make sure they appear in your output as \
threats (with matching invariants).\n\
- If the document describes **invariants or security properties** that \
are relevant to this feature, make sure they appear as invariants under \
an appropriate threat.\n\n\
Security Notes:\n";

/// Result of parsing LLM threats: threats, invariants, and topic metadata.
pub struct ParsedThreats {
  pub threats: BTreeMap<topic::Topic, Threat>,
  pub invariants: BTreeMap<topic::Topic, Invariant>,
  pub topic_metadata: BTreeMap<topic::Topic, core::TopicMetadata>,
  /// Maps feature topic -> new threat topics to append
  pub feature_threat_topics: BTreeMap<topic::Topic, Vec<topic::Topic>>,
}

/// Parse the LLM response for a single feature into threats and invariants,
/// assigning sequential T-prefixed and I-prefixed topic IDs.
fn parse_threats_response(
  response: &str,
  feature_topic: &topic::Topic,
  threat_start: i32,
  invariant_start: i32,
) -> Result<ParsedThreats, String> {
  // Strip markdown code fences if present
  let json_str = response
    .trim()
    .strip_prefix("```json")
    .or_else(|| response.trim().strip_prefix("```"))
    .unwrap_or(response.trim());
  let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

  let raw_threats: Vec<LLMThreat> =
    serde_json::from_str(json_str).map_err(|e| {
      eprintln!(
        "Failed to parse threats JSON: {}\nResponse:\n{}",
        e, json_str
      );
      format!("Failed to parse threats JSON: {}", e)
    })?;

  let mut threats = BTreeMap::new();
  let mut invariants = BTreeMap::new();
  let mut topic_metadata = BTreeMap::new();
  let mut threat_topics = Vec::new();
  let mut threat_counter = threat_start;
  let mut inv_counter = invariant_start;

  for raw in raw_threats {
    threat_counter += 1;
    let threat_topic = topic::new_attack_vector_topic(threat_counter);
    let threat_severity =
      ThreatSeverity::from_str(&raw.severity).unwrap_or(ThreatSeverity::Medium);

    let mut invariant_topics = Vec::new();
    for inv_description in raw.invariants {
      inv_counter += 1;
      let inv_topic = topic::new_invariant_topic(inv_counter);

      topic_metadata.insert(
        inv_topic.clone(),
        core::TopicMetadata::InvariantTopic {
          topic: inv_topic.clone(),
          description: inv_description,
          threat_topic: threat_topic.clone(),
          author_id: AUTHOR_AGENT,
          created_at: String::new(),
          severity: threat_severity,
        },
      );
      invariants.insert(
        inv_topic.clone(),
        Invariant {
          source_topics: Vec::new(),
        },
      );
      invariant_topics.push(inv_topic);
    }

    topic_metadata.insert(
      threat_topic.clone(),
      core::TopicMetadata::ThreatTopic {
        topic: threat_topic.clone(),
        description: raw.description,
        feature_topic: feature_topic.clone(),
        author_id: AUTHOR_AGENT,
        created_at: String::new(),
        severity: threat_severity,
      },
    );
    threats.insert(threat_topic.clone(), Threat { invariant_topics });
    threat_topics.push(threat_topic);
  }

  let mut feature_threat_topics = BTreeMap::new();
  if !threat_topics.is_empty() {
    feature_threat_topics.insert(feature_topic.clone(), threat_topics);
  }

  Ok(ParsedThreats {
    threats,
    invariants,
    topic_metadata,
    feature_threat_topics,
  })
}

/// Build threats and invariants for a single feature via LLM.
///
/// The caller renders the feature JSON while holding the lock, then passes
/// it to this function after releasing it. `threat_start` and `invariant_start`
/// are the current max IDs so new topics get unique sequential IDs.
///
/// When `security_notes` is provided, the security document is injected into
/// the prompt so the LLM can incorporate roles, known threats, and invariants.
pub async fn build_threats_for_feature(
  feature_topic: &topic::Topic,
  feature_json: &str,
  threat_start: i32,
  invariant_start: i32,
  security_notes: Option<&str>,
) -> Result<ParsedThreats, String> {
  let prompt = match security_notes {
    Some(notes) => format!(
      "{}{}{}\n\nFeature:\n{}",
      BUILD_THREATS_PROMPT, SECURITY_NOTES_PREAMBLE, notes, feature_json
    ),
    None => format!("{}Feature:\n{}", BUILD_THREATS_PROMPT, feature_json),
  };
  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some(feature_topic.id()),
  )
  .await?;

  parse_threats_response(
    &response,
    feature_topic,
    threat_start,
    invariant_start,
  )
}

/// Prompt for the security coverage review pass.
const REVIEW_SECURITY_COVERAGE_PROMPT: &str = "\
You are reviewing a smart contract audit's threat model for completeness \
against a **Security Notes** document provided by the project developers.\n\n\
Below you will find:\n\
1. The Security Notes document (roles, known threats, invariants, and \
   other security considerations written by the developers).\n\
2. All features that were extracted from the project documentation, each \
   with their generated threats and invariants.\n\n\
Your task is to identify any information in the Security Notes that is NOT \
yet accounted for in the generated threats and invariants. For each gap, \
produce additional threats and invariants under the most appropriate \
feature.\n\n\
Output rules:\n\
- Return a JSON array of objects, one per feature that needs additions. \
  Each object has:\n\
  - `feature_topic`: the topic ID string of the feature (e.g., \"F1\")\n\
  - `threats`: an array of threat objects (same format as the threat \
    builder: `description`, `severity`, `invariants`)\n\
- If all Security Notes content is already covered, return an empty array `[]`.\n\
- Do NOT repeat threats or invariants that already exist.\n\
- If the Security Notes define **roles**, ensure that access-control \
  invariants referencing those roles exist under appropriate threats. \
  Only use the roles defined in the Security Notes.\n\
- Return ONLY the JSON array, no other text.\n\n";

/// A single feature's additional threats from the review pass.
#[derive(Deserialize)]
struct LLMReviewEntry {
  feature_topic: String,
  threats: Vec<LLMThreat>,
}

/// Review the security notes against all generated features/threats/invariants
/// and return additional threats to fill any gaps.
pub async fn review_security_coverage(
  security_notes: &str,
  features_json: &str,
  threat_start: i32,
  invariant_start: i32,
) -> Result<Vec<ParsedThreats>, String> {
  let prompt = format!(
    "{}Security Notes:\n{}\n\nFeatures with existing threats:\n{}",
    REVIEW_SECURITY_COVERAGE_PROMPT, security_notes, features_json
  );

  let response = router::chat_completion(
    TaskSize::Large,
    router::SYSTEM_MESSAGE_DOCUMENTATION,
    &prompt,
    Some("security_review"),
  )
  .await?;

  // Strip markdown code fences if present
  let json_str = response
    .trim()
    .strip_prefix("```json")
    .or_else(|| response.trim().strip_prefix("```"))
    .unwrap_or(response.trim());
  let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

  let review_entries: Vec<LLMReviewEntry> = serde_json::from_str(json_str)
    .map_err(|e| {
      eprintln!(
        "Failed to parse security review JSON: {}\nResponse:\n{}",
        e, json_str
      );
      format!("Failed to parse security review JSON: {}", e)
    })?;

  // Convert each entry into ParsedThreats using the same parser
  let mut results = Vec::new();
  let mut t_offset = threat_start;
  let mut i_offset = invariant_start;

  for entry in review_entries {
    if entry.threats.is_empty() {
      continue;
    }
    let feature_topic = topic::new_topic(&entry.feature_topic);

    // Build a synthetic JSON response to reuse parse_threats_response
    let synthetic = serde_json::to_string(&entry.threats)
      .map_err(|e| format!("Failed to re-serialize review threats: {}", e))?;

    let parsed =
      parse_threats_response(&synthetic, &feature_topic, t_offset, i_offset)?;

    // Advance offsets past the IDs we just used
    t_offset += parsed.threats.len() as i32;
    i_offset += parsed.invariants.len() as i32;

    results.push(parsed);
  }

  Ok(results)
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

/// Prompt for linking requirements to contract members.
const LINK_REQUIREMENTS_PROMPT: &str = "Below is a smart contract and a feature with its \
requirements, extracted from project documentation.\n\n\
The contract is rendered as a JSON object with an N-prefixed topic ID on the \
contract itself and on each of its members (functions, state variables, \
modifiers, events, structs, enums, errors). Member bodies are omitted — \
only signatures are shown.\n\n\
The feature has R-prefixed requirement topic IDs on each requirement.\n\n\
Your task is to determine which of the feature's requirements are relevant \
to this contract's members. A requirement is relevant to a member if the \
member's implementation would need to satisfy or could violate that \
requirement.\n\n\
For each relevant requirement, return the requirement's R-prefixed topic ID \
and an array of N-prefixed member topic IDs that the requirement applies to. \
A requirement may apply to multiple members, and a member may be relevant to \
multiple requirements.\n\n\
Rules:\n\
- Only link requirements to members where there is a clear, direct \
  relationship. Do not force links.\n\
- If none of the feature's requirements apply to this contract, return an \
  empty array `[]`.\n\
- Omit requirements that do not apply to any member of this contract.\n\
- Return ONLY a JSON array of objects, each with:\n\
  - `requirement_topic`: the R-prefixed topic ID string\n\
  - `source_topics`: an array of N-prefixed member topic ID strings\n\
- No other text.\n\n";

/// A pre-rendered (contract JSON, feature JSON) pair for the linking task.
pub struct ContractFeaturePair {
  pub contract_topic: topic::Topic,
  pub contract_json: String,
  pub feature_json: String,
  /// Label for dry-run output and logging.
  pub label: String,
}

/// Collect all (contract, feature) pairs from audit data, pre-rendered as JSON.
pub fn collect_contract_feature_pairs(
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) -> Vec<ContractFeaturePair> {
  use crate::solidity::parser::ASTNode;

  // Collect contract AST nodes
  let mut contracts: Vec<(&ASTNode, String)> = Vec::new();
  for (_path, ast) in &audit_data.asts {
    let sol_ast = match ast {
      AST::Solidity(sol_ast) => sol_ast,
      _ => continue,
    };
    for node in &sol_ast.nodes {
      if let ASTNode::ContractDefinition { .. } = node {
        let contract_json = match context::render_contract_members_for_linking(
          node,
          audit_data,
          source_text_cache,
        ) {
          Some(json) => json,
          None => continue,
        };
        contracts.push((node, contract_json));
      }
    }
  }

  // Collect features
  let feature_json =
      match context::render_feature_to_json(ft, feature, audit_data, None) {
        Some(json) => json,
        None => continue,
      };

    for (node, contract_json) in &contracts {
      let contract_topic = topic::new_node_topic(&node.node_id());
      let label = format!("{}_{}", contract_topic.id(), ft.id());
      pairs.push(ContractFeaturePair {
        contract_topic,
        contract_json: contract_json.clone(),
        feature_json: feature_json.clone(),
        label,
      });
    }
  }

  pairs
}

/// Collect contract×feature pairs for a single feature, optionally filtered to
/// a single requirement. Used by reactive triggers after user creates a feature
/// or requirement.
pub fn collect_single_feature_pairs(
  feature_topic: &topic::Topic,
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
  requirement_filter: Option<&topic::Topic>,
) -> Vec<ContractFeaturePair> {
  use crate::solidity::parser::ASTNode;

  let feature = match audit_data.features.get(feature_topic) {
    Some(f) => f,
    None => return Vec::new(),
  };

  let feature_json = match context::render_feature_to_json(
    feature_topic,
    feature,
    audit_data,
    requirement_filter,
  ) {
    Some(json) => json,
    None => return Vec::new(),
  };

  let mut pairs = Vec::new();
  for (_path, ast) in &audit_data.asts {
    let sol_ast = match ast {
      AST::Solidity(sol_ast) => sol_ast,
      _ => continue,
    };
    for node in &sol_ast.nodes {
      if let ASTNode::ContractDefinition { .. } = node {
        let contract_json = match context::render_contract_members_for_linking(
          node,
          audit_data,
          source_text_cache,
        ) {
          Some(json) => json,
          None => continue,
          let label = format!("{}_{}", contract_topic.id(), feature_topic.id());
        pairs.push(ContractFeaturePair {
          contract_topic,
          contract_json,
          feature_json: feature_json.clone(),
          label,
        });
      }
    }
  }

  pairs
}

/// A single requirement-to-source link returned by the LLM.
#[derive(Deserialize)]
struct LLMRequirementLink {
  requirement_topic: String,
  source_topics: Vec<String>,
}

/// Result of linking requirements to source code: maps requirement topics
/// to their linked source (member) topics.
pub struct ParsedRequirementLinks {
  pub links: BTreeMap<topic::Topic, Vec<topic::Topic>>,
}

/// Link requirements to contract members via LLM (Option D: one call per
/// contract×feature pair, large model).
///
/// The caller collects pairs while holding the lock, then passes them here
/// after releasing it. Returns a merged map of requirement topics to source
/// member topics across all contracts and features.
pub async fn link_requirements_to_source(
  pairs: &[ContractFeaturePair],
) -> Result<ParsedRequirementLinks, String> {
  if pairs.is_empty() {
    return Err("No contract-feature pairs to process".to_string());
  }

  let mut handles = Vec::new();
  for pair in pairs {
    let prompt = format!(
      "{}Contract:\n{}\n\nFeature:\n{}",
      LINK_REQUIREMENTS_PROMPT, pair.contract_json, pair.feature_json
    );
    let label = pair.label.clone();
    handles.push(tokio::spawn(async move {
      let result = router::chat_completion(
        TaskSize::Large,
        router::SYSTEM_MESSAGE_CODE,
        &prompt,
        Some(&label),
      )
      .await;
      (label, result)
    }));
  }

  let mut links: BTreeMap<topic::Topic, Vec<topic::Topic>> = BTreeMap::new();
  Ok((_, Ok(response))) => match parse_requirement_links(&response) {
        Ok(parsed) => {
          for (req_topic, source_topics) in parsed {
            links.entry(req_topic).or_default().extend(source_topics);
          }
        }
        Err(e) => eprintln!("parse_requirement_links failed: {}", e),
      },
      Ok((label, Err(e))) => {
        eprintln!("link_requirements_to_source failed for {}: {}", label, e);
      }
      Err(e) => {
        eprintln!("link_requirements_to_source task panicked: {}", e);
      }
    }
  }

  // Deduplicate source topics per requirement
  for source_topics in links.values_mut() {
    source_topics.sort();
    source_topics.dedup();
  }

  if links.is_empty() {
    return Err("No requirement-to-source links found".to_string());
  }

  Ok(ParsedRequirementLinks { links })
}

/// Parse the LLM response for requirement-to-source links.
fn parse_requirement_links(
  response: &str,
) -> Result<Vec<(topic::Topic, Vec<topic::Topic>)>, String> {
  let json_str = response
    .trim()
    .strip_prefix("```json")
    .or_else(|| response.trim().strip_prefix("```"))
    .unwrap_or(response.trim());
    let raw_links: Vec<LLMRequirementLink> = serde_json::from_str(json_str)
    .map_err(|e| {
      eprintln!(
        "Failed to parse requirement links JSON: {}\nResponse:\n{}",
        e, json_str
      );
      format!("Failed to parse requirement links JSON: {}", e)
    })?;

  let mut result = Vec::new();
  for link in raw_links {
    let req_topic = topic::new_topic(&link.requirement_topic);
    let source_topics: Vec<topic::Topic> = link
      .source_topics
      .into_iter()
      .map(|id| topic::new_topic(&id))
      .collect();
    if !source_topics.is_empty() {
      result.push((req_topic, source_topics));
    }
  }

  Ok(result)
}

/// Generate documentation for all members of a specific contract.
pub fn document_contract_members(
  contract_topic_id: &str,
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) {
  todo!()
}

/// Generate documentation for a specific topic.
pub fn document_topic(
  topic_id: &str,
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) {
  todo!()
}

/// Answer a user question in the context of a specific topic.
pub fn answer_question(
  topic_id: &str,
  question: &str,
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) {
  todo!()
}

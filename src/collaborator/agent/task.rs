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
balance must be updated before any external call is made during withdrawal\")\n\n\
Rules:\n\
- Consider all common smart contract attack classes: reentrancy, access control \
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

/// Generate documentation for all top-level contracts in the audit.
pub fn document_contracts(
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) {
  todo!()
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

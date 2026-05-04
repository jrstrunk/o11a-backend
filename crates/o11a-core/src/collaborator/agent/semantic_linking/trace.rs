//! Mechanical name-resolution trace mode (`--semantic-linking-mechanical-trace`).
//!
//! Runs only Pass 1 (`mechanical_semantic_links` → contract anchors) and
//! Pass 2 (`mechanical_section_to_members` → member candidates) against the
//! parsed audit. Writes a single pretty-printed JSON file (an array of
//! per-section records) describing every section's:
//!
//!   - Rendered text and title
//!   - Every inline-code reference found (resolved or not) — so a reviewer
//!     can spot identifiers the doc parser failed to resolve
//!   - Distinct resolved declarations with their kind and scope summary
//!   - Contract anchors derived from those declarations
//!   - Per-anchored-contract member candidates
//!
//! No LLM calls are made. The intent is to validate that the deterministic
//! name resolver is correctly catching the code identifiers it should
//! before that data is used as the floor of the comparison benchmark.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::collaborator::agent::context;
use crate::collaborator::agent::task;
use crate::domain::{AuditData, DataContext, Scope, TopicMetadata, topic};

/// One row in `mechanical-trace.jsonl`: everything the mechanical name
/// resolver produced for one section, plus the raw inline-code references
/// it had to work from.
#[derive(Debug, Serialize)]
struct TraceRecord {
  section_topic: String,
  section_path: String,
  section_title: String,
  section_text: String,
  /// Every CodeIdentifier in the section's documentation tree, in
  /// document order. `resolved=false` means the parser saw the identifier
  /// but couldn't link it to a declaration — usually a name resolution
  /// failure worth investigating.
  code_references: Vec<CodeReferenceRecord>,
  resolved_count: usize,
  unresolved_count: usize,
  /// Distinct topics resolved by the parser (deduplicated from
  /// `code_references` by `referenced_topic`). Carries kind/scope
  /// metadata not present on individual occurrences.
  resolved_declarations: Vec<DeclarationRecord>,
  /// Distinct unresolved identifiers (deduplicated by literal `text`,
  /// each carrying its occurrence count). These are the names the parser
  /// saw as code identifiers but couldn't link to a declaration — the
  /// most diagnostic signal for name-resolution gaps. Sorted by count
  /// descending so the most frequent misses come first.
  unresolved_declarations: Vec<UnresolvedReferenceRecord>,
  /// Contracts derived by walking each resolved declaration up to its
  /// containing contract — i.e. what `mechanical_semantic_links`
  /// produces for this section.
  contract_anchors: Vec<TopicNameRecord>,
  /// Per-anchored-contract: members produced by
  /// `mechanical_section_to_members` from this section's declarations.
  members_by_contract: Vec<ContractMembersRecord>,
}

#[derive(Debug, Serialize)]
struct CodeReferenceRecord {
  text: String,
  resolved: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  topic: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  name: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  kind: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeclarationRecord {
  topic: String,
  name: String,
  kind: String,
  /// Human-readable scope (e.g. `"in NudgeCampaign::initialize"` or
  /// `"file: src/foo.sol"`). For quick diagnostic skim.
  scope_summary: String,
}

#[derive(Debug, Serialize)]
struct UnresolvedReferenceRecord {
  text: String,
  /// Number of times this exact identifier text appears unresolved in
  /// the section.
  count: usize,
}

#[derive(Debug, Serialize)]
struct TopicNameRecord {
  topic: String,
  name: String,
}

#[derive(Debug, Serialize)]
struct ContractMembersRecord {
  contract_topic: String,
  contract_name: String,
  members: Vec<DeclarationRecord>,
}

/// Run the mechanical-only trace and write `mechanical-trace.jsonl` to
/// `output_dir`. Returns the path written.
pub fn run_mechanical_trace(
  data_context: Arc<Mutex<DataContext>>,
  audit_id: &str,
  output_dir: &Path,
) -> std::io::Result<std::path::PathBuf> {
  let ctx = data_context.lock().map_err(|e| {
    std::io::Error::other(format!("data_context poisoned: {}", e))
  })?;
  let audit_data = match ctx.get_audit(audit_id) {
    Some(a) => a,
    None => {
      return Err(std::io::Error::other(format!(
        "audit '{}' not found",
        audit_id
      )));
    }
  };

  let sections = task::collect_documentation_sections(audit_data);
  let mechanical = context::mechanical_semantic_links(audit_data);

  let mut records: Vec<TraceRecord> = Vec::with_capacity(sections.len());
  for section_topic in &sections {
    let section_text = context::render_section_text(section_topic, audit_data)
      .unwrap_or_default();
    let section_path = section_path_for(section_topic, audit_data);
    let section_title = audit_data
      .topic_metadata
      .get(section_topic)
      .and_then(|m| m.name())
      .unwrap_or_default()
      .to_string();

    let code_refs =
      context::enumerate_section_code_references(section_topic, audit_data);
    let resolved_count = code_refs
      .iter()
      .filter(|r| r.resolved_topic.is_some())
      .count();
    let unresolved_count = code_refs.len() - resolved_count;

    let code_references: Vec<CodeReferenceRecord> = code_refs
      .iter()
      .map(|r| CodeReferenceRecord {
        text: r.text.clone(),
        resolved: r.resolved_topic.is_some(),
        topic: r.resolved_topic.map(|t| t.id().to_string()),
        name: r.resolved_name.clone(),
        kind: r.resolved_kind.as_ref().map(|k| format!("{:?}", k)),
      })
      .collect();

    // Distinct resolved declarations (dedup, preserve first-seen order).
    let mut seen: std::collections::HashSet<topic::Topic> =
      std::collections::HashSet::new();
    let mut resolved_declarations: Vec<DeclarationRecord> = Vec::new();
    for r in &code_refs {
      if let Some(t) = r.resolved_topic
        && seen.insert(t)
      {
        resolved_declarations.push(declaration_record(&t, audit_data));
      }
    }

    // Distinct unresolved identifiers (dedup by literal text, count
    // occurrences). Sorted by count descending so the most frequent
    // misses surface first.
    let mut unresolved_counts: std::collections::HashMap<String, usize> =
      std::collections::HashMap::new();
    for r in &code_refs {
      if r.resolved_topic.is_none() {
        *unresolved_counts.entry(r.text.clone()).or_insert(0) += 1;
      }
    }
    let mut unresolved_declarations: Vec<UnresolvedReferenceRecord> =
      unresolved_counts
        .into_iter()
        .map(|(text, count)| UnresolvedReferenceRecord { text, count })
        .collect();
    unresolved_declarations
      .sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.text.cmp(&b.text)));

    // Contract anchors from mechanical_semantic_links.
    let anchored_contracts = mechanical
      .section_to_contracts
      .get(section_topic)
      .cloned()
      .unwrap_or_default();
    let contract_anchors: Vec<TopicNameRecord> = anchored_contracts
      .iter()
      .map(|ct| TopicNameRecord {
        topic: ct.id().to_string(),
        name: audit_data
          .topic_metadata
          .get(ct)
          .and_then(|m| m.name())
          .unwrap_or_default()
          .to_string(),
      })
      .collect();

    // Members per anchored contract.
    let section_decls = mechanical
      .section_to_declarations
      .get(section_topic)
      .cloned()
      .unwrap_or_default();
    let mut members_by_contract: Vec<ContractMembersRecord> = Vec::new();
    for ct in &anchored_contracts {
      let members =
        context::mechanical_section_to_members(&section_decls, ct, audit_data);
      let contract_name = audit_data
        .topic_metadata
        .get(ct)
        .and_then(|m| m.name())
        .unwrap_or_default()
        .to_string();
      members_by_contract.push(ContractMembersRecord {
        contract_topic: ct.id().to_string(),
        contract_name,
        members: members
          .iter()
          .map(|m| declaration_record(m, audit_data))
          .collect(),
      });
    }

    records.push(TraceRecord {
      section_topic: section_topic.id().to_string(),
      section_path,
      section_title,
      section_text,
      code_references,
      resolved_count,
      unresolved_count,
      resolved_declarations,
      unresolved_declarations,
      contract_anchors,
      members_by_contract,
    });
  }

  // Deterministic order.
  records.sort_by(|a, b| a.section_topic.cmp(&b.section_topic));

  // Single pretty-printed JSON array (not JSONL) so standard JSON
  // formatters / IDE folding work on the output. Atomic via tmp+rename.
  let path = output_dir.join("mechanical-trace.json");
  let tmp = path.with_extension("json.tmp");
  let json = serde_json::to_string_pretty(&records).map_err(|e| {
    std::io::Error::other(format!("serializing mechanical trace: {}", e))
  })?;
  {
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(json.as_bytes())?;
    f.write_all(b"\n")?;
  }
  std::fs::rename(&tmp, &path)?;

  // Summary log for the operator.
  let total_refs: usize = records.iter().map(|r| r.code_references.len()).sum();
  let total_resolved: usize = records.iter().map(|r| r.resolved_count).sum();
  let total_unresolved: usize =
    records.iter().map(|r| r.unresolved_count).sum();
  let total_anchors: usize =
    records.iter().map(|r| r.contract_anchors.len()).sum();
  let sections_with_unresolved =
    records.iter().filter(|r| r.unresolved_count > 0).count();
  tracing::info!(
    "mechanical trace: {} sections, {} code refs ({} resolved, {} unresolved across {} sections), {} contract anchors -> {}",
    records.len(),
    total_refs,
    total_resolved,
    total_unresolved,
    sections_with_unresolved,
    total_anchors,
    path.display(),
  );

  Ok(path)
}

fn declaration_record(
  t: &topic::Topic,
  audit_data: &AuditData,
) -> DeclarationRecord {
  let meta = audit_data.topic_metadata.get(t);
  let (name, kind) = match meta {
    Some(TopicMetadata::NamedTopic { name, kind, .. }) => {
      (name.clone(), format!("{:?}", kind))
    }
    Some(other) => (
      other.name().unwrap_or_default().to_string(),
      "non-named".to_string(),
    ),
    None => (String::new(), "unknown".to_string()),
  };
  let scope_summary = meta
    .map(|m| scope_summary_for(m, audit_data))
    .unwrap_or_default();
  DeclarationRecord {
    topic: t.id().to_string(),
    name,
    kind,
    scope_summary,
  }
}

fn scope_summary_for(
  metadata: &TopicMetadata,
  audit_data: &AuditData,
) -> String {
  let name_of = |t: &topic::Topic| -> String {
    audit_data
      .topic_metadata
      .get(t)
      .and_then(|m| m.name())
      .unwrap_or("?")
      .to_string()
  };
  match metadata.scope() {
    Scope::Container { container } => {
      format!("file: {}", container.file_path)
    }
    Scope::Component {
      container,
      component,
    } => {
      format!("in {} ({})", name_of(component), container.file_path)
    }
    Scope::Member {
      member, component, ..
    } => format!("in {}::{}", name_of(component), name_of(member)),
    Scope::ContainingBlock {
      member, component, ..
    } => {
      format!("inside {}::{}", name_of(component), name_of(member))
    }
    Scope::Global => "global".to_string(),
  }
}

fn section_path_for(
  section_topic: &topic::Topic,
  audit_data: &AuditData,
) -> String {
  audit_data
    .topic_metadata
    .get(section_topic)
    .and_then(|m| match m.scope() {
      Scope::Container { container }
      | Scope::Component { container, .. }
      | Scope::Member { container, .. }
      | Scope::ContainingBlock { container, .. } => {
        Some(container.file_path.clone())
      }
      _ => None,
    })
    .unwrap_or_default()
}

//! Diagnostic dumps of parsed audit-data internals.
//!
//! Exposes a small set of "kind" values that the operator can request via
//! the `dump` CLI subcommand. Each kind serializes a focused slice of
//! [`AuditData`] to a pretty-printed JSON file (one root array or object,
//! valid for any standard JSON formatter / IDE folding) so operators can
//! manually inspect parsed state and spot edge cases without running the
//! full pipeline or hunting through the binary artifact.
//!
//! Adding a new dump kind:
//!   1. Add a variant to [`DumpKind`].
//!   2. Add an arm to [`DumpKind::parse`] (accept both kebab and snake
//!      case for the user input — both forms feel natural on a CLI).
//!   3. Add an arm to [`DumpKind::file_name`].
//!   4. Add an arm to [`dump_to_file`] that writes the JSON.
//! Everything else is mechanical.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::domain::{
  AuditData, NamedTopicKind, Scope, TopicMetadata, topic,
};

/// One kind of audit-data dump. The set is small on purpose — each variant
/// represents a curated diagnostic view, not a raw struct dump.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum DumpKind {
  /// Maps every interface-stub topic to its implementation topic via
  /// `transitive_topic`. Useful for spotting interface methods that
  /// should map to an implementation but don't.
  InterfaceMapping,
  /// Every simple identifier name in the audit, the full set of topic
  /// candidates that share it, and whether the resolver was able to
  /// disambiguate to a single topic. Useful for spotting names that fail
  /// to resolve due to ambiguity.
  NameIndex,
}

impl DumpKind {
  /// Parse a CLI argument. Accepts kebab-case (`interface-mapping`),
  /// snake_case (`interface_mapping`), and the special value `all` (which
  /// is handled by [`parse_kinds`] rather than producing a single kind).
  pub fn parse(s: &str) -> Result<Self, String> {
    let normalized = s.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
      "interface-mapping" => Ok(DumpKind::InterfaceMapping),
      "name-index" => Ok(DumpKind::NameIndex),
      other => Err(format!(
        "unknown dump kind '{}' (expected one of: interface-mapping, name-index, all)",
        other
      )),
    }
  }

  pub fn file_name(&self) -> &'static str {
    match self {
      DumpKind::InterfaceMapping => "interface-mapping.json",
      DumpKind::NameIndex => "name-index.json",
    }
  }

  pub fn all() -> Vec<DumpKind> {
    vec![DumpKind::InterfaceMapping, DumpKind::NameIndex]
  }
}

/// Parse a list of CLI dump-kind arguments. `"all"` (anywhere in the list)
/// expands to every kind. Duplicate kinds are deduped while preserving
/// order. Unknown kinds produce an error.
pub fn parse_kinds(args: &[String]) -> Result<Vec<DumpKind>, String> {
  let mut out: Vec<DumpKind> = Vec::new();
  let mut seen = std::collections::HashSet::new();
  for raw in args {
    for piece in raw.split(',') {
      let piece = piece.trim();
      if piece.is_empty() {
        continue;
      }
      if piece.eq_ignore_ascii_case("all") {
        for k in DumpKind::all() {
          if seen.insert(k) {
            out.push(k);
          }
        }
        continue;
      }
      let k = DumpKind::parse(piece)?;
      if seen.insert(k) {
        out.push(k);
      }
    }
  }
  Ok(out)
}

/// Run the dump for `kind` against `audit_data` and write the result as a
/// pretty-printed JSON file to `<output_dir>/<file_name>`. Returns the
/// final file path.
pub fn dump_to_file(
  kind: DumpKind,
  audit_data: &AuditData,
  output_dir: &Path,
) -> std::io::Result<PathBuf> {
  let path = output_dir.join(kind.file_name());
  let json = match kind {
    DumpKind::InterfaceMapping => {
      let records = dump_interface_mapping(audit_data);
      serde_json::to_string_pretty(&records)
    }
    DumpKind::NameIndex => {
      let entries = dump_name_index(audit_data);
      serde_json::to_string_pretty(&entries)
    }
  };
  let json = json.map_err(|e| {
    std::io::Error::other(format!("serializing {} dump: {}", kind.file_name(), e))
  })?;

  let tmp = path.with_extension("json.tmp");
  {
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(json.as_bytes())?;
    f.write_all(b"\n")?;
  }
  std::fs::rename(&tmp, &path)?;
  Ok(path)
}

// ---------------------------------------------------------------------------
// interface-mapping
// ---------------------------------------------------------------------------

/// One row in `interface-mapping.json`: a topic that proxies to another
/// (`transitive_topic` is `Some`) — typically an interface stub pointing
/// at its implementation. Both ends are surfaced with full identifying
/// metadata so a reviewer can spot mappings that look wrong (or, more
/// often, mappings that *should* exist but don't — that absence shows up
/// as an interface stub appearing in `name-index.json` with a non-empty
/// candidate list and missing from this file).
#[derive(Debug, Clone, Serialize)]
struct InterfaceMappingRecord {
  proxy_topic: String,
  proxy_name: String,
  proxy_kind: String,
  proxy_qualified_name: String,
  proxy_scope: String,
  target_topic: String,
  target_name: String,
  target_kind: String,
  target_qualified_name: String,
  target_scope: String,
}

fn dump_interface_mapping(
  audit_data: &AuditData,
) -> Vec<InterfaceMappingRecord> {
  let mut out: Vec<InterfaceMappingRecord> = Vec::new();
  for (proxy_topic, meta) in &audit_data.topic_metadata {
    let Some(target_topic) = meta.transitive_topic() else {
      continue;
    };
    // Only `NamedTopic` ↔ `NamedTopic` mappings are interesting here —
    // these are the interface-stub → implementation links. The AST-level
    // stub graph (used for cross-file reference resolution) also populates
    // `transitive_topic` on unnamed nodes, but those aren't what
    // "interface mapping" means semantically and would otherwise drown
    // out the signal.
    let TopicMetadata::NamedTopic { .. } = meta else {
      continue;
    };
    let target_meta = audit_data.topic_metadata.get(target_topic);
    let Some(TopicMetadata::NamedTopic { .. }) = target_meta else {
      continue;
    };

    out.push(InterfaceMappingRecord {
      proxy_topic: proxy_topic.id().to_string(),
      proxy_name: meta.name().unwrap_or("").to_string(),
      proxy_kind: kind_label(meta),
      proxy_qualified_name: meta
        .qualified_name(audit_data)
        .unwrap_or_default(),
      proxy_scope: scope_summary(meta.scope(), audit_data),
      target_topic: target_topic.id().to_string(),
      target_name: target_meta
        .and_then(|m| m.name())
        .unwrap_or("")
        .to_string(),
      target_kind: target_meta.map(kind_label).unwrap_or_default(),
      target_qualified_name: target_meta
        .and_then(|m| m.qualified_name(audit_data))
        .unwrap_or_default(),
      target_scope: target_meta
        .map(|m| scope_summary(m.scope(), audit_data))
        .unwrap_or_default(),
    });
  }
  out.sort_by(|a, b| {
    (a.proxy_qualified_name.as_str(), a.proxy_topic.as_str()).cmp(&(
      b.proxy_qualified_name.as_str(),
      b.proxy_topic.as_str(),
    ))
  });
  out
}

// ---------------------------------------------------------------------------
// name-index
// ---------------------------------------------------------------------------

/// One entry in `name-index.json`: a simple identifier name and the full
/// list of topic candidates that share it, plus whether the resolver was
/// able to pick a unique answer. The candidates carry enough metadata to
/// see what kind / scope / qualified-name each candidate has, so a
/// reviewer can spot ambiguities that should be resolvable (e.g. exactly
/// one `StateVariable` plus N `LocalVariable` parameters).
#[derive(Debug, Clone, Serialize)]
struct NameIndexEntry {
  name: String,
  /// True when the name is in `is_common_word`'s English-connective
  /// stoplist and was therefore excluded from the simple-name index.
  is_common_word: bool,
  /// True when there are >1 non-transitive candidates AND the resolver
  /// did not pick a unique answer (i.e. lookup returns `None`).
  ambiguous: bool,
  /// The topic the simple-name index points to, if it resolved uniquely.
  /// `None` when the name is ambiguous, common-word filtered, or absent.
  #[serde(skip_serializing_if = "Option::is_none")]
  resolved_topic: Option<String>,
  /// Every NamedTopic with this exact simple name, in deterministic order.
  candidates: Vec<NameCandidate>,
}

#[derive(Debug, Clone, Serialize)]
struct NameCandidate {
  topic: String,
  qualified_name: String,
  kind: String,
  scope: String,
  is_transitive: bool,
  /// When `is_transitive` is true, the topic this candidate proxies to.
  #[serde(skip_serializing_if = "Option::is_none")]
  transitive_target: Option<String>,
}

fn dump_name_index(audit_data: &AuditData) -> Vec<NameIndexEntry> {
  // Group every NamedTopic by simple name. Skip empty names — these are
  // unnamed AST nodes (e.g. constructor parameter lists) that share an
  // empty `name` field; lumping them together here is noise.
  let mut by_name: BTreeMap<String, Vec<topic::Topic>> = BTreeMap::new();
  for (t, meta) in &audit_data.topic_metadata {
    if let TopicMetadata::NamedTopic { name, .. } = meta
      && !name.is_empty()
    {
      by_name.entry(name.clone()).or_default().push(*t);
    }
  }

  let mut out: Vec<NameIndexEntry> = Vec::with_capacity(by_name.len());
  for (name, topics) in by_name {
    let is_common = is_common_word(&name);
    let resolved =
      audit_data.name_index.get_by_simple_name(&name).copied();
    // Ambiguous: resolver couldn't pick a unique answer despite >1
    // candidates with this simple name. Common-word filtering shows up
    // here too but is flagged separately.
    let ambiguous = resolved.is_none() && topics.len() > 1 && !is_common;

    let mut candidates: Vec<NameCandidate> = topics
      .iter()
      .map(|t| {
        let meta = audit_data.topic_metadata.get(t);
        let qualified_name = meta
          .and_then(|m| m.qualified_name(audit_data))
          .unwrap_or_default();
        let kind = meta.map(kind_label).unwrap_or_default();
        let scope = meta
          .map(|m| scope_summary(m.scope(), audit_data))
          .unwrap_or_default();
        let transitive = meta.and_then(|m| m.transitive_topic());
        NameCandidate {
          topic: t.id().to_string(),
          qualified_name,
          kind,
          scope,
          is_transitive: transitive.is_some(),
          transitive_target: transitive.map(|tt| tt.id().to_string()),
        }
      })
      .collect();
    candidates.sort_by(|a, b| {
      (a.qualified_name.as_str(), a.topic.as_str()).cmp(&(
        b.qualified_name.as_str(),
        b.topic.as_str(),
      ))
    });

    out.push(NameIndexEntry {
      name,
      is_common_word: is_common,
      ambiguous,
      resolved_topic: resolved.map(|t| t.id().to_string()),
      candidates,
    });
  }

  // Order: ambiguous first (most diagnostic), then alphabetical.
  out.sort_by(|a, b| {
    b.ambiguous
      .cmp(&a.ambiguous)
      .then_with(|| a.name.cmp(&b.name))
  });
  out
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// English-connective stoplist used by `TopicNameIndex` to keep common
/// words from polluting the simple-name index. Duplicated here from
/// `domain::is_common_word` so this dump can flag entries the resolver
/// filtered for that reason. Kept in sync manually — list is small and
/// stable.
fn is_common_word(name: &str) -> bool {
  matches!(
    name,
    "a" | "an"
      | "as"
      | "at"
      | "be"
      | "by"
      | "do"
      | "for"
      | "from"
      | "if"
      | "in"
      | "is"
      | "it"
      | "no"
      | "of"
      | "on"
      | "or"
      | "so"
      | "to"
      | "up"
      | "we"
  )
}

fn kind_label(meta: &TopicMetadata) -> String {
  match meta {
    TopicMetadata::NamedTopic { kind, .. } => match kind {
      NamedTopicKind::Function(k) => format!("Function({:?})", k),
      NamedTopicKind::Modifier => "Modifier".to_string(),
      NamedTopicKind::Event => "Event".to_string(),
      NamedTopicKind::Error => "Error".to_string(),
      NamedTopicKind::Struct => "Struct".to_string(),
      NamedTopicKind::Enum => "Enum".to_string(),
      NamedTopicKind::EnumMember => "EnumMember".to_string(),
      NamedTopicKind::StateVariable(m) => format!("StateVariable({:?})", m),
      NamedTopicKind::LocalVariable => "LocalVariable".to_string(),
      NamedTopicKind::Contract(k) => format!("Contract({:?})", k),
      NamedTopicKind::Builtin => "Builtin".to_string(),
    },
    TopicMetadata::TitledTopic { kind, .. } => format!("Titled({:?})", kind),
    TopicMetadata::DocumentationTopic { .. } => "Documentation".to_string(),
    other => format!("{:?}", std::mem::discriminant(other)),
  }
}

fn scope_summary(scope: &Scope, audit_data: &AuditData) -> String {
  let name_of = |t: &topic::Topic| -> String {
    audit_data
      .topic_metadata
      .get(t)
      .and_then(|m| m.name())
      .unwrap_or("?")
      .to_string()
  };
  match scope {
    Scope::Global => "global".to_string(),
    Scope::Container { container } => {
      format!("file: {}", container.file_path)
    }
    Scope::Component {
      container,
      component,
    } => format!(
      "in {} (file: {})",
      name_of(component),
      container.file_path
    ),
    Scope::Member {
      member,
      component,
      signature_container,
      ..
    } => {
      let sig = if signature_container.is_some() {
        " [signature]"
      } else {
        ""
      };
      format!("in {}::{}{}", name_of(component), name_of(member), sig)
    }
    Scope::ContainingBlock {
      member, component, ..
    } => format!("inside {}::{}", name_of(component), name_of(member)),
  }
}


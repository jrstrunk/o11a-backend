use std::path::{Path, PathBuf};

pub mod topic;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

// ============================================================================
// Comment Type
// ============================================================================

/// Comment type for classification. Used in TopicMetadata::CommentTopic and
/// in the collaborator layer for DB serialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommentType {
  Note,             // General observation or annotation
  Info,             // Informational context or explanation
  Question,         // Question needing an answer
  Answer,           // Answer to a question
  Todo,             // Action item to be completed
  FindingLead,      // Potential vulnerability or issue to investigate
  DevTechnical,     // Inline developer comment from source code (// and /* */)
  DevDocumentation, // NatSpec @notice docstring from source code
}

impl CommentType {
  pub fn as_str(&self) -> &'static str {
    match self {
      CommentType::Note => "note",
      CommentType::Info => "info",
      CommentType::Question => "question",
      CommentType::Answer => "answer",
      CommentType::Todo => "todo",
      CommentType::FindingLead => "finding_lead",
      CommentType::DevTechnical => "dev_technical",
      CommentType::DevDocumentation => "dev_documentation",
    }
  }

  pub fn parse_str(s: &str) -> Option<Self> {
    match s {
      "note" => Some(CommentType::Note),
      "info" => Some(CommentType::Info),
      "question" => Some(CommentType::Question),
      "answer" => Some(CommentType::Answer),
      "todo" => Some(CommentType::Todo),
      "finding_lead" => Some(CommentType::FindingLead),
      "dev_technical" => Some(CommentType::DevTechnical),
      "dev_documentation" => Some(CommentType::DevDocumentation),
      _ => None,
    }
  }
}

// ============================================================================
// Solidity Type System (for checker module)
// ============================================================================

/// Represents a Solidity type for use in the checker.
/// Contains enough detail to derive valid value ranges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SolidityType {
  /// Elementary types with full detail for value range derivation
  Elementary(ElementaryType),
  /// User-defined types reference the declaration topic
  UserDefined { declaration_topic: topic::Topic },
  /// Array types - length is Some for fixed-size arrays
  Array {
    base_type: Box<SolidityType>,
    length: Option<u64>,
  },
  /// Mapping types
  Mapping {
    key_type: Box<SolidityType>,
    value_type: Box<SolidityType>,
  },
  /// Function types
  Function {
    parameter_types: Vec<SolidityType>,
    return_types: Vec<SolidityType>,
  },
}

/// Elementary types with enough detail to derive value ranges.
/// Numbers include bit size so the checker can compute min/max values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ElementaryType {
  /// Boolean: range is {false, true}
  Bool,
  /// Address: 20 bytes, range is 0 to 2^160-1
  Address,
  /// Payable address: same range as Address
  AddressPayable,
  /// Fixed-size bytes: bytesN where N is 1-32
  /// Range is 0 to 2^(N*8)-1
  FixedBytes(u8),
  /// Dynamic bytes: no fixed range
  Bytes,
  /// String: no fixed numeric range
  String,
  /// Signed integer: intN where bits is 8, 16, 24, ... 256
  /// Range is -2^(bits-1) to 2^(bits-1)-1
  Int { bits: u16 },
  /// Unsigned integer: uintN where bits is 8, 16, 24, ... 256
  /// Range is 0 to 2^bits-1
  Uint { bits: u16 },
}

impl ElementaryType {
  /// Returns true if this type has a numeric value range
  pub fn is_numeric(&self) -> bool {
    matches!(
      self,
      ElementaryType::Int { .. } | ElementaryType::Uint { .. }
    )
  }

  /// Returns true if this type is an address
  pub fn is_address(&self) -> bool {
    matches!(
      self,
      ElementaryType::Address | ElementaryType::AddressPayable
    )
  }
}

// ============================================================================
// Revert Constraint Types (for checker module)
// ============================================================================

// ============================================================================
// Block Annotation Types
// ============================================================================

/// Describes the annotation on a containing block layer — either a control flow
/// statement whose body is this block, or an annotated block type like
/// `unchecked` or `assembly`.
///
/// Branch information is encoded directly in the kind — only `If` has branches,
/// so this avoids a disjoint field that would be meaningless for other kinds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockAnnotation {
  /// The topic of the annotating node (the control flow statement or
  /// the annotated block itself).
  pub topic: topic::Topic,
  pub kind: BlockAnnotationKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BlockAnnotationKind {
  // Control flow
  If(ControlFlowBranch),
  For,
  While,
  DoWhile,
  // Annotated blocks
  Unchecked,
  InlineAssembly,
}

/// Branchless kind for the TopicMetadata::ControlFlow variant.
/// Unlike BlockAnnotationKind (which encodes branch info for scope tracking),
/// this simply identifies the statement type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlFlowStatementKind {
  If,
  For,
  While,
  DoWhile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ControlFlowBranch {
  True,
  False,
}

/// One layer in the containing block nesting chain.
/// Pairs a semantic block with an optional annotation describing what
/// kind of block it is (control flow body, unchecked, assembly, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainingBlockLayer {
  /// The semantic block at this nesting level.
  pub block: topic::Topic,
  /// The annotation on this block layer, if any.
  /// None for plain semantic blocks with no governing statement or keyword.
  pub annotation: Option<BlockAnnotation>,
}

// ============================================================================
// Revert Info Types
// ============================================================================

/// Simple revert/require statement info stored on FunctionModProperties.
///
/// `error_topic` exposes the custom-error declaration referenced by
/// `revert MyError(...)` so downstream consumers (e.g. the
/// resolution-graph extractor's `error-thrown` edges) can recover it
/// without re-walking the AST. `None` for `require(cond, "string")` and
/// bare `revert("string")` — those have no associated error declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevertInfo {
  pub topic: topic::Topic,
  pub kind: RevertConstraintKind,
  #[serde(default)]
  pub error_topic: Option<topic::Topic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RevertConstraintKind {
  /// require(condition) - reverts when condition is false
  Require,
  /// revert with enclosing if conditions
  Revert,
}

/// A single call site recorded on `FunctionModProperties.calls`.
///
/// `site` is the FunctionCall expression node; `callee` is the resolved
/// callee declaration. `in_try_block` mirrors Solidity's `tryCall` flag
/// — true iff this call expression is the `external_call` of a
/// `TryStatement`, in which case reverts originating from the callee
/// (or transitively through it) are caught by the wrapping try/catch
/// and do not propagate into the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallInfo {
  pub site: topic::Topic,
  pub callee: topic::Topic,
  #[serde(default)]
  pub in_try_block: bool,
}

/// One revert that a function can transitively raise. Produced by the
/// bottom-up fold in `effective_properties.rs`. `origin` is the
/// function or modifier whose body directly raises `revert` — i.e.,
/// the leaf of the propagation chain. The intermediate call path is
/// not stored; it can be reconstructed from the call graph if a
/// render site needs it, and storing one canonical "via" hop would
/// lose information when two paths converge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveRevert {
  pub revert: RevertInfo,
  pub origin: topic::Topic,
}

/// One transitive non-revert side-effect entry — a state-variable
/// access (read or write) or an event emission. Shared across the
/// three `effective_mutations` / `effective_reads` /
/// `effective_events_emitted` fields. `topic` is the state variable
/// or event being referenced; `origin` is the function/modifier whose
/// body directly triggers it (the leaf of the propagation chain).
/// Same not-storing-path rationale as `EffectiveRevert`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveTopic {
  pub topic: topic::Topic,
  pub origin: topic::Topic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FunctionKind {
  Constructor,
  Function,
  Fallback,
  Receive,
  FreeFunction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContractKind {
  Contract,
  Library,
  Abstract,
  Interface,
}

/// Severity level for threats and invariants.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum ThreatSeverity {
  Low,
  Medium,
  High,
  Critical,
}

impl ThreatSeverity {
  pub fn as_str(&self) -> &'static str {
    match self {
      ThreatSeverity::Low => "low",
      ThreatSeverity::Medium => "medium",
      ThreatSeverity::High => "high",
      ThreatSeverity::Critical => "critical",
    }
  }

  pub fn parse_str(s: &str) -> Option<ThreatSeverity> {
    match s {
      "low" => Some(ThreatSeverity::Low),
      "medium" => Some(ThreatSeverity::Medium),
      "high" => Some(ThreatSeverity::High),
      "critical" => Some(ThreatSeverity::Critical),
      _ => None,
    }
  }
}

/// An intermediate value carrying a semantic from one of the synthesis
/// steps (`task::link_contracts`, `task::link_member_signatures`,
/// `task::link_member_bodies`) through the per-step condensation into a
/// `FunctionalSemanticTopic`. Field names align with
/// `FunctionalSemanticTopic` for direct mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticLink {
  /// D-prefixed documentation topics that contributed to this semantic
  pub documentation_topics: Vec<topic::Topic>,
  /// The N-prefixed code declaration topic
  pub declaration_topic: topic::Topic,
  /// The semantic meaning derived from this link
  pub description: String,
  /// Provenance: which workflow variant produced the (section, member) match
  /// that this link was derived from.
  pub match_source: MatchSource,
}

/// Provenance for a semantic link: which workflow variant produced the
/// (section, member) match. See `docs/specs/semantic-linking.md`.
///
/// Serialized as a lowercase string for clarity in `audit.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchSource {
  /// The match came from the mechanical layer alone (inline reference
  /// resolution + scope walking + state-variable mutation fanout).
  Mechanical,
  /// The match came from BM25 expansion within an anchored contract.
  Bm25,
}

impl MatchSource {
  pub fn as_str(self) -> &'static str {
    match self {
      MatchSource::Mechanical => "mechanical",
      MatchSource::Bm25 => "bm25",
    }
  }

  /// Higher confidence wins when condensation merges links from different
  /// sources. Order: mechanical > bm25.
  pub fn merge(self, other: MatchSource) -> MatchSource {
    use MatchSource::*;
    match (self, other) {
      (Mechanical, _) | (_, Mechanical) => Mechanical,
      _ => Bm25,
    }
  }
}

/// A behavioral requirement belonging to a feature.
/// Requirements are what the documentation claims the system does. They are
/// verified via reconciliation against behaviors, not by direct source linking.
/// Each requirement has at least one linked documentation topic that informed it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Requirement {
  /// D-prefixed topic IDs of documentation sections that informed this requirement
  pub documentation_topics: Vec<topic::Topic>,
}

/// Relationship type between a threat and a feature in impact analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreatFeatureRelation {
  /// The subject is part of the attack surface for a concern within the feature
  IsVulnerableTo,
  /// The subject is part of the defense against a concern within the feature
  DefendsAgainst,
}

impl ThreatFeatureRelation {
  pub fn as_str(&self) -> &'static str {
    match self {
      ThreatFeatureRelation::IsVulnerableTo => "is_vulnerable_to",
      ThreatFeatureRelation::DefendsAgainst => "defends_against",
    }
  }

  pub fn parse_str(s: &str) -> Option<ThreatFeatureRelation> {
    match s {
      "is_vulnerable_to" => Some(ThreatFeatureRelation::IsVulnerableTo),
      "defends_against" => Some(ThreatFeatureRelation::DefendsAgainst),
      _ => None,
    }
  }
}

/// A link between a threat and a feature, established during impact analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreatFeatureLink {
  pub threat_topic: topic::Topic,
  pub feature_topic: topic::Topic,
  pub relation: ThreatFeatureRelation,
  pub severity: ThreatSeverity,
}

/// A threat describing how an attacker could compromise a feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Threat {
  /// A-prefixed topic IDs of invariants that defend against this threat
  pub invariant_topics: Vec<topic::Topic>,
}

/// An invariant that must hold to prevent a threat.
/// Linked to source code topics where the invariant is enforced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invariant {
  /// N-prefixed topic IDs of source code topics that enforce this invariant
  pub source_topics: Vec<topic::Topic>,
}

/// Contains all data for a single audit
pub struct AuditData {
  // The name of the audit being audited, like "Chainlink"
  pub audit_name: String,
  // A list of files that are in scope for this audit
  pub in_scope_files: HashSet<ProjectPath>,
  /// Free-form security notes loaded from security.md. Contains role
  /// definitions, known threats/invariants, and other security considerations
  /// that the threat-building agent should incorporate.
  pub security_notes: Option<String>,
  // Contains the ASTs for a given file path
  pub asts: BTreeMap<ProjectPath, AST>,
  // Contains the node for a given topic
  pub nodes: BTreeMap<topic::Topic, Node>,
  // Contains the declaration for a given topic
  pub topic_metadata: BTreeMap<topic::Topic, TopicMetadata>,
  // Contains the function properties for a given topic
  pub function_properties: BTreeMap<topic::Topic, FunctionModProperties>,
  /// Maps variable topic IDs to their Solidity types (for checker module)
  pub variable_types: BTreeMap<topic::Topic, SolidityType>,
  /// Pre-computed name indexes for fast topic lookup by name.
  /// Built after all topic_metadata insertions are complete.
  pub name_index: TopicNameIndex,
  /// Reverse index: target topic ID → non-hidden comment topics.
  /// Updated on comment create and status change.
  pub comment_index: HashMap<topic::Topic, Vec<topic::Topic>>,
  /// Primary source context for each topic, stored separately from TopicMetadata.
  pub topic_context: BTreeMap<topic::Topic, Vec<SourceContext>>,
  /// Expanded source context for each topic — related browsable references
  /// rendered in the secondary panel alongside the primary `topic_context`.
  /// Only populated for topics that have a meaningful expanded view
  /// (NamedTopics, documentation TitledTopics/UnnamedTopics, FeatureTopics,
  /// BehaviorTopics, FunctionalSemanticTopics).
  pub expanded_topic_context: BTreeMap<topic::Topic, Vec<SourceContext>>,
  /// Requirements keyed by R-prefixed topic ID. Links to features are in feature_requirement_links.
  pub requirements: BTreeMap<topic::Topic, Requirement>,
  /// Reverse index: D-prefixed section topic → R-prefixed requirement topics.
  /// Derived from RequirementTopic.section_topic, rebuilt with rebuild_feature_context.
  pub section_requirements: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Reverse index: N-prefixed member topic → B-prefixed behavior topics.
  /// Derived from BehaviorTopic.member_topic, rebuilt with rebuild_feature_context.
  pub member_behaviors: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Reverse index: N-prefixed declaration topic → P-prefixed semantic topics.
  /// Derived from FunctionalSemanticTopic.declaration_topic, rebuilt with rebuild_feature_context.
  pub declaration_semantics: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Reverse index: non-pure subject topic → P-prefixed functional purpose
  /// topic. At most one purpose per subject; later writes replace the entry.
  /// Derived from `FunctionalPurposeTopic.subject_topic`, rebuilt with
  /// `rebuild_feature_context`.
  pub subject_purposes: BTreeMap<topic::Topic, topic::Topic>,
  /// Reverse index: non-pure subject topic → P-prefixed placement rationale
  /// topic. At most one placement per subject; later writes replace the entry.
  /// Derived from `PlacementRationaleTopic.subject_topic`, rebuilt with
  /// `rebuild_feature_context`.
  pub subject_placements: BTreeMap<topic::Topic, topic::Topic>,
  /// Reverse index: non-pure subject topic → A-prefixed condition topics.
  /// Each subject has zero or more conditions; later writes append rather
  /// than replace (a condition is its own topic, addressed by topic ID, so
  /// duplicates would already be distinct topics). Derived from
  /// `ConditionTopic.subject_topic`, rebuilt with `rebuild_feature_context`.
  pub subject_conditions: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Impact analysis links between threats and features.
  pub threat_feature_links: Vec<ThreatFeatureLink>,
  /// Threats keyed by A-prefixed topic ID. Each belongs to one feature.
  pub threats: BTreeMap<topic::Topic, Threat>,
  /// Invariants keyed by A-prefixed topic ID. Each belongs to one threat.
  pub invariants: BTreeMap<topic::Topic, Invariant>,
  /// Feature-to-requirement links (many-to-many). Keyed by F-prefixed topic.
  pub feature_requirement_links: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Feature-to-behavior links (many-to-many). Keyed by F-prefixed topic.
  pub feature_behavior_links: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Reverse index: mentioned topic → comment topics that mention it. Updated
  /// on comment create. Feeds the conversation panel.
  ///
  /// Doc-sourced references are not stored here; they live as a static field
  /// (`doc_references`) on the referenced NamedTopic/FeatureTopic metadata.
  pub mentions_index: HashMap<topic::Topic, Vec<topic::Topic>>,
  /// Contract inheritance edges: contract topic → its base contracts/
  /// interfaces. Sparse — contracts with no bases are absent. Each
  /// `Vec<Topic>` is sorted ascending. Populated between first_pass and
  /// tree_shake from `FirstPassDeclaration::Contract::base_contracts`,
  /// which is otherwise dropped after tree-shaking.
  pub inheritance: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Typed weighted graph used by the personalized-PageRank resolver for
  /// ambiguous code references. Populated by
  /// `o11a_core::resolution_graph::build` at audit-load time, after every
  /// language analyzer completes. `None` until that step has run.
  pub resolution_graph: Option<crate::resolution_graph::ResolutionGraph>,
  /// Per-resolution explanation records emitted by the graph-driven
  /// resolution passes (Phases B/C/D/E). One entry per ambiguous
  /// reference the resolver attempted, regardless of whether a winner
  /// was picked. Keyed by `ResolutionRefId` so doc-tree nodes (Phase 6)
  /// and dev-doc references (Phase 7+) share one store. Sorted by key
  /// for deterministic dump output.
  pub resolution_traces: BTreeMap<
    crate::resolution_graph::ResolutionRefId,
    crate::resolution_graph::ResolutionTrace,
  >,
}

/// Common short English words that should not match as simple topic names.
/// These appear frequently in documentation prose inside backticks but are
/// almost never intended to reference a Solidity declaration.
/// Qualified names like "ERC20.transfer.from" are unaffected.
///
/// Exposed crate-internally so the diagnostic dump in `audit_dump` can flag
/// names that the resolver filtered for this reason — must stay one source
/// of truth.
pub(crate) fn is_common_word(name: &str) -> bool {
  matches!(
    name,
    "a"
      | "an"
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

/// Pre-computed name indexes for fast topic lookup by name.
/// Built once after all topic_metadata insertions are complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicNameIndex {
  by_qualified_name: HashMap<String, topic::Topic>,
  by_simple_name: HashMap<String, topic::Topic>,
  /// Pre-dedup candidates per simple name: every NamedTopic whose simple
  /// name matches, regardless of how many candidates exist. Sorted
  /// ascending by topic ID. Used by the personalized-PageRank resolver
  /// for ambiguous code references.
  by_simple_name_candidates: BTreeMap<String, Vec<topic::Topic>>,
}

impl TopicNameIndex {
  pub fn empty() -> Self {
    TopicNameIndex {
      by_qualified_name: HashMap::new(),
      by_simple_name: HashMap::new(),
      by_simple_name_candidates: BTreeMap::new(),
    }
  }

  pub fn build(audit_data: &AuditData) -> Self {
    let mut by_qualified_name = HashMap::new();
    let mut simple_name_candidates: HashMap<String, Vec<topic::Topic>> =
      HashMap::new();

    // Only NamedTopic (code declarations) participates in name_index lookup.
    // Feature names and section titles are user-supplied phrases, not code
    // identifiers, and must not shadow declarations.
    for (topic, metadata) in &audit_data.topic_metadata {
      if let TopicMetadata::NamedTopic { name: sname, .. } = metadata {
        if let Some(qname) = metadata.qualified_name(audit_data) {
          by_qualified_name.insert(qname, *topic);
        }
        if !is_common_word(sname) {
          simple_name_candidates
            .entry(sname.to_string())
            .or_default()
            .push(*topic);
        }
      }
    }

    let by_simple_name_candidates: BTreeMap<String, Vec<topic::Topic>> =
      simple_name_candidates
        .iter()
        .map(|(name, topics)| {
          let mut sorted = topics.clone();
          sorted.sort();
          (name.clone(), sorted)
        })
        .collect();

    let by_simple_name = simple_name_candidates
      .into_iter()
      .filter_map(|(name, topics)| {
        if topics.len() == 1 {
          Some((name, topics.into_iter().next().unwrap()))
        } else {
          // When multiple topics share a name, prefer non-transitive members.
          // Transitive topics are proxies (e.g., interface members with one
          // implementation) — resolve to the real declaration instead.
          let non_transitive: Vec<_> = topics
            .iter()
            .filter(|t| {
              !matches!(
                audit_data.topic_metadata.get(t),
                Some(m) if m.transitive_topic().is_some()
              )
            })
            .collect();

          if non_transitive.len() == 1 {
            Some((name, *non_transitive[0]))
          } else {
            None
          }
        }
      })
      .collect();

    TopicNameIndex {
      by_qualified_name,
      by_simple_name,
      by_simple_name_candidates,
    }
  }

  pub fn get_by_qualified_name(&self, name: &str) -> Option<&topic::Topic> {
    self.by_qualified_name.get(name)
  }

  pub fn get_by_simple_name(&self, name: &str) -> Option<&topic::Topic> {
    self.by_simple_name.get(name)
  }

  /// Every NamedTopic whose simple name matches, regardless of how many
  /// candidates share the name. Returns an empty slice when the name is
  /// unknown. Sorted ascending by topic ID.
  pub fn candidates_by_simple_name(&self, name: &str) -> &[topic::Topic] {
    self
      .by_simple_name_candidates
      .get(name)
      .map(|v| v.as_slice())
      .unwrap_or(&[])
  }

  pub fn qualified_names(&self) -> Vec<&str> {
    self.by_qualified_name.keys().map(|s| s.as_str()).collect()
  }
}

pub struct DataContext {
  // Map of audit_id to audit data
  pub audits: BTreeMap<String, AuditData>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Node {
  Solidity(crate::solidity::ast::ASTNode),
  Documentation(crate::documentation::ast::DocumentationNode),
  Comment(Vec<crate::collaborator::parser::CommentNode>),
  /// A Rust AST node. Inert until the Rust analyzer lands — included
  /// so polyglot dispatch sites (resolution graph, web renderers) can
  /// be wired without later breaking match exhaustiveness.
  Rust(crate::rust::ast::ASTNode),
}

impl Node {
  /// Returns the source location start (byte offset) for this node.
  pub fn source_location_start(&self) -> Option<usize> {
    match self {
      Node::Solidity(ast_node) => ast_node.src_location().start,
      Node::Documentation(doc_node) => doc_node.position(),
      Node::Comment(_) => None,
      Node::Rust(ast_node) => ast_node.src_location().start,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AST {
  Solidity(crate::solidity::ast::SolidityAST),
  Documentation(crate::documentation::ast::DocumentationAST),
  /// A Rust source file. Inert until the Rust analyzer lands; the
  /// `RustExtractor` registered in `resolution_graph::builder` reads
  /// from this variant.
  Rust(crate::rust::ast::RustAST),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Scope {
  Global,
  Container {
    container: ProjectPath,
  },
  Component {
    container: ProjectPath,
    component: topic::Topic,
  },
  Member {
    container: ProjectPath,
    component: topic::Topic,
    member: topic::Topic,
    /// When the node is inside a member's signature, this holds the
    /// containing signature list node (e.g. the ParameterList for parameters
    /// or return values, or the ModifierList for modifier specifiers).
    /// None for nodes that are not inside a signature.
    signature_container: Option<topic::Topic>,
  },
  ContainingBlock {
    container: ProjectPath,
    component: topic::Topic,
    member: topic::Topic,
    containing_blocks: Vec<ContainingBlockLayer>,
  },
}

impl Scope {
  /// Returns all ancestor topics in the scope chain.
  /// For Component scope, yields the component.
  /// For Member scope, yields the component and member.
  /// For ContainingBlock scope, yields the component, member, and all containing blocks.
  pub fn ancestor_topics(&self) -> Vec<&topic::Topic> {
    match self {
      Scope::Global | Scope::Container { .. } => vec![],
      Scope::Component { component, .. } => vec![component],
      Scope::Member {
        component, member, ..
      } => vec![component, member],
      Scope::ContainingBlock {
        component,
        member,
        containing_blocks,
        ..
      } => {
        let mut ancestors = vec![component, member];
        for layer in containing_blocks {
          ancestors.push(&layer.block);
        }
        ancestors
      }
    }
  }
}

/// Walks up the scope chain starting from `start_topic`, returning the
/// topic itself followed by each enclosing scope topic from innermost to
/// outermost. Terminates at `Scope::Container` / `Scope::Global` (which
/// produce no further ancestors) — for typical Solidity inputs that
/// means the chain ends at the contract topic.
///
/// Used by the dev-doc graph resolution pass (Phase 7 of the
/// semantic-resolution-graph build plan) to seed personalized PageRank
/// from the source-tree scope chain of a NatSpec target topic. Read the
/// seed table in
/// `crates/o11a-analyze/docs/build-plans/semantic-resolution-graph.md`
/// (Phase 7 → Context) for the consumer-side weighting rule.
///
/// Examples (using `Scope` produced by the Solidity analyzer):
///
/// * Contract topic (`Scope::Container`)            → `[contract]`
/// * Function topic (`Scope::Component { contract }`) →
///   `[function, contract]`
/// * State variable                                 → `[state-var, contract]`
/// * Top-level block in function                    →
///   `[block, function, contract]`
/// * Inner block (`ContainingBlock` with one outer) →
///   `[inner_block, outer_block, function, contract]`
///
/// When `start_topic` has no entry in `topic_metadata`, the chain is
/// just `[start_topic]` — the helper never panics on a missing topic so
/// callers can pass in any topic without a precondition check.
pub fn scope_ancestor_chain(
  audit_data: &AuditData,
  start_topic: topic::Topic,
) -> Vec<topic::Topic> {
  let mut chain = vec![start_topic];
  let Some(metadata) = audit_data.topic_metadata.get(&start_topic) else {
    return chain;
  };
  // `Scope::ancestor_topics()` yields topics outermost-first
  // (`[contract, function, block_outer, ..., block_inner]`). The chain
  // we want is innermost-first (immediate enclosing scope at distance
  // 1, contract at the tail), so iterate in reverse and skip any
  // duplicate of `start_topic` itself — defensive against the
  // (theoretical) case where a topic's scope contains the topic.
  for ancestor in metadata.scope().ancestor_topics().into_iter().rev() {
    if *ancestor != start_topic {
      chain.push(*ancestor);
    }
  }
  chain
}

#[cfg(test)]
mod scope_chain_tests {
  use super::*;
  use std::collections::HashSet;

  fn audit() -> AuditData {
    new_audit_data("t".to_string(), HashSet::new(), None)
  }

  fn nt(id: i32) -> topic::Topic {
    topic::new_node_topic(&id)
  }

  fn pp() -> ProjectPath {
    ProjectPath {
      file_path: "x.sol".to_string(),
    }
  }

  fn named_with_scope(t: topic::Topic, scope: Scope) -> TopicMetadata {
    TopicMetadata::NamedTopic {
      topic: t,
      scope,
      kind: NamedTopicKind::Builtin,
      visibility: NamedTopicVisibility::Public,
      name: "x".to_string(),
      is_mutable: false,
      mutations: Vec::new(),
      ancestors: Vec::new(),
      descendants: Vec::new(),
      relatives: Vec::new(),
      transitive_topic: None,
      doc_references: Vec::new(),
    }
  }

  #[test]
  fn unknown_topic_yields_just_itself() {
    let a = audit();
    assert_eq!(scope_ancestor_chain(&a, nt(99)), vec![nt(99)]);
  }

  #[test]
  fn contract_topic_with_container_scope_yields_just_itself() {
    let mut a = audit();
    let c = nt(1);
    a.topic_metadata
      .insert(c, named_with_scope(c, Scope::Container { container: pp() }));
    assert_eq!(scope_ancestor_chain(&a, c), vec![c]);
  }

  #[test]
  fn function_topic_yields_function_then_contract() {
    let mut a = audit();
    let contract = nt(1);
    let func = nt(10);
    a.topic_metadata.insert(
      contract,
      named_with_scope(contract, Scope::Container { container: pp() }),
    );
    a.topic_metadata.insert(
      func,
      named_with_scope(
        func,
        Scope::Component {
          container: pp(),
          component: contract,
        },
      ),
    );
    assert_eq!(scope_ancestor_chain(&a, func), vec![func, contract]);
  }

  #[test]
  fn parameter_topic_yields_param_then_function_then_contract() {
    let mut a = audit();
    let contract = nt(1);
    let func = nt(10);
    let param = nt(20);
    a.topic_metadata.insert(
      param,
      named_with_scope(
        param,
        Scope::Member {
          container: pp(),
          component: contract,
          member: func,
          signature_container: None,
        },
      ),
    );
    assert_eq!(scope_ancestor_chain(&a, param), vec![param, func, contract],);
  }

  #[test]
  fn nested_block_yields_innermost_first_chain() {
    let mut a = audit();
    let contract = nt(1);
    let func = nt(10);
    let outer_block = nt(20);
    let inner_block = nt(30);
    a.topic_metadata.insert(
      inner_block,
      named_with_scope(
        inner_block,
        Scope::ContainingBlock {
          container: pp(),
          component: contract,
          member: func,
          containing_blocks: vec![ContainingBlockLayer {
            block: outer_block,
            annotation: None,
          }],
        },
      ),
    );
    assert_eq!(
      scope_ancestor_chain(&a, inner_block),
      vec![inner_block, outer_block, func, contract],
      "nested-block chain runs innermost to outermost",
    );
  }

  #[test]
  fn global_scope_topic_yields_just_itself() {
    let mut a = audit();
    let g = nt(1);
    a.topic_metadata
      .insert(g, named_with_scope(g, Scope::Global));
    assert_eq!(scope_ancestor_chain(&a, g), vec![g]);
  }
}

pub fn add_to_scope(scope: &Scope, topic: topic::Topic) -> Scope {
  match scope {
    Scope::Global => Scope::Global, // Global scope cannot be nested
    Scope::Container { container } => Scope::Component {
      container: container.clone(),
      component: topic,
    },
    Scope::Component {
      container,
      component,
    } => Scope::Member {
      container: container.clone(),
      component: *component,
      member: topic,
      signature_container: None,
    },
    Scope::Member {
      container,
      component,
      member,
      ..
    } => {
      let containing_blocks = vec![ContainingBlockLayer {
        block: topic,
        annotation: None,
      }];
      Scope::ContainingBlock {
        container: container.clone(),
        component: *component,
        member: *member,
        containing_blocks,
      }
    }
    Scope::ContainingBlock {
      container,
      component,
      member,
      containing_blocks,
    } => {
      let mut containing_blocks = containing_blocks.clone();
      containing_blocks.push(ContainingBlockLayer {
        block: topic,
        annotation: None,
      });
      Scope::ContainingBlock {
        container: container.clone(),
        component: *component,
        member: *member,
        containing_blocks,
      }
    }
  }
}

/// Attaches an annotation to the innermost containing block layer.
/// Used when a control flow statement or annotated block (unchecked, assembly)
/// is encountered within a semantic block.
///
/// Panics if the scope is not `ContainingBlock` (annotated blocks cannot
/// exist outside a semantic block) or if the innermost layer already has
/// an annotation (each block has at most one annotation on a nesting path).
pub fn add_annotation_to_scope(
  scope: &Scope,
  annotation: BlockAnnotation,
) -> Scope {
  match scope {
    Scope::ContainingBlock {
      container,
      component,
      member,
      containing_blocks,
    } => {
      let last = containing_blocks
        .last()
        .expect("ContainingBlock scope must have at least one layer");
      assert!(
        last.annotation.is_none(),
        "Invariant violation: innermost containing block layer already has an annotation.\n\
         Existing annotation: {:?}\n\
         New annotation: {:?}\n\
         Block topic: {:?}\n\
         Scope: {:?}",
        last.annotation,
        annotation,
        last.block,
        scope,
      );
      let mut containing_blocks = containing_blocks.clone();
      let last_mut = containing_blocks.last_mut().unwrap();
      last_mut.annotation = Some(annotation);
      Scope::ContainingBlock {
        container: container.clone(),
        component: *component,
        member: *member,
        containing_blocks,
      }
    }
    _ => panic!(
      "Invariant violation: annotated block node encountered outside a containing block scope"
    ),
  }
}

/// Sets the member in a scope, replacing any existing member.
/// Used for nested headings in documentation where sub-H1 sections
/// should replace the current member rather than nesting further.
pub fn set_member(scope: &Scope, topic: topic::Topic) -> Scope {
  match scope {
    Scope::Global => Scope::Global, // Global scope cannot have members
    Scope::Container { .. } => scope.clone(), // Container needs a component first
    Scope::Component {
      container,
      component,
    } => Scope::Member {
      container: container.clone(),
      component: *component,
      member: topic,
      signature_container: None,
    },
    Scope::Member {
      container,
      component,
      ..
    } => Scope::Member {
      container: container.clone(),
      component: *component,
      member: topic,
      signature_container: None,
    },
    Scope::ContainingBlock {
      container,
      component,
      ..
    } => Scope::Member {
      container: container.clone(),
      component: *component,
      member: topic,
      signature_container: None,
    },
  }
}

/// Sets the signature_container on a Member scope.
/// Panics if the scope is not `Member`.
pub fn set_signature_container(
  scope: &Scope,
  container: topic::Topic,
) -> Scope {
  match scope {
    Scope::Member {
      container: proj,
      component,
      member,
      ..
    } => Scope::Member {
      container: proj.clone(),
      component: *component,
      member: *member,
      signature_container: Some(container),
    },
    _ => panic!(
      "Invariant violation: set_signature_container called on non-Member scope"
    ),
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VariableMutability {
  Mutable,
  Immutable,
  Constant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NamedTopicKind {
  Contract(ContractKind),
  Function(FunctionKind),
  Modifier,
  Event,
  Error,
  Struct,
  Enum,
  EnumMember,
  StateVariable(VariableMutability),
  LocalVariable,
  Builtin,
}

/// Kinds of titled topics (topics with a title but not a full declaration)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TitledTopicKind {
  /// Documentation section (H1 becomes component, sub-H1 becomes member)
  DocumentationSection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnnamedTopicKind {
  VariableMutation,
  Arithmetic,
  Comparison,
  Logical,
  Bitwise,
  Conditional,
  FunctionCall(CallKind),
  TypeConversion,
  StructConstruction,
  NewExpression,
  Literal,
  SemanticBlock,
  ContractMemberGroup,
  Break,
  Continue,
  Emit,
  InlineAssembly,
  LoopExpression,
  Placeholder,
  Return,
  Revert,
  Try,
  UncheckedBlock,
  Reference,
  MutableReference,
  Signature,
  DocumentationHeading,
  DocumentationParagraph,
  DocumentationSentence,
  DocumentationCodeBlock,
  DocumentationInlineCode,
  DocumentationList,
  DocumentationBlockQuote,
  Other,
}

/// Classifies whether a source code subject is pure (closed threat surface,
/// covered by type convergences) or non-pure (interacts with persistent state,
/// external code, or blockchain environment, requiring structured analysis).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubjectPurity {
  /// Pure: arithmetic, comparisons, boolean logic, local variable assignments.
  /// Threat surface is fully covered by type convergences and functional semantics.
  Pure,
  /// Non-pure: state writes, state reads of mutables, external calls,
  /// delegatecalls, assembly blocks, selfdestruct/create/create2.
  /// Receives conditions in step 6 and threats in step 7.
  NonPure,
}

/// The category of assertion this condition expresses — what must hold
/// for the subject's functional purpose and placement rationale to be
/// fulfilled. Loose taxonomy; the LLM picks; the auditor groups by
/// category in the review UI. Use `Other` for genuinely novel assertions
/// rather than forcing a fit. Threats (step 7) are adversarial inversions
/// of these assertions; the kinds here name what holds, not what fails.
///
/// **Variant order is wire-format-relevant.** `AuditDataSnapshot` is
/// serialized via bincode, which encodes enum variants by their declaration
/// index (not by name). Renaming a variant in place is safe; reordering,
/// removing, or inserting variants is a wire-format break that requires
/// bumping `analysis_artifact::ARTIFACT_SCHEMA_VERSION`. The HTTP API
/// emits the variant name (`format!("{:?}", kind)` in handlers.rs), so
/// any rename is also a surface-level API change for downstream clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionKind {
  /// Triggering of this interaction is constrained to expected runtime
  /// contexts.
  RestrictedReachability,
  /// The caller carries the privilege the subject's purpose presumes.
  AuthorizedAccess,
  /// On failure, the system is in a recoverable state.
  ErrorRecoverability,
  /// Inputs and read state are not attacker-controlled in a way that
  /// defeats the purpose.
  InputIntegrity,
  /// The value being read reflects the latest committed state relevant
  /// to the purpose.
  ValueFreshness,
  /// No interleaving operation observes inconsistent state across this
  /// point.
  AtomicConsistency,
  /// Shared resources remain available under expected use.
  ResourceAvailability,
  /// Genuinely novel assertion; description carries the structure.
  Other,
}

/// Non-pure subject type. Filter facet on the auditor UI; classifies
/// each subject by interaction-surface category for grouping and review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NonPureSubjectType {
  StateWrite,
  StateRead,
  ExternalCall,
  DelegateCall,
  InlineAssembly,
  Create,
}

/// Purity classification for a function call site. Determined by the callee's
/// observable side effects, not by whether the callee is external — an
/// external view function with no state effects is still `Pure`. Populated by
/// the analyzer's call-purity post-pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallKind {
  Pure,
  NonPure,
}

impl UnnamedTopicKind {
  /// Returns the purity classification for this unnamed topic kind.
  pub fn purity(&self) -> SubjectPurity {
    match self {
      UnnamedTopicKind::VariableMutation => SubjectPurity::NonPure,
      UnnamedTopicKind::InlineAssembly => SubjectPurity::NonPure,
      UnnamedTopicKind::NewExpression => SubjectPurity::NonPure,
      UnnamedTopicKind::FunctionCall(CallKind::NonPure) => {
        SubjectPurity::NonPure
      }
      UnnamedTopicKind::FunctionCall(CallKind::Pure) => SubjectPurity::Pure,
      _ => SubjectPurity::Pure,
    }
  }
}

impl NamedTopicKind {
  /// Returns the purity classification for this named topic kind.
  pub fn purity(&self) -> SubjectPurity {
    match self {
      NamedTopicKind::StateVariable(VariableMutability::Mutable) => {
        SubjectPurity::NonPure
      }
      _ => SubjectPurity::Pure,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NamedTopicVisibility {
  Public,
  Private,
  Internal,
  External,
}

/// Represents a reference to a topic, with type information about its source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reference {
  /// A reference from project analysis (solidity analyzer or documentation analyzer).
  ProjectReference {
    reference_topic: topic::Topic,
    sort_key: Option<usize>,
  },
  /// A project reference that is also targeted by one or more comment mentions.
  ProjectReferenceWithMentions {
    reference_topic: topic::Topic,
    mention_topics: Vec<topic::Topic>,
    sort_key: Option<usize>,
  },
  /// A reference from user comments only (not present in source code).
  CommentMention {
    reference_topic: topic::Topic,
    mention_topics: Vec<topic::Topic>,
    sort_key: Option<usize>,
  },
}

impl Reference {
  /// Returns the primary reference topic.
  pub fn reference_topic(&self) -> &topic::Topic {
    match self {
      Reference::ProjectReference {
        reference_topic, ..
      }
      | Reference::ProjectReferenceWithMentions {
        reference_topic, ..
      }
      | Reference::CommentMention {
        reference_topic, ..
      } => reference_topic,
    }
  }

  /// Returns the mention topics, if any.
  pub fn mention_topics(&self) -> Option<&[topic::Topic]> {
    match self {
      Reference::ProjectReference { .. } => None,
      Reference::ProjectReferenceWithMentions { mention_topics, .. }
      | Reference::CommentMention { mention_topics, .. } => {
        Some(mention_topics)
      }
    }
  }

  /// Returns the sort key (source location start) for ordering within a group.
  pub fn sort_key(&self) -> Option<usize> {
    match self {
      Reference::ProjectReference { sort_key, .. }
      | Reference::ProjectReferenceWithMentions { sort_key, .. }
      | Reference::CommentMention { sort_key, .. } => *sort_key,
    }
  }

  /// Creates a new ProjectReference.
  pub fn project_reference(
    reference_topic: topic::Topic,
    sort_key: Option<usize>,
  ) -> Self {
    Reference::ProjectReference {
      reference_topic,
      sort_key,
    }
  }

  /// Creates a new CommentMention.
  pub fn comment_mention(
    reference_topic: topic::Topic,
    mention_topic: topic::Topic,
    sort_key: Option<usize>,
  ) -> Self {
    Reference::CommentMention {
      reference_topic,
      mention_topics: vec![mention_topic],
      sort_key,
    }
  }
}

/// Organizes topics hierarchically by their source scope.
/// For Solidity: scope is a contract, scope_references are contract-level refs, nested_references are function-level refs.
/// For Documentation: scope is a file, scope_references are file-level refs, nested_references are section-level refs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceContext {
  /// The grouping scope where these references occur (contract for Solidity, file for documentation, feature for doc expanded context)
  scope: topic::Topic,
  /// Source location start for sorting groups relative to each other
  sort_key: Option<usize>,
  /// Whether this scope is defined in one of the audit's in-scope files
  is_in_scope: bool,
  /// References at the scope level (inheritance/using-for for Solidity, file-level for documentation)
  scope_references: Vec<Reference>,
  /// References within nested scopes (functions for Solidity, sections for documentation)
  nested_references: Vec<NestedSourceContext>,
}

impl SourceContext {
  pub fn new_with_scope_references(
    scope: topic::Topic,
    sort_key: Option<usize>,
    is_in_scope: bool,
    scope_references: Vec<Reference>,
  ) -> Self {
    SourceContext {
      scope,
      sort_key,
      is_in_scope,
      scope_references,
      nested_references: Vec::new(),
    }
  }

  pub fn scope(&self) -> &topic::Topic {
    &self.scope
  }

  pub fn sort_key(&self) -> Option<usize> {
    self.sort_key
  }

  pub fn is_in_scope(&self) -> bool {
    self.is_in_scope
  }

  pub fn scope_references(&self) -> &[Reference] {
    &self.scope_references
  }

  pub fn nested_references(&self) -> &[NestedSourceContext] {
    &self.nested_references
  }
}

/// A child element within a nested or annotated block source context.
/// Unifies references and annotated block groups into a single ordered list
/// so that correct linear source order is preserved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SourceChild {
  /// A direct reference to a topic at this level.
  Reference(Reference),
  /// A nested annotated block group (control flow body, unchecked, assembly, etc.).
  AnnotatedBlock(AnnotatedBlockSourceContext),
}

impl SourceChild {
  /// Returns the sort key for ordering children relative to each other.
  pub fn sort_key(&self) -> Option<usize> {
    match self {
      SourceChild::Reference(r) => r.sort_key(),
      SourceChild::AnnotatedBlock(a) => a.sort_key(),
    }
  }
}

/// Groups references within an annotated block (control flow body, unchecked, assembly, etc.).
/// Recursive to handle nesting (e.g. if inside for inside unchecked).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotatedBlockSourceContext {
  /// The block annotation that groups these references
  annotation: BlockAnnotation,
  /// Source location start for sorting groups relative to each other
  sort_key: Option<usize>,
  /// Ordered children (references and nested annotated blocks) in source order
  children: Vec<SourceChild>,
  /// Whether this If branch has a sibling branch (true body has false body, or vice versa)
  has_sibling_branch: bool,
}

impl AnnotatedBlockSourceContext {
  pub fn annotation(&self) -> &BlockAnnotation {
    &self.annotation
  }

  pub fn sort_key(&self) -> Option<usize> {
    self.sort_key
  }

  pub fn children(&self) -> &[SourceChild] {
    &self.children
  }

  pub fn has_sibling_branch(&self) -> bool {
    self.has_sibling_branch
  }
}

/// Groups references within a nested scope.
/// For Solidity: represents references within a function/modifier.
/// For Documentation: represents references within a section (component).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NestedSourceContext {
  /// The nested scope containing these references (function for Solidity, section for documentation)
  subscope: topic::Topic,
  /// Source location start for sorting nested groups relative to each other
  sort_key: Option<usize>,
  /// Ordered children (references and annotated block groups) in source order
  children: Vec<SourceChild>,
}

impl NestedSourceContext {
  pub fn new(
    subscope: topic::Topic,
    sort_key: Option<usize>,
    children: Vec<SourceChild>,
  ) -> Self {
    NestedSourceContext {
      subscope,
      sort_key,
      children,
    }
  }

  pub fn subscope(&self) -> &topic::Topic {
    &self.subscope
  }

  pub fn sort_key(&self) -> Option<usize> {
    self.sort_key
  }

  pub fn children(&self) -> &[SourceChild] {
    &self.children
  }
}

/// Merges a list of SourceContext entries, combining entries that share the
/// same scope into a single group with merged references.
pub fn merge_context_groups(
  contexts: Vec<SourceContext>,
) -> Vec<SourceContext> {
  let mut merged: Vec<SourceContext> = Vec::new();
  for ctx in contexts {
    ensure_context(&mut merged, ctx.scope, ctx.sort_key, ctx.is_in_scope);
    let group = merged.iter_mut().find(|g| g.scope == ctx.scope).unwrap();
    for r in ctx.scope_references {
      insert_ref_sorted(&mut group.scope_references, r);
    }
    for nested in ctx.nested_references {
      insert_nested_sorted(&mut group.nested_references, nested);
    }
  }
  merged
}

/// Inserts a NestedSourceContext into a sorted vec, merging children if the
/// subscope already exists.
fn insert_nested_sorted(
  nested_refs: &mut Vec<NestedSourceContext>,
  nested: NestedSourceContext,
) {
  if let Some(existing) = nested_refs
    .iter_mut()
    .find(|n| n.subscope == nested.subscope)
  {
    for child in nested.children {
      existing.children.push(child);
    }
  } else {
    let pos = nested_refs
      .binary_search_by(|n| n.sort_key.cmp(&nested.sort_key))
      .unwrap_or_else(|pos| pos);
    nested_refs.insert(pos, nested);
  }
}

/// Ensures a SourceContext exists for the given scope, creating one at the
/// correct sorted position if absent. Does not add any references.
pub fn ensure_context(
  groups: &mut Vec<SourceContext>,
  scope: topic::Topic,
  scope_sort_key: Option<usize>,
  is_in_scope: bool,
) {
  if groups.iter().any(|g| g.scope == scope) {
    return;
  }
  let pos = groups
    .binary_search_by(|g| g.sort_key.cmp(&scope_sort_key))
    .unwrap_or_else(|pos| pos);
  groups.insert(
    pos,
    SourceContext {
      scope,
      sort_key: scope_sort_key,
      is_in_scope,
      scope_references: Vec::new(),
      nested_references: Vec::new(),
    },
  );
}

/// Inserts a reference into a sorted, deduplicated Vec<SourceContext>.
///
/// Finds or creates the appropriate SourceContext (by scope topic) and, if a subscope
/// is provided, the appropriate NestedSourceContext. If an annotation chain is provided,
/// the reference is nested within recursive AnnotatedBlockSourceContext(s) inside the
/// NestedSourceContext.
///
/// Inserts the reference at the correct sorted position. Skips insertion if a reference
/// with the same reference_topic already exists at that level.
pub fn insert_into_context(
  groups: &mut Vec<SourceContext>,
  scope: topic::Topic,
  scope_sort_key: Option<usize>,
  is_in_scope: bool,
  subscope: Option<(topic::Topic, Option<usize>)>,
  annotation_chain: &[BlockAnnotation],
  reference: Reference,
) {
  // Ensure the context exists
  ensure_context(groups, scope, scope_sort_key, is_in_scope);

  // We know the group exists now — find it
  let group = groups.iter_mut().find(|g| g.scope == scope).unwrap();

  match subscope {
    None => {
      // Insert into scope_references with dedup (no control flow at scope level)
      insert_ref_sorted(&mut group.scope_references, reference);
    }
    Some((subscope_topic, subscope_sort_key)) => {
      // Find or create the NestedSourceContext for this subscope
      if !group
        .nested_references
        .iter()
        .any(|n| n.subscope == subscope_topic)
      {
        let pos = group
          .nested_references
          .binary_search_by(|n| n.sort_key.cmp(&subscope_sort_key))
          .unwrap_or_else(|pos| pos);
        group.nested_references.insert(
          pos,
          NestedSourceContext {
            subscope: subscope_topic,
            sort_key: subscope_sort_key,
            children: Vec::new(),
          },
        );
      }

      let nested = group
        .nested_references
        .iter_mut()
        .find(|n| n.subscope == subscope_topic)
        .unwrap();

      if annotation_chain.is_empty() {
        insert_child_ref(&mut nested.children, reference);
      } else {
        // Walk the annotation chain, creating/finding groups at each level
        let target_children = find_or_create_annotation_context(
          &mut nested.children,
          annotation_chain,
        );
        insert_child_ref(target_children, reference);
      }
    }
  }
}

/// Walks an annotation chain, creating or finding `AnnotatedBlockSourceContext`s at
/// each level, and returns a mutable reference to the `children` vec at the final level.
fn find_or_create_annotation_context<'a>(
  children: &'a mut Vec<SourceChild>,
  chain: &[BlockAnnotation],
) -> &'a mut Vec<SourceChild> {
  assert!(!chain.is_empty());

  let ann = &chain[0];

  // Find or create the group for this annotation (matched by topic + kind)
  let exists = children.iter().any(|c| {
    matches!(
      c,
      SourceChild::AnnotatedBlock(g)
        if g.annotation.topic == ann.topic && g.annotation.kind == ann.kind
    )
  });

  if !exists {
    // When inserting an If branch, check if the sibling branch already exists
    let has_sibling = matches!(ann.kind, BlockAnnotationKind::If(_))
      && children.iter().any(|c| {
        matches!(
          c,
          SourceChild::AnnotatedBlock(g)
            if g.annotation.topic == ann.topic && g.annotation.kind != ann.kind
        )
      });

    let sort_key = Some(ann.topic.numeric_id() as usize);
    let pos = children
      .binary_search_by(|c| c.sort_key().cmp(&sort_key))
      .unwrap_or_else(|pos| pos);
    children.insert(
      pos,
      SourceChild::AnnotatedBlock(AnnotatedBlockSourceContext {
        annotation: ann.clone(),
        sort_key,
        children: Vec::new(),
        has_sibling_branch: has_sibling,
      }),
    );

    // If we found a sibling, mark the existing sibling too
    if has_sibling {
      for child in children.iter_mut() {
        if let SourceChild::AnnotatedBlock(g) = child
          && g.annotation.topic == ann.topic
          && g.annotation.kind != ann.kind
        {
          g.has_sibling_branch = true;
          break;
        }
      }
    }
  }

  let group = children
    .iter_mut()
    .find_map(|c| match c {
      SourceChild::AnnotatedBlock(g)
        if g.annotation.topic == ann.topic && g.annotation.kind == ann.kind =>
      {
        Some(g)
      }
      _ => None,
    })
    .unwrap();

  if chain.len() == 1 {
    &mut group.children
  } else {
    find_or_create_annotation_context(&mut group.children, &chain[1..])
  }
}

/// Merges `incoming` into `existing` when they share the same reference_topic.
///
/// Merge rules:
/// - ProjectReference + ProjectReference → skip (already present)
/// - CommentMention + CommentMention → merge mention_topics
/// - ProjectReference + CommentMention → promote to ProjectReferenceWithMentions
/// - ProjectReferenceWithMentions + CommentMention → merge mention_topics
/// - CommentMention + ProjectReference → promote to ProjectReferenceWithMentions
fn merge_reference(existing: &mut Reference, incoming: &Reference) {
  let ref_topic = *existing.reference_topic();

  match (&mut *existing, incoming) {
    // ProjectReference + ProjectReference → already present, skip
    (
      Reference::ProjectReference { .. },
      Reference::ProjectReference { .. },
    ) => {}

    // ProjectReference + CommentMention → promote to ProjectReferenceWithMentions
    (
      existing_ref @ Reference::ProjectReference { .. },
      Reference::CommentMention { mention_topics, .. },
    ) => {
      let sort_key = existing_ref.sort_key();
      *existing_ref = Reference::ProjectReferenceWithMentions {
        reference_topic: ref_topic,
        mention_topics: mention_topics.clone(),
        sort_key,
      };
    }

    // ProjectReferenceWithMentions + CommentMention → merge mention_topics
    (
      Reference::ProjectReferenceWithMentions {
        mention_topics: existing_mentions,
        ..
      },
      Reference::CommentMention {
        mention_topics: new_mentions,
        ..
      },
    ) => {
      for mt in new_mentions {
        if !existing_mentions.contains(mt) {
          existing_mentions.push(*mt);
        }
      }
    }

    // CommentMention + CommentMention → merge mention_topics
    (
      Reference::CommentMention {
        mention_topics: existing_mentions,
        ..
      },
      Reference::CommentMention {
        mention_topics: new_mentions,
        ..
      },
    ) => {
      for mt in new_mentions {
        if !existing_mentions.contains(mt) {
          existing_mentions.push(*mt);
        }
      }
    }

    // CommentMention + ProjectReference → promote to ProjectReferenceWithMentions
    (
      existing_ref @ Reference::CommentMention { .. },
      Reference::ProjectReference { .. },
    ) => {
      let sort_key = existing_ref.sort_key();
      let mention_topics = existing_ref.mention_topics().unwrap().to_vec();
      *existing_ref = Reference::ProjectReferenceWithMentions {
        reference_topic: ref_topic,
        mention_topics,
        sort_key,
      };
    }

    // All other combinations with ProjectReferenceWithMentions as the incoming
    // reference shouldn't occur in practice, but handle gracefully
    _ => {}
  }
}

/// Inserts a reference into a sorted Vec<Reference>, merging by reference_topic.
/// Used for SourceContext.scope_references which remains Vec<Reference>.
fn insert_ref_sorted(refs: &mut Vec<Reference>, reference: Reference) {
  if let Some(existing) = refs
    .iter_mut()
    .find(|r| *r.reference_topic() == *reference.reference_topic())
  {
    merge_reference(existing, &reference);
    return;
  }

  let sort_key = reference.sort_key();
  let pos = refs
    .binary_search_by(|r| r.sort_key().cmp(&sort_key))
    .unwrap_or_else(|pos| pos);
  refs.insert(pos, reference);
}

/// Inserts a Reference as a SourceChild into a sorted children list,
/// merging by reference_topic if a matching Reference already exists.
fn insert_child_ref(children: &mut Vec<SourceChild>, reference: Reference) {
  if let Some(existing) = children.iter_mut().find_map(|c| match c {
    SourceChild::Reference(r)
      if *r.reference_topic() == *reference.reference_topic() =>
    {
      Some(r)
    }
    _ => None,
  }) {
    merge_reference(existing, &reference);
    return;
  }

  let sort_key = reference.sort_key();
  let pos = children
    .binary_search_by(|c| c.sort_key().cmp(&sort_key))
    .unwrap_or_else(|pos| pos);
  children.insert(pos, SourceChild::Reference(reference));
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TopicMetadata {
  NamedTopic {
    topic: topic::Topic,
    scope: Scope,
    kind: NamedTopicKind,
    name: String,
    visibility: NamedTopicVisibility,
    /// Whether this topic has mutations (was previously NamedMutableTopic)
    is_mutable: bool,
    /// The assignment or unary operation nodes that mutate this variable.
    /// Empty for non-mutable topics.
    mutations: Vec<topic::Topic>,
    /// Variables that are true ancestors of this variable
    /// Only populated for variable declarations.
    ancestors: Vec<topic::Topic>,
    /// Variables whose values are derived from this variable.
    /// Only populated for variable declarations.
    descendants: Vec<topic::Topic>,
    /// Variables that are related to this variable:
    ///   1. Appear together in comparison, arithmetic, or bitwise binary operations
    ///   2. Appear as alternatives in conditional (ternary) expressions
    ///   3. Are involved in this variable's assignment (RHS of assignments)
    ///
    /// Only populated for variable declarations.
    relatives: Vec<topic::Topic>,
    /// When set, this declaration is a transparent proxy for another declaration.
    /// Features should resolve through this to the target topic instead of
    /// operating on this declaration directly. The canonical case is an interface
    /// member with exactly one in-scope implementation — the interface member is
    /// transitive to the implementation member.
    transitive_topic: Option<topic::Topic>,
    /// Documentation topics that reference this declaration via inline code
    /// references. Populated by documentation analyzer at startup.
    doc_references: Vec<topic::Topic>,
  },
  UnnamedTopic {
    topic: topic::Topic,
    scope: Scope,
    kind: UnnamedTopicKind,
    /// When set, this topic is a transparent proxy for another topic.
    /// The canonical case is a semantic block containing exactly one statement.
    transitive_topic: Option<topic::Topic>,
  },
  /// A control flow statement (if/for/while/do-while) with its condition topic.
  ControlFlow {
    topic: topic::Topic,
    scope: Scope,
    kind: ControlFlowStatementKind,
    /// The condition expression topic.
    condition: topic::Topic,
  },
  /// A topic with a title (like documentation sections) but not a full declaration
  TitledTopic {
    topic: topic::Topic,
    scope: Scope,
    kind: TitledTopicKind,
    title: String,
  },
  /// A comment topic with immutable metadata
  CommentTopic {
    topic: topic::Topic,
    scope: Scope,
    target_topic: topic::Topic,
    comment_type: CommentType,
    author: crate::collaborator::models::Author,
    created_at: String,
    mentioned_topics: Vec<topic::Topic>,
  },
  /// A feature extracted from documentation
  FeatureTopic {
    topic: topic::Topic,
    name: String,
    description: String,
    author: crate::collaborator::models::Author,
    /// `None` for pipeline-produced entities — the per-batch
    /// `generated_at` on the audit report already locates them in time.
    /// `Some` when authored by a user or a server-side agent.
    created_at: Option<String>,
  },
  /// A behavioral requirement extracted from documentation. Links to features
  /// are in feature_requirement_links.
  RequirementTopic {
    topic: topic::Topic,
    description: String,
    /// The D-prefixed documentation section this requirement was extracted from
    section_topic: topic::Topic,
    author: crate::collaborator::models::Author,
    /// `None` for pipeline-produced entities — see FeatureTopic for rationale.
    created_at: Option<String>,
  },
  /// A behavior observed during code review, belonging to one code member.
  BehaviorTopic {
    topic: topic::Topic,
    description: String,
    /// The N-prefixed code member (function/modifier/contract) this behavior belongs to
    member_topic: topic::Topic,
    author: crate::collaborator::models::Author,
    /// `None` for pipeline-produced entities — see FeatureTopic for rationale.
    created_at: Option<String>,
  },
  /// A functional semantic — what a code declaration represents in the context
  /// of the project. Derived from one or more documentation sections.
  FunctionalSemanticTopic {
    topic: topic::Topic,
    /// The semantic meaning text (e.g., "proportional reward multiplier").
    description: String,
    /// The N-prefixed code declaration this semantic describes.
    declaration_topic: topic::Topic,
    /// D-prefixed documentation topics this semantic was derived from.
    documentation_topics: Vec<topic::Topic>,
    author: crate::collaborator::models::Author,
    /// `None` for pipeline-produced entities — see FeatureTopic for rationale.
    created_at: Option<String>,
    /// Provenance: which workflow variant produced the underlying match.
    /// `None` for entries authored by humans (only pipeline-produced
    /// semantics carry a match source).
    #[serde(default)]
    match_source: Option<MatchSource>,
  },
  /// A functional purpose — the business-logic reason a non-pure subject
  /// exists, derived from the feature it belongs to. Sibling of
  /// `PlacementRationaleTopic`; both are generated together in pipeline
  /// step 5 and persist independently for granular review.
  FunctionalPurposeTopic {
    topic: topic::Topic,
    /// Why this subject exists in business terms.
    description: String,
    /// The non-pure subject this purpose is on.
    subject_topic: topic::Topic,
    author: crate::collaborator::models::Author,
    /// `None` for pipeline-produced entities — see FeatureTopic for rationale.
    created_at: Option<String>,
  },
  /// A placement rationale — the ordering reason a non-pure subject is at
  /// this point in its containing function rather than earlier or later.
  /// Sibling of `FunctionalPurposeTopic`.
  PlacementRationaleTopic {
    topic: topic::Topic,
    /// Why this subject is here, in terms of neighboring operations.
    description: String,
    /// The non-pure subject this placement rationale is on.
    subject_topic: topic::Topic,
    author: crate::collaborator::models::Author,
    /// `None` for pipeline-produced entities — see FeatureTopic for rationale.
    created_at: Option<String>,
  },
  /// A threat on a non-pure source code subject
  ThreatTopic {
    topic: topic::Topic,
    description: String,
    /// The non-pure subject this threat belongs to
    subject_topic: topic::Topic,
    author: crate::collaborator::models::Author,
    created_at: String,
    /// Severity is assigned during impact analysis; None means pending
    severity: Option<ThreatSeverity>,
  },
  /// A condition — an assertion that must hold for the non-pure subject's
  /// functional purpose and placement rationale to be fulfilled.
  /// Generated in pipeline step 6 from the subject's functional purpose
  /// and placement rationale. Each assertion is its own ConditionTopic;
  /// subjects typically have multiple. Step 7 (threats) generates
  /// adversarial scenarios that falsify these assertions; an auditor
  /// disagreeing with a threat does not invalidate the underlying
  /// assertion. See SPEC's "Conditions vs. Invariants" for the role
  /// distinction with InvariantTopic.
  ConditionTopic {
    topic: topic::Topic,
    /// The assertion, in prose, phrased affirmatively ("X holds," "the
    /// caller is …", "the value reflects …"). One thing the auditor can
    /// agree or disagree with independently.
    description: String,
    /// The non-pure subject whose purpose+placement this assertion
    /// supports.
    subject_topic: topic::Topic,
    /// Category of assertion this condition expresses.
    kind: ConditionKind,
    /// Topic IDs the LLM cited as justifying the assertion. May include
    /// subject siblings, called functions, declarations the function uses,
    /// or documentation topics. Validated for well-formedness only in this
    /// work; cross-pipeline rendered-context validation is a later
    /// refinement.
    evidence_topics: Vec<topic::Topic>,
    author: crate::collaborator::models::Author,
    /// `None` for pipeline-produced entities — see FeatureTopic for
    /// rationale. (Following the FunctionalPurposeTopic pattern; not the
    /// ThreatTopic/InvariantTopic non-Option shape.)
    created_at: Option<String>,
  },
  /// An invariant that must hold to prevent a threat
  InvariantTopic {
    topic: topic::Topic,
    description: String,
    threat_topic: topic::Topic,
    author: crate::collaborator::models::Author,
    created_at: String,
    /// Inherited from parent threat; None when threat severity is pending
    severity: Option<ThreatSeverity>,
  },
  /// A documentation root topic (project or technical documentation)
  DocumentationTopic {
    topic: topic::Topic,
    scope: Scope,
    is_technical: bool,
  },
}

impl TopicMetadata {
  pub fn scope(&self) -> &Scope {
    match self {
      TopicMetadata::NamedTopic { scope, .. }
      | TopicMetadata::UnnamedTopic { scope, .. }
      | TopicMetadata::ControlFlow { scope, .. }
      | TopicMetadata::TitledTopic { scope, .. }
      | TopicMetadata::CommentTopic { scope, .. }
      | TopicMetadata::DocumentationTopic { scope, .. } => scope,
      TopicMetadata::FeatureTopic { .. }
      | TopicMetadata::RequirementTopic { .. }
      | TopicMetadata::BehaviorTopic { .. }
      | TopicMetadata::FunctionalSemanticTopic { .. }
      | TopicMetadata::FunctionalPurposeTopic { .. }
      | TopicMetadata::PlacementRationaleTopic { .. }
      | TopicMetadata::ConditionTopic { .. }
      | TopicMetadata::ThreatTopic { .. }
      | TopicMetadata::InvariantTopic { .. } => &Scope::Global,
    }
  }

  pub fn name(&self) -> Option<&str> {
    match self {
      TopicMetadata::NamedTopic { name, .. }
      | TopicMetadata::FeatureTopic { name, .. } => Some(name),
      TopicMetadata::TitledTopic { title, .. } => Some(title),
      TopicMetadata::UnnamedTopic { .. }
      | TopicMetadata::ControlFlow { .. }
      | TopicMetadata::CommentTopic { .. }
      | TopicMetadata::RequirementTopic { .. }
      | TopicMetadata::BehaviorTopic { .. }
      | TopicMetadata::FunctionalSemanticTopic { .. }
      | TopicMetadata::FunctionalPurposeTopic { .. }
      | TopicMetadata::PlacementRationaleTopic { .. }
      | TopicMetadata::ConditionTopic { .. }
      | TopicMetadata::ThreatTopic { .. }
      | TopicMetadata::InvariantTopic { .. }
      | TopicMetadata::DocumentationTopic { .. } => None,
    }
  }

  pub fn topic(&self) -> &topic::Topic {
    match self {
      TopicMetadata::NamedTopic { topic, .. }
      | TopicMetadata::UnnamedTopic { topic, .. }
      | TopicMetadata::ControlFlow { topic, .. }
      | TopicMetadata::TitledTopic { topic, .. }
      | TopicMetadata::CommentTopic { topic, .. }
      | TopicMetadata::FeatureTopic { topic, .. }
      | TopicMetadata::RequirementTopic { topic, .. }
      | TopicMetadata::BehaviorTopic { topic, .. }
      | TopicMetadata::FunctionalSemanticTopic { topic, .. }
      | TopicMetadata::FunctionalPurposeTopic { topic, .. }
      | TopicMetadata::PlacementRationaleTopic { topic, .. }
      | TopicMetadata::ConditionTopic { topic, .. }
      | TopicMetadata::ThreatTopic { topic, .. }
      | TopicMetadata::InvariantTopic { topic, .. }
      | TopicMetadata::DocumentationTopic { topic, .. } => topic,
    }
  }

  pub fn ancestors(&self) -> &[topic::Topic] {
    match self {
      TopicMetadata::NamedTopic { ancestors, .. } => ancestors,
      _ => &[],
    }
  }

  /// When set, this topic is a transparent proxy for another topic. Features
  /// should resolve through to the target instead of operating on this topic
  /// directly. For example, an interface function with exactly one in-scope
  /// implementation is transitive to the implementation function.
  pub fn transitive_topic(&self) -> Option<&topic::Topic> {
    match self {
      TopicMetadata::NamedTopic {
        transitive_topic, ..
      } => transitive_topic.as_ref(),
      TopicMetadata::UnnamedTopic {
        transitive_topic, ..
      } => transitive_topic.as_ref(),
      _ => None,
    }
  }

  pub fn descendants(&self) -> &[topic::Topic] {
    match self {
      TopicMetadata::NamedTopic { descendants, .. } => descendants,
      _ => &[],
    }
  }

  pub fn relatives(&self) -> &[topic::Topic] {
    match self {
      TopicMetadata::NamedTopic { relatives, .. } => relatives,
      _ => &[],
    }
  }

  pub fn mutations(&self) -> &[topic::Topic] {
    match self {
      TopicMetadata::NamedTopic { mutations, .. } => mutations,
      _ => &[],
    }
  }

  pub fn is_mutable(&self) -> bool {
    match self {
      TopicMetadata::NamedTopic { is_mutable, .. } => *is_mutable,
      _ => false,
    }
  }

  pub fn target_topic(&self) -> Option<&topic::Topic> {
    match self {
      TopicMetadata::CommentTopic { target_topic, .. } => Some(target_topic),
      TopicMetadata::ThreatTopic { subject_topic, .. } => Some(subject_topic),
      TopicMetadata::FunctionalPurposeTopic { subject_topic, .. } => {
        Some(subject_topic)
      }
      TopicMetadata::PlacementRationaleTopic { subject_topic, .. } => {
        Some(subject_topic)
      }
      TopicMetadata::ConditionTopic { subject_topic, .. } => {
        Some(subject_topic)
      }
      TopicMetadata::InvariantTopic { threat_topic, .. } => Some(threat_topic),
      _ => None,
    }
  }

  pub fn author(&self) -> Option<crate::collaborator::models::Author> {
    match self {
      TopicMetadata::CommentTopic { author, .. }
      | TopicMetadata::FeatureTopic { author, .. }
      | TopicMetadata::RequirementTopic { author, .. }
      | TopicMetadata::BehaviorTopic { author, .. }
      | TopicMetadata::FunctionalSemanticTopic { author, .. }
      | TopicMetadata::FunctionalPurposeTopic { author, .. }
      | TopicMetadata::PlacementRationaleTopic { author, .. }
      | TopicMetadata::ConditionTopic { author, .. }
      | TopicMetadata::ThreatTopic { author, .. }
      | TopicMetadata::InvariantTopic { author, .. } => Some(*author),
      _ => None,
    }
  }

  pub fn author_id(&self) -> Option<i64> {
    self.author().map(|a| a.as_i64())
  }

  /// Returns the description text for variants that have one. Maps to
  /// `description` for generated/threat/invariant variants and to the
  /// feature's `description` for `FeatureTopic`.
  pub fn description(&self) -> Option<&str> {
    match self {
      TopicMetadata::FeatureTopic { description, .. }
      | TopicMetadata::RequirementTopic { description, .. }
      | TopicMetadata::BehaviorTopic { description, .. }
      | TopicMetadata::FunctionalSemanticTopic { description, .. }
      | TopicMetadata::FunctionalPurposeTopic { description, .. }
      | TopicMetadata::PlacementRationaleTopic { description, .. }
      | TopicMetadata::ConditionTopic { description, .. }
      | TopicMetadata::ThreatTopic { description, .. }
      | TopicMetadata::InvariantTopic { description, .. } => {
        Some(description.as_str())
      }
      _ => None,
    }
  }

  pub fn comment_type(&self) -> Option<&CommentType> {
    match self {
      TopicMetadata::CommentTopic { comment_type, .. } => Some(comment_type),
      _ => None,
    }
  }

  pub fn created_at(&self) -> Option<&str> {
    match self {
      TopicMetadata::CommentTopic { created_at, .. }
      | TopicMetadata::ThreatTopic { created_at, .. }
      | TopicMetadata::InvariantTopic { created_at, .. } => {
        Some(created_at.as_str())
      }
      TopicMetadata::FeatureTopic { created_at, .. }
      | TopicMetadata::RequirementTopic { created_at, .. }
      | TopicMetadata::BehaviorTopic { created_at, .. }
      | TopicMetadata::FunctionalSemanticTopic { created_at, .. }
      | TopicMetadata::FunctionalPurposeTopic { created_at, .. }
      | TopicMetadata::PlacementRationaleTopic { created_at, .. }
      | TopicMetadata::ConditionTopic { created_at, .. } => {
        created_at.as_deref()
      }
      _ => None,
    }
  }

  /// Returns the qualified name of the declaration, or None for unnamed topics.
  /// Format: component.member.name, component.name, or name
  /// Uses the declaration names from the scope components, falling back to topic IDs if not found.
  pub fn qualified_name(&self, audit_data: &AuditData) -> Option<String> {
    let name = self.name()?;
    Some(match &self.scope() {
      Scope::Global | Scope::Container { .. } => name.to_string(),
      Scope::Component { component, .. } => {
        let component_name = audit_data
          .topic_metadata
          .get(component)
          .and_then(|d| d.name())
          .map(|s| s.to_string())
          .unwrap_or_else(|| component.id());
        format!("{}.{}", component_name, name)
      }
      Scope::Member {
        component, member, ..
      }
      | Scope::ContainingBlock {
        component, member, ..
      } => {
        let component_name = audit_data
          .topic_metadata
          .get(component)
          .and_then(|d| d.name())
          .map(|s| s.to_string())
          .unwrap_or_else(|| component.id());
        let member_name = audit_data
          .topic_metadata
          .get(member)
          .and_then(|d| d.name())
          .map(|s| s.to_string())
          .unwrap_or_else(|| member.id());
        format!("{}.{}.{}", component_name, member_name, name)
      }
    })
  }
}

/// Resolve a topic through its transitive chain to the canonical target.
/// Returns the original topic if it has no transitive relationship.
/// Follows the chain until a non-transitive topic is found.
///
/// Use this whenever looking up comments or other per-topic data to ensure
/// signature nodes, single-statement semantic blocks, and other transitive
/// proxies redirect to their canonical declaration.
pub fn resolve_transitive_topic(
  topic: &topic::Topic,
  topic_metadata: &BTreeMap<topic::Topic, TopicMetadata>,
) -> topic::Topic {
  let mut current = *topic;
  let mut visited = HashSet::new();
  while let Some(meta) = topic_metadata.get(&current) {
    if !visited.insert(current) {
      break; // cycle guard
    }
    match meta.transitive_topic() {
      Some(next) => current = *next,
      None => break,
    }
  }
  current
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FunctionModProperties {
  FunctionProperties {
    reverts: Vec<RevertInfo>,
    /// Transitive union of `reverts` plus the `effective_reverts` of
    /// every non-try callee (resolved through proxies). Computed over
    /// the *non-try propagation graph* — try-call sites are excluded
    /// because try/catch absorbs them. Populated by
    /// `effective_properties::compute_transitive_effects` at the tail
    /// of the analyzer pass.
    #[serde(default)]
    effective_reverts: Vec<EffectiveRevert>,
    calls: Vec<CallInfo>,
    mutations: Vec<topic::Topic>,
    /// Transitive union of `mutations` plus the `effective_mutations`
    /// of every callee (resolved through proxies). Computed over the
    /// *full call graph* — try-call sites are INCLUDED, since
    /// try/catch doesn't suppress state changes from successful
    /// callees, only catches reverts from failing ones.
    #[serde(default)]
    effective_mutations: Vec<EffectiveTopic>,
    /// Variable references whose value is consumed (read) by this
    /// function. The LHS base of a pure assignment (`x = ...`) and of
    /// `delete x` are excluded so write-only statements appear only
    /// in `mutations`; compound assignments (`x +=`) and `++`/`--`
    /// surface the operand in both. Populated alongside `mutations`
    /// by the first-pass reference walker; consumers (e.g. the
    /// agent-context renderer) filter to state-variable kind.
    #[serde(default)]
    reads: Vec<topic::Topic>,
    /// Transitive union of `reads` plus the `effective_reads` of
    /// every callee. Same propagation graph as `effective_mutations`
    /// — try doesn't suppress reads from a successful callee.
    #[serde(default)]
    effective_reads: Vec<EffectiveTopic>,
    /// Events this function emits, sorted ascending by topic ID and
    /// deduped. Populated by the first-pass `EmitStatement` walker.
    #[serde(default)]
    events_emitted: Vec<topic::Topic>,
    /// Transitive union of `events_emitted` plus the
    /// `effective_events_emitted` of every callee. Same propagation
    /// graph as `effective_mutations` — try doesn't suppress events
    /// from a successful callee.
    #[serde(default)]
    effective_events_emitted: Vec<EffectiveTopic>,
  },
  ModifierProperties {
    reverts: Vec<RevertInfo>,
    /// Same shape and semantics as `FunctionProperties::effective_reverts`.
    #[serde(default)]
    effective_reverts: Vec<EffectiveRevert>,
    calls: Vec<CallInfo>,
    mutations: Vec<topic::Topic>,
    /// Same shape and semantics as `FunctionProperties::effective_mutations`.
    #[serde(default)]
    effective_mutations: Vec<EffectiveTopic>,
    /// Variable references whose value is consumed (read) by this
    /// modifier. Same shape and semantics as
    /// `FunctionProperties::reads`.
    #[serde(default)]
    reads: Vec<topic::Topic>,
    /// Same shape and semantics as `FunctionProperties::effective_reads`.
    #[serde(default)]
    effective_reads: Vec<EffectiveTopic>,
    /// Events this modifier emits, sorted ascending by topic ID and
    /// deduped. Populated by the first-pass `EmitStatement` walker.
    #[serde(default)]
    events_emitted: Vec<topic::Topic>,
    /// Same shape and semantics as `FunctionProperties::effective_events_emitted`.
    #[serde(default)]
    effective_events_emitted: Vec<EffectiveTopic>,
  },
}

#[derive(
  Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
/// This type represents a path within a project, making sure that it is
/// a relative path to the project root.
pub struct ProjectPath {
  pub file_path: String,
}

pub fn new_project_path(
  file_path: &String,
  project_root: &Path,
) -> ProjectPath {
  new_project_path_from_path(Path::new(file_path), project_root)
}

pub fn new_project_path_from_path(
  file_path: &Path,
  project_root: &Path,
) -> ProjectPath {
  // Convert relative paths to absolute by joining with project root
  let absolute_path = if file_path.is_relative() {
    project_root.join(file_path)
  } else {
    file_path.to_path_buf()
  };

  // Normalize the path by removing "." and ".." components
  let normalized = normalize_path(&absolute_path);

  // Strip the project root prefix to get a clean relative path
  let relative_path = normalized
    .strip_prefix(project_root)
    .unwrap_or(&normalized)
    .to_string_lossy()
    .to_string();

  ProjectPath {
    file_path: relative_path,
  }
}

pub fn project_path_to_absolute_path(
  project_path: &ProjectPath,
  project_root: &Path,
) -> PathBuf {
  project_root.join(&project_path.file_path)
}

/// Normalizes a path by resolving "." and ".." components
/// This is similar to canonicalize but doesn't require the path to exist
fn normalize_path(path: &Path) -> PathBuf {
  let mut components = Vec::new();

  for component in path.components() {
    match component {
      std::path::Component::CurDir => {
        // Skip "." components
      }
      std::path::Component::ParentDir => {
        // Remove the last component for ".."
        if !components.is_empty() {
          components.pop();
        }
      }
      _ => {
        // Add normal components (RootDir, Prefix, Normal)
        components.push(component);
      }
    }
  }

  components.iter().collect()
}

/// Errors produced by the project configuration loaders (`scope.txt`,
/// `documents.txt`, `name.txt`, `security.md`).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
  #[error("{file} not found in project root")]
  MissingFile { file: &'static str },
  #[error("failed to read {file}: {source}")]
  Io {
    file: &'static str,
    #[source]
    source: std::io::Error,
  },
  #[error("{file}: {reason}")]
  Invalid { file: &'static str, reason: String },
}

pub fn load_in_scope_files(
  project_root: &Path,
) -> Result<HashSet<ProjectPath>, ConfigError> {
  let scope_file = project_root.join("scope.txt");
  if !scope_file.exists() {
    return Err(ConfigError::MissingFile { file: "scope.txt" });
  }

  let content =
    std::fs::read_to_string(&scope_file).map_err(|e| ConfigError::Io {
      file: "scope.txt",
      source: e,
    })?;

  let mut in_scope_files = HashSet::new();
  for line in content.lines() {
    let line = line.trim();
    if !line.is_empty() {
      let project_path = new_project_path(&line.to_string(), project_root);
      in_scope_files.insert(project_path);
    }
  }

  Ok(in_scope_files)
}

/// A document file entry from documents.txt, with its technical flag.
#[derive(Debug, Clone)]
pub struct DocumentFileEntry {
  pub project_path: ProjectPath,
  pub is_technical: bool,
}

/// Reads "documents.txt" from the project root and returns an ordered list
/// of document file entries. Order matters — documents are parsed in this order
/// to produce deterministic node IDs. New documents should be appended to the
/// end of the file to preserve existing IDs.
///
/// Lines prefixed with "technical:" indicate technical documentation.
/// Plain lines are project documentation.
pub fn load_document_files(
  project_root: &Path,
) -> Result<Vec<DocumentFileEntry>, ConfigError> {
  let doc_file = project_root.join("documents.txt");
  if !doc_file.exists() {
    return Err(ConfigError::MissingFile {
      file: "documents.txt",
    });
  }

  let content =
    std::fs::read_to_string(&doc_file).map_err(|e| ConfigError::Io {
      file: "documents.txt",
      source: e,
    })?;

  let mut document_files = Vec::new();
  for line in content.lines() {
    let line = line.trim();
    if !line.is_empty() {
      let (path_str, is_technical) =
        if let Some(path) = line.strip_prefix("technical:") {
          (path.trim().to_string(), true)
        } else {
          (line.to_string(), false)
        };
      let project_path = new_project_path(&path_str, project_root);
      document_files.push(DocumentFileEntry {
        project_path,
        is_technical,
      });
    }
  }

  Ok(document_files)
}

/// Reads the first line of the "name.txt" file in the project root
pub fn load_audit_name(project_root: &Path) -> Result<String, ConfigError> {
  let name_file = project_root.join("name.txt");
  if !name_file.exists() {
    return Err(ConfigError::MissingFile { file: "name.txt" });
  }

  let content =
    std::fs::read_to_string(&name_file).map_err(|e| ConfigError::Io {
      file: "name.txt",
      source: e,
    })?;

  let audit_name = content
    .lines()
    .next()
    .ok_or_else(|| ConfigError::Invalid {
      file: "name.txt",
      reason: "file is empty".to_string(),
    })?
    .trim()
    .to_string();

  if audit_name.is_empty() {
    return Err(ConfigError::Invalid {
      file: "name.txt",
      reason: "first line is empty".to_string(),
    });
  }

  Ok(audit_name)
}

/// Reads "security.md" from the project root and returns its contents.
/// This file contains free-form prose describing roles, known threats,
/// invariants, and other security considerations for the audit.
/// Returns `None` if the file does not exist (security notes are optional).
pub fn load_security_notes(
  project_root: &Path,
) -> Result<Option<String>, ConfigError> {
  let security_file = project_root.join("security.md");
  if !security_file.exists() {
    return Err(ConfigError::MissingFile {
      file: "security.md",
    });
  }

  let content =
    std::fs::read_to_string(&security_file).map_err(|e| ConfigError::Io {
      file: "security.md",
      source: e,
    })?;

  let trimmed = content.trim();
  if trimmed.is_empty() {
    return Ok(None);
  }

  Ok(Some(trimmed.to_string()))
}

/// Builds nested references for invariants under their parent threats.
/// Each threat with invariants becomes a NestedSourceContext (subscope = threat topic)
/// containing only the invariant references as children.
/// The threat itself is expected to be in scope_references already.
fn build_invariant_nested_refs(
  threat_topics: &[topic::Topic],
  threats: &std::collections::BTreeMap<topic::Topic, Threat>,
) -> Vec<NestedSourceContext> {
  let mut nested = Vec::new();
  for tt in threat_topics {
    let threat = match threats.get(tt) {
      Some(t) if !t.invariant_topics.is_empty() => t,
      _ => continue,
    };
    let sort_key = Some(tt.numeric_id() as usize);
    let children = threat
      .invariant_topics
      .iter()
      .map(|inv_topic| {
        let inv_sort_key = Some(inv_topic.numeric_id() as usize);
        SourceChild::Reference(Reference::ProjectReference {
          reference_topic: *inv_topic,
          sort_key: inv_sort_key,
        })
      })
      .collect();
    nested.push(NestedSourceContext {
      subscope: *tt,
      sort_key,
      children,
    });
  }
  nested
}

/// Collect the semantic text strings for a single declaration by resolving
/// through `declaration_semantics` (decl → P-topics) and reading each
/// P-topic's `description` from `topic_metadata`.
pub fn semantic_texts_for_declaration(
  audit_data: &AuditData,
  decl_topic: &topic::Topic,
) -> Vec<String> {
  let Some(sem_topics) = audit_data.declaration_semantics.get(decl_topic)
  else {
    return Vec::new();
  };
  sem_topics
    .iter()
    .filter_map(|sem_topic| {
      if let Some(TopicMetadata::FunctionalSemanticTopic {
        description, ..
      }) = audit_data.topic_metadata.get(sem_topic)
      {
        Some(description.clone())
      } else {
        None
      }
    })
    .collect()
}

/// Build a lookup map from declaration topic to the semantic text strings
/// describing it. Resolves through `declaration_semantics` (decl → P-topics)
/// and reads each P-topic's `description` from `topic_metadata`.
pub fn semantic_texts_by_declaration(
  audit_data: &AuditData,
) -> BTreeMap<topic::Topic, Vec<String>> {
  let mut out: BTreeMap<topic::Topic, Vec<String>> = BTreeMap::new();
  for (decl_topic, sem_topics) in &audit_data.declaration_semantics {
    let mut texts = Vec::with_capacity(sem_topics.len());
    for sem_topic in sem_topics {
      if let Some(TopicMetadata::FunctionalSemanticTopic {
        description, ..
      }) = audit_data.topic_metadata.get(sem_topic)
      {
        texts.push(description.clone());
      }
    }
    if !texts.is_empty() {
      out.insert(*decl_topic, texts);
    }
  }
  out
}

/// Rebuilds feature-related context:
/// - `expanded_context` on documentation TitledTopics/UnnamedTopics (linked features)
/// - `topic_context` for FeatureTopics (linked requirements)
/// - `topic_context` for RequirementTopics (parent feature)
pub fn rebuild_feature_context(audit_data: &mut AuditData) {
  // Rebuild section_requirements: section D-topic → R-topics
  audit_data.section_requirements.clear();
  for (req_topic, metadata) in &audit_data.topic_metadata {
    if let TopicMetadata::RequirementTopic {
      section_topic: st, ..
    } = metadata
    {
      audit_data
        .section_requirements
        .entry(*st)
        .or_default()
        .push(*req_topic);
    }
  }

  // Rebuild member_behaviors: member N-topic → B-topics
  audit_data.member_behaviors.clear();
  for (beh_topic, metadata) in &audit_data.topic_metadata {
    if let TopicMetadata::BehaviorTopic { member_topic, .. } = metadata {
      audit_data
        .member_behaviors
        .entry(*member_topic)
        .or_default()
        .push(*beh_topic);
    }
  }

  // Rebuild declaration_semantics: declaration N-topic → P-topics
  audit_data.declaration_semantics.clear();
  for (sem_topic, metadata) in &audit_data.topic_metadata {
    if let TopicMetadata::FunctionalSemanticTopic {
      declaration_topic: decl_topic,
      ..
    } = metadata
    {
      audit_data
        .declaration_semantics
        .entry(*decl_topic)
        .or_default()
        .push(*sem_topic);
    }
  }

  // Rebuild subject_purposes: non-pure subject → P-topic
  // and subject_placements: non-pure subject → P-topic
  audit_data.subject_purposes.clear();
  audit_data.subject_placements.clear();
  for (prop_topic, metadata) in &audit_data.topic_metadata {
    match metadata {
      TopicMetadata::FunctionalPurposeTopic { subject_topic, .. } => {
        audit_data
          .subject_purposes
          .insert(*subject_topic, *prop_topic);
      }
      TopicMetadata::PlacementRationaleTopic { subject_topic, .. } => {
        audit_data
          .subject_placements
          .insert(*subject_topic, *prop_topic);
      }
      _ => {}
    }
  }

  // Rebuild subject_conditions: non-pure subject → A-prefixed condition topics
  audit_data.subject_conditions.clear();
  for (cond_topic, metadata) in &audit_data.topic_metadata {
    if let TopicMetadata::ConditionTopic { subject_topic, .. } = metadata {
      audit_data
        .subject_conditions
        .entry(*subject_topic)
        .or_default()
        .push(*cond_topic);
    }
  }

  // Build reverse index: doc_topic -> [requirement_topics]
  let mut doc_to_requirements: HashMap<topic::Topic, Vec<topic::Topic>> =
    HashMap::new();
  for (req_topic, requirement) in &audit_data.requirements {
    for doc_topic in &requirement.documentation_topics {
      doc_to_requirements
        .entry(*doc_topic)
        .or_default()
        .push(*req_topic);
    }
  }

  // Build reverse index: requirement → [features] from feature_requirement_links
  let mut req_to_features: HashMap<topic::Topic, Vec<topic::Topic>> =
    HashMap::new();
  for (ft, req_topics) in &audit_data.feature_requirement_links {
    for rt in req_topics {
      let features = req_to_features.entry(*rt).or_default();
      if !features.contains(ft) {
        features.push(*ft);
      }
    }
  }

  // Update expanded_context for documentation topics (TitledTopic/UnnamedTopic)
  // Show the parent feature(s) of the requirements that link to this doc topic
  let mut doc_to_features: HashMap<topic::Topic, Vec<topic::Topic>> =
    HashMap::new();
  for (doc_topic, req_topics) in &doc_to_requirements {
    for rt in req_topics {
      if let Some(fts) = req_to_features.get(rt) {
        let features = doc_to_features.entry(*doc_topic).or_default();
        for ft in fts {
          if !features.contains(ft) {
            features.push(*ft);
          }
        }
      }
    }
  }

  for (topic, metadata) in &audit_data.topic_metadata {
    if !matches!(
      metadata,
      TopicMetadata::TitledTopic { .. } | TopicMetadata::UnnamedTopic { .. }
    ) {
      continue;
    }
    let feature_topics = doc_to_features.remove(topic).unwrap_or_default();
    let expanded_context: Vec<SourceContext> = feature_topics
      .into_iter()
      .map(|ft| {
        let sort_key = Some(ft.numeric_id() as usize);
        SourceContext {
          scope: ft,
          sort_key,
          is_in_scope: true,
          scope_references: vec![Reference::ProjectReference {
            reference_topic: ft,
            sort_key,
          }],
          nested_references: vec![],
        }
      })
      .collect();
    if expanded_context.is_empty() {
      audit_data.expanded_topic_context.remove(topic);
    } else {
      audit_data
        .expanded_topic_context
        .insert(*topic, expanded_context);
    }
  }

  // Build context for FeatureTopics: feature + requirements + behaviors as scope refs
  for (feature_topic, metadata) in &audit_data.topic_metadata {
    if !matches!(metadata, TopicMetadata::FeatureTopic { .. }) {
      continue;
    }
    let mut scope_references = vec![Reference::ProjectReference {
      reference_topic: *feature_topic,
      sort_key: Some(0),
    }];

    // Requirements linked to this feature
    if let Some(req_topics) =
      audit_data.feature_requirement_links.get(feature_topic)
    {
      for rt in req_topics {
        let sort_key = Some(rt.numeric_id() as usize);
        scope_references.push(Reference::ProjectReference {
          reference_topic: *rt,
          sort_key,
        });
      }
    }

    // Feature + requirements as the first context entry
    let mut context = vec![SourceContext {
      scope: *feature_topic,
      sort_key: Some(0),
      is_in_scope: true,
      scope_references,
      nested_references: vec![],
    }];

    // Behaviors linked to this feature, grouped by contract → member → behaviors
    let mut member_behaviors: std::collections::BTreeMap<
      topic::Topic,
      Vec<topic::Topic>,
    > = std::collections::BTreeMap::new();
    if let Some(beh_topics) =
      audit_data.feature_behavior_links.get(feature_topic)
    {
      for bt in beh_topics {
        if let Some(TopicMetadata::BehaviorTopic { member_topic, .. }) =
          audit_data.topic_metadata.get(bt)
        {
          member_behaviors.entry(*member_topic).or_default().push(*bt);
        }
      }
    }

    // Group members by their containing contract
    let mut contract_members: std::collections::BTreeMap<
      topic::Topic,
      Vec<(topic::Topic, Vec<topic::Topic>)>,
    > = std::collections::BTreeMap::new();
    for (mt, beh_topics) in member_behaviors {
      let contract = audit_data
        .topic_metadata
        .get(&mt)
        .and_then(|m| match m.scope() {
          Scope::Component { component, .. } => Some(*component),
          _ => None,
        })
        .unwrap_or(mt);
      contract_members
        .entry(contract)
        .or_default()
        .push((mt, beh_topics));
    }

    // Create a SourceContext per contract with members as nested scopes
    for (contract_topic, members) in contract_members {
      let contract_sort_key = Some(contract_topic.numeric_id() as usize);
      let nested_references: Vec<NestedSourceContext> = members
        .into_iter()
        .map(|(mt, beh_topics)| {
          let children = beh_topics
            .into_iter()
            .map(|bt| {
              let sort_key = Some(bt.numeric_id() as usize);
              SourceChild::Reference(Reference::ProjectReference {
                reference_topic: bt,
                sort_key,
              })
            })
            .collect();
          let sort_key = Some(mt.numeric_id() as usize);
          NestedSourceContext::new(mt, sort_key, children)
        })
        .collect();

      context.push(SourceContext {
        scope: contract_topic,
        sort_key: contract_sort_key,
        is_in_scope: true,
        scope_references: vec![],
        nested_references,
      });
    }
    audit_data.topic_context.insert(*feature_topic, context);
  }

  // Build reverse index: behavior → [features] from feature_behavior_links
  let mut beh_to_features: HashMap<topic::Topic, Vec<topic::Topic>> =
    HashMap::new();
  for (ft, beh_topics) in &audit_data.feature_behavior_links {
    for bt in beh_topics {
      let features = beh_to_features.entry(*bt).or_default();
      if !features.contains(ft) {
        features.push(*ft);
      }
    }
  }

  // RequirementTopics have no topic_context (nothing in the body panel).
  // Their linked documentation sections are shown in the documentation panel.

  // Build context for BehaviorTopics (rendered like requirements)
  for (beh_topic, metadata) in &audit_data.topic_metadata {
    if !matches!(metadata, TopicMetadata::BehaviorTopic { .. }) {
      continue;
    }
    let context = vec![SourceContext {
      scope: *beh_topic,
      sort_key: Some(beh_topic.numeric_id() as usize),
      is_in_scope: true,
      scope_references: vec![Reference::ProjectReference {
        reference_topic: *beh_topic,
        sort_key: Some(beh_topic.numeric_id() as usize),
      }],
      nested_references: vec![],
    }];
    audit_data.topic_context.insert(*beh_topic, context);
  }

  // Build context for FunctionalSemanticTopics (same self-ref pattern)
  for (sem_topic, metadata) in &audit_data.topic_metadata {
    if !matches!(metadata, TopicMetadata::FunctionalSemanticTopic { .. }) {
      continue;
    }
    let sort_key = Some(sem_topic.numeric_id() as usize);
    let context = vec![SourceContext {
      scope: *sem_topic,
      sort_key,
      is_in_scope: true,
      scope_references: vec![Reference::ProjectReference {
        reference_topic: *sem_topic,
        sort_key,
      }],
      nested_references: vec![],
    }];
    audit_data.topic_context.insert(*sem_topic, context);
  }

  // Build context for ThreatTopics: subject + threat as scope refs,
  // invariants as nested refs indented under the threat
  for (threat_topic, metadata) in &audit_data.topic_metadata {
    if let TopicMetadata::ThreatTopic { subject_topic, .. } = metadata {
      let subj_sort_key = Some(subject_topic.numeric_id() as usize);
      let threat_sort_key = Some(threat_topic.numeric_id() as usize);
      let scope_references = vec![
        Reference::ProjectReference {
          reference_topic: *subject_topic,
          sort_key: subj_sort_key,
        },
        Reference::ProjectReference {
          reference_topic: *threat_topic,
          sort_key: threat_sort_key,
        },
      ];
      let nested_references = build_invariant_nested_refs(
        std::slice::from_ref(threat_topic),
        &audit_data.threats,
      );
      let context = vec![SourceContext {
        scope: *threat_topic,
        sort_key: threat_sort_key,
        is_in_scope: true,
        scope_references,
        nested_references,
      }];
      audit_data.topic_context.insert(*threat_topic, context);
    }
  }

  // Build context for InvariantTopics: parent threat as a SourceContext entry
  for (inv_topic, metadata) in &audit_data.topic_metadata {
    if let TopicMetadata::InvariantTopic { threat_topic, .. } = metadata {
      let sort_key = Some(threat_topic.numeric_id() as usize);
      let context = vec![SourceContext {
        scope: *inv_topic,
        sort_key,
        is_in_scope: true,
        scope_references: vec![Reference::ProjectReference {
          reference_topic: *threat_topic,
          sort_key,
        }],
        nested_references: vec![],
      }];
      audit_data.topic_context.insert(*inv_topic, context);
    }
  }

  // Populate expanded_context for BehaviorTopics: show the source member
  let behavior_contexts: Vec<(topic::Topic, Vec<SourceContext>)> = audit_data
    .topic_metadata
    .iter()
    .filter_map(|(bt, m)| {
      if let TopicMetadata::BehaviorTopic { member_topic, .. } = m {
        let ctx = audit_data
          .topic_context
          .get(member_topic)
          .cloned()
          .unwrap_or_default();
        if !ctx.is_empty() {
          Some((*bt, ctx))
        } else {
          None
        }
      } else {
        None
      }
    })
    .collect();

  for (bt, ctx) in behavior_contexts {
    audit_data.expanded_topic_context.insert(bt, ctx);
  }

  // Populate expanded_context for FunctionalSemanticTopics: show the
  // source declaration the semantic describes.
  let semantic_contexts: Vec<(topic::Topic, Vec<SourceContext>)> = audit_data
    .topic_metadata
    .iter()
    .filter_map(|(pt, m)| {
      if let TopicMetadata::FunctionalSemanticTopic {
        declaration_topic, ..
      } = m
      {
        let ctx = audit_data
          .topic_context
          .get(declaration_topic)
          .cloned()
          .unwrap_or_default();
        if !ctx.is_empty() {
          Some((*pt, ctx))
        } else {
          None
        }
      } else {
        None
      }
    })
    .collect();

  for (pt, ctx) in semantic_contexts {
    audit_data.expanded_topic_context.insert(pt, ctx);
  }

  // Populate expanded_context for FeatureTopics: show deduplicated source
  // members from all linked behaviors
  let feature_contexts: Vec<(topic::Topic, Vec<SourceContext>)> = audit_data
    .feature_behavior_links
    .iter()
    .map(|(ft, beh_topics)| {
      let mut member_topics: Vec<topic::Topic> = Vec::new();
      for bt in beh_topics {
        if let Some(TopicMetadata::BehaviorTopic { member_topic, .. }) =
          audit_data.topic_metadata.get(bt)
          && !member_topics.contains(member_topic)
        {
          member_topics.push(*member_topic);
        }
      }

      let mut all_contexts: Vec<SourceContext> = Vec::new();
      for mt in &member_topics {
        if let Some(ctx) = audit_data.topic_context.get(mt) {
          all_contexts.extend(ctx.iter().cloned());
        }
      }

      (*ft, merge_context_groups(all_contexts))
    })
    .collect();

  for (ft, ctx) in feature_contexts {
    if ctx.is_empty() {
      audit_data.expanded_topic_context.remove(&ft);
    } else {
      audit_data.expanded_topic_context.insert(ft, ctx);
    }
  }
}

pub fn new_audit_data(
  audit_name: String,
  in_scope_files: HashSet<ProjectPath>,
  security_notes: Option<String>,
) -> AuditData {
  let mut topic_metadata = BTreeMap::new();

  // Pre-populate with Solidity globals
  // keccak256 function with node_id -8
  let keccak256_topic = topic::new_node_topic(&-8);
  topic_metadata.insert(
    keccak256_topic,
    TopicMetadata::NamedTopic {
      topic: keccak256_topic,
      scope: Scope::Global,
      kind: NamedTopicKind::Builtin,
      visibility: NamedTopicVisibility::Public,
      name: "keccak256".to_string(),
      is_mutable: false,
      mutations: Vec::new(),
      ancestors: Vec::new(),
      descendants: Vec::new(),
      relatives: Vec::new(),
      transitive_topic: None,
      doc_references: Vec::new(),
    },
  );

  // type() function with node_id -27
  let type_topic = topic::new_node_topic(&-27);
  topic_metadata.insert(
    type_topic,
    TopicMetadata::NamedTopic {
      topic: type_topic,
      scope: Scope::Global,
      kind: NamedTopicKind::Builtin,
      visibility: NamedTopicVisibility::Public,
      name: "type".to_string(),
      is_mutable: false,
      mutations: Vec::new(),
      ancestors: Vec::new(),
      descendants: Vec::new(),
      relatives: Vec::new(),
      transitive_topic: None,
      doc_references: Vec::new(),
    },
  );

  // this keyword with node_id -28
  let this_topic = topic::new_node_topic(&-28);
  topic_metadata.insert(
    this_topic,
    TopicMetadata::NamedTopic {
      topic: this_topic,
      scope: Scope::Global,
      kind: NamedTopicKind::Builtin,
      visibility: NamedTopicVisibility::Public,
      name: "this".to_string(),
      is_mutable: false,
      mutations: Vec::new(),
      ancestors: Vec::new(),
      descendants: Vec::new(),
      relatives: Vec::new(),
      transitive_topic: None,
      doc_references: Vec::new(),
    },
  );

  AuditData {
    audit_name,
    in_scope_files,
    security_notes,
    asts: BTreeMap::new(),
    nodes: BTreeMap::new(),
    topic_metadata,
    function_properties: BTreeMap::new(),
    variable_types: BTreeMap::new(),
    name_index: TopicNameIndex::empty(),
    comment_index: HashMap::new(),
    topic_context: BTreeMap::new(),
    expanded_topic_context: BTreeMap::new(),
    requirements: BTreeMap::new(),
    section_requirements: BTreeMap::new(),
    member_behaviors: BTreeMap::new(),
    declaration_semantics: BTreeMap::new(),
    subject_purposes: BTreeMap::new(),
    subject_placements: BTreeMap::new(),
    subject_conditions: BTreeMap::new(),
    threat_feature_links: Vec::new(),
    threats: BTreeMap::new(),
    invariants: BTreeMap::new(),
    feature_requirement_links: BTreeMap::new(),
    feature_behavior_links: BTreeMap::new(),
    mentions_index: HashMap::new(),
    inheritance: BTreeMap::new(),
    resolution_graph: None,
    resolution_traces: BTreeMap::new(),
  }
}

pub fn new_data_context() -> DataContext {
  DataContext {
    audits: BTreeMap::new(),
  }
}

impl DataContext {
  /// Creates a new audit and returns true if successful, false if audit already exists
  pub fn create_audit(
    &mut self,
    audit_id: String,
    audit_name: String,
    in_scope_files: HashSet<ProjectPath>,
    security_notes: Option<String>,
  ) -> bool {
    if self.audits.contains_key(&audit_id) {
      return false;
    }
    self.audits.insert(
      audit_id,
      new_audit_data(audit_name, in_scope_files, security_notes),
    );
    true
  }

  /// Gets a reference to an audit's data
  pub fn get_audit(&self, audit_id: &str) -> Option<&AuditData> {
    self.audits.get(audit_id)
  }

  /// Gets a mutable reference to an audit's data
  pub fn get_audit_mut(&mut self, audit_id: &str) -> Option<&mut AuditData> {
    self.audits.get_mut(audit_id)
  }

  /// Removes an audit and returns true if it existed
  pub fn delete_audit(&mut self, audit_id: &str) -> bool {
    self.audits.remove(audit_id).is_some()
  }

  /// Lists all audit IDs
  pub fn list_audits(&self) -> Vec<String> {
    self.audits.keys().cloned().collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  #[test]
  fn test_new_project_path_strips_dot_slash() {
    let project_root = PathBuf::from("/home/user/project");
    let file_path = String::from("./src/my.sol");

    let result = new_project_path(&file_path, &project_root);

    assert_eq!(result.file_path, "src/my.sol");
  }

  #[test]
  fn test_new_project_path_from_path_strips_dot_slash() {
    let project_root = PathBuf::from("/home/user/project");
    let file_path = Path::new("./src/my.sol");

    let result = new_project_path_from_path(file_path, &project_root);

    assert_eq!(result.file_path, "src/my.sol");
  }

  #[test]
  fn test_new_project_path_handles_simple_relative() {
    let project_root = PathBuf::from("/home/user/project");
    let file_path = String::from("src/my.sol");

    let result = new_project_path(&file_path, &project_root);

    assert_eq!(result.file_path, "src/my.sol");
  }

  #[test]
  fn test_new_project_path_handles_absolute() {
    let project_root = PathBuf::from("/home/user/project");
    let file_path = String::from("/home/user/project/src/my.sol");

    let result = new_project_path(&file_path, &project_root);

    assert_eq!(result.file_path, "src/my.sol");
  }

  #[test]
  fn test_new_project_path_handles_parent_directory() {
    let project_root = PathBuf::from("/home/user/project");
    let file_path = String::from("./src/../contracts/my.sol");

    let result = new_project_path(&file_path, &project_root);

    assert_eq!(result.file_path, "contracts/my.sol");
  }

  #[test]
  fn test_new_project_path_handles_nested_dot_slash() {
    let project_root = PathBuf::from("/home/user/project");
    let file_path = String::from("./src/./contracts/./my.sol");

    let result = new_project_path(&file_path, &project_root);

    assert_eq!(result.file_path, "src/contracts/my.sol");
  }

  fn test_named_topic(t: topic::Topic, name: &str) -> TopicMetadata {
    TopicMetadata::NamedTopic {
      topic: t,
      scope: Scope::Global,
      kind: NamedTopicKind::Builtin,
      visibility: NamedTopicVisibility::Public,
      name: name.to_string(),
      is_mutable: false,
      mutations: Vec::new(),
      ancestors: Vec::new(),
      descendants: Vec::new(),
      relatives: Vec::new(),
      transitive_topic: None,
      doc_references: Vec::new(),
    }
  }

  #[test]
  fn candidates_by_simple_name_returns_all_pre_dedup() {
    let mut audit = new_audit_data("test".to_string(), HashSet::new(), None);
    let t1 = topic::new_node_topic(&100);
    let t2 = topic::new_node_topic(&200);
    audit
      .topic_metadata
      .insert(t1, test_named_topic(t1, "shared"));
    audit
      .topic_metadata
      .insert(t2, test_named_topic(t2, "shared"));

    let index = TopicNameIndex::build(&audit);

    // Both candidates returned, sorted ascending by topic ID.
    assert_eq!(index.candidates_by_simple_name("shared"), &[t1, t2]);
    // Two non-transitive collisions → dedup yields no unique winner.
    assert_eq!(index.get_by_simple_name("shared"), None);
    // Unknown name → empty slice.
    assert_eq!(index.candidates_by_simple_name("missing"), &[]);
  }

  #[test]
  fn candidates_by_simple_name_excludes_common_words() {
    let mut audit = new_audit_data("test".to_string(), HashSet::new(), None);
    let t1 = topic::new_node_topic(&1);
    audit.topic_metadata.insert(t1, test_named_topic(t1, "for"));

    let index = TopicNameIndex::build(&audit);

    // "for" is filtered as a common English word.
    assert_eq!(index.candidates_by_simple_name("for"), &[]);
    assert_eq!(index.get_by_simple_name("for"), None);
  }

  #[test]
  fn candidates_by_simple_name_single_unique_candidate() {
    let mut audit = new_audit_data("test".to_string(), HashSet::new(), None);
    let t1 = topic::new_node_topic(&7);
    audit
      .topic_metadata
      .insert(t1, test_named_topic(t1, "Solo"));

    let index = TopicNameIndex::build(&audit);

    // Single candidate: candidate list contains it AND get_by_simple_name
    // resolves it.
    assert_eq!(index.candidates_by_simple_name("Solo"), &[t1]);
    assert_eq!(index.get_by_simple_name("Solo"), Some(&t1));
  }

  #[test]
  fn candidates_by_simple_name_sorted_with_negative_node_ids() {
    let mut audit = new_audit_data("test".to_string(), HashSet::new(), None);
    // Node topics are signed; negative IDs (built-ins) must sort below
    // positive IDs.
    let t_neg = topic::new_node_topic(&-50);
    let t_pos = topic::new_node_topic(&50);
    audit
      .topic_metadata
      .insert(t_neg, test_named_topic(t_neg, "Mixed"));
    audit
      .topic_metadata
      .insert(t_pos, test_named_topic(t_pos, "Mixed"));

    let index = TopicNameIndex::build(&audit);

    assert_eq!(index.candidates_by_simple_name("Mixed"), &[t_neg, t_pos]);
  }

  #[test]
  fn candidates_by_simple_name_returns_disjoint_names_independently() {
    let mut audit = new_audit_data("test".to_string(), HashSet::new(), None);
    let t1 = topic::new_node_topic(&1);
    let t2 = topic::new_node_topic(&2);
    let t3 = topic::new_node_topic(&3);
    audit
      .topic_metadata
      .insert(t1, test_named_topic(t1, "Alpha"));
    audit
      .topic_metadata
      .insert(t2, test_named_topic(t2, "Beta"));
    audit
      .topic_metadata
      .insert(t3, test_named_topic(t3, "Beta"));

    let index = TopicNameIndex::build(&audit);

    assert_eq!(index.candidates_by_simple_name("Alpha"), &[t1]);
    assert_eq!(index.candidates_by_simple_name("Beta"), &[t2, t3]);
  }

  #[test]
  fn revert_info_with_error_topic_round_trips_through_serde() {
    let info = RevertInfo {
      topic: topic::new_node_topic(&10),
      kind: RevertConstraintKind::Revert,
      error_topic: Some(topic::new_node_topic(&42)),
    };
    let json = serde_json::to_string(&info).unwrap();
    let back: RevertInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(back.topic, info.topic);
    assert_eq!(back.kind, info.kind);
    assert_eq!(back.error_topic, info.error_topic);
  }

  #[test]
  fn revert_info_without_error_topic_deserializes_legacy_payload() {
    // Payloads written before `error_topic` was added must still
    // deserialize, with `error_topic = None`.
    let legacy = r#"{"topic":"N5","kind":"Require"}"#;
    let info: RevertInfo = serde_json::from_str(legacy).unwrap();
    assert_eq!(info.topic, topic::new_node_topic(&5));
    assert_eq!(info.kind, RevertConstraintKind::Require);
    assert_eq!(info.error_topic, None);
  }

  #[test]
  fn function_mod_properties_events_emitted_deserializes_legacy_payload() {
    // Payloads written before `events_emitted` / `reads` and the four
    // `effective_*` fields were added must still deserialize, with
    // each defaulting to `[]`. Tested for both variants since they
    // gain optional fields together — a serde-default regression on
    // either is symmetric tech debt. The four `effective_*` fields
    // are computed by the transitive-effects fold post-analyzer; old
    // payloads predating that work won't carry them, and consumers
    // must read them as empty.
    let legacy_function =
      r#"{"FunctionProperties":{"reverts":[],"calls":[],"mutations":[]}}"#;
    match serde_json::from_str::<FunctionModProperties>(legacy_function)
      .unwrap()
    {
      FunctionModProperties::FunctionProperties {
        events_emitted,
        reads,
        effective_reverts,
        effective_mutations,
        effective_reads,
        effective_events_emitted,
        ..
      } => {
        assert!(events_emitted.is_empty());
        assert!(reads.is_empty());
        assert!(effective_reverts.is_empty());
        assert!(effective_mutations.is_empty());
        assert!(effective_reads.is_empty());
        assert!(effective_events_emitted.is_empty());
      }
      _ => panic!("expected FunctionProperties"),
    }

    let legacy_modifier =
      r#"{"ModifierProperties":{"reverts":[],"calls":[],"mutations":[]}}"#;
    match serde_json::from_str::<FunctionModProperties>(legacy_modifier)
      .unwrap()
    {
      FunctionModProperties::ModifierProperties {
        events_emitted,
        reads,
        effective_reverts,
        effective_mutations,
        effective_reads,
        effective_events_emitted,
        ..
      } => {
        assert!(events_emitted.is_empty());
        assert!(reads.is_empty());
        assert!(effective_reverts.is_empty());
        assert!(effective_mutations.is_empty());
        assert!(effective_reads.is_empty());
        assert!(effective_events_emitted.is_empty());
      }
      _ => panic!("expected ModifierProperties"),
    }
  }

  #[test]
  fn audit_data_phase0_fields_default_empty() {
    let audit = new_audit_data("test".to_string(), HashSet::new(), None);
    assert!(audit.inheritance.is_empty());
    assert!(audit.resolution_graph.is_none());
  }

  #[test]
  fn condition_topic_round_trips_through_serde() {
    use crate::collaborator::models::Author;

    let cond_topic = topic::new_adversarial_property_topic(1);
    let subject_topic = topic::new_node_topic(&42);
    let evidence = vec![topic::new_node_topic(&10), topic::new_node_topic(&20)];

    let metadata = TopicMetadata::ConditionTopic {
      topic: cond_topic,
      description: "The caller carries the privilege the subject's purpose presumes."
        .to_string(),
      subject_topic,
      kind: ConditionKind::InputIntegrity,
      evidence_topics: evidence.clone(),
      author: Author::AgentLarge,
      created_at: None,
    };

    // Serialize and deserialize the TopicMetadata
    let json = serde_json::to_string(&metadata).unwrap();
    let back: TopicMetadata = serde_json::from_str(&json).unwrap();
    assert_eq!(back.topic(), &cond_topic);
    assert_eq!(
      back.description(),
      Some("The caller carries the privilege the subject's purpose presumes.")
    );
    assert_eq!(back.target_topic(), Some(&subject_topic));
    assert_eq!(back.author(), Some(Author::AgentLarge));
    assert!(back.created_at().is_none());

    // Verify ConditionKind serde round-trip for each variant
    for kind in [
      ConditionKind::RestrictedReachability,
      ConditionKind::AuthorizedAccess,
      ConditionKind::ErrorRecoverability,
      ConditionKind::InputIntegrity,
      ConditionKind::ValueFreshness,
      ConditionKind::AtomicConsistency,
      ConditionKind::ResourceAvailability,
      ConditionKind::Other,
    ] {
      let kind_json = serde_json::to_string(&kind).unwrap();
      let kind_back: ConditionKind = serde_json::from_str(&kind_json).unwrap();
      assert_eq!(kind, kind_back, "round-trip failed for {:?}", kind);
    }

    // Insert into topic_metadata map and verify lookups
    let mut audit = new_audit_data("test".to_string(), HashSet::new(), None);
    audit.topic_metadata.insert(cond_topic, metadata);

    let retrieved = audit.topic_metadata.get(&cond_topic).unwrap();
    assert!(matches!(retrieved, TopicMetadata::ConditionTopic { .. }));
    if let TopicMetadata::ConditionTopic {
      kind,
      evidence_topics,
      ..
    } = retrieved
    {
      assert_eq!(*kind, ConditionKind::InputIntegrity);
      assert_eq!(*evidence_topics, evidence);
    }
  }

  #[test]
  fn rebuild_feature_context_populates_subject_conditions() {
    use crate::collaborator::models::Author;

    let mut audit = new_audit_data("test".to_string(), HashSet::new(), None);
    let subject_a = topic::new_node_topic(&10);
    let subject_b = topic::new_node_topic(&20);

    // Two conditions for subject_a, one for subject_b.
    let cond_a1 = topic::new_adversarial_property_topic(1);
    let cond_a2 = topic::new_adversarial_property_topic(2);
    let cond_b1 = topic::new_adversarial_property_topic(3);

    audit.topic_metadata.insert(
      cond_a1,
      TopicMetadata::ConditionTopic {
        topic: cond_a1,
        description: "first assertion on a".to_string(),
        subject_topic: subject_a,
        kind: ConditionKind::RestrictedReachability,
        evidence_topics: vec![],
        author: Author::System,
        created_at: None,
      },
    );
    audit.topic_metadata.insert(
      cond_a2,
      TopicMetadata::ConditionTopic {
        topic: cond_a2,
        description: "second assertion on a".to_string(),
        subject_topic: subject_a,
        kind: ConditionKind::AuthorizedAccess,
        evidence_topics: vec![topic::new_node_topic(&99)],
        author: Author::System,
        created_at: None,
      },
    );
    audit.topic_metadata.insert(
      cond_b1,
      TopicMetadata::ConditionTopic {
        topic: cond_b1,
        description: "assertion on b".to_string(),
        subject_topic: subject_b,
        kind: ConditionKind::ValueFreshness,
        evidence_topics: vec![],
        author: Author::System,
        created_at: None,
      },
    );

    rebuild_feature_context(&mut audit);

    // subject_a has two conditions
    let a_conds = audit
      .subject_conditions
      .get(&subject_a)
      .expect("subject_a should have conditions");
    assert_eq!(a_conds.len(), 2);
    assert!(a_conds.contains(&cond_a1));
    assert!(a_conds.contains(&cond_a2));

    // subject_b has one condition
    let b_conds = audit
      .subject_conditions
      .get(&subject_b)
      .expect("subject_b should have conditions");
    assert_eq!(b_conds.len(), 1);
    assert_eq!(b_conds[0], cond_b1);

    // A subject with no conditions is absent from the index
    let subject_c = topic::new_node_topic(&30);
    assert!(!audit.subject_conditions.contains_key(&subject_c));
  }

  #[test]
  fn rebuild_feature_context_clears_subject_conditions_before_rebuilding() {
    // Calling rebuild_feature_context twice must not accumulate stale
    // entries — the index is always rebuilt from scratch.
    use crate::collaborator::models::Author;

    let mut audit = new_audit_data("test".to_string(), HashSet::new(), None);
    let subject_a = topic::new_node_topic(&10);
    let cond_a1 = topic::new_adversarial_property_topic(1);
    audit.topic_metadata.insert(
      cond_a1,
      TopicMetadata::ConditionTopic {
        topic: cond_a1,
        description: "assertion".to_string(),
        subject_topic: subject_a,
        kind: ConditionKind::RestrictedReachability,
        evidence_topics: vec![],
        author: Author::System,
        created_at: None,
      },
    );

    // First rebuild
    rebuild_feature_context(&mut audit);
    assert_eq!(
      audit.subject_conditions.get(&subject_a).map(|v| v.len()),
      Some(1)
    );

    // Remove the condition topic and rebuild — old entry must be gone
    audit.topic_metadata.remove(&cond_a1);
    rebuild_feature_context(&mut audit);
    assert!(
      !audit.subject_conditions.contains_key(&subject_a),
      "subject_conditions must be cleared before rebuild; stale entry from previous rebuild should be gone"
    );
  }

  #[test]
  fn rebuild_feature_context_with_no_conditions_yields_empty_index() {
    let mut audit = new_audit_data("test".to_string(), HashSet::new(), None);
    rebuild_feature_context(&mut audit);
    assert!(
      audit.subject_conditions.is_empty(),
      "no ConditionTopic entries means empty subject_conditions"
    );
  }
}

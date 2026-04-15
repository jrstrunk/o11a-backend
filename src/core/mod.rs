use std::path::{Path, PathBuf};

pub mod project;
pub mod topic;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

// ============================================================================
// Solidity Type System (for checker module)
// ============================================================================

/// Represents a Solidity type for use in the checker.
/// Contains enough detail to derive valid value ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq)]
pub struct BlockAnnotation {
  /// The topic of the annotating node (the control flow statement or
  /// the annotated block itself).
  pub topic: topic::Topic,
  pub kind: BlockAnnotationKind,
}

#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlFlowStatementKind {
  If,
  For,
  While,
  DoWhile,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlFlowBranch {
  True,
  False,
}

/// One layer in the containing block nesting chain.
/// Pairs a semantic block with an optional annotation describing what
/// kind of block it is (control flow body, unchecked, assembly, etc.).
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct RevertInfo {
  pub topic: topic::Topic,
  pub kind: RevertConstraintKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevertConstraintKind {
  /// require(condition) - reverts when condition is false
  Require,
  /// revert with enclosing if conditions
  Revert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionKind {
  Constructor,
  Function,
  Fallback,
  Receive,
  FreeFunction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContractKind {
  Contract,
  Library,
  Abstract,
  Interface,
}

/// Severity level for threats and invariants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

  pub fn from_str(s: &str) -> Option<ThreatSeverity> {
    match s {
      "low" => Some(ThreatSeverity::Low),
      "medium" => Some(ThreatSeverity::Medium),
      "high" => Some(ThreatSeverity::High),
      "critical" => Some(ThreatSeverity::Critical),
      _ => None,
    }
  }
}

/// A project feature extracted from documentation.
/// Groups requirements. Feature-threat linkage is established during
/// impact analysis, not at creation time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
  /// R-prefixed topic IDs of requirements belonging to this feature
  pub requirement_topics: Vec<topic::Topic>,
}

/// A functional semantic with provenance — what a declaration represents in the
/// context of the project, linked to the documentation topics it was derived from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionalSemantic {
  /// The semantic meaning text (e.g., "proportional reward multiplier")
  pub text: String,
  /// D-prefixed documentation topics this semantic was derived from
  pub documentation_topics: Vec<topic::Topic>,
  pub author_id: i64,
  pub created_at: String,
}

/// A functional purpose — why a subject exists in the context of the project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionalPurpose {
  pub text: String,
  pub author_id: i64,
  pub created_at: String,
}

/// A link between documentation sections and a code declaration, established
/// during the semantic linking pipeline step. These links define what each
/// declaration means in the context of the project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticLink {
  /// D-prefixed documentation topics that contributed to this semantic
  pub documentation_topics: Vec<topic::Topic>,
  /// The N-prefixed code declaration topic
  pub declaration_topic: topic::Topic,
  /// The semantic meaning derived from this link
  pub semantic_text: String,
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

  pub fn from_str(s: &str) -> Option<ThreatFeatureRelation> {
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

/// A condition evaluation on a non-pure subject. Conditions are the concrete,
/// enumerable aspects of a subject's interaction surface that must be evaluated
/// before threat generation. Each condition has standardized questions determined
/// by its type, and the answers are stored as part of the evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
  /// The non-pure subject this condition belongs to
  pub subject_topic: topic::Topic,
  /// The type of non-pure interaction this condition represents
  pub condition_type: NonPureSubjectType,
  /// Description of the specific condition (e.g. "Revert PAIR_EXISTS on createPair")
  pub description: String,
  /// Question/answer pairs from the standardized evaluation
  pub evaluations: Vec<ConditionEvaluation>,
}

/// A single question/answer pair in a condition evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionEvaluation {
  pub question: String,
  pub answer: String,
}

/// A behavior observed during code review. Behaviors capture what the code
/// actually does and are reconciliation artifacts — compared against
/// requirements to surface gaps between documentation and implementation.
/// Each behavior belongs to exactly one code member (one-to-many).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Behavior {}

/// A threat describing how an attacker could compromise a feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Threat {
  /// I-prefixed topic IDs of invariants that defend against this threat
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
  /// Features extracted from documentation, keyed by F-prefixed topic ID.
  pub features: BTreeMap<topic::Topic, Feature>,
  /// Requirements keyed by R-prefixed topic ID. Each belongs to one feature.
  pub requirements: BTreeMap<topic::Topic, Requirement>,
  /// Functional purpose stored per subject topic: "why does this exist?"
  pub functional_purposes: BTreeMap<topic::Topic, FunctionalPurpose>,
  /// Functional semantics with provenance: "what does this represent?" + doc source.
  /// Multiple semantics per topic are supported — different documentation sections
  /// may contribute distinct semantic facets for the same declaration.
  pub functional_semantics: BTreeMap<topic::Topic, Vec<FunctionalSemantic>>,
  /// Links between documentation sections and code declarations from semantic linking.
  pub semantic_links: Vec<SemanticLink>,
  /// Reverse index: D-prefixed section topic → R-prefixed requirement topics.
  /// Derived from RequirementTopic.section_topic, rebuilt with rebuild_feature_context.
  pub section_requirements: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Reverse index: N-prefixed member topic → B-prefixed behavior topics.
  /// Derived from BehaviorTopic.member_topic, rebuilt with rebuild_feature_context.
  pub member_behaviors: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Impact analysis links between threats and features.
  pub threat_feature_links: Vec<ThreatFeatureLink>,
  /// Conditions keyed by their database ID. Each belongs to a non-pure subject
  /// and contains standardized question/answer evaluations.
  pub conditions: Vec<Condition>,
  /// Behaviors keyed by B-prefixed topic ID. Each belongs to one code member.
  pub behaviors: BTreeMap<topic::Topic, Behavior>,
  /// Threats keyed by A-prefixed topic ID. Each belongs to one feature.
  pub threats: BTreeMap<topic::Topic, Threat>,
  /// Invariants keyed by I-prefixed topic ID. Each belongs to one threat.
  pub invariants: BTreeMap<topic::Topic, Invariant>,
  /// Source-to-feature links: maps source code member topics (N-prefixed) to
  /// the features they participate in (F-prefixed). Built by the pipeline's
  /// source-to-feature linking step.
  pub source_feature_links: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  /// Reverse index: mentioned topic → comment topics that mention it.
  /// Updated on comment create.
  pub mentions_index: HashMap<topic::Topic, Vec<topic::Topic>>,
}

/// Common short English words that should not match as simple topic names.
/// These appear frequently in documentation prose inside backticks but are
/// almost never intended to reference a Solidity declaration.
/// Qualified names like "ERC20.transfer.from" are unaffected.
fn is_common_word(name: &str) -> bool {
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
pub struct TopicNameIndex {
  by_qualified_name: HashMap<String, topic::Topic>,
  by_simple_name: HashMap<String, topic::Topic>,
}

impl TopicNameIndex {
  pub fn empty() -> Self {
    TopicNameIndex {
      by_qualified_name: HashMap::new(),
      by_simple_name: HashMap::new(),
    }
  }

  pub fn build(audit_data: &AuditData) -> Self {
    let mut by_qualified_name = HashMap::new();
    let mut simple_name_candidates: HashMap<String, Vec<topic::Topic>> =
      HashMap::new();

    for (topic, metadata) in &audit_data.topic_metadata {
      if let Some(qname) = metadata.qualified_name(audit_data) {
        by_qualified_name.insert(qname, topic.clone());
      }

      if let Some(sname) = metadata.name() {
        if !is_common_word(sname) {
          simple_name_candidates
            .entry(sname.to_string())
            .or_default()
            .push(topic.clone());
        }
      }
    }

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
            Some((name, non_transitive[0].clone()))
          } else {
            None
          }
        }
      })
      .collect();

    TopicNameIndex {
      by_qualified_name,
      by_simple_name,
    }
  }

  pub fn get_by_qualified_name(&self, name: &str) -> Option<&topic::Topic> {
    self.by_qualified_name.get(name)
  }

  pub fn get_by_simple_name(&self, name: &str) -> Option<&topic::Topic> {
    self.by_simple_name.get(name)
  }

  pub fn qualified_names(&self) -> Vec<&str> {
    self.by_qualified_name.keys().map(|s| s.as_str()).collect()
  }
}


/// Cached static parts of a topic view (AST-derived, never invalidated).
#[derive(Debug, Clone)]
pub struct CachedTopicView {
  pub topic_panel_html: String,
  pub expanded_references_panel_html: String,
  pub breadcrumb_html: String,
  pub highlight_css: String,
}

pub struct DataContext {
  // Map of audit_id to audit data
  pub audits: BTreeMap<String, AuditData>,
  /// Cached rendered HTML keyed by (audit_id, topic_id)
  pub source_text_cache: HashMap<String, HashMap<String, String>>,
  /// Cached topic view static panels keyed by (audit_id, topic_id).
  /// These are purely AST-derived and never invalidated.
  pub topic_view_cache: HashMap<String, HashMap<String, CachedTopicView>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
  Solidity(crate::solidity::parser::ASTNode),
  Documentation(crate::documentation::parser::DocumentationNode),
  Comment(Vec<crate::collaborator::parser::CommentNode>),
}

impl Node {
  /// Returns the source location start (byte offset) for this node.
  pub fn source_location_start(&self) -> Option<usize> {
    match self {
      Node::Solidity(ast_node) => ast_node.src_location().start,
      Node::Documentation(doc_node) => doc_node.position(),
      Node::Comment(_) => None,
    }
  }
}

pub enum AST {
  Solidity(crate::solidity::parser::SolidityAST),
  Documentation(crate::documentation::parser::DocumentationAST),
}

#[derive(Debug, Clone)]
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
      component: component.clone(),
      member: topic,
      signature_container: None,
    },
    Scope::Member {
      container,
      component,
      member,
      ..
    } => {
      let mut containing_blocks = Vec::new();
      containing_blocks.push(ContainingBlockLayer {
        block: topic,
        annotation: None,
      });
      Scope::ContainingBlock {
        container: container.clone(),
        component: component.clone(),
        member: member.clone(),
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
        component: component.clone(),
        member: member.clone(),
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
        component: component.clone(),
        member: member.clone(),
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
      component: component.clone(),
      member: topic,
      signature_container: None,
    },
    Scope::Member {
      container,
      component,
      ..
    } => Scope::Member {
      container: container.clone(),
      component: component.clone(),
      member: topic,
      signature_container: None,
    },
    Scope::ContainingBlock {
      container,
      component,
      ..
    } => Scope::Member {
      container: container.clone(),
      component: component.clone(),
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
      component: component.clone(),
      member: member.clone(),
      signature_container: Some(container),
    },
    _ => panic!(
      "Invariant violation: set_signature_container called on non-Member scope"
    ),
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VariableMutability {
  Mutable,
  Immutable,
  Constant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitledTopicKind {
  /// Documentation section (H1 becomes component, sub-H1 becomes member)
  DocumentationSection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnnamedTopicKind {
  VariableMutation,
  Arithmetic,
  Comparison,
  Logical,
  Bitwise,
  Conditional,
  FunctionCall,
  TypeConversion,
  StructConstruction,
  NewExpression,
  Literal,
  SemanticBlock,
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
  /// Requires condition evaluation and threat generation.
  NonPure,
}

/// Non-pure subject type, determining which standardized condition questions apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NonPureSubjectType {
  StateWrite,
  StateRead,
  ExternalCall,
  DelegateCall,
  InlineAssembly,
  Create,
}

impl UnnamedTopicKind {
  /// Returns the purity classification for this unnamed topic kind.
  pub fn purity(&self) -> SubjectPurity {
    match self {
      UnnamedTopicKind::VariableMutation => SubjectPurity::NonPure,
      UnnamedTopicKind::InlineAssembly => SubjectPurity::NonPure,
      UnnamedTopicKind::NewExpression => SubjectPurity::NonPure,
      // FunctionCall purity depends on whether it's external — caller must check
      UnnamedTopicKind::FunctionCall => SubjectPurity::Pure,
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

#[derive(Debug, Clone)]
pub enum NamedTopicVisibility {
  Public,
  Private,
  Internal,
  External,
}

/// Represents a reference to a topic, with type information about its source.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
pub struct NestedSourceContext {
  /// The nested scope containing these references (function for Solidity, section for documentation)
  subscope: topic::Topic,
  /// Source location start for sorting nested groups relative to each other
  sort_key: Option<usize>,
  /// Ordered children (references and annotated block groups) in source order
  children: Vec<SourceChild>,
}

impl NestedSourceContext {
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
pub fn merge_context_groups(contexts: Vec<SourceContext>) -> Vec<SourceContext> {
  let mut merged: Vec<SourceContext> = Vec::new();
  for ctx in contexts {
    ensure_context(&mut merged, ctx.scope.clone(), ctx.sort_key, ctx.is_in_scope);
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
  if let Some(existing) = nested_refs.iter_mut().find(|n| n.subscope == nested.subscope) {
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
  ensure_context(groups, scope.clone(), scope_sort_key, is_in_scope);

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
            subscope: subscope_topic.clone(),
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

    let sort_key = ann.topic.underlying_id().ok().map(|id| id as usize);
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
        if let SourceChild::AnnotatedBlock(g) = child {
          if g.annotation.topic == ann.topic && g.annotation.kind != ann.kind {
            g.has_sibling_branch = true;
            break;
          }
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
  let ref_topic = existing.reference_topic().clone();

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
          existing_mentions.push(mt.clone());
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
          existing_mentions.push(mt.clone());
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

#[derive(Debug, Clone)]
pub enum TopicMetadata {
  NamedTopic {
    topic: topic::Topic,
    scope: Scope,
    kind: NamedTopicKind,
    name: String,
    visibility: NamedTopicVisibility,
    /// Context derived from recursively traversing all ancestors, descendants, and relatives.
    expanded_context: Vec<SourceContext>,
    /// Context derived from recursively traversing ancestors and descendants only.
    ancestry: Vec<SourceContext>,
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
    /// 1. Appear together in comparison, arithmetic, or bitwise binary operations
    /// 2. Appear as alternatives in conditional (ternary) expressions
    /// 3. Are involved in this variable's assignment (RHS of assignments)
    /// Only populated for variable declarations.
    relatives: Vec<topic::Topic>,
    /// When set, this declaration is a transparent proxy for another declaration.
    /// Features should resolve through this to the target topic instead of
    /// operating on this declaration directly. The canonical case is an interface
    /// member with exactly one in-scope implementation — the interface member is
    /// transitive to the implementation member.
    transitive_topic: Option<topic::Topic>,
  },
  UnnamedTopic {
    topic: topic::Topic,
    scope: Scope,
    kind: UnnamedTopicKind,
    /// Expanded context derived from features linked to this topic.
    /// Only populated for documentation topics.
    expanded_context: Vec<SourceContext>,
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
    /// Expanded context derived from features linked to this documentation topic.
    expanded_context: Vec<SourceContext>,
  },
  /// A comment topic with immutable metadata
  CommentTopic {
    topic: topic::Topic,
    scope: Scope,
    target_topic: topic::Topic,
    comment_type: String,
    author_id: i64,
    created_at: String,
    mentioned_topics: Vec<topic::Topic>,
  },
  /// A feature extracted from documentation
  FeatureTopic {
    topic: topic::Topic,
    name: String,
    description: String,
    author_id: i64,
    created_at: String,
  },
  /// A behavioral requirement belonging to a feature
  RequirementTopic {
    topic: topic::Topic,
    description: String,
    feature_topic: topic::Topic,
    /// The D-prefixed documentation section this requirement was extracted from
    section_topic: Option<topic::Topic>,
    author_id: i64,
    created_at: String,
  },
  /// A behavior observed during code review, belonging to one code member
  BehaviorTopic {
    topic: topic::Topic,
    description: String,
    /// The N-prefixed code member (function/modifier/contract) this behavior belongs to
    member_topic: topic::Topic,
    author_id: i64,
    created_at: String,
  },
  /// A threat on a non-pure source code subject
  ThreatTopic {
    topic: topic::Topic,
    description: String,
    /// The non-pure subject this threat belongs to
    subject_topic: topic::Topic,
    author_id: i64,
    created_at: String,
    /// Severity is assigned during impact analysis; None means pending
    severity: Option<ThreatSeverity>,
  },
  /// An invariant that must hold to prevent a threat
  InvariantTopic {
    topic: topic::Topic,
    description: String,
    threat_topic: topic::Topic,
    author_id: i64,
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
      | TopicMetadata::ThreatTopic { topic, .. }
      | TopicMetadata::InvariantTopic { topic, .. }
      | TopicMetadata::DocumentationTopic { topic, .. } => topic,
    }
  }


  pub fn expanded_context(&self) -> &[SourceContext] {
    match self {
      TopicMetadata::NamedTopic {
        expanded_context, ..
      }
      | TopicMetadata::UnnamedTopic {
        expanded_context, ..
      }
      | TopicMetadata::TitledTopic {
        expanded_context, ..
      } => expanded_context,
      _ => &[],
    }
  }

  pub fn ancestry(&self) -> &[SourceContext] {
    match self {
      TopicMetadata::NamedTopic { ancestry, .. } => ancestry,
      _ => &[],
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
      TopicMetadata::RequirementTopic { feature_topic, .. } => {
        Some(feature_topic)
      }
      TopicMetadata::ThreatTopic { subject_topic, .. } => {
        Some(subject_topic)
      }
      TopicMetadata::InvariantTopic { threat_topic, .. } => {
        Some(threat_topic)
      }
      _ => None,
    }
  }

  pub fn author_id(&self) -> Option<i64> {
    match self {
      TopicMetadata::CommentTopic { author_id, .. }
      | TopicMetadata::FeatureTopic { author_id, .. }
      | TopicMetadata::RequirementTopic { author_id, .. }
      | TopicMetadata::ThreatTopic { author_id, .. }
      | TopicMetadata::InvariantTopic { author_id, .. } => Some(*author_id),
      _ => None,
    }
  }

  pub fn comment_type(&self) -> Option<&str> {
    match self {
      TopicMetadata::CommentTopic { comment_type, .. } => {
        Some(comment_type.as_str())
      }
      _ => None,
    }
  }

  pub fn created_at(&self) -> Option<&str> {
    match self {
      TopicMetadata::CommentTopic { created_at, .. }
      | TopicMetadata::FeatureTopic { created_at, .. }
      | TopicMetadata::RequirementTopic { created_at, .. }
      | TopicMetadata::ThreatTopic { created_at, .. }
      | TopicMetadata::InvariantTopic { created_at, .. } => {
        Some(created_at.as_str())
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
          .unwrap_or_else(|| component.id());
        let member_name = audit_data
          .topic_metadata
          .get(member)
          .and_then(|d| d.name())
          .unwrap_or_else(|| member.id());
        format!("{}.{}.{}", component_name, member_name, name)
      }
    })
  }
}

pub enum FunctionModProperties {
  FunctionProperties {
    reverts: Vec<RevertInfo>,
    calls: Vec<topic::Topic>,
    mutations: Vec<topic::Topic>,
  },
  ModifierProperties {
    reverts: Vec<RevertInfo>,
    calls: Vec<topic::Topic>,
    mutations: Vec<topic::Topic>,
  },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

pub fn load_in_scope_files(
  project_root: &Path,
) -> Result<HashSet<ProjectPath>, String> {
  let scope_file = project_root.join("scope.txt");
  if !scope_file.exists() {
    return Err("scope.txt file not found in project root".to_string());
  }

  let content = std::fs::read_to_string(&scope_file)
    .map_err(|e| format!("Failed to read scope.txt: {}", e))?;

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
) -> Result<Vec<DocumentFileEntry>, String> {
  let doc_file = project_root.join("documents.txt");
  if !doc_file.exists() {
    return Err("documents.txt file not found in project root".to_string());
  }

  let content = std::fs::read_to_string(&doc_file)
    .map_err(|e| format!("Failed to read documents.txt: {}", e))?;

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
pub fn load_audit_name(project_root: &Path) -> Result<String, String> {
  let name_file = project_root.join("name.txt");
  if !name_file.exists() {
    return Err("name.txt file not found in project root".to_string());
  }

  let content = std::fs::read_to_string(&name_file)
    .map_err(|e| format!("Failed to read name.txt: {}", e))?;

  let audit_name = content
    .lines()
    .next()
    .ok_or_else(|| "name.txt is empty".to_string())
    .map_err(|_| "Failed to parse name.txt".to_string())?
    .trim()
    .to_string();

  if audit_name.is_empty() {
    return Err("First line of name.txt is empty".to_string());
  }

  Ok(audit_name)
}

/// Reads "security.md" from the project root and returns its contents.
/// This file contains free-form prose describing roles, known threats,
/// invariants, and other security considerations for the audit.
/// Returns `None` if the file does not exist (security notes are optional).
pub fn load_security_notes(
  project_root: &Path,
) -> Result<Option<String>, String> {
  let security_file = project_root.join("security.md");
  if !security_file.exists() {
    return Err(
      "security.md file not found in project root".to_string(),
    );
  }

  let content = std::fs::read_to_string(&security_file)
    .map_err(|e| format!("Failed to read security.md: {}", e))?;

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
    let sort_key = tt.numeric_id().map(|id| id as usize);
    let children = threat
      .invariant_topics
      .iter()
      .map(|inv_topic| {
        let inv_sort_key = inv_topic.numeric_id().map(|id| id as usize);
        SourceChild::Reference(Reference::ProjectReference {
          reference_topic: inv_topic.clone(),
          sort_key: inv_sort_key,
        })
      })
      .collect();
    nested.push(NestedSourceContext {
      subscope: tt.clone(),
      sort_key,
      children,
    });
  }
  nested
}

/// Rebuilds feature-related context:
/// - `expanded_context` on documentation TitledTopics/UnnamedTopics (linked features)
/// - `topic_context` for FeatureTopics (linked requirements)
/// - `topic_context` for RequirementTopics (parent feature)
pub fn rebuild_feature_context(audit_data: &mut AuditData) {
  // Rebuild section_requirements: section D-topic → R-topics
  audit_data.section_requirements.clear();
  for (req_topic, metadata) in &audit_data.topic_metadata {
    if let TopicMetadata::RequirementTopic { section_topic: Some(st), .. } = metadata {
      audit_data
        .section_requirements
        .entry(st.clone())
        .or_default()
        .push(req_topic.clone());
    }
  }

  // Rebuild member_behaviors: member N-topic → B-topics
  audit_data.member_behaviors.clear();
  for (beh_topic, metadata) in &audit_data.topic_metadata {
    if let TopicMetadata::BehaviorTopic { member_topic, .. } = metadata {
      audit_data
        .member_behaviors
        .entry(member_topic.clone())
        .or_default()
        .push(beh_topic.clone());
    }
  }

  // Build reverse index: doc_topic -> [requirement_topics]
  let mut doc_to_requirements: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
  for (req_topic, requirement) in &audit_data.requirements {
    for doc_topic in &requirement.documentation_topics {
      doc_to_requirements
        .entry(doc_topic.clone())
        .or_default()
        .push(req_topic.clone());
    }
  }

  // Update expanded_context for documentation topics (TitledTopic/UnnamedTopic)
  // Show the parent feature(s) of the requirements that link to this doc topic
  let mut doc_to_features: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
  for (doc_topic, req_topics) in &doc_to_requirements {
    for rt in req_topics {
      if let Some(TopicMetadata::RequirementTopic { feature_topic, .. }) =
        audit_data.topic_metadata.get(rt)
      {
        let features = doc_to_features.entry(doc_topic.clone()).or_default();
        if !features.contains(feature_topic) {
          features.push(feature_topic.clone());
        }
      }
    }
  }

  for (topic, metadata) in audit_data.topic_metadata.iter_mut() {
    let expanded_context = match metadata {
      TopicMetadata::TitledTopic {
        expanded_context, ..
      }
      | TopicMetadata::UnnamedTopic {
        expanded_context, ..
      } => expanded_context,
      _ => continue,
    };
    let feature_topics = doc_to_features.remove(topic).unwrap_or_default();
    *expanded_context = feature_topics
      .into_iter()
      .map(|ft| {
        let sort_key = ft.numeric_id().map(|id| id as usize);
        SourceContext {
          scope: ft.clone(),
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
  }

  // Build context for FeatureTopics: feature + requirements + behaviors as scope refs,
  // with threats/invariants as nested refs
  for (feature_topic, metadata) in &audit_data.topic_metadata {
    if !matches!(metadata, TopicMetadata::FeatureTopic { .. }) {
      continue;
    }
    let feature = match audit_data.features.get(feature_topic) {
      Some(f) => f,
      None => continue,
    };
    let mut scope_references = vec![Reference::ProjectReference {
      reference_topic: feature_topic.clone(),
      sort_key: Some(0),
    }];
    for rt in &feature.requirement_topics {
      let sort_key = rt.numeric_id().map(|id| id as usize);
      scope_references.push(Reference::ProjectReference {
        reference_topic: rt.clone(),
        sort_key,
      });
    }

    // Find behaviors linked to this feature via source_feature_links:
    // member_topic -> [feature_topics], then find BehaviorTopics for those members
    let linked_members: Vec<topic::Topic> = audit_data
      .source_feature_links
      .iter()
      .filter(|(_, fts)| fts.contains(feature_topic))
      .map(|(mt, _)| mt.clone())
      .collect();

    for (beh_topic, beh_metadata) in &audit_data.topic_metadata {
      if let TopicMetadata::BehaviorTopic { member_topic, .. } = beh_metadata {
        if linked_members.contains(member_topic) {
          let sort_key = beh_topic.numeric_id().map(|id| id as usize);
          scope_references.push(Reference::ProjectReference {
            reference_topic: beh_topic.clone(),
            sort_key,
          });
        }
      }
    }

    let context = vec![SourceContext {
      scope: feature_topic.clone(),
      sort_key: Some(0),
      is_in_scope: true,
      scope_references,
      nested_references: vec![],
    }];
    audit_data
      .topic_context
      .insert(feature_topic.clone(), context);
  }

  // Build context for RequirementTopics: parent feature as a SourceContext entry
  for (req_topic, metadata) in &audit_data.topic_metadata {
    if let TopicMetadata::RequirementTopic { feature_topic, .. } = metadata {
      let sort_key = feature_topic.numeric_id().map(|id| id as usize);
      let context = vec![SourceContext {
        scope: req_topic.clone(),
        sort_key,
        is_in_scope: true,
        scope_references: vec![Reference::ProjectReference {
          reference_topic: feature_topic.clone(),
          sort_key,
        }],
        nested_references: vec![],
      }];
      audit_data
        .topic_context
        .insert(req_topic.clone(), context);
    }
  }

  // Build context for ThreatTopics: subject + threat as scope refs,
  // invariants as nested refs indented under the threat
  for (threat_topic, metadata) in &audit_data.topic_metadata {
    if let TopicMetadata::ThreatTopic { subject_topic, .. } = metadata {
      let subj_sort_key = subject_topic.numeric_id().map(|id| id as usize);
      let threat_sort_key = threat_topic.numeric_id().map(|id| id as usize);
      let scope_references = vec![
        Reference::ProjectReference {
          reference_topic: subject_topic.clone(),
          sort_key: subj_sort_key,
        },
        Reference::ProjectReference {
          reference_topic: threat_topic.clone(),
          sort_key: threat_sort_key,
        },
      ];
      let nested_references =
        build_invariant_nested_refs(&[threat_topic.clone()], &audit_data.threats);
      let context = vec![SourceContext {
        scope: threat_topic.clone(),
        sort_key: threat_sort_key,
        is_in_scope: true,
        scope_references,
        nested_references,
      }];
      audit_data
        .topic_context
        .insert(threat_topic.clone(), context);
    }
  }

  // Build context for InvariantTopics: parent threat as a SourceContext entry
  for (inv_topic, metadata) in &audit_data.topic_metadata {
    if let TopicMetadata::InvariantTopic { threat_topic, .. } = metadata {
      let sort_key = threat_topic.numeric_id().map(|id| id as usize);
      let context = vec![SourceContext {
        scope: inv_topic.clone(),
        sort_key,
        is_in_scope: true,
        scope_references: vec![Reference::ProjectReference {
          reference_topic: threat_topic.clone(),
          sort_key,
        }],
        nested_references: vec![],
      }];
      audit_data
        .topic_context
        .insert(inv_topic.clone(), context);
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
    keccak256_topic.clone(),
    TopicMetadata::NamedTopic {
      topic: keccak256_topic,
      scope: Scope::Global,
      kind: NamedTopicKind::Builtin,
      visibility: NamedTopicVisibility::Public,
      name: "keccak256".to_string(),
      expanded_context: Vec::new(),
      ancestry: Vec::new(),
      is_mutable: false,
      mutations: Vec::new(),
      ancestors: Vec::new(),
      descendants: Vec::new(),
      relatives: Vec::new(),
      transitive_topic: None,
    },
  );

  // type() function with node_id -27
  let type_topic = topic::new_node_topic(&-27);
  topic_metadata.insert(
    type_topic.clone(),
    TopicMetadata::NamedTopic {
      topic: type_topic,
      scope: Scope::Global,
      kind: NamedTopicKind::Builtin,
      visibility: NamedTopicVisibility::Public,
      name: "type".to_string(),
      expanded_context: Vec::new(),
      ancestry: Vec::new(),
      is_mutable: false,
      mutations: Vec::new(),
      ancestors: Vec::new(),
      descendants: Vec::new(),
      relatives: Vec::new(),
      transitive_topic: None,
    },
  );

  // this keyword with node_id -28
  let this_topic = topic::new_node_topic(&-28);
  topic_metadata.insert(
    this_topic.clone(),
    TopicMetadata::NamedTopic {
      topic: this_topic,
      scope: Scope::Global,
      kind: NamedTopicKind::Builtin,
      visibility: NamedTopicVisibility::Public,
      name: "this".to_string(),
      expanded_context: Vec::new(),
      ancestry: Vec::new(),
      is_mutable: false,
      mutations: Vec::new(),
      ancestors: Vec::new(),
      descendants: Vec::new(),
      relatives: Vec::new(),
      transitive_topic: None,
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
    features: BTreeMap::new(),
    requirements: BTreeMap::new(),
    functional_purposes: BTreeMap::new(),
    functional_semantics: BTreeMap::new(),
    semantic_links: Vec::new(),
    section_requirements: BTreeMap::new(),
    member_behaviors: BTreeMap::new(),
    threat_feature_links: Vec::new(),
    conditions: Vec::new(),
    behaviors: BTreeMap::new(),
    threats: BTreeMap::new(),
    invariants: BTreeMap::new(),
    source_feature_links: BTreeMap::new(),
    mentions_index: HashMap::new(),
  }
}

pub fn new_data_context() -> DataContext {
  DataContext {
    audits: BTreeMap::new(),
    source_text_cache: HashMap::new(),
    topic_view_cache: HashMap::new(),
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

  /// Cache rendered source text HTML for a topic
  pub fn cache_source_text(
    &mut self,
    audit_id: &str,
    topic_id: &str,
    html: String,
  ) {
    self
      .source_text_cache
      .entry(audit_id.to_string())
      .or_default()
      .insert(topic_id.to_string(), html);
  }

  /// Get cached source text HTML for a topic
  pub fn get_cached_source_text(
    &self,
    audit_id: &str,
    topic_id: &str,
  ) -> Option<&String> {
    self
      .source_text_cache
      .get(audit_id)
      .and_then(|m| m.get(topic_id))
  }

  /// Cache static topic view panels (AST-derived, never invalidated)
  pub fn cache_topic_view(
    &mut self,
    audit_id: &str,
    topic_id: &str,
    cached: CachedTopicView,
  ) {
    self
      .topic_view_cache
      .entry(audit_id.to_string())
      .or_default()
      .insert(topic_id.to_string(), cached);
  }

  /// Get cached static topic view panels
  pub fn get_cached_topic_view(
    &self,
    audit_id: &str,
    topic_id: &str,
  ) -> Option<&CachedTopicView> {
    self
      .topic_view_cache
      .get(audit_id)
      .and_then(|m| m.get(topic_id))
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
}

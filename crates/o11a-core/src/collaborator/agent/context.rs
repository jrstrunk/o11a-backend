use std::collections::HashSet;

use serde::Serialize;
use serde_json::json;

use crate::collaborator::parser as comment_parser;
use crate::domain::{
  self, AuditData, BlockAnnotationKind, CommentType, ContractKind,
  ControlFlowStatementKind, FunctionKind, NamedTopicKind, NamedTopicVisibility,
  Node, Reference, SourceChild, SourceContext, TitledTopicKind, TopicMetadata,
  UnnamedTopicKind, VariableMutability, topic,
};

use crate::documentation::ast::DocumentationNode;
use crate::solidity::ast::{ASTNode, contract_members};

// ============================================================================
// Response Types
// ============================================================================

#[derive(Debug, Serialize)]
pub struct AgentTopicContext {
  pub topic: String,
  pub name: String,
  pub kind: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub sub_kind: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub condition: Option<serde_json::Value>,
  pub context: Vec<AgentSourceGroup>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub expanded_context: Option<Vec<AgentSourceGroup>>,
  pub doc_references: Vec<String>,
  pub mentions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AgentScopeTitle {
  pub name: String,
  pub topic: String,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub comments: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AgentSourceGroup {
  pub scope: AgentScopeTitle,
  pub in_scope: bool,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub scope_references: Vec<serde_json::Value>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub nested_references: Vec<AgentNestedGroup>,
}

#[derive(Debug, Serialize)]
pub struct AgentNestedGroup {
  pub subscope: AgentScopeTitle,
  pub children: Vec<serde_json::Value>,
}

// A source child is a raw JSON value — either an AST snippet (for
// references) or an annotated block wrapper.

// ============================================================================
// Utility: Topic name resolution
// ============================================================================

/// Resolve a topic to its display name.
fn resolve_topic_name(topic: &topic::Topic, audit_data: &AuditData) -> String {
  match audit_data.topic_metadata.get(topic) {
    Some(TopicMetadata::NamedTopic { name, .. }) => name.clone(),
    Some(TopicMetadata::TitledTopic { title, .. }) => title.clone(),
    Some(TopicMetadata::UnnamedTopic { kind, .. }) => {
      unnamed_kind_to_string(kind)
    }
    Some(TopicMetadata::ControlFlow { kind, .. }) => {
      control_flow_kind_to_string(kind).to_string()
    }
    Some(TopicMetadata::CommentTopic { comment_type, .. }) => {
      comment_type.as_str().to_string()
    }
    Some(TopicMetadata::FeatureTopic { name, .. }) => name.clone(),
    Some(TopicMetadata::RequirementTopic { description, .. })
    | Some(TopicMetadata::BehaviorTopic { description, .. })
    | Some(TopicMetadata::FunctionalSemanticTopic { description, .. })
    | Some(TopicMetadata::FunctionalPurposeTopic { description, .. })
    | Some(TopicMetadata::PlacementRationaleTopic { description, .. })
    | Some(TopicMetadata::ThreatTopic { description, .. })
    | Some(TopicMetadata::InvariantTopic { description, .. }) => {
      description.clone()
    }
    Some(TopicMetadata::DocumentationTopic { .. }) => topic.id().to_string(),
    None => topic.id().to_string(),
  }
}

fn unnamed_kind_to_string(kind: &UnnamedTopicKind) -> String {
  format!("{:?}", kind)
}

fn control_flow_kind_to_string(
  kind: &ControlFlowStatementKind,
) -> &'static str {
  match kind {
    ControlFlowStatementKind::If => "if",
    ControlFlowStatementKind::For => "for",
    ControlFlowStatementKind::While => "while",
    ControlFlowStatementKind::DoWhile => "do-while",
  }
}

// ============================================================================
// Utility: Plaintext highlighted name
// ============================================================================

/// Produce a plaintext highlighted name for a topic, mirroring the HTML
/// `highlighted_name` used on the frontend.
fn plaintext_name(topic: &topic::Topic, audit_data: &AuditData) -> String {
  match audit_data.topic_metadata.get(topic) {
    Some(metadata) => plaintext_name_from_metadata(metadata),
    None => topic.id().to_string(),
  }
}

fn visibility_prefix(visibility: &NamedTopicVisibility) -> &'static str {
  match visibility {
    NamedTopicVisibility::Public => "pub ",
    NamedTopicVisibility::Private => "priv ",
    NamedTopicVisibility::Internal => "int ",
    NamedTopicVisibility::External => "ext ",
  }
}

fn plaintext_name_from_metadata(metadata: &TopicMetadata) -> String {
  match metadata {
    TopicMetadata::NamedTopic {
      name,
      kind,
      visibility,
      is_mutable,
      ..
    } => match (kind, *is_mutable) {
      (NamedTopicKind::Contract(contract_kind), _) => {
        let kw = match contract_kind {
          ContractKind::Contract => "contract",
          ContractKind::Interface => "interface",
          ContractKind::Library => "library",
          ContractKind::Abstract => "abstract",
        };
        format!("{} {}", kw, name)
      }
      (NamedTopicKind::Function(FunctionKind::Function), _)
      | (NamedTopicKind::Function(FunctionKind::FreeFunction), _) => {
        format!("{}fn {}", visibility_prefix(visibility), name)
      }
      (NamedTopicKind::Function(FunctionKind::Receive), _) => {
        format!("{}receive", visibility_prefix(visibility))
      }
      (NamedTopicKind::Function(FunctionKind::Fallback), _) => {
        format!("{}fallback", visibility_prefix(visibility))
      }
      (NamedTopicKind::Function(FunctionKind::Constructor), _) => {
        "constructor".to_string()
      }
      (NamedTopicKind::Modifier, _) => format!("mod {}", name),
      (NamedTopicKind::Event, _) => {
        format!("{}event {}", visibility_prefix(visibility), name)
      }
      (NamedTopicKind::Error, _) => {
        format!("{}error {}", visibility_prefix(visibility), name)
      }
      (NamedTopicKind::Struct, _) => {
        format!("{}struct {}", visibility_prefix(visibility), name)
      }
      (NamedTopicKind::Enum, _) => {
        format!("{}enum {}", visibility_prefix(visibility), name)
      }
      (NamedTopicKind::EnumMember, _) => name.clone(),
      (NamedTopicKind::StateVariable(_), true)
      | (NamedTopicKind::StateVariable(VariableMutability::Mutable), _) => {
        format!("{}{}", visibility_prefix(visibility), name)
      }
      (NamedTopicKind::StateVariable(VariableMutability::Constant), false) => {
        format!("{}const {}", visibility_prefix(visibility), name)
      }
      (NamedTopicKind::StateVariable(VariableMutability::Immutable), false) => {
        format!("{}immutable {}", visibility_prefix(visibility), name)
      }
      (NamedTopicKind::LocalVariable, _) => name.clone(),
      (NamedTopicKind::Builtin, _) => name.clone(),
    },
    TopicMetadata::TitledTopic { title, .. } => title.clone(),
    TopicMetadata::UnnamedTopic { kind, .. } => unnamed_kind_to_string(kind),
    TopicMetadata::ControlFlow { kind, .. } => {
      control_flow_kind_to_string(kind).to_string()
    }
    TopicMetadata::CommentTopic { comment_type, .. } => {
      comment_type.as_str().to_string()
    }
    TopicMetadata::FeatureTopic { name, .. } => name.clone(),
    TopicMetadata::RequirementTopic { description, .. }
    | TopicMetadata::BehaviorTopic { description, .. }
    | TopicMetadata::FunctionalSemanticTopic { description, .. }
    | TopicMetadata::FunctionalPurposeTopic { description, .. }
    | TopicMetadata::PlacementRationaleTopic { description, .. }
    | TopicMetadata::ThreatTopic { description, .. }
    | TopicMetadata::InvariantTopic { description, .. } => description.clone(),
    TopicMetadata::DocumentationTopic { .. } => {
      metadata.topic().id().to_string()
    }
  }
}

/// Build an `AgentScopeTitle` for a topic: plaintext name, topic id, and
/// any comments targeting that topic. See [`lookup_topic_comments`] for the
/// meaning of `include_untrusted`.
fn build_scope_title(
  topic: &topic::Topic,
  audit_data: &AuditData,
  include_untrusted: bool,
) -> AgentScopeTitle {
  let name = plaintext_name(topic, audit_data);
  let comments = lookup_topic_comments(topic, audit_data, include_untrusted);
  AgentScopeTitle {
    name,
    topic: topic.id().to_string(),
    comments,
  }
}

/// Look up comments targeting a topic from the CommentIndex.
///
/// Resolves through the transitive chain so that looking up a signature topic
/// finds comments stored on its canonical definition topic.
///
/// `include_untrusted` controls whether source-derived comments
/// (`DevTechnical` from inline `//` and `/* */`, `DevDocumentation` from
/// NatSpec) are returned. Auditor-authored `Info` comments are always
/// returned. Contexts that feed agent tasks which must operate only on
/// trusted, pipeline-generated content (e.g. behavior extraction) should pass
/// `false`; contexts that surface the developer's own prose to humans or to
/// semantic-linking agents should pass `true`.
fn lookup_topic_comments(
  topic: &topic::Topic,
  audit_data: &AuditData,
  include_untrusted: bool,
) -> Vec<String> {
  let resolved =
    domain::resolve_transitive_topic(topic, &audit_data.topic_metadata);
  let comment_topics = audit_data
    .comment_index
    .get(&resolved)
    .map(|v| v.as_slice())
    .unwrap_or(&[]);
  comment_topics
    .iter()
    .filter_map(|comment_topic| {
      let metadata = audit_data.topic_metadata.get(comment_topic)?;
      let TopicMetadata::CommentTopic { comment_type, .. } = metadata else {
        return None;
      };
      let is_untrusted = *comment_type == CommentType::DevTechnical
        || *comment_type == CommentType::DevDocumentation;
      let is_relevant = *comment_type == CommentType::Info || is_untrusted;
      if !is_relevant {
        return None;
      }
      if is_untrusted && !include_untrusted {
        return None;
      }
      let content = match audit_data.nodes.get(comment_topic) {
        Some(Node::Comment(nodes)) => {
          comment_parser::render_comment_plain_text(nodes)
        }
        _ => return None,
      };
      let content = content.trim().to_string();
      if content.is_empty() {
        return None;
      }
      // Prefix developer documentation comments so the agent can distinguish
      // them from auditor-authored comments.
      let prefixed = match comment_type {
        CommentType::DevTechnical => format!("[dev] {}", content),
        CommentType::DevDocumentation => format!("[dev docs] {}", content),
        _ => content,
      };
      Some(prefixed)
    })
    .collect()
}

// ============================================================================
// Utility: Kind/visibility formatting
// ============================================================================

fn named_kind_to_string(kind: &NamedTopicKind) -> (String, Option<String>) {
  match kind {
    NamedTopicKind::Contract(contract_kind) => {
      ("Contract".to_string(), Some(format!("{:?}", contract_kind)))
    }
    NamedTopicKind::Function(function_kind) => {
      ("Function".to_string(), Some(format!("{:?}", function_kind)))
    }
    NamedTopicKind::StateVariable(mutability) => (
      "StateVariable".to_string(),
      Some(format!("{:?}", mutability)),
    ),
    kind => (format!("{:?}", kind), None),
  }
}

// ============================================================================
// Utility: Control flow annotation rendering
// ============================================================================

/// Render the condition of a control flow annotation as an AST snippet.
fn render_condition_ast_snippet(
  annotation_topic: &topic::Topic,
  annotation_kind: &BlockAnnotationKind,
  target_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Option<serde_json::Value> {
  match annotation_kind {
    BlockAnnotationKind::If(_)
    | BlockAnnotationKind::While
    | BlockAnnotationKind::DoWhile
    | BlockAnnotationKind::For => {}
    _ => return None,
  }

  let condition_topic = match audit_data.nodes.get(annotation_topic) {
    Some(Node::Solidity(ast_node)) => get_condition_topic(ast_node),
    _ => None,
  }?;

  match audit_data.nodes.get(&condition_topic) {
    Some(Node::Solidity(node)) => {
      let render_ctx = ASTRenderContext {
        target_topic: *target_topic,
        omit_function_and_modifier_bodies: false,
        include_untrusted_comments: true,
        flag_non_pure_subjects: false,
      };
      Some(render_solidity_ast_snippet(node, &render_ctx, audit_data))
    }
    _ => None,
  }
}

/// Get the condition topic from a control flow AST node.
fn get_condition_topic(node: &ASTNode) -> Option<topic::Topic> {
  match node {
    ASTNode::IfStatement { condition, .. }
    | ASTNode::WhileStatement { condition, .. }
    | ASTNode::DoWhileStatement { condition, .. } => {
      Some(topic::new_node_topic(&condition.node_id()))
    }
    ASTNode::ForStatement { condition, .. } => {
      Some(topic::new_node_topic(&condition.node_id()))
    }
    _ => None,
  }
}

fn annotation_kind_to_string(kind: &BlockAnnotationKind) -> &'static str {
  match kind {
    BlockAnnotationKind::If(domain::ControlFlowBranch::True) => "if_true",
    BlockAnnotationKind::If(domain::ControlFlowBranch::False) => "if_false",
    BlockAnnotationKind::For => "for",
    BlockAnnotationKind::While => "while",
    BlockAnnotationKind::DoWhile => "do_while",
    BlockAnnotationKind::Unchecked => "unchecked",
    BlockAnnotationKind::InlineAssembly => "assembly",
  }
}

// ============================================================================
// AST Snippet Rendering
// ============================================================================

/// Controls how an agent task wants Solidity AST nodes rendered. The same
/// underlying renderer (`render_solidity_ast_snippet`) is shared across
/// every agent task — semantic linking, behavior extraction, contract-list
/// rendering — and this context is the single knob that differentiates the
/// variants. Mirrors the `RenderContext`-style pattern used by the
/// web-backend's `topic_view::render_source_text`.
pub struct ASTRenderContext {
  /// "Keep this member's body expanded even when
  /// `omit_function_and_modifier_bodies` is true." Useful when rendering a
  /// whole contract tree where every other member should appear as a
  /// signature stub but one specific member's body should be expanded
  /// (e.g., topic views).
  ///
  /// **Sentinel for "no override":** when `omit_function_and_modifier_bodies`
  /// should apply uniformly with no per-member exception, set
  /// `target_topic` to `topic::new_node_topic(&-1)` (no real AST node has
  /// negative `node_id`, so it never matches). Single-member render calls
  /// that want pure signature behavior should use this sentinel —
  /// otherwise `target_topic == member_topic` would re-expand the very
  /// body the caller asked to strip.
  pub target_topic: topic::Topic,
  /// When true, function and modifier bodies are stripped. The
  /// `target_topic` field above can override this on a per-member basis
  /// during tree rendering.
  pub omit_function_and_modifier_bodies: bool,
  /// Whether source-derived (untrusted) comments — inline `//` dev
  /// comments and NatSpec docstrings — should appear in the rendered
  /// output. Set to `false` when the rendering feeds an agent task that
  /// must operate only on trusted, pipeline-generated content (behavior
  /// extraction, where only inline semantic/behavior annotations are
  /// trusted). Set to `true` when the developer's prose is useful context
  /// (semantic linking, topic views). Auditor-authored `Info` comments
  /// are always included regardless.
  pub include_untrusted_comments: bool,
  /// When true, every rendered AST node whose topic resolves to a non-pure
  /// subject gets `"is_non_pure": true` injected into its JSON. Used by the
  /// functional purpose / placement rationale generation step (pipeline
  /// step 5) so the LLM can see, inline in the AST, which subjects it
  /// must produce output for. Defaults to false everywhere else.
  pub flag_non_pure_subjects: bool,
}

/// Render a type AST node to a plain-text string directly from its fields.
fn render_type_name(node: &ASTNode, audit_data: &AuditData) -> String {
  let resolved = node.resolve(&audit_data.nodes);
  match resolved {
    ASTNode::ElementaryTypeName { name, .. } => name.clone(),
    ASTNode::UserDefinedTypeName { path_node, .. } => {
      render_type_name(path_node, audit_data)
    }
    ASTNode::IdentifierPath { name, .. } => name.clone(),
    ASTNode::Identifier { name, .. } => name.clone(),
    ASTNode::ArrayTypeName { base_type, .. } => {
      format!("{}[]", render_type_name(base_type, audit_data))
    }
    ASTNode::Mapping {
      key_type,
      value_type,
      ..
    } => {
      format!(
        "mapping({} => {})",
        render_type_name(key_type, audit_data),
        render_type_name(value_type, audit_data)
      )
    }
    ASTNode::FunctionTypeName { .. } => "function".to_string(),
    _ => "unknown".to_string(),
  }
}

/// Look up comments targeting a node. See [`lookup_topic_comments`] for the
/// meaning of `include_untrusted`.
fn lookup_node_comments(
  node_id: i32,
  audit_data: &AuditData,
  include_untrusted: bool,
) -> Vec<String> {
  let node_topic = topic::new_node_topic(&node_id);
  lookup_topic_comments(&node_topic, audit_data, include_untrusted)
}

/// Look up functional semantics for a topic, returning (topic, description) pairs.
fn lookup_topic_semantics(
  node_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<serde_json::Value> {
  let Some(sem_topics) = audit_data.declaration_semantics.get(node_topic)
  else {
    return Vec::new();
  };
  sem_topics
    .iter()
    .filter_map(|sem_topic| {
      if let Some(TopicMetadata::FunctionalSemanticTopic {
        description,
        topic: sem_id,
        ..
      }) = audit_data.topic_metadata.get(sem_topic)
      {
        Some(json!({
          "topic": sem_id.id(),
          "description": description,
        }))
      } else {
        None
      }
    })
    .collect()
}

/// Look up behaviors for a topic, returning (topic, description) pairs.
/// Only member-level topics (functions/modifiers) will have behaviors.
fn lookup_topic_behaviors(
  node_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<serde_json::Value> {
  let Some(beh_topics) = audit_data.member_behaviors.get(node_topic) else {
    return Vec::new();
  };
  beh_topics
    .iter()
    .filter_map(|beh_topic| {
      if let Some(TopicMetadata::BehaviorTopic {
        topic: beh_id,
        description,
        ..
      }) = audit_data.topic_metadata.get(beh_topic)
      {
        Some(json!({
          "topic": beh_id.id(),
          "description": description,
        }))
      } else {
        None
      }
    })
    .collect()
}

/// Convenience: look up semantics for a node by its node_id.
fn lookup_node_semantics(
  node_id: i32,
  audit_data: &AuditData,
) -> Vec<serde_json::Value> {
  let node_topic = topic::new_node_topic(&node_id);
  lookup_topic_semantics(&node_topic, audit_data)
}

/// Convenience: look up behaviors for a node by its node_id.
fn lookup_node_behaviors(
  node_id: i32,
  audit_data: &AuditData,
) -> Vec<serde_json::Value> {
  let node_topic = topic::new_node_topic(&node_id);
  lookup_topic_behaviors(&node_topic, audit_data)
}

fn lookup_doc_node_comments(
  node_id: i32,
  audit_data: &AuditData,
  include_untrusted: bool,
) -> Vec<String> {
  let doc_topic = topic::new_documentation_topic(node_id);
  lookup_topic_comments(&doc_topic, audit_data, include_untrusted)
}

/// Build a JSON object for a node, attaching comments, semantics, and
/// behaviors if present.
fn make_node_json(
  mut obj: serde_json::Value,
  comments: Vec<String>,
  semantics: Vec<serde_json::Value>,
  behaviors: Vec<serde_json::Value>,
) -> serde_json::Value {
  if !comments.is_empty() {
    obj["comments"] = json!(comments);
  }
  if !semantics.is_empty() {
    obj["semantics"] = json!(semantics);
  }
  if !behaviors.is_empty() {
    obj["behaviors"] = json!(behaviors);
  }
  obj
}

/// Returns true when the topic for `node_id` is an `UnnamedTopic` whose
/// kind classifies as `NonPure` per `UnnamedTopicKind::purity()`. Used by
/// the renderer when `ASTRenderContext::flag_non_pure_subjects` is set, to
/// inject `is_non_pure: true` on each non-pure subject's JSON.
fn node_is_non_pure(node_id: i32, audit_data: &AuditData) -> bool {
  let topic = topic::new_node_topic(&node_id);
  match audit_data.topic_metadata.get(&topic) {
    Some(TopicMetadata::UnnamedTopic { kind, .. }) => {
      matches!(kind.purity(), crate::domain::SubjectPurity::NonPure)
    }
    _ => false,
  }
}

/// Render an ASTNode as a structured AST snippet (JSON value).
fn render_solidity_ast_snippet(
  node: &ASTNode,
  render_ctx: &ASTRenderContext,
  audit_data: &AuditData,
) -> serde_json::Value {
  let resolved = node.resolve(&audit_data.nodes);

  // Unresolved stub → TopicRef
  if let ASTNode::Stub { node_id, topic, .. } = resolved {
    let name = resolve_topic_name(topic, audit_data);
    let comments = lookup_node_comments(
      *node_id,
      audit_data,
      render_ctx.include_untrusted_comments,
    );
    // Semantics and behaviors belong to the referenced topic, not the
    // stub itself.
    let semantics = lookup_topic_semantics(topic, audit_data);
    let behaviors = lookup_topic_behaviors(topic, audit_data);
    return make_node_json(
      json!({
        "type": "topic_ref",
        "id": topic.id(),
        "name": name,
      }),
      comments,
      semantics,
      behaviors,
    );
  }

  let node_id = resolved.node_id();
  let id = topic::new_node_topic(&node_id).id().to_string();
  let comments = lookup_node_comments(
    node_id,
    audit_data,
    render_ctx.include_untrusted_comments,
  );
  let semantics = lookup_node_semantics(node_id, audit_data);
  let behaviors = lookup_node_behaviors(node_id, audit_data);

  // Helper closure for recursive conversion
  let recurse = |child: &ASTNode| -> serde_json::Value {
    render_solidity_ast_snippet(child, render_ctx, audit_data)
  };

  // Flatten comment-less SemanticBlocks when rendering statement lists
  let recurse_statements = |stmts: &[ASTNode]| -> Vec<serde_json::Value> {
    stmts
      .iter()
      .flat_map(|s| {
        let resolved_s = s.resolve(&audit_data.nodes);
        if let ASTNode::SemanticBlock { statements, .. } = resolved_s {
          let node_id = resolved_s.node_id();
          let comments = lookup_node_comments(
            node_id,
            audit_data,
            render_ctx.include_untrusted_comments,
          );
          if comments.is_empty() {
            // Flatten: recurse into the inner statements directly
            return statements
              .iter()
              .map(|inner| {
                render_solidity_ast_snippet(inner, render_ctx, audit_data)
              })
              .collect::<Vec<_>>();
          }
        }
        vec![render_solidity_ast_snippet(s, render_ctx, audit_data)]
      })
      .collect()
  };

  // Extract statements from a body node (Block/SemanticBlock/UncheckedBlock)
  let body_statements = |body: &ASTNode| -> Vec<serde_json::Value> {
    let resolved_body = body.resolve(&audit_data.nodes);
    let stmts = match resolved_body {
      ASTNode::Block { statements, .. }
      | ASTNode::SemanticBlock { statements, .. }
      | ASTNode::UncheckedBlock { statements, .. } => statements,
      _ => {
        return vec![render_solidity_ast_snippet(body, render_ctx, audit_data)];
      }
    };
    recurse_statements(stmts)
  };

  let obj = match resolved {
    // === Leaf nodes ===
    ASTNode::Identifier {
      name,
      referenced_declaration,
      ..
    }
    | ASTNode::IdentifierPath {
      name,
      referenced_declaration,
      ..
    } => json!({
      "type": "identifier",
      "name": name,
      "referenced_declaration": topic::new_node_topic(referenced_declaration).id(),
    }),

    ASTNode::Literal { kind, value, .. } => json!({
      "type": "literal",
      "id": id,
      "kind": kind.as_str(),
      "value": value,
    }),

    // === Type nodes ===
    ASTNode::ElementaryTypeName { .. }
    | ASTNode::UserDefinedTypeName { .. }
    | ASTNode::ArrayTypeName { .. }
    | ASTNode::Mapping { .. }
    | ASTNode::FunctionTypeName { .. } => json!({
      "type": "type_name",
      "id": id,
      "name": render_type_name(resolved, audit_data),
    }),

    // === Expression nodes ===
    ASTNode::Assignment {
      operator,
      left_hand_side,
      right_hand_side,
      ..
    } => json!({
      "type": "assignment",
      "id": id,
      "operator": operator.as_str(),
      "left": recurse(left_hand_side),
      "right": recurse(right_hand_side),
    }),

    ASTNode::BinaryOperation {
      operator,
      left_expression,
      right_expression,
      ..
    } => json!({
      "type": "binary_operation",
      "id": id,
      "operator": operator.as_str(),
      "left": recurse(left_expression),
      "right": recurse(right_expression),
    }),

    ASTNode::UnaryOperation {
      operator,
      prefix,
      sub_expression,
      ..
    } => json!({
      "type": "unary_operation",
      "id": id,
      "operator": operator.as_str(),
      "prefix": prefix,
      "operand": recurse(sub_expression),
    }),

    ASTNode::FunctionCall {
      expression,
      arguments,
      ..
    } => json!({
      "type": "function_call",
      "id": id,
      "expression": recurse(expression),
      "arguments": arguments.iter().map(&recurse).collect::<Vec<_>>(),
    }),

    ASTNode::TypeConversion {
      expression,
      argument,
      ..
    } => json!({
      "type": "function_call",
      "id": id,
      "expression": recurse(expression),
      "arguments": [recurse(argument)],
    }),

    ASTNode::StructConstructor {
      expression,
      arguments,
      ..
    } => json!({
      "type": "function_call",
      "id": id,
      "expression": recurse(expression),
      "arguments": arguments.iter().map(&recurse).collect::<Vec<_>>(),
    }),

    ASTNode::MemberAccess {
      expression,
      member_name,
      referenced_declaration,
      ..
    } => {
      let mut obj = json!({
        "type": "member_access",
        "id": id,
        "expression": recurse(expression),
        "member": member_name,
      });
      if let Some(ref_decl) = referenced_declaration {
        obj["referenced_declaration"] =
          json!(topic::new_node_topic(ref_decl).id());
      }
      obj
    }

    ASTNode::IndexAccess {
      base_expression,
      index_expression,
      ..
    } => {
      let mut obj = json!({
        "type": "index_access",
        "id": id,
        "base": recurse(base_expression),
      });
      if let Some(index) = index_expression {
        obj["index"] = recurse(index);
      }
      obj
    }

    ASTNode::Conditional {
      condition,
      true_expression,
      false_expression,
      ..
    } => {
      let mut obj = json!({
        "type": "conditional",
        "id": id,
        "condition": recurse(condition),
        "true_expression": recurse(true_expression),
      });
      if let Some(false_expr) = false_expression {
        obj["false_expression"] = recurse(false_expr);
      }
      obj
    }

    ASTNode::TupleExpression { components, .. } => json!({
      "type": "tuple",
      "id": id,
      "components": components.iter().map(&recurse).collect::<Vec<_>>(),
    }),

    // === Statement nodes ===
    ASTNode::ExpressionStatement { expression, .. } => json!({
      "type": "statement",
      "id": id,
      "kind": "expression",
      "expression": recurse(expression),
    }),

    ASTNode::Return { expression, .. } => {
      let mut obj = json!({
        "type": "statement",
        "id": id,
        "kind": "return",
      });
      if let Some(expr) = expression {
        obj["expression"] = recurse(expr);
      }
      obj
    }

    ASTNode::EmitStatement { event_call, .. } => json!({
      "type": "statement",
      "id": id,
      "kind": "emit",
      "expression": recurse(event_call),
    }),

    ASTNode::RevertStatement { error_call, .. } => json!({
      "type": "statement",
      "id": id,
      "kind": "revert",
      "expression": recurse(error_call),
    }),

    ASTNode::Break { .. } => json!({
      "type": "statement",
      "id": id,
      "kind": "break",
    }),

    ASTNode::Continue { .. } => json!({
      "type": "statement",
      "id": id,
      "kind": "continue",
    }),

    ASTNode::PlaceholderStatement { .. } => json!({
      "type": "statement",
      "id": id,
      "kind": "placeholder",
    }),

    // === Variable declarations ===
    ASTNode::VariableDeclarationStatement {
      declarations,
      initial_value,
      ..
    } => {
      let mut obj = json!({
        "type": "variable_declaration",
        "id": id,
        "declarations": declarations.iter().map(&recurse).collect::<Vec<_>>(),
      });
      if let Some(val) = initial_value {
        obj["initial_value"] = recurse(val);
      }
      obj
    }

    ASTNode::VariableDeclaration {
      name,
      type_name,
      value,
      parameter_variable,
      ..
    } => {
      let decl_type = if parameter_variable.is_some() {
        "param_variable_declaration"
      } else {
        "variable_declaration"
      };
      let mut obj = json!({
        "type": decl_type,
        "id": id,
        "name": name,
        "type_name": render_type_name(type_name, audit_data),
      });
      if let Some(val) = value {
        obj["initial_value"] = recurse(val);
      }
      obj
    }

    // === Block nodes ===
    ASTNode::Block { statements, .. } => json!({
      "type": "block",
      "id": id,
      "statements": recurse_statements(statements),
    }),

    ASTNode::SemanticBlock { statements, .. } => json!({
      "type": "block",
      "id": id,
      "kind": "semantic",
      "statements": recurse_statements(statements),
    }),

    ASTNode::ContractMemberGroup { members, .. } => json!({
      "type": "block",
      "id": id,
      "kind": "contract_member_group",
      "members": recurse_statements(members),
    }),

    ASTNode::UncheckedBlock { statements, .. } => json!({
      "type": "block",
      "id": id,
      "kind": "unchecked",
      "statements": recurse_statements(statements),
    }),

    // === Control flow ===
    ASTNode::IfStatement {
      condition,
      true_body,
      false_body,
      ..
    } => {
      let mut obj = json!({
        "type": "control_flow",
        "id": id,
        "kind": "if",
        "condition": recurse(condition),
        "true_body_statements": body_statements(true_body),
      });
      if let Some(fb) = false_body {
        obj["false_body_statements"] = json!(body_statements(fb));
      }
      obj
    }

    ASTNode::ForStatement {
      condition, body, ..
    } => json!({
      "type": "control_flow",
      "id": id,
      "kind": "for",
      "condition": recurse(condition),
      "body_statements": body_statements(body),
    }),

    ASTNode::WhileStatement {
      condition, body, ..
    } => {
      let mut obj = json!({
        "type": "control_flow",
        "id": id,
        "kind": "while",
        "condition": recurse(condition),
      });
      if let Some(b) = body {
        obj["body_statements"] = json!(body_statements(b));
      }
      obj
    }

    ASTNode::DoWhileStatement {
      condition, body, ..
    } => {
      let mut obj = json!({
        "type": "control_flow",
        "id": id,
        "kind": "do_while",
        "condition": recurse(condition),
      });
      if let Some(b) = body {
        obj["body_statements"] = json!(body_statements(b));
      }
      obj
    }

    // === Definitions ===
    ASTNode::ContractDefinition {
      signature, nodes, ..
    } => {
      let (name, kind) = match signature.as_ref() {
        ASTNode::ContractSignature {
          name,
          contract_kind,
          ..
        } => (name.clone(), format!("{:?}", contract_kind).to_lowercase()),
        _ => ("unknown".to_string(), "contract".to_string()),
      };

      let member_ctx = ASTRenderContext {
        target_topic: render_ctx.target_topic,
        omit_function_and_modifier_bodies: true,
        include_untrusted_comments: render_ctx.include_untrusted_comments,
        flag_non_pure_subjects: render_ctx.flag_non_pure_subjects,
      };
      let members: Vec<serde_json::Value> = nodes
        .iter()
        .map(|n| render_solidity_ast_snippet(n, &member_ctx, audit_data))
        .collect();

      json!({
        "type": "contract_definition",
        "id": id,
        "name": name,
        "kind": kind,
        "signature": render_solidity_ast_snippet(signature, render_ctx, audit_data),
        "members": members,
      })
    }

    ASTNode::FunctionDefinition {
      node_id,
      signature,
      body,
      ..
    } => {
      let (name, kind) = match signature.as_ref() {
        ASTNode::FunctionSignature { name, kind, .. } => {
          (name.clone(), format!("{:?}", kind).to_lowercase())
        }
        ASTNode::ModifierSignature { name, .. } => {
          (name.clone(), "modifier".to_string())
        }
        _ => ("unknown".to_string(), "function".to_string()),
      };

      let sig_json =
        render_solidity_ast_snippet(signature, render_ctx, audit_data);

      let is_target = render_ctx.target_topic == topic::new_node_topic(node_id);
      let body_stmts =
        if !render_ctx.omit_function_and_modifier_bodies || is_target {
          body.as_ref().map(|b| body_statements(b))
        } else {
          None
        };

      let mut obj = json!({
        "type": "function_definition",
        "id": id,
        "name": name,
        "kind": kind,
        "signature": sig_json,
      });
      if let Some(stmts) = body_stmts {
        obj["body_statements"] = json!(stmts);
      }
      obj
    }

    ASTNode::ModifierDefinition {
      node_id,
      signature,
      body,
      ..
    } => {
      let name = match signature.as_ref() {
        ASTNode::ModifierSignature { name, .. } => name.clone(),
        _ => "unknown".to_string(),
      };

      let sig_json =
        render_solidity_ast_snippet(signature, render_ctx, audit_data);

      let is_target = render_ctx.target_topic == topic::new_node_topic(node_id);
      let body_stmts =
        if !render_ctx.omit_function_and_modifier_bodies || is_target {
          Some(body_statements(body))
        } else {
          None
        };

      let mut obj = json!({
        "type": "function_definition",
        "id": id,
        "name": name,
        "kind": "modifier",
        "signature": sig_json,
      });
      if let Some(stmts) = body_stmts {
        obj["body_statements"] = json!(stmts);
      }
      obj
    }

    // === Additional definitions ===
    ASTNode::ErrorDefinition {
      name, parameters, ..
    } => json!({
      "type": "error_definition",
      "id": id,
      "name": name,
      "parameters": recurse(parameters),
    }),

    ASTNode::EventDefinition {
      name, parameters, ..
    } => json!({
      "type": "event_definition",
      "id": id,
      "name": name,
      "parameters": recurse(parameters),
    }),

    ASTNode::StructDefinition { name, members, .. } => json!({
      "type": "struct_definition",
      "id": id,
      "name": name,
      "members": members.iter().map(&recurse).collect::<Vec<_>>(),
    }),

    ASTNode::EnumDefinition { name, members, .. } => json!({
      "type": "enum_definition",
      "id": id,
      "name": name,
      "members": members.iter().map(&recurse).collect::<Vec<_>>(),
    }),

    ASTNode::EnumValue { name, .. } => json!({
      "type": "enum_value",
      "id": id,
      "name": name,
    }),

    ASTNode::UserDefinedValueTypeDefinition {
      name,
      underlying_type,
      ..
    } => json!({
      "type": "type_definition",
      "id": id,
      "name": name,
      "underlying_type": render_type_name(underlying_type, audit_data),
    }),

    // === Signatures ===
    ASTNode::ContractSignature {
      name,
      contract_kind,
      abstract_,
      base_contracts,
      directives,
      ..
    } => {
      let mut obj = json!({
        "type": "contract_signature",
        "id": id,
        "name": name,
        "kind": format!("{:?}", contract_kind).to_lowercase(),
      });
      if *abstract_ {
        obj["abstract"] = json!(true);
      }
      if !base_contracts.is_empty() {
        obj["base_contracts"] =
          json!(base_contracts.iter().map(&recurse).collect::<Vec<_>>());
      }
      if !directives.is_empty() {
        obj["directives"] =
          json!(directives.iter().map(&recurse).collect::<Vec<_>>());
      }
      obj
    }

    ASTNode::FunctionSignature {
      name,
      kind,
      visibility,
      state_mutability,
      parameters,
      return_parameters,
      modifiers,
      virtual_,
      ..
    } => {
      let mut obj = json!({
        "type": "function_signature",
        "id": id,
        "name": name,
        "kind": format!("{:?}", kind).to_lowercase(),
        "visibility": format!("{:?}", visibility).to_lowercase(),
        "state_mutability": format!("{:?}", state_mutability).to_lowercase(),
        "parameters": recurse(parameters),
        "return_parameters": recurse(return_parameters),
        "modifiers": recurse(modifiers),
      });
      if *virtual_ {
        obj["virtual"] = json!(true);
      }
      obj
    }

    ASTNode::ModifierSignature {
      name,
      parameters,
      virtual_,
      ..
    } => {
      let mut obj = json!({
        "type": "modifier_signature",
        "id": id,
        "name": name,
        "parameters": recurse(parameters),
      });
      if *virtual_ {
        obj["virtual"] = json!(true);
      }
      obj
    }

    // === Parameter/modifier lists ===
    ASTNode::ParameterList { parameters, .. } => json!({
      "type": "parameter_list",
      "id": id,
      "parameters": parameters.iter().map(&recurse).collect::<Vec<_>>(),
    }),

    ASTNode::ModifierList { modifiers, .. } => json!({
      "type": "modifier_list",
      "id": id,
      "modifiers": modifiers.iter().map(&recurse).collect::<Vec<_>>(),
    }),

    ASTNode::ModifierInvocation {
      modifier_name,
      arguments,
      ..
    } => {
      let mut obj = json!({
        "type": "modifier_invocation",
        "id": id,
        "name": recurse(modifier_name),
      });
      if let Some(args) = arguments {
        obj["arguments"] = json!(args.iter().map(&recurse).collect::<Vec<_>>());
      }
      obj
    }

    ASTNode::InheritanceSpecifier { base_name, .. } => json!({
      "type": "inheritance",
      "id": id,
      "base": recurse(base_name),
    }),

    // === Other structural nodes ===
    ASTNode::UsingForDirective {
      library_name,
      type_name,
      global,
      ..
    } => {
      let mut obj = json!({
        "type": "using_directive",
        "id": id,
        "global": global,
      });
      if let Some(lib) = library_name {
        obj["library"] = recurse(lib);
      }
      if let Some(ty) = type_name {
        obj["for_type"] = json!(render_type_name(ty, audit_data));
      }
      obj
    }

    ASTNode::StructuredDocumentation { text, .. } => json!({
      "type": "documentation",
      "id": id,
      "text": text,
    }),

    ASTNode::ElementaryTypeNameExpression { type_name, .. } => json!({
      "type": "type_name",
      "id": id,
      "name": render_type_name(type_name, audit_data),
    }),

    ASTNode::Argument {
      parameter,
      argument,
      ..
    } => {
      let mut obj = json!({
        "type": "argument",
        "id": id,
        "argument": recurse(argument),
      });
      if let Some(param) = parameter {
        obj["parameter"] = recurse(param);
      }
      obj
    }

    ASTNode::FunctionCallOptions {
      expression,
      options,
      ..
    } => json!({
      "type": "function_call_options",
      "id": id,
      "expression": recurse(expression),
      "options": options.iter().map(&recurse).collect::<Vec<_>>(),
    }),

    ASTNode::IndexRangeAccess { nodes, body, .. } => {
      let mut obj = json!({
        "type": "index_range_access",
        "id": id,
        "nodes": nodes.iter().map(&recurse).collect::<Vec<_>>(),
      });
      if let Some(b) = body {
        obj["body"] = recurse(b);
      }
      obj
    }

    ASTNode::NewExpression { type_name, .. } => json!({
      "type": "new_expression",
      "id": id,
      "type_name": render_type_name(type_name, audit_data),
    }),

    ASTNode::LoopExpression {
      initialization_expression,
      condition,
      loop_expression,
      is_simple_counter_loop,
      ..
    } => {
      let mut obj = json!({
        "type": "loop_expression",
        "id": id,
        "is_simple_counter_loop": is_simple_counter_loop,
      });
      if let Some(init) = initialization_expression {
        obj["initialization"] = recurse(init);
      }
      if let Some(cond) = condition {
        obj["condition"] = recurse(cond);
      }
      if let Some(loop_expr) = loop_expression {
        obj["loop_expression"] = recurse(loop_expr);
      }
      obj
    }

    ASTNode::InlineAssembly { .. } => json!({
      "type": "inline_assembly",
      "id": id,
    }),

    ASTNode::TryStatement {
      clauses,
      external_call,
      ..
    } => json!({
      "type": "control_flow",
      "id": id,
      "kind": "try",
      "external_call": recurse(external_call),
      "clauses": clauses.iter().map(&recurse).collect::<Vec<_>>(),
    }),

    ASTNode::PragmaDirective { literals, .. } => json!({
      "type": "pragma_directive",
      "id": id,
      "literals": literals,
    }),

    ASTNode::ImportDirective {
      absolute_path,
      file,
      ..
    } => json!({
      "type": "import_directive",
      "id": id,
      "file": file,
      "absolute_path": absolute_path,
    }),

    ASTNode::SourceUnit { nodes, .. } => json!({
      "type": "source_unit",
      "id": id,
      "nodes": nodes.iter().map(&recurse).collect::<Vec<_>>(),
    }),

    ASTNode::TryCatchClause {
      error_name,
      block,
      parameters,
      ..
    } => {
      let mut obj = json!({
        "type": "try_catch_clause",
        "id": id,
        "error_name": error_name,
        "body_statements": body_statements(block),
      });
      if let Some(params) = parameters {
        obj["parameters"] = recurse(params);
      }
      obj
    }

    // Note: Stub is handled at the top of the function before id/comments
    // are computed. This arm handles the unreachable case after resolve().
    ASTNode::Stub { topic, .. } => {
      let name = resolve_topic_name(topic, audit_data);
      json!({
        "type": "topic_ref",
        "id": topic.id(),
        "name": name,
      })
    }

    // === Other node type ===
    ASTNode::Other { .. } => {
      json!({
        "type": "other",
        "id": id,
      })
    }
  };

  let mut obj = make_node_json(obj, comments, semantics, behaviors);
  if render_ctx.flag_non_pure_subjects && node_is_non_pure(node_id, audit_data)
  {
    obj["is_non_pure"] = json!(true);
  }
  obj
}

// ============================================================================
// Documentation AST Rendering
// ============================================================================

/// Flattens inline documentation children into a plain markdown text string,
/// collecting code references along the way.
fn flatten_inline_content(
  children: &[DocumentationNode],
  audit_data: &AuditData,
) -> (String, Vec<serde_json::Value>) {
  let mut text = String::new();
  let mut refs = Vec::new();
  flatten_inline_recursive(children, audit_data, &mut text, &mut refs);
  (text, refs)
}

fn flatten_inline_recursive(
  children: &[DocumentationNode],
  audit_data: &AuditData,
  text: &mut String,
  refs: &mut Vec<serde_json::Value>,
) {
  for child in children {
    let resolved = child.resolve(&audit_data.nodes);
    match resolved {
      DocumentationNode::Text { value, .. } => text.push_str(value),

      DocumentationNode::InlineCode {
        value, children, ..
      } => {
        text.push('`');
        text.push_str(value);
        text.push('`');
        // Inject functional semantics for resolved identifiers within this code span
        for child in children {
          if let DocumentationNode::CodeIdentifier {
            referenced_topic: Some(t),
            ..
          } = child.resolve(&audit_data.nodes)
          {
            let texts = domain::semantic_texts_for_declaration(audit_data, t);
            if !texts.is_empty() {
              text.push_str(" (");
              text.push_str(&texts.join("; "));
              text.push(')');
              break; // one semantic annotation per inline code span
            }
          }
        }
      }

      DocumentationNode::CodeKeyword { value, .. }
      | DocumentationNode::CodeOperator { value, .. }
      | DocumentationNode::CodeText { value, .. } => {
        text.push_str(value);
      }

      DocumentationNode::CodeIdentifier {
        value,
        referenced_topic,
        ..
      } => {
        text.push_str(value);
        if let Some(t) = referenced_topic {
          refs.push(json!({"name": value, "topic": t.id()}));
          // Inject functional semantic inline when outside of InlineCode
          let texts = domain::semantic_texts_for_declaration(audit_data, t);
          if !texts.is_empty() {
            text.push_str(" (");
            text.push_str(&texts.join("; "));
            text.push(')');
          }
        }
      }

      DocumentationNode::Emphasis { children, .. }
      | DocumentationNode::Strong { children, .. }
      | DocumentationNode::Link { children, .. }
      | DocumentationNode::Sentence { children, .. }
      | DocumentationNode::Paragraph { children, .. }
      | DocumentationNode::ListItem { children, .. } => {
        flatten_inline_recursive(children, audit_data, text, refs);
      }

      _ => {}
    }
  }
}

/// Controls selective rendering of documentation sections.
///
/// When provided, only sections on the path from the root ancestor to the
/// target are rendered. Ancestor sections render only their direct content
/// (paragraphs, lists, etc.) and skip sibling subsections not on the path.
/// The target section is rendered fully with all its children.
pub struct DocRenderContext {
  /// Node IDs of sections that are ancestors of the target.
  /// These render only their direct (non-section) content.
  pub ancestor_node_ids: HashSet<i32>,
  /// The node ID of the target section to render fully.
  pub target_node_id: i32,
}

/// Render a DocumentationNode as a structured AST snippet (JSON value).
///
/// Only meaningful structural nodes (Section, Paragraph, List, CodeBlock,
/// BlockQuote) get their own JSON objects with topic IDs. Everything else
/// (Root, Heading, Sentence, ListItem, inline content) is flattened
/// transitively into the parent. Text values use raw markdown formatting.
///
/// When `render_ctx` is provided, sections are rendered selectively: ancestor
/// sections include only direct content, the target section renders fully,
/// and all other sections are skipped.
pub fn render_documentation_ast_snippet(
  node: &DocumentationNode,
  audit_data: &AuditData,
  render_ctx: Option<&DocRenderContext>,
) -> serde_json::Value {
  let resolved = node.resolve(&audit_data.nodes);

  // Unresolved Stub → topic_ref
  if let DocumentationNode::Stub { topic, node_id, .. } = resolved {
    let name = resolve_topic_name(topic, audit_data);
    // Documentation rendering currently only feeds the auditor-facing topic
    // view; untrusted comments are always included.
    let comments = lookup_doc_node_comments(*node_id, audit_data, true);
    return make_node_json(
      json!({"type": "topic_ref", "id": topic.id(), "name": name}),
      comments,
      Vec::new(),
      Vec::new(),
    );
  }

  let node_id = resolved.node_id();
  let id = topic::new_documentation_topic(node_id).id().to_string();
  let comments = lookup_doc_node_comments(node_id, audit_data, true);

  let recurse = |child: &DocumentationNode,
                 ctx: Option<&DocRenderContext>|
   -> serde_json::Value {
    render_documentation_ast_snippet(child, audit_data, ctx)
  };

  let render_children = |children: &[DocumentationNode],
                         ctx: Option<&DocRenderContext>|
   -> Vec<serde_json::Value> {
    children
      .iter()
      .map(|c| recurse(c, ctx))
      .filter(|v| !v.is_null())
      .collect()
  };

  let obj = match resolved {
    // === Transparent nodes: flatten into parent ===

    // Root: return array of rendered children
    DocumentationNode::Root { children, .. } => {
      return json!(render_children(children, render_ctx));
    }

    // Heading: render its section child if present, otherwise flatten text.
    // When selective rendering is active, skip headings whose sections are
    // not on the path to the target.
    DocumentationNode::Heading {
      section, children, ..
    } => {
      if let Some(sec) = section {
        if let Some(ctx) = render_ctx {
          let sec_resolved = sec.resolve(&audit_data.nodes);
          let sec_id = sec_resolved.node_id();
          if sec_id != ctx.target_node_id
            && !ctx.ancestor_node_ids.contains(&sec_id)
          {
            return json!(null);
          }
        }
        return recurse(sec, render_ctx);
      }
      let (text, _) = flatten_inline_content(children, audit_data);
      return json!(text);
    }

    // Sentence: flatten to text
    DocumentationNode::Sentence { children, .. } => {
      let (text, _) = flatten_inline_content(children, audit_data);
      return json!(text);
    }

    // === Structural nodes with topic IDs ===
    DocumentationNode::Section {
      title, children, ..
    } => {
      // Selective rendering: ancestor sections render only direct content,
      // the target section renders fully, others are skipped.
      let children_ctx = if let Some(ctx) = render_ctx {
        if node_id == ctx.target_node_id {
          // Target section: render all children fully (no selective context)
          None
        } else if ctx.ancestor_node_ids.contains(&node_id) {
          // Ancestor section: pass context through so child sections are filtered
          render_ctx
        } else {
          // Not on path: skip entirely
          return json!(null);
        }
      } else {
        None
      };
      json!({
        "type": "section",
        "id": id,
        "title": title,
        "children": render_children(children, children_ctx),
      })
    }

    DocumentationNode::Paragraph { children, .. } => {
      let (text, refs) = flatten_inline_content(children, audit_data);
      let mut obj = json!({
        "type": "paragraph",
        "id": id,
        "text": text,
      });
      if !refs.is_empty() {
        obj["references"] = json!(refs);
      }
      obj
    }

    DocumentationNode::List {
      ordered, children, ..
    } => {
      let mut all_refs = Vec::new();
      let items: Vec<serde_json::Value> = children
        .iter()
        .map(|item| {
          let resolved_item = item.resolve(&audit_data.nodes);
          match resolved_item {
            DocumentationNode::ListItem { children, .. } => {
              let (text, refs) = flatten_inline_content(children, audit_data);
              all_refs.extend(refs);
              json!(text)
            }
            _ => recurse(item, None),
          }
        })
        .collect();
      let mut obj = json!({
        "type": "list",
        "id": id,
        "ordered": ordered,
        "items": items,
      });
      if !all_refs.is_empty() {
        obj["references"] = json!(all_refs);
      }
      obj
    }

    DocumentationNode::CodeBlock {
      lang,
      value,
      children,
      ..
    } => {
      let (_, refs) = flatten_inline_content(children, audit_data);
      let mut obj = json!({
        "type": "code_block",
        "id": id,
        "code": value,
      });
      if let Some(l) = lang {
        obj["lang"] = json!(l);
      }
      if !refs.is_empty() {
        obj["references"] = json!(refs);
      }
      obj
    }

    DocumentationNode::BlockQuote { children, .. } => json!({
      "type": "block_quote",
      "id": id,
      "children": render_children(children, None),
    }),

    // === Inline/leaf nodes at top level (uncommon) ===
    DocumentationNode::Text { value, .. } => return json!(value),
    DocumentationNode::InlineCode {
      value, children, ..
    } => {
      // Check children for resolved identifiers with semantics
      let mut semantic_suffix = String::new();
      for child in children {
        if let DocumentationNode::CodeIdentifier {
          referenced_topic: Some(t),
          ..
        } = child.resolve(&audit_data.nodes)
        {
          let texts = domain::semantic_texts_for_declaration(audit_data, t);
          if !texts.is_empty() {
            semantic_suffix = format!(" ({})", texts.join("; "));
            break;
          }
        }
      }
      return json!(format!("`{}`{}", value, semantic_suffix));
    }
    DocumentationNode::ListItem { children, .. } => {
      let (text, _) = flatten_inline_content(children, audit_data);
      return json!(text);
    }
    DocumentationNode::Emphasis { children, .. }
    | DocumentationNode::Strong { children, .. }
    | DocumentationNode::Link { children, .. } => {
      let (text, _) = flatten_inline_content(children, audit_data);
      return json!(text);
    }
    DocumentationNode::CodeKeyword { value, .. }
    | DocumentationNode::CodeOperator { value, .. }
    | DocumentationNode::CodeText { value, .. } => return json!(value),
    DocumentationNode::CodeIdentifier {
      value,
      referenced_topic,
      ..
    } => {
      if let Some(t) = referenced_topic {
        return json!({"name": value, "topic": t.id()});
      }
      return json!(value);
    }
    DocumentationNode::ThematicBreak { .. }
    | DocumentationNode::Break { .. }
    | DocumentationNode::Definition { .. } => return json!(null),

    DocumentationNode::Delete { children, .. }
    | DocumentationNode::LinkReference { children, .. } => {
      let (text, _) = flatten_inline_content(children, audit_data);
      return json!(text);
    }

    DocumentationNode::Image { alt, .. } => {
      return json!(format!("[image: {}]", alt));
    }
    DocumentationNode::ImageReference { alt, .. } => {
      return json!(format!("[image: {}]", alt));
    }

    DocumentationNode::Table { children, .. } => json!({
      "type": "table",
      "id": id,
      "children": render_children(children, None),
    }),

    DocumentationNode::TableRow { children, .. } => {
      let cells: Vec<serde_json::Value> = render_children(children, None);
      return json!(cells);
    }

    DocumentationNode::TableCell { children, .. } => {
      let (text, _) = flatten_inline_content(children, audit_data);
      return json!(text);
    }

    DocumentationNode::Html { value, .. } => return json!(value),

    DocumentationNode::FootnoteDefinition {
      identifier,
      children,
      ..
    } => {
      let (text, _) = flatten_inline_content(children, audit_data);
      return json!(format!("[^{}]: {}", identifier, text));
    }

    DocumentationNode::FootnoteReference { identifier, .. } => {
      return json!(format!("[^{}]", identifier));
    }

    DocumentationNode::Frontmatter { value, .. } => json!({
      "type": "frontmatter",
      "id": id,
      "text": value,
    }),

    DocumentationNode::Math { value, .. } => json!({
      "type": "math",
      "id": id,
      "text": value,
    }),

    DocumentationNode::InlineMath { value, .. } => {
      return json!(format!("${value}$"));
    }

    DocumentationNode::Stub { .. } => return json!(null),
  };

  make_node_json(obj, comments, Vec::new(), Vec::new())
}

// ============================================================================
// Source Context Conversion
// ============================================================================

/// Convert a list of SourceContext groups to AgentSourceGroup entries.
fn convert_source_groups(
  groups: &[SourceContext],
  target_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<AgentSourceGroup> {
  groups
    .iter()
    .map(|group| convert_source_group(group, target_topic, audit_data))
    .collect()
}

fn convert_source_group(
  group: &SourceContext,
  target_topic: &topic::Topic,
  audit_data: &AuditData,
) -> AgentSourceGroup {
  // These scope titles feed auditor-facing topic views where the developer's
  // own prose is useful signal; untrusted comments are always included.
  let scope = build_scope_title(group.scope(), audit_data, true);

  let scope_references = group
    .scope_references()
    .iter()
    .map(|r| convert_reference(r, target_topic, audit_data))
    .collect();

  let nested_references = group
    .nested_references()
    .iter()
    .map(|nested| {
      let subscope = build_scope_title(nested.subscope(), audit_data, true);
      let children =
        convert_source_children(nested.children(), target_topic, audit_data);
      AgentNestedGroup { subscope, children }
    })
    .collect();

  AgentSourceGroup {
    scope,
    in_scope: group.is_in_scope(),
    scope_references,
    nested_references,
  }
}

/// Recursively convert SourceChild entries to JSON values.
fn convert_source_children(
  children: &[SourceChild],
  target_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<serde_json::Value> {
  children
    .iter()
    .flat_map(|child| match child {
      SourceChild::Reference(reference) => {
        let snippet = convert_reference(reference, target_topic, audit_data);
        // Flatten comment-less semantic blocks
        if snippet.get("kind").and_then(|v| v.as_str()) == Some("semantic")
          && snippet.get("type").and_then(|v| v.as_str()) == Some("block")
          && snippet.get("comments").is_none()
          && let Some(stmts) =
            snippet.get("statements").and_then(|v| v.as_array())
        {
          return stmts.clone();
        }
        vec![snippet]
      }
      SourceChild::AnnotatedBlock(block) => {
        let annotation = block.annotation();
        let kind = annotation_kind_to_string(&annotation.kind).to_string();
        let condition = render_condition_ast_snippet(
          &annotation.topic,
          &annotation.kind,
          target_topic,
          audit_data,
        );
        let children =
          convert_source_children(block.children(), target_topic, audit_data);
        let mut obj = json!({
          "type": "annotated_block",
          "kind": kind,
          "children": children,
        });
        if let Some(cond) = condition {
          obj["condition"] = cond;
        }
        vec![obj]
      }
    })
    .collect()
}

/// Convert a single Reference to a JSON value.
///
/// Renders the referenced Solidity node as a structured AST snippet.
/// Mention comments (info only) are merged into the snippet.
fn convert_reference(
  reference: &Reference,
  target_topic: &topic::Topic,
  audit_data: &AuditData,
) -> serde_json::Value {
  let ref_topic = reference.reference_topic();

  let mut snippet = match audit_data.nodes.get(ref_topic) {
    Some(Node::Solidity(solidity_node)) => {
      let render_ctx = ASTRenderContext {
        target_topic: *target_topic,
        omit_function_and_modifier_bodies: false,
        include_untrusted_comments: true,
        flag_non_pure_subjects: false,
      };
      render_solidity_ast_snippet(solidity_node, &render_ctx, audit_data)
    }
    Some(Node::Documentation(doc_node)) => {
      render_documentation_ast_snippet(doc_node, audit_data, None)
    }
    _ => json!({"type": "unknown_ref", "id": ref_topic.id()}),
  };

  // Merge info mention comments into the snippet
  let mut mention_comments = Vec::new();

  if let Some(mention_topics) = reference.mention_topics() {
    for mention_topic in mention_topics {
      let is_info = matches!(audit_data.topic_metadata.get(mention_topic),
        Some(TopicMetadata::CommentTopic { comment_type, .. })
          if *comment_type == CommentType::Info
      );
      if !is_info {
        continue;
      }
      let content = match audit_data.nodes.get(mention_topic) {
        Some(Node::Comment(nodes)) => {
          comment_parser::render_comment_plain_text(nodes)
        }
        _ => continue,
      };
      let content = content.trim().to_string();
      if content.is_empty() {
        continue;
      }
      mention_comments.push(content);
    }
  }

  if !mention_comments.is_empty() {
    snippet["mention_comments"] = json!(mention_comments);
  }

  snippet
}

// ============================================================================
// Public API: Build Agent Context
// ============================================================================

/// Build context for a documentation section topic, including ancestor
/// section headers and their direct content above the target.
///
/// When the section has ancestors in its scope (e.g., an H3 under H2 under H1),
/// this renders from the outermost ancestor section with selective context so
/// that only the path to the target is included.
fn build_documentation_section_context(
  topic: &topic::Topic,
  scope: &domain::Scope,
  audit_data: &AuditData,
) -> Vec<AgentSourceGroup> {
  let ancestors = scope.ancestor_topics();

  // Find the outermost ancestor that is a documentation section.
  // Ancestors are ordered [component, member, ...containing_blocks].
  let root_ancestor = ancestors.iter().find(|t| {
    matches!(***t, topic::Topic::Documentation(_))
      && matches!(
        audit_data.topic_metadata.get(t),
        Some(TopicMetadata::TitledTopic {
          kind: TitledTopicKind::DocumentationSection,
          ..
        })
      )
  });

  let root_ancestor = match root_ancestor {
    Some(ancestor) => ancestor,
    None => {
      // No ancestor sections — render self directly.
      let node = match audit_data.nodes.get(topic) {
        Some(Node::Documentation(doc_node)) => doc_node,
        _ => return vec![],
      };
      let rendered = render_documentation_ast_snippet(node, audit_data, None);
      let scope_title = build_scope_title(topic, audit_data, true);
      return vec![AgentSourceGroup {
        scope: scope_title,
        in_scope: true,
        scope_references: vec![rendered],
        nested_references: vec![],
      }];
    }
  };

  // Build the set of ancestor node IDs (all doc section ancestors on the path).
  let ancestor_node_ids: HashSet<i32> = ancestors
    .iter()
    .filter_map(|t| {
      if matches!(
        audit_data.topic_metadata.get(t),
        Some(TopicMetadata::TitledTopic {
          kind: TitledTopicKind::DocumentationSection,
          ..
        })
      ) {
        Some(t.numeric_id())
      } else {
        None
      }
    })
    .collect();

  let target_node_id = topic.numeric_id();

  let render_ctx = DocRenderContext {
    ancestor_node_ids,
    target_node_id,
  };

  // Look up the root ancestor's node and render selectively.
  let root_node = match audit_data.nodes.get(root_ancestor) {
    Some(Node::Documentation(doc_node)) => doc_node,
    _ => return vec![],
  };
  let rendered =
    render_documentation_ast_snippet(root_node, audit_data, Some(&render_ctx));

  let scope_title = build_scope_title(root_ancestor, audit_data, true);
  vec![AgentSourceGroup {
    scope: scope_title,
    in_scope: true,
    scope_references: vec![rendered],
    nested_references: vec![],
  }]
}

/// Build the agent context for a given topic.
///
/// Returns a resolved JSON-serializable structure where all topic IDs
/// are replaced with human-readable values. Solidity topics are rendered
/// as structured AST snippets; documentation and comments preserve their
/// HTML representation.
pub fn build_agent_topic_context(
  topic_id: &str,
  audit_data: &AuditData,
  include_expanded_context: bool,
) -> Option<AgentTopicContext> {
  let topic = topic::new_topic(topic_id);
  let metadata = audit_data.topic_metadata.get(&topic)?;

  // Resolve through transitive chain so signature topics find their
  // definition's comments and mentions.
  let resolved_topic =
    domain::resolve_transitive_topic(&topic, &audit_data.topic_metadata);

  let topic_id_string = topic_id.to_string();
  let name = resolve_topic_name(&topic, audit_data);

  let empty_ctx: Vec<crate::domain::SourceContext> = vec![];
  let topic_ctx = audit_data
    .topic_context
    .get(&resolved_topic)
    .unwrap_or(&empty_ctx);
  let context = convert_source_groups(topic_ctx, &topic, audit_data);
  let doc_references: Vec<String> = match audit_data.topic_metadata.get(&topic)
  {
    Some(TopicMetadata::NamedTopic { doc_references, .. }) => {
      doc_references.iter().map(|t| t.id()).collect()
    }
    _ => Vec::new(),
  };
  let mentions: Vec<String> = audit_data
    .mentions_index
    .get(&resolved_topic)
    .map(|topics| topics.iter().map(|t| t.id()).collect())
    .unwrap_or_default();

  match metadata {
    TopicMetadata::NamedTopic { kind, .. } => {
      let (kind_str, sub_kind) = named_kind_to_string(kind);

      let expanded = if include_expanded_context {
        let empty_ctx: Vec<crate::domain::SourceContext> = vec![];
        let expanded_ctx = audit_data
          .expanded_topic_context
          .get(&topic)
          .unwrap_or(&empty_ctx);
        Some(convert_source_groups(expanded_ctx, &topic, audit_data))
      } else {
        None
      };

      Some(AgentTopicContext {
        topic: topic_id_string.clone(),
        name,
        kind: kind_str,
        sub_kind,
        condition: None,
        context,
        expanded_context: expanded,
        doc_references,
        mentions,
      })
    }

    TopicMetadata::UnnamedTopic { kind, .. } => Some(AgentTopicContext {
      topic: topic_id_string,
      name,
      kind: unnamed_kind_to_string(kind),
      sub_kind: None,
      condition: None,
      context,
      expanded_context: None,
      doc_references,
      mentions,
    }),

    TopicMetadata::ControlFlow {
      kind, condition, ..
    } => {
      let condition_snippet = match audit_data.nodes.get(condition) {
        Some(Node::Solidity(node)) => {
          let render_ctx = ASTRenderContext {
            target_topic: topic,
            omit_function_and_modifier_bodies: false,
            include_untrusted_comments: true,
            flag_non_pure_subjects: false,
          };
          Some(render_solidity_ast_snippet(node, &render_ctx, audit_data))
        }
        _ => None,
      };

      Some(AgentTopicContext {
        topic: topic_id_string,
        name,
        kind: control_flow_kind_to_string(kind).to_string(),
        sub_kind: None,
        condition: condition_snippet,
        context,
        expanded_context: None,
        doc_references,
        mentions,
      })
    }

    TopicMetadata::TitledTopic { kind, scope, .. } => {
      let kind_str = match kind {
        TitledTopicKind::DocumentationSection => "DocumentationSection",
      };

      // For documentation sections with ancestors, render from the root
      // ancestor with selective context so that ancestor headers and their
      // direct content are included above the target section.
      let context =
        build_documentation_section_context(&topic, scope, audit_data);

      Some(AgentTopicContext {
        topic: topic_id_string,
        name,
        kind: kind_str.to_string(),
        sub_kind: None,
        condition: None,
        context,
        expanded_context: None,
        doc_references,
        mentions,
      })
    }

    TopicMetadata::CommentTopic { comment_type, .. } => {
      Some(AgentTopicContext {
        topic: topic_id_string,
        name,
        kind: "Comment".to_string(),
        sub_kind: Some(comment_type.as_str().to_string()),
        condition: None,
        context,
        expanded_context: None,
        doc_references,
        mentions,
      })
    }

    TopicMetadata::FeatureTopic { .. }
    | TopicMetadata::RequirementTopic { .. }
    | TopicMetadata::BehaviorTopic { .. }
    | TopicMetadata::FunctionalSemanticTopic { .. }
    | TopicMetadata::FunctionalPurposeTopic { .. }
    | TopicMetadata::PlacementRationaleTopic { .. }
    | TopicMetadata::ThreatTopic { .. }
    | TopicMetadata::InvariantTopic { .. } => {
      let kind = match metadata {
        TopicMetadata::FeatureTopic { .. } => "Feature",
        TopicMetadata::RequirementTopic { .. } => "Requirement",
        TopicMetadata::BehaviorTopic { .. } => "Behavior",
        TopicMetadata::FunctionalSemanticTopic { .. } => "Semantic",
        TopicMetadata::FunctionalPurposeTopic { .. } => "Purpose",
        TopicMetadata::PlacementRationaleTopic { .. } => "Placement",
        TopicMetadata::ThreatTopic { .. } => "Threat",
        TopicMetadata::InvariantTopic { .. } => "Invariant",
        _ => unreachable!(),
      };
      Some(AgentTopicContext {
        topic: topic_id_string,
        name,
        kind: kind.to_string(),
        sub_kind: None,
        condition: None,
        context,
        expanded_context: None,
        doc_references,
        mentions,
      })
    }

    TopicMetadata::DocumentationTopic { is_technical, .. } => {
      Some(AgentTopicContext {
        topic: topic_id_string,
        name,
        kind: if *is_technical {
          "TechnicalDocumentation"
        } else {
          "Documentation"
        }
        .to_string(),
        sub_kind: None,
        condition: None,
        context,
        expanded_context: None,
        doc_references,
        mentions,
      })
    }
  }
}

/// Render a contract's members (signatures only, no bodies) as a JSON object
/// with N-prefixed topic IDs. Used by semantic linking pass 1.
pub fn render_contract_members_for_semantic_linking(
  contract_node: &crate::solidity::ast::ASTNode,
  audit_data: &AuditData,
) -> Option<String> {
  use crate::solidity::ast::ASTNode;

  let (name, kind) = match contract_node {
    ASTNode::ContractDefinition { signature, .. } => {
      let resolved_sig = signature.resolve(&audit_data.nodes);
      match resolved_sig {
        ASTNode::ContractSignature {
          name,
          contract_kind,
          ..
        } => (name.clone(), format!("{:?}", contract_kind).to_lowercase()),
        _ => {
          // Signature is an unresolved stub — try to get name from metadata
          let ct = topic::new_node_topic(&contract_node.node_id());
          let name = audit_data
            .topic_metadata
            .get(&ct)
            .and_then(|m| m.name())
            .unwrap_or("unknown")
            .to_string();
          (name, "contract".to_string())
        }
      }
    }
    _ => return None,
  };

  // Include the developer's own inline comments and NatSpec — the
  // semantic-linking agent needs that prose to recognize groups and topics.
  // Iterate raw contract nodes so ContractMemberGroup wrappers reach
  // `render_solidity_ast_snippet`; groups with comments render as a wrapper
  // (carrying the group-level `[dev]` comment), comment-less groups flatten
  // below.
  let render_ctx = ASTRenderContext {
    target_topic: topic::new_node_topic(&-1),
    omit_function_and_modifier_bodies: true,
    include_untrusted_comments: true,
    flag_non_pure_subjects: false,
  };

  let nodes = match contract_node {
    ASTNode::ContractDefinition { nodes, .. } => nodes.as_slice(),
    _ => return None,
  };
  let member_snippets: Vec<serde_json::Value> = nodes
    .iter()
    .flat_map(|n| {
      let resolved = n.resolve(&audit_data.nodes);
      if let ASTNode::ContractMemberGroup { members, .. } = resolved {
        let comments = lookup_node_comments(
          resolved.node_id(),
          audit_data,
          render_ctx.include_untrusted_comments,
        );
        if comments.is_empty() {
          return members
            .iter()
            .map(|inner| {
              render_solidity_ast_snippet(inner, &render_ctx, audit_data)
            })
            .collect::<Vec<_>>();
        }
      }
      vec![render_solidity_ast_snippet(n, &render_ctx, audit_data)]
    })
    .collect();

  let contract_topic = topic::new_node_topic(&contract_node.node_id());
  let obj = serde_json::json!({
    "contract_topic": contract_topic.id(),
    "name": name,
    "kind": kind,
    "members": member_snippets,
  });

  Some(serde_json::to_string(&obj).unwrap_or_default())
}

// ============================================================================
// Behavior Extraction: Batch rendering (DAG-driven)
// ============================================================================

/// One batch's pre-rendered JSON, ready to send to the LLM. Members are
/// dependency-ordered: every callee that has behaviors already had them
/// extracted in an earlier batch and appears in `called_function_behaviors`.
pub struct BatchForExtraction {
  pub members: Vec<topic::Topic>,
  pub label: String,
  pub json: String,
}

/// Render a batch of in-scope function/modifier topics for the
/// behavior-extraction LLM call. Each member object carries: its
/// rendered AST (signature + body), a flat `semantics` map for its
/// scoped declarations, and a `called_function_behaviors` map of every
/// callee's prior-extracted behaviors. See
/// `crates/o11a-analyze/docs/build-plans/pipeline-dag.md` step 4.
pub fn render_batch_for_behavior_extraction(
  members: &[topic::Topic],
  audit_data: &AuditData,
) -> Option<BatchForExtraction> {
  let render_ctx = ASTRenderContext {
    target_topic: topic::new_node_topic(&-1),
    omit_function_and_modifier_bodies: false,
    include_untrusted_comments: false,
    flag_non_pure_subjects: false,
  };

  let mut member_objs: Vec<serde_json::Value> = Vec::new();
  for member in members {
    let Some(obj) =
      render_member_for_batch(member, &render_ctx, audit_data, false)
    else {
      continue;
    };
    member_objs.push(obj);
  }

  if member_objs.is_empty() {
    return None;
  }

  let label = batch_label(members, audit_data);
  let obj = json!({ "batch": member_objs });
  Some(BatchForExtraction {
    members: members.to_vec(),
    label,
    json: serde_json::to_string(&obj).unwrap_or_default(),
  })
}

/// Render one batch member as a JSON object with its definition,
/// scoped semantics, and called-function behaviors. Shared between the
/// behavior-extraction batch render (step 3) and the functional-
/// property batch render (step 5); the only difference is whether the
/// renderer flags non-pure subjects (`flag_non_pure_subjects` on
/// `render_ctx`). Returns `None` if the member's AST node cannot be
/// resolved or the topic is not a function/modifier.
fn render_member_for_batch(
  member: &topic::Topic,
  render_ctx: &ASTRenderContext,
  audit_data: &AuditData,
  include_behaviors: bool,
) -> Option<serde_json::Value> {
  let Some(crate::domain::Node::Solidity(node)) = audit_data.nodes.get(member)
  else {
    return None;
  };
  let kind = match node {
    ASTNode::FunctionDefinition { .. } => "function",
    ASTNode::ModifierDefinition { .. } => "modifier",
    _ => return None,
  };
  let name =
    crate::collaborator::agent::function_dag::callable_name(member, audit_data);

  let definition = render_solidity_ast_snippet(node, render_ctx, audit_data);
  let semantics = collect_member_semantics(member, audit_data);
  let called_behaviors = collect_called_function_behaviors(member, audit_data);

  let mut obj = json!({
    "topic": member.id(),
    "name": name,
    "kind": kind,
    "definition": definition,
    "semantics": semantics,
    "called_function_behaviors": called_behaviors,
  });
  if include_behaviors {
    let prior = crate::collaborator::agent::function_dag::behaviors_of(
      member, audit_data,
    );
    obj["behaviors"] = json!(prior);
  }
  Some(obj)
}

/// Collect a flat map of declaration topic → {name, semantic} for every
/// declaration scoped inside `member` (parameters, returns, body locals)
/// plus every state variable mutated by the member. Declarations
/// without a functional semantic are still listed (with `semantic: null`)
/// so the LLM has a complete inventory of in-scope identifiers.
fn collect_member_semantics(
  member: &topic::Topic,
  audit_data: &AuditData,
) -> serde_json::Value {
  use crate::domain::Scope;
  let mut entries = serde_json::Map::new();

  for (decl_topic, metadata) in &audit_data.topic_metadata {
    let TopicMetadata::NamedTopic { name, scope, .. } = metadata else {
      continue;
    };
    let in_member = match scope {
      Scope::Member { member: m, .. }
      | Scope::ContainingBlock { member: m, .. } => m == member,
      _ => false,
    };
    if !in_member {
      continue;
    }
    let semantic = first_semantic(decl_topic, audit_data);
    entries.insert(
      decl_topic.id(),
      json!({
        "name": name,
        "semantic": semantic,
      }),
    );
  }

  // State variable mutations: pull the names + semantics for any state
  // variable this member writes. Reads-only state vars surface inline
  // through the renderer's per-node semantics.
  if let Some(props) = audit_data.function_properties.get(member) {
    let mutations = match props {
      crate::domain::FunctionModProperties::FunctionProperties {
        mutations,
        ..
      }
      | crate::domain::FunctionModProperties::ModifierProperties {
        mutations,
        ..
      } => mutations,
    };
    for state_var in mutations {
      if entries.contains_key(&state_var.id()) {
        continue;
      }
      let Some(TopicMetadata::NamedTopic { name, .. }) =
        audit_data.topic_metadata.get(state_var)
      else {
        continue;
      };
      let semantic = first_semantic(state_var, audit_data);
      entries.insert(
        state_var.id(),
        json!({
          "name": name,
          "semantic": semantic,
        }),
      );
    }
  }

  serde_json::Value::Object(entries)
}

/// Collect a flat map of callee topic → {name, behaviors} for every
/// in-scope or out-of-scope function this member calls. Out-of-scope
/// callees appear with an empty `behaviors` array, signalling "no
/// behaviors available" to the LLM rather than leaving the callee
/// implicit (see pipeline-dag pivotal decision #7).
fn collect_called_function_behaviors(
  member: &topic::Topic,
  audit_data: &AuditData,
) -> serde_json::Value {
  let callees =
    crate::collaborator::agent::function_dag::callees_of(member, audit_data);
  let mut entries = serde_json::Map::new();
  for callee in callees {
    let name = crate::collaborator::agent::function_dag::callable_name(
      &callee, audit_data,
    );
    let behaviors = crate::collaborator::agent::function_dag::behaviors_of(
      &callee, audit_data,
    );
    entries.insert(
      callee.id(),
      json!({
        "name": name,
        "behaviors": behaviors,
      }),
    );
  }
  serde_json::Value::Object(entries)
}

/// Best-effort lookup of a single semantic description for a declaration.
/// Returns `None` if no semantic exists; if multiple are present
/// (condensation should have collapsed to one but the data shape allows
/// many), returns the first by topic ID and warns so the divergence is
/// surfaced rather than silently swallowed.
fn first_semantic(
  decl_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Option<String> {
  let sem_topics = audit_data.declaration_semantics.get(decl_topic)?;
  if sem_topics.len() > 1 {
    tracing::warn!(
      declaration = %decl_topic.id(),
      count = sem_topics.len(),
      "declaration has multiple functional semantics; using the first \u{2014} \
       condensation may have failed"
    );
  }
  for sem_topic in sem_topics {
    if let Some(TopicMetadata::FunctionalSemanticTopic {
      description, ..
    }) = audit_data.topic_metadata.get(sem_topic)
    {
      return Some(description.clone());
    }
  }
  None
}

/// A short batch label suitable for log lines and LLM-call labels.
/// Uses the first member's qualified name plus the member count.
fn batch_label(members: &[topic::Topic], audit_data: &AuditData) -> String {
  let first = members
    .first()
    .map(|m| {
      audit_data
        .topic_metadata
        .get(m)
        .and_then(|md| md.name())
        .unwrap_or("unknown")
        .to_string()
    })
    .unwrap_or_else(|| "empty".to_string());
  if members.len() == 1 {
    first
  } else {
    format!("{}+{}", first, members.len() - 1)
  }
}

/// Render a batch for the functional-purpose / placement-rationale LLM
/// call (pipeline step 5). Same per-member structure as the behavior
/// batch (definition + semantics + called_function_behaviors), plus:
///
/// - `non_pure_subjects` at the top level: the canonical list of
///   non-pure subject topics the LLM must produce output for.
/// - `behaviors` per member: the function's already-extracted behaviors.
/// - `feature` per member: the linked feature's description and
///   requirements (or `null` when no feature link exists; callers should
///   filter out featureless members before rendering).
/// - `is_non_pure: true` injected on each non-pure node in the rendered
///   AST (via `flag_non_pure_subjects: true`).
///
/// Returns `None` if no member has any non-pure subjects (no work to do
/// for this batch).
pub fn render_batch_for_functional_properties(
  members: &[topic::Topic],
  audit_data: &AuditData,
) -> Option<BatchForExtraction> {
  let render_ctx = ASTRenderContext {
    target_topic: topic::new_node_topic(&-1),
    omit_function_and_modifier_bodies: false,
    include_untrusted_comments: false,
    flag_non_pure_subjects: true,
  };

  let mut member_objs: Vec<serde_json::Value> = Vec::new();
  let mut all_non_pure_subjects: Vec<String> = Vec::new();

  for member in members {
    let Some(mut obj) =
      render_member_for_batch(member, &render_ctx, audit_data, true)
    else {
      continue;
    };
    let feature = lookup_member_feature(member, audit_data);
    if feature.is_null() {
      // No feature link — skip this member entirely. The caller has
      // already been warned about the reconciliation gap.
      continue;
    }
    obj["feature"] = feature;

    let mut subjects = collect_non_pure_subjects_in_member(member, audit_data);
    if subjects.is_empty() {
      // Pure-only function: nothing to ask the LLM about.
      continue;
    }
    all_non_pure_subjects.append(&mut subjects);

    member_objs.push(obj);
  }

  if member_objs.is_empty() || all_non_pure_subjects.is_empty() {
    return None;
  }

  let label = batch_label(members, audit_data);
  let obj = json!({
    "non_pure_subjects": all_non_pure_subjects,
    "batch": member_objs,
  });
  Some(BatchForExtraction {
    members: members.to_vec(),
    label,
    json: serde_json::to_string(&obj).unwrap_or_default(),
  })
}

/// Look up the feature linked to a member via reconciliation behaviors.
/// Returns a JSON object with name/description/requirements, or
/// `Value::Null` when no feature link exists (the caller treats this as
/// a skip signal for the member). Warns when more than one feature
/// matches so the auditor sees the ambiguity and reconciliation can be
/// reviewed.
fn lookup_member_feature(
  member: &topic::Topic,
  audit_data: &AuditData,
) -> serde_json::Value {
  let Some(beh_topics) = audit_data.member_behaviors.get(member) else {
    return serde_json::Value::Null;
  };
  let matches: Vec<&topic::Topic> = audit_data
    .feature_behavior_links
    .iter()
    .filter_map(|(feat_topic, feat_behs)| {
      if beh_topics.iter().any(|bt| feat_behs.contains(bt)) {
        Some(feat_topic)
      } else {
        None
      }
    })
    .collect();
  if matches.len() > 1 {
    let ids: Vec<String> = matches.iter().map(|t| t.id()).collect();
    tracing::warn!(
      member = %member.id(),
      "member is linked to {} features ({:?}); using the first \u{2014} \
       reconciliation may need review",
      matches.len(),
      ids
    );
  }
  for feat_topic in matches {
    let Some(TopicMetadata::FeatureTopic {
      name, description, ..
    }) = audit_data.topic_metadata.get(feat_topic)
    else {
      continue;
    };
    let requirements = audit_data
      .feature_requirement_links
      .get(feat_topic)
      .map(|reqs| {
        reqs
          .iter()
          .filter_map(|r| {
            if let Some(TopicMetadata::RequirementTopic {
              description, ..
            }) = audit_data.topic_metadata.get(r)
            {
              Some(description.clone())
            } else {
              None
            }
          })
          .collect::<Vec<_>>()
      })
      .unwrap_or_default();
    return json!({
      "topic": feat_topic.id(),
      "name": name,
      "description": description,
      "requirements": requirements,
    });
  }
  serde_json::Value::Null
}

/// Walk a member's body and collect the topic IDs of every non-pure
/// subject within it. Order is source-order via the AST, matching how
/// the renderer emits them. Used to populate the top-level
/// `non_pure_subjects` list in the functional-property batch JSON.
fn collect_non_pure_subjects_in_member(
  member: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<String> {
  let Some(crate::domain::Node::Solidity(node)) = audit_data.nodes.get(member)
  else {
    return Vec::new();
  };
  let mut out: Vec<String> = Vec::new();
  let mut seen: HashSet<topic::Topic> = HashSet::new();
  walk_for_non_pure(node, audit_data, &mut out, &mut seen);
  out
}

fn walk_for_non_pure(
  node: &ASTNode,
  audit_data: &AuditData,
  out: &mut Vec<String>,
  seen: &mut HashSet<topic::Topic>,
) {
  let resolved = node.resolve(&audit_data.nodes);
  let node_topic = topic::new_node_topic(&resolved.node_id());
  if let Some(TopicMetadata::UnnamedTopic { kind, .. }) =
    audit_data.topic_metadata.get(&node_topic)
    && matches!(kind.purity(), crate::domain::SubjectPurity::NonPure)
    && seen.insert(node_topic)
  {
    out.push(node_topic.id());
  }
  for child in resolved.nodes() {
    walk_for_non_pure(child, audit_data, out, seen);
  }
}

// ============================================================================
// Semantic Linking: Synthesis Step Context Rendering (steps 2, 4, 5)
// ============================================================================

/// Step 2 — render the list of contract entities needing semantics. One JSON
/// array entry per contract: the topic id, the contract name, and the kind
/// string `"contract"`. Pairs with `render_contract_summaries_for_semantics`
/// for the source-code disambiguation block.
pub fn render_contract_entities_for_semantics(
  contract_topics: &[topic::Topic],
  audit_data: &AuditData,
) -> String {
  let mut declarations: Vec<serde_json::Value> = Vec::new();
  for ct in contract_topics {
    let Some(TopicMetadata::NamedTopic {
      name,
      kind: NamedTopicKind::Contract(_),
      ..
    }) = audit_data.topic_metadata.get(ct)
    else {
      continue;
    };
    declarations.push(json!({
      "topic": ct.id(),
      "name": name,
      "kind": "contract",
    }));
  }
  serde_json::to_string(&declarations).unwrap_or_else(|_| "[]".to_string())
}

/// Step 2 — render a textual summary of each contract: name, contract-level
/// NatSpec, and the names of its public-facing members (external/public
/// functions and modifiers, public state variables, all events/errors, and
/// all struct/enum definitions). This is the "source code (for disambiguation
/// only)" payload the step 2 LLM call sees.
pub fn render_contract_summaries_for_semantics(
  contract_topics: &[topic::Topic],
  audit_data: &AuditData,
) -> String {
  let mut parts: Vec<String> = Vec::new();
  for ct in contract_topics {
    let Some(TopicMetadata::NamedTopic {
      name: contract_name,
      kind: NamedTopicKind::Contract(_),
      ..
    }) = audit_data.topic_metadata.get(ct)
    else {
      continue;
    };

    let mut block = String::new();
    block.push_str(&format!("Contract {}:", contract_name));
    block.push('\n');

    let natspec = collect_natspec_text(ct, audit_data);
    if !natspec.trim().is_empty() {
      block.push_str("  NatSpec: ");
      block.push_str(natspec.trim());
      block.push('\n');
    }

    let mut public_members: Vec<String> = Vec::new();
    for meta in audit_data.topic_metadata.values() {
      let TopicMetadata::NamedTopic {
        name,
        kind,
        visibility,
        scope,
        ..
      } = meta
      else {
        continue;
      };
      let component = match scope {
        domain::Scope::Component { component, .. } => component,
        _ => continue,
      };
      if component != ct {
        continue;
      }
      if !is_public_member_kind(kind, visibility) {
        continue;
      }
      public_members.push(name.clone());
    }
    if !public_members.is_empty() {
      block.push_str("  Public members: ");
      block.push_str(&public_members.join(", "));
      block.push('\n');
    }

    parts.push(block);
  }
  parts.join("\n")
}

/// Step 4 (member-scoped batch) — render declarations whose semantics this
/// batch will produce: each member topic itself plus its parameters and
/// return values. Body locals are excluded — they belong to step 5.
pub fn render_member_signature_declarations_for_semantics(
  member_topics: &[topic::Topic],
  audit_data: &AuditData,
) -> String {
  let mut declarations: Vec<serde_json::Value> = Vec::new();

  for member_topic in member_topics {
    if let Some(metadata) = audit_data.topic_metadata.get(member_topic)
      && let Some(name) = metadata.name()
    {
      declarations.push(json!({
        "topic": member_topic.id(),
        "name": name,
        "kind": "member",
      }));
    }

    for (decl_topic, metadata) in &audit_data.topic_metadata {
      let in_signature = matches!(
        metadata.scope(),
        domain::Scope::Member { member, .. } if member == member_topic
      );
      if !in_signature {
        continue;
      }
      let TopicMetadata::NamedTopic { name, kind, .. } = metadata else {
        continue;
      };
      declarations.push(json!({
        "topic": decl_topic.id(),
        "name": name,
        "kind": format!("{:?}", kind),
      }));
    }
  }

  serde_json::to_string(&declarations).unwrap_or_else(|_| "[]".to_string())
}

/// Step 4 (contract-scoped batch) — render non-function component-scoped
/// declarations for the listed contracts. Includes state variables, events,
/// errors, struct/enum definitions, struct fields, and enum members.
/// Functions and modifiers are *excluded* — they belong to the
/// member-scoped batch in step 4 (alongside their params/returns).
pub fn render_contract_level_declarations_for_semantics(
  contract_topics: &[topic::Topic],
  audit_data: &AuditData,
) -> String {
  let mut declarations: Vec<serde_json::Value> = Vec::new();

  for ct in contract_topics {
    for (decl_topic, metadata) in &audit_data.topic_metadata {
      let TopicMetadata::NamedTopic { name, kind, .. } = metadata else {
        continue;
      };

      if matches!(
        kind,
        NamedTopicKind::Function(_)
          | NamedTopicKind::Modifier
          | NamedTopicKind::Contract(_)
      ) {
        continue;
      }

      if !component_belongs_to_contract(metadata.scope(), ct, audit_data) {
        continue;
      }

      declarations.push(json!({
        "topic": decl_topic.id(),
        "name": name,
        "kind": format!("{:?}", kind),
      }));
    }
  }

  serde_json::to_string(&declarations).unwrap_or_else(|_| "[]".to_string())
}

/// Step 5 — render the body-local declarations for each member. These are
/// the items in `Scope::ContainingBlock` (locals declared inside the
/// function/modifier body). Member signatures and parameters are *not*
/// included — those are handled by step 4.
pub fn render_member_body_local_declarations_for_semantics(
  member_topics: &[topic::Topic],
  audit_data: &AuditData,
) -> String {
  let mut declarations: Vec<serde_json::Value> = Vec::new();

  for member_topic in member_topics {
    for (decl_topic, metadata) in &audit_data.topic_metadata {
      let in_body = matches!(
        metadata.scope(),
        domain::Scope::ContainingBlock { member, .. } if member == member_topic
      );
      if !in_body {
        continue;
      }
      let TopicMetadata::NamedTopic { name, kind, .. } = metadata else {
        continue;
      };
      declarations.push(json!({
        "topic": decl_topic.id(),
        "name": name,
        "kind": format!("{:?}", kind),
      }));
    }
  }

  serde_json::to_string(&declarations).unwrap_or_else(|_| "[]".to_string())
}

fn is_public_member_kind(
  kind: &NamedTopicKind,
  visibility: &NamedTopicVisibility,
) -> bool {
  use NamedTopicKind as K;
  use NamedTopicVisibility as V;
  match kind {
    K::Function(_) | K::Modifier => {
      matches!(visibility, V::Public | V::External)
    }
    K::StateVariable(_) => matches!(visibility, V::Public),
    K::Event | K::Error | K::Struct | K::Enum => true,
    _ => false,
  }
}

/// True when a declaration's `Component` scope rolls up to `contract_topic`,
/// either directly or through one parent hop (the struct-field / enum-member
/// case). Mirrors `bm25::corpus::belongs_to_contract` but only for
/// `Scope::Component` — the other scope kinds are handled by their own
/// renderers (step 4 member-scoped, step 5 body-locals).
fn component_belongs_to_contract(
  scope: &domain::Scope,
  contract_topic: &topic::Topic,
  audit_data: &AuditData,
) -> bool {
  let domain::Scope::Component { component, .. } = scope else {
    return false;
  };
  if component == contract_topic {
    return true;
  }
  audit_data
    .topic_metadata
    .get(component)
    .map(TopicMetadata::scope)
    .map(|parent| match parent {
      domain::Scope::Member { component: c, .. }
      | domain::Scope::ContainingBlock { component: c, .. }
      | domain::Scope::Component { component: c, .. } => c == contract_topic,
      _ => false,
    })
    .unwrap_or(false)
}

/// Concatenate the surface text of all NatSpec / dev comments attached to
/// `topic`, separated by single spaces. Used by
/// `render_contract_summaries_for_semantics` to assemble the per-contract
/// NatSpec line for the step 2 prompt.
fn collect_natspec_text(
  topic: &topic::Topic,
  audit_data: &AuditData,
) -> String {
  use crate::collaborator::parser::CommentNode;

  fn append(node: &CommentNode, out: &mut String) {
    match node {
      CommentNode::Text { value }
      | CommentNode::CodeText { value }
      | CommentNode::CodeKeyword { value }
      | CommentNode::CodeOperator { value }
      | CommentNode::CodeIdentifier { value, .. } => {
        out.push(' ');
        out.push_str(value);
      }
      CommentNode::InlineCode { children, .. } => {
        for c in children {
          append(c, out);
        }
      }
      CommentNode::Emphasis { text }
      | CommentNode::Strong { text }
      | CommentNode::Link { text, .. } => {
        out.push(' ');
        out.push_str(text);
      }
    }
  }

  let mut out = String::new();
  if let Some(comment_topics) = audit_data.comment_index.get(topic) {
    for ct in comment_topics {
      if let Some(domain::Node::Comment(nodes)) = audit_data.nodes.get(ct) {
        for node in nodes {
          append(node, &mut out);
        }
      }
    }
  }
  out
}

/// Render a single member (function, modifier, event, error, state
/// variable, struct, enum) as a JSON snippet, using `render_ctx` to
/// control body inclusion and dev-comment visibility.
///
/// Returns `None` when no AST member with `member_topic`'s `node_id`
/// exists. Used by every agent task that needs single-member source
/// context — semantic-linking steps 4a and 5, the BM25 per-member corpus,
/// etc.
///
/// **Caller note for signature-only rendering:** to actually strip
/// function/modifier bodies, set `render_ctx.omit_function_and_modifier_bodies
/// = true` AND `render_ctx.target_topic = topic::new_node_topic(&-1)`
/// (the sentinel). Setting `target_topic = *member_topic` re-expands
/// the body via the per-member override — see `ASTRenderContext::target_topic`.
pub fn render_member_for_agent(
  member_topic: &topic::Topic,
  render_ctx: &ASTRenderContext,
  audit_data: &AuditData,
) -> Option<String> {
  for ast in audit_data.asts.values() {
    let sol_ast = match ast {
      domain::AST::Solidity(sol_ast) => sol_ast,
      _ => continue,
    };
    for contract_node in &sol_ast.nodes {
      let resolved_contract = contract_node.resolve(&audit_data.nodes);
      if let ASTNode::ContractDefinition { .. } = resolved_contract {
        for member_node in contract_members(resolved_contract) {
          let resolved_member = member_node.resolve(&audit_data.nodes);
          let node_topic = topic::new_node_topic(&resolved_member.node_id());
          if node_topic == *member_topic {
            let rendered = render_solidity_ast_snippet(
              resolved_member,
              render_ctx,
              audit_data,
            );
            return Some(serde_json::to_string(&rendered).unwrap_or_default());
          }
        }
      }
    }
  }
  None
}

/// Render a contract's non-function component-scoped members (state
/// variables, events, errors, struct/enum definitions) as a JSON array
/// snippet. Functions and modifiers are filtered out — those are rendered
/// separately by `render_member_for_agent` per call.
///
/// Used by semantic-linking step 4b (the contract-scoped batch). Honors
/// `render_ctx.include_untrusted_comments` so callers can opt in or out
/// of NatSpec / inline-comment leakage; `omit_function_and_modifier_bodies`
/// is irrelevant here since functions are filtered out anyway.
pub fn render_contract_non_function_members_for_agent(
  contract_topic: &topic::Topic,
  render_ctx: &ASTRenderContext,
  audit_data: &AuditData,
) -> String {
  for ast in audit_data.asts.values() {
    let sol_ast = match ast {
      domain::AST::Solidity(sol_ast) => sol_ast,
      _ => continue,
    };
    for contract_node in &sol_ast.nodes {
      let resolved = contract_node.resolve(&audit_data.nodes);
      let node_topic = topic::new_node_topic(&resolved.node_id());
      if node_topic != *contract_topic {
        continue;
      }
      if let ASTNode::ContractDefinition { .. } = resolved {
        let snippets: Vec<serde_json::Value> = contract_members(resolved)
          .iter()
          .filter(|n| {
            let resolved_n = n.resolve(&audit_data.nodes);
            !matches!(
              resolved_n,
              ASTNode::FunctionDefinition { .. }
                | ASTNode::ModifierDefinition { .. }
            )
          })
          .map(|n| render_solidity_ast_snippet(n, render_ctx, audit_data))
          .collect();
        return serde_json::to_string(&snippets)
          .unwrap_or_else(|_| "[]".to_string());
      }
    }
  }
  "[]".to_string()
}

pub fn mechanical_section_to_members(
  section_declarations: &[topic::Topic],
  contract_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<topic::Topic> {
  let mut members: Vec<topic::Topic> = Vec::new();

  for decl_topic in section_declarations {
    if let Some(metadata) = audit_data.topic_metadata.get(decl_topic) {
      match metadata.scope() {
        // Declaration is inside a member — add the member
        domain::Scope::Member {
          member, component, ..
        }
        | domain::Scope::ContainingBlock {
          member, component, ..
        } if component == contract_topic => {
          if !members.contains(member) {
            members.push(*member);
          }
        }
        // Declaration is at component level (state variable) — find members that use it
        domain::Scope::Component { component, .. }
          if component == contract_topic =>
        {
          // Check function properties for mutations and calls referencing this variable
          for (fn_topic, props) in &audit_data.function_properties {
            let (mutations, _calls) = match props {
              domain::FunctionModProperties::FunctionProperties {
                mutations,
                calls,
                ..
              } => (mutations, calls),
              domain::FunctionModProperties::ModifierProperties {
                mutations,
                calls,
                ..
              } => (mutations, calls),
            };
            if mutations.contains(decl_topic) && !members.contains(fn_topic) {
              members.push(*fn_topic);
            }
          }
        }
        _ => {}
      }
    }
  }

  members
}

// ============================================================================
// Semantic Linking: Mechanical Layer
// ============================================================================

/// Result of mechanical semantic linking: confirmed section→contract associations
/// derived from inline code references in documentation.
pub struct MechanicalLinkResult {
  /// Maps D-prefixed section topics to the N-prefixed contract topics they reference
  pub section_to_contracts:
    std::collections::HashMap<topic::Topic, Vec<topic::Topic>>,
  /// Maps D-prefixed section topics to the specific N-prefixed declaration topics
  /// that were resolved from inline code references
  pub section_to_declarations:
    std::collections::HashMap<topic::Topic, Vec<topic::Topic>>,
}

/// Walk the documentation ASTs and resolve inline code references to find
/// confirmed section→contract associations. This is the mechanical layer
/// of semantic linking — perfect confidence because the documentation
/// literally names the declaration.
pub fn mechanical_semantic_links(
  audit_data: &AuditData,
) -> MechanicalLinkResult {
  let mut section_to_contracts: std::collections::HashMap<
    topic::Topic,
    Vec<topic::Topic>,
  > = std::collections::HashMap::new();
  let mut section_to_declarations: std::collections::HashMap<
    topic::Topic,
    Vec<topic::Topic>,
  > = std::collections::HashMap::new();

  for ast in audit_data.asts.values() {
    let doc_ast = match ast {
      domain::AST::Documentation(doc_ast) => doc_ast,
      _ => continue,
    };

    for node in &doc_ast.nodes {
      collect_mechanical_links_recursive(
        node,
        None, // no parent section yet
        audit_data,
        &mut section_to_contracts,
        &mut section_to_declarations,
      );
    }
  }

  MechanicalLinkResult {
    section_to_contracts,
    section_to_declarations,
  }
}

/// Find the contract topic that contains `ref_topic`. If the reference
/// is itself a contract, return it directly; otherwise walk one step
/// up its scope. Returns `None` for global / file-scoped declarations
/// (no containing contract). Shared between the resolved-reference
/// branch and the Phase E (anchor-by-name) fallback branch in
/// `collect_mechanical_links_recursive`.
fn containing_contract_topic(
  audit_data: &AuditData,
  ref_topic: topic::Topic,
) -> Option<topic::Topic> {
  let metadata = audit_data.topic_metadata.get(&ref_topic)?;
  match metadata {
    TopicMetadata::NamedTopic {
      kind: domain::NamedTopicKind::Contract(_),
      ..
    } => Some(ref_topic),
    _ => match metadata.scope() {
      domain::Scope::Component { component, .. } => Some(*component),
      domain::Scope::Member { component, .. } => Some(*component),
      domain::Scope::ContainingBlock { component, .. } => Some(*component),
      _ => None,
    },
  }
}

/// Recursively walk documentation nodes, tracking the top-level section.
/// When a CodeIdentifier with a resolved reference is found, walk up
/// the reference's scope to find the containing contract and record
/// the section→contract and section→declaration associations.
///
/// Only the first (top-level) section sets `current_section`; nested
/// child sections inherit the parent so that all mechanical links roll
/// up to the top-level section that the pipeline actually processes.
fn collect_mechanical_links_recursive(
  node: &crate::documentation::ast::DocumentationNode,
  current_section: Option<&topic::Topic>,
  audit_data: &AuditData,
  section_to_contracts: &mut std::collections::HashMap<
    topic::Topic,
    Vec<topic::Topic>,
  >,
  section_to_declarations: &mut std::collections::HashMap<
    topic::Topic,
    Vec<topic::Topic>,
  >,
) {
  // Resolve Stubs through audit_data.nodes — the AST contains stubs after analysis
  let node = node.resolve(&audit_data.nodes);
  match node {
    DocumentationNode::Section {
      node_id, children, ..
    } => {
      // Only set current_section for the top-level section; nested
      // sections keep the parent so links roll up to the root.
      let section_topic = topic::new_documentation_topic(*node_id);
      let effective_section = current_section.unwrap_or(&section_topic);
      for child in children {
        collect_mechanical_links_recursive(
          child,
          Some(effective_section),
          audit_data,
          section_to_contracts,
          section_to_declarations,
        );
      }
    }

    DocumentationNode::Heading {
      section, children, ..
    } => {
      // Process heading text with current section context
      for child in children {
        collect_mechanical_links_recursive(
          child,
          current_section,
          audit_data,
          section_to_contracts,
          section_to_declarations,
        );
      }
      // Process section content
      if let Some(sec) = section {
        collect_mechanical_links_recursive(
          sec,
          current_section,
          audit_data,
          section_to_contracts,
          section_to_declarations,
        );
      }
    }

    DocumentationNode::CodeIdentifier {
      referenced_topic: Some(ref_topic),
      ..
    } => {
      if let Some(section_topic) = current_section {
        // Record section → declaration
        let decls = section_to_declarations.entry(*section_topic).or_default();
        if !decls.contains(ref_topic) {
          decls.push(*ref_topic);
        }

        // Walk up the declaration's scope to find the containing contract.
        if let Some(ct) = containing_contract_topic(audit_data, *ref_topic) {
          let contracts =
            section_to_contracts.entry(*section_topic).or_default();
          if !contracts.contains(&ct) {
            contracts.push(ct);
          }
        }
      }
    }

    // Phase E (anchor-by-name) fallback: the resolver could not pin a
    // single declaration but recorded the full candidate list on the
    // node. Union each candidate's containing contract into the
    // section's anchor set. Per spec, no member is added to
    // `section_to_declarations` — only contracts to
    // `section_to_contracts`.
    DocumentationNode::CodeIdentifier {
      referenced_topic: None,
      referenced_topic_candidates,
      ..
    } if !referenced_topic_candidates.is_empty() => {
      if let Some(section_topic) = current_section {
        // Only touch the entry once we know we have a contract to add
        // — otherwise sections whose only Phase E candidates live at
        // global scope would gain a phantom empty Vec.
        for candidate in referenced_topic_candidates {
          if let Some(ct) = containing_contract_topic(audit_data, *candidate) {
            let contracts =
              section_to_contracts.entry(*section_topic).or_default();
            if !contracts.contains(&ct) {
              contracts.push(ct);
            }
          }
        }
      }
    }

    // Recurse into other node types
    DocumentationNode::Root { children, .. }
    | DocumentationNode::Paragraph { children, .. }
    | DocumentationNode::Sentence { children, .. }
    | DocumentationNode::InlineCode { children, .. }
    | DocumentationNode::List { children, .. }
    | DocumentationNode::ListItem { children, .. }
    | DocumentationNode::BlockQuote { children, .. }
    | DocumentationNode::Emphasis { children, .. }
    | DocumentationNode::Strong { children, .. }
    | DocumentationNode::CodeBlock { children, .. } => {
      for child in children {
        collect_mechanical_links_recursive(
          child,
          current_section,
          audit_data,
          section_to_contracts,
          section_to_declarations,
        );
      }
    }

    // Leaf nodes and nodes without relevant children.
    _ => {}
  }
}

/// Render a list of in-scope contracts with their names and topic IDs
/// for LLM pass 1 of semantic linking. Only contracts from files listed
/// in scope.txt are included — dependencies are excluded.
pub fn render_contract_list_for_semantic_linking(
  audit_data: &AuditData,
) -> Vec<(topic::Topic, String)> {
  use crate::solidity::ast::ASTNode;

  let mut contracts = Vec::new();
  for (path, ast) in &audit_data.asts {
    // Only include contracts from in-scope files
    if !audit_data.in_scope_files.contains(path) {
      continue;
    }
    let sol_ast = match ast {
      domain::AST::Solidity(sol_ast) => sol_ast,
      _ => continue,
    };
    for node in &sol_ast.nodes {
      let resolved = node.resolve(&audit_data.nodes);
      if let ASTNode::ContractDefinition { .. } = resolved {
        let contract_topic = topic::new_node_topic(&resolved.node_id());
        if let Some(json) =
          render_contract_members_for_semantic_linking(resolved, audit_data)
        {
          contracts.push((contract_topic, json));
        }
      }
    }
  }

  contracts
}

/// Render a documentation section's text content as a plain string for LLM context.
pub fn render_section_text(
  section_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Option<String> {
  let node_id = section_topic.numeric_id();
  let doc_topic = topic::new_documentation_topic(node_id);

  // Find the section's title from metadata
  let title = match audit_data.topic_metadata.get(&doc_topic) {
    Some(TopicMetadata::TitledTopic { title, .. }) => Some(title.as_str()),
    _ => {
      tracing::warn!(
        "render_section_text: no TitledTopic metadata for {} (node_id={})",
        section_topic.id(),
        node_id
      );
      None
    }
  };

  // Render the section content from the documentation AST
  let doc_node = find_doc_node_by_id(audit_data, node_id);
  if doc_node.is_none() {
    tracing::warn!(
      "render_section_text: find_doc_node_by_id returned None for node_id={}",
      node_id
    );
    return None;
  }

  let rendered = render_documentation_ast_snippet(doc_node?, audit_data, None);

  let json_text = serde_json::to_string(&rendered).unwrap_or_default();

  if let Some(t) = title {
    Some(format!("Section: {}\n{}", t, json_text))
  } else {
    Some(json_text)
  }
}

/// Find a documentation node by its node_id across all documentation ASTs.
/// Resolves Stub nodes through `audit_data.nodes`, following the same pattern
/// as `render_documentation_ast_snippet` and other AST traversals.
fn find_doc_node_by_id(
  audit_data: &AuditData,
  target_id: i32,
) -> Option<&crate::documentation::ast::DocumentationNode> {
  fn search_node<'a>(
    node: &'a crate::documentation::ast::DocumentationNode,
    target_id: i32,
    nodes_map: &'a std::collections::BTreeMap<topic::Topic, domain::Node>,
  ) -> Option<&'a crate::documentation::ast::DocumentationNode> {
    let resolved = node.resolve(nodes_map);
    if resolved.node_id() == target_id {
      return Some(resolved);
    }
    for child in resolved.children() {
      if let Some(found) = search_node(child, target_id, nodes_map) {
        return Some(found);
      }
    }
    None
  }

  for ast in audit_data.asts.values() {
    let doc_ast = match ast {
      domain::AST::Documentation(doc_ast) => doc_ast,
      _ => continue,
    };
    for node in &doc_ast.nodes {
      if let Some(found) = search_node(node, target_id, &audit_data.nodes) {
        return Some(found);
      }
    }
  }

  None
}

/// One CodeIdentifier found inside a documentation section, with its
/// resolution status. Used by the mechanical-trace mode to surface every
/// inline-code reference (resolved or not) for diagnostic review.
#[derive(Debug, Clone)]
pub struct CodeReference {
  /// The literal text of the identifier as it appears in the doc.
  pub text: String,
  /// `Some` if the parser resolved this identifier to a declaration.
  pub resolved_topic: Option<topic::Topic>,
  /// Resolved declaration kind (when resolved).
  pub resolved_kind: Option<domain::NamedTopicKind>,
  /// Resolved declaration's canonical name (when resolved). May differ
  /// from `text` if the original referred via an alias.
  pub resolved_name: Option<String>,
}

/// Walk a section's documentation AST and return every CodeIdentifier
/// node found inside, in left-to-right order. Resolution status (whether
/// the identifier resolved to a declaration topic) is preserved on each
/// returned record.
pub fn enumerate_section_code_references(
  section_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<CodeReference> {
  let Some(section_node) =
    find_doc_node_by_id(audit_data, section_topic.numeric_id())
  else {
    return Vec::new();
  };
  let mut out: Vec<CodeReference> = Vec::new();
  collect_code_identifiers_recursive(section_node, audit_data, &mut out);
  out
}

fn collect_code_identifiers_recursive(
  node: &crate::documentation::ast::DocumentationNode,
  audit_data: &AuditData,
  out: &mut Vec<CodeReference>,
) {
  let resolved = node.resolve(&audit_data.nodes);
  if let DocumentationNode::CodeIdentifier {
    value,
    referenced_topic,
    kind,
    referenced_name,
    ..
  } = resolved
  {
    out.push(CodeReference {
      text: value.clone(),
      resolved_topic: *referenced_topic,
      resolved_kind: kind.clone(),
      resolved_name: referenced_name.clone(),
    });
    return;
  }
  for child in resolved.children() {
    collect_code_identifiers_recursive(child, audit_data, out);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::collaborator::synthetic;
  use crate::domain::{
    ContractKind, NamedTopicKind, NamedTopicVisibility, Scope,
  };
  use crate::solidity::ast::SourceLocation;
  use std::collections::HashSet;

  fn dummy_src_location() -> SourceLocation {
    SourceLocation {
      start: None,
      length: None,
      index: None,
    }
  }

  fn empty_parameter_list(node_id: i32) -> ASTNode {
    ASTNode::ParameterList {
      node_id,
      src_location: dummy_src_location(),
      parameters: vec![],
      is_return_parameters: false,
    }
  }

  /// Builds a minimal ContractDefinition containing a single
  /// ContractMemberGroup that wraps one EventDefinition. Returns the contract
  /// node alongside the topics for the group and the event so tests can
  /// assert against them.
  fn make_single_member_contract(
    contract_id: i32,
    signature_id: i32,
    group_id: i32,
    event_id: i32,
    event_name: &str,
    doc_text: Option<&str>,
  ) -> (ASTNode, topic::Topic, topic::Topic) {
    let event_node = ASTNode::EventDefinition {
      node_id: event_id,
      src_location: dummy_src_location(),
      name: event_name.to_string(),
      name_location: dummy_src_location(),
      parameters: Box::new(empty_parameter_list(event_id + 1)),
    };
    let group_node = ASTNode::ContractMemberGroup {
      node_id: group_id,
      src_location: dummy_src_location(),
      documentation: doc_text.map(str::to_string),
      members: vec![event_node],
    };
    let signature_node = ASTNode::ContractSignature {
      node_id: signature_id,
      src_location: dummy_src_location(),
      documentation: None,
      name: "TestContract".to_string(),
      name_location: dummy_src_location(),
      declaration_id: contract_id,
      contract_kind: ContractKind::Contract,
      abstract_: false,
      base_contracts: vec![],
      directives: vec![],
    };
    let contract_node = ASTNode::ContractDefinition {
      node_id: contract_id,
      src_location: dummy_src_location(),
      signature: Box::new(signature_node),
      nodes: vec![group_node],
    };
    (
      contract_node,
      topic::new_node_topic(&group_id),
      topic::new_node_topic(&event_id),
    )
  }

  /// Registers a NamedTopic for the event so that `render_solidity_ast_snippet`
  /// can look up its metadata.
  fn register_event_metadata(
    audit_data: &mut AuditData,
    event_topic: &topic::Topic,
    name: &str,
  ) {
    audit_data.topic_metadata.insert(
      event_topic.clone(),
      TopicMetadata::NamedTopic {
        topic: event_topic.clone(),
        scope: Scope::Global,
        kind: NamedTopicKind::Event,
        name: name.to_string(),
        visibility: NamedTopicVisibility::Public,
        is_mutable: false,
        mutations: vec![],
        ancestors: vec![],
        descendants: vec![],
        relatives: vec![],
        transitive_topic: None,
        doc_references: vec![],
      },
    );
  }

  #[test]
  fn test_dev_comment_from_contract_member_group_reaches_semantic_linking_render()
   {
    // End-to-end: a ContractMemberGroup with a single member and an inline
    // `// comment` should produce a DevTechnical synthetic comment that
    // `render_contract_members_for_semantic_linking` includes in its JSON output for
    // the member.
    let mut audit_data =
      domain::new_audit_data("test".to_string(), HashSet::new(), None);

    let (contract_node, group_topic, event_topic) = make_single_member_contract(
      1,
      2,
      -400,
      100,
      "Approved",
      Some("Fires when the admin approves"),
    );

    // Store the ContractMemberGroup (with its nested EventDefinition) in
    // audit_data.nodes so inject_developer_documentation can find it.
    let group_node_owned = match &contract_node {
      ASTNode::ContractDefinition { nodes, .. } => nodes[0].clone(),
      _ => unreachable!(),
    };
    audit_data
      .nodes
      .insert(group_topic.clone(), Node::Solidity(group_node_owned));

    // Single-member group → UnnamedTopic metadata with transitive_topic so
    // the synthetic comment resolves through to the event's topic.
    audit_data.topic_metadata.insert(
      group_topic.clone(),
      TopicMetadata::UnnamedTopic {
        topic: group_topic.clone(),
        scope: Scope::Global,
        kind: domain::UnnamedTopicKind::ContractMemberGroup,
        transitive_topic: Some(event_topic.clone()),
      },
    );
    register_event_metadata(&mut audit_data, &event_topic, "Approved");

    // Inject developer documentation — this is the real code path that
    // creates synthetic DevTechnical comments from group docs.
    synthetic::create_synthetic_dev_comment(
      &event_topic,
      "Fires when the admin approves",
      CommentType::DevTechnical,
      crate::collaborator::models::Author::DevTechnical,
      &mut audit_data,
    );

    let rendered =
      render_contract_members_for_semantic_linking(&contract_node, &audit_data)
        .expect("semantic linking render returned None");

    let value: serde_json::Value = serde_json::from_str(&rendered)
      .expect("semantic linking render produced invalid JSON");
    let members = value
      .get("members")
      .and_then(|m| m.as_array())
      .expect("members field missing or wrong type");
    assert_eq!(members.len(), 1, "expected exactly one flattened member");

    let comments = members[0]
      .get("comments")
      .and_then(|c| c.as_array())
      .expect("comments field missing on member");
    assert!(
      comments
        .iter()
        .filter_map(|c| c.as_str())
        .any(|s| s.contains("[dev]") && s.contains("approves")),
      "expected [dev] comment on member, got: {:?}",
      comments
    );
  }

  #[test]
  fn test_multi_member_group_comment_surfaces_in_semantic_linking_render() {
    // A multi-member ContractMemberGroup (with no transitive topic) stores
    // its dev comment on the group topic itself. The semantic linking render should
    // NOT flatten the group — it should emit the wrapper with the comment
    // attached so the agent sees the group header alongside its members.
    let mut audit_data =
      domain::new_audit_data("test".to_string(), HashSet::new(), None);

    let contract_id = 1;
    let contract_sig_id = 2;
    let group_id = -600;
    let event_a_id = 300;
    let event_b_id = 301;

    let group_topic = topic::new_node_topic(&group_id);
    let event_a_topic = topic::new_node_topic(&event_a_id);
    let event_b_topic = topic::new_node_topic(&event_b_id);

    let event_a = ASTNode::EventDefinition {
      node_id: event_a_id,
      src_location: dummy_src_location(),
      name: "AdminSet".to_string(),
      name_location: dummy_src_location(),
      parameters: Box::new(empty_parameter_list(event_a_id + 100)),
    };
    let event_b = ASTNode::EventDefinition {
      node_id: event_b_id,
      src_location: dummy_src_location(),
      name: "AdminRevoked".to_string(),
      name_location: dummy_src_location(),
      parameters: Box::new(empty_parameter_list(event_b_id + 100)),
    };
    let group_node = ASTNode::ContractMemberGroup {
      node_id: group_id,
      src_location: dummy_src_location(),
      documentation: Some("Admin lifecycle events".to_string()),
      members: vec![event_a, event_b],
    };
    let contract_node = ASTNode::ContractDefinition {
      node_id: contract_id,
      src_location: dummy_src_location(),
      signature: Box::new(ASTNode::ContractSignature {
        node_id: contract_sig_id,
        src_location: dummy_src_location(),
        documentation: None,
        name: "TestContract".to_string(),
        name_location: dummy_src_location(),
        declaration_id: contract_id,
        contract_kind: ContractKind::Contract,
        abstract_: false,
        base_contracts: vec![],
        directives: vec![],
      }),
      nodes: vec![group_node.clone()],
    };

    audit_data
      .nodes
      .insert(group_topic.clone(), Node::Solidity(group_node));

    // Multi-member group → NO transitive topic; comment lands on the group.
    audit_data.topic_metadata.insert(
      group_topic.clone(),
      TopicMetadata::UnnamedTopic {
        topic: group_topic.clone(),
        scope: Scope::Global,
        kind: domain::UnnamedTopicKind::ContractMemberGroup,
        transitive_topic: None,
      },
    );
    register_event_metadata(&mut audit_data, &event_a_topic, "AdminSet");
    register_event_metadata(&mut audit_data, &event_b_topic, "AdminRevoked");

    synthetic::create_synthetic_dev_comment(
      &group_topic,
      "Admin lifecycle events",
      CommentType::DevTechnical,
      crate::collaborator::models::Author::DevTechnical,
      &mut audit_data,
    );

    let rendered =
      render_contract_members_for_semantic_linking(&contract_node, &audit_data)
        .expect("semantic linking render returned None");

    let value: serde_json::Value = serde_json::from_str(&rendered)
      .expect("semantic linking render produced invalid JSON");
    let members = value
      .get("members")
      .and_then(|m| m.as_array())
      .expect("members field missing");
    assert_eq!(
      members.len(),
      1,
      "multi-member group with comment should render as a single wrapper"
    );

    let wrapper = &members[0];
    assert_eq!(
      wrapper.get("kind").and_then(|k| k.as_str()),
      Some("contract_member_group"),
      "wrapper should be a ContractMemberGroup"
    );
    let group_comments = wrapper
      .get("comments")
      .and_then(|c| c.as_array())
      .expect("group wrapper should carry its dev comment");
    assert!(
      group_comments
        .iter()
        .filter_map(|c| c.as_str())
        .any(|s| s.contains("[dev]") && s.contains("Admin lifecycle")),
      "expected group-level [dev] comment, got: {:?}",
      group_comments
    );
    let inner = wrapper
      .get("members")
      .and_then(|m| m.as_array())
      .expect("group wrapper should nest inner members");
    assert_eq!(inner.len(), 2, "both events should render inside the group");
  }

  #[test]
  fn test_dev_comment_from_contract_member_group_reaches_behavior_render() {
    // Same end-to-end verification but for the behavior-extraction renderer.
    // That renderer only emits function/modifier members, so we use a
    // FunctionDefinition here.
    let mut audit_data =
      domain::new_audit_data("test".to_string(), HashSet::new(), None);

    let contract_id = 1;
    let contract_sig_id = 2;
    let group_id = -500;
    let function_id = 200;
    let function_sig_id = 201;
    let params_id = 202;
    let returns_id = 203;
    let modifiers_id = 204;
    let body_id = 205;

    let event_topic = topic::new_node_topic(&function_id);
    let group_topic = topic::new_node_topic(&group_id);
    let contract_topic = topic::new_node_topic(&contract_id);

    let function_node = ASTNode::FunctionDefinition {
      node_id: function_id,
      src_location: dummy_src_location(),
      implemented: true,
      signature: Box::new(ASTNode::FunctionSignature {
        node_id: function_sig_id,
        src_location: dummy_src_location(),
        documentation: None,
        kind: crate::domain::FunctionKind::Function,
        modifiers: Box::new(ASTNode::ModifierList {
          node_id: modifiers_id,
          src_location: dummy_src_location(),
          modifiers: vec![],
        }),
        name: "doThing".to_string(),
        name_location: dummy_src_location(),
        declaration_id: function_id,
        parameters: Box::new(empty_parameter_list(params_id)),
        return_parameters: Box::new(ASTNode::ParameterList {
          node_id: returns_id,
          src_location: dummy_src_location(),
          parameters: vec![],
          is_return_parameters: true,
        }),
        scope: contract_id,
        state_mutability:
          crate::solidity::ast::FunctionStateMutability::NonPayable,
        virtual_: false,
        visibility: crate::solidity::ast::FunctionVisibility::External,
        implementation_declaration: None,
      }),
      body: Some(Box::new(ASTNode::Block {
        node_id: body_id,
        src_location: dummy_src_location(),
        statements: vec![],
      })),
    };

    let group_node = ASTNode::ContractMemberGroup {
      node_id: group_id,
      src_location: dummy_src_location(),
      documentation: Some("Admin-only entry point".to_string()),
      members: vec![function_node],
    };

    let contract_node = ASTNode::ContractDefinition {
      node_id: contract_id,
      src_location: dummy_src_location(),
      signature: Box::new(ASTNode::ContractSignature {
        node_id: contract_sig_id,
        src_location: dummy_src_location(),
        documentation: None,
        name: "TestContract".to_string(),
        name_location: dummy_src_location(),
        declaration_id: contract_id,
        contract_kind: ContractKind::Contract,
        abstract_: false,
        base_contracts: vec![],
        directives: vec![],
      }),
      nodes: vec![group_node.clone()],
    };

    audit_data
      .nodes
      .insert(group_topic.clone(), Node::Solidity(group_node.clone()));

    // The batch renderer looks up the function node by its member topic
    // (audit_data.nodes), so the function definition needs to live there
    // too. In a real audit this is populated by the analyzer's
    // populate_nodes_pass.
    let function_node_for_lookup = match &group_node {
      ASTNode::ContractMemberGroup { members, .. } => members[0].clone(),
      _ => unreachable!(),
    };
    audit_data.nodes.insert(
      event_topic.clone(),
      Node::Solidity(function_node_for_lookup),
    );

    audit_data.topic_metadata.insert(
      group_topic.clone(),
      TopicMetadata::UnnamedTopic {
        topic: group_topic.clone(),
        scope: Scope::Global,
        kind: domain::UnnamedTopicKind::ContractMemberGroup,
        transitive_topic: Some(event_topic.clone()),
      },
    );
    audit_data.topic_metadata.insert(
      event_topic.clone(),
      TopicMetadata::NamedTopic {
        topic: event_topic.clone(),
        scope: Scope::Component {
          container: crate::domain::ProjectPath {
            file_path: "test.sol".to_string(),
          },
          component: contract_topic.clone(),
        },
        kind: NamedTopicKind::Function(crate::domain::FunctionKind::Function),
        name: "doThing".to_string(),
        visibility: NamedTopicVisibility::Public,
        is_mutable: false,
        mutations: vec![],
        ancestors: vec![],
        descendants: vec![],
        relatives: vec![],
        transitive_topic: None,
        doc_references: vec![],
      },
    );

    // Untrusted dev comment: must NOT leak into the behavior render.
    synthetic::create_synthetic_dev_comment(
      &event_topic,
      "Admin-only entry point",
      CommentType::DevTechnical,
      crate::collaborator::models::Author::DevTechnical,
      &mut audit_data,
    );

    // Trusted auditor comment: should still surface.
    synthetic::create_synthetic_dev_comment(
      &event_topic,
      "Auditor: confirmed role-restricted",
      CommentType::Info,
      crate::collaborator::models::Author::System,
      &mut audit_data,
    );

    // Trusted pipeline-generated functional semantic annotation.
    let semantic_topic = topic::new_functional_property_topic(9001);
    audit_data.topic_metadata.insert(
      semantic_topic.clone(),
      TopicMetadata::FunctionalSemanticTopic {
        topic: semantic_topic.clone(),
        description: "Only callable by the admin role".to_string(),
        declaration_topic: event_topic.clone(),
        documentation_topics: vec![],
        author: crate::collaborator::models::Author::System,
        created_at: None,
        match_source: None,
      },
    );
    audit_data
      .declaration_semantics
      .entry(event_topic.clone())
      .or_default()
      .push(semantic_topic);

    // contract_node is unused under the batch renderer (which keys off
    // member topics directly). Reference it once to suppress the
    // unused-variable lint without changing the rest of the test setup.
    let _ = &contract_node;

    let rendered =
      render_batch_for_behavior_extraction(&[event_topic.clone()], &audit_data)
        .expect("behavior extraction returned None");

    let value: serde_json::Value = serde_json::from_str(&rendered.json)
      .expect("behavior extraction produced invalid JSON");
    let members = value
      .get("batch")
      .and_then(|m| m.as_array())
      .expect("batch field missing");
    assert_eq!(members.len(), 1, "expected one rendered function member");

    // The function's own comments live on its definition (the inner
    // FunctionDefinition AST node).
    let definition = members[0]
      .get("definition")
      .expect("definition field missing on batch member");
    let comments: Vec<&str> = definition
      .get("comments")
      .and_then(|c| c.as_array())
      .map(|arr| arr.iter().filter_map(|c| c.as_str()).collect())
      .unwrap_or_default();

    assert!(
      comments.iter().all(|s| !s.contains("[dev]")),
      "dev comments must not leak into behavior extraction, got: {:?}",
      comments
    );
    assert!(
      comments.iter().any(|s| s.contains("Auditor:")),
      "auditor-authored Info comments should still surface, got: {:?}",
      comments
    );

    // The batch render emits semantics as a flat per-function map keyed
    // by declaration topic. The per-node inline semantics still live on
    // the definition's AST nodes; either is acceptable evidence that the
    // trusted annotation surfaced.
    let inline_semantics = definition
      .get("semantics")
      .and_then(|s| s.as_array())
      .map(|arr| {
        arr
          .iter()
          .filter_map(|s| {
            s.get("description")
              .and_then(|v| v.as_str())
              .map(String::from)
          })
          .collect::<Vec<_>>()
      })
      .unwrap_or_default();
    assert!(
      inline_semantics.iter().any(|t| t.contains("admin role")),
      "expected trusted inline semantic annotation on function definition, \
       got: {:?}",
      inline_semantics
    );
  }

  // ---------------------------------------------------------------------
  // Phase E (anchor-by-name) downstream consumer contract
  //
  // The doc-tree resolution pass (in `o11a-analyze`) writes the full
  // candidate list onto `referenced_topic_candidates` for refs Phase D
  // could not pin. The downstream consumer here (`mechanical_semantic_links`)
  // unions each candidate's containing contract into
  // `section_to_contracts` without contributing to
  // `section_to_declarations`. Tests below pin that contract directly,
  // bypassing the resolver — they construct the post-resolution AST
  // shape by hand so the consumer's contribution is testable in
  // isolation.
  // ---------------------------------------------------------------------

  /// Build an audit whose Section contains one ambiguous-but-Phase-E
  /// `CodeIdentifier` whose candidate list spans two contracts. Returns
  /// the audit, the section topic, and both contract topics.
  fn audit_with_phase_e_candidates_in_two_contracts()
  -> (AuditData, topic::Topic, topic::Topic, topic::Topic) {
    use crate::documentation::ast::{DocumentationAST, DocumentationNode};

    let mut audit =
      domain::new_audit_data("test".to_string(), HashSet::new(), None);

    let vault = topic::new_node_topic(&100);
    let token = topic::new_node_topic(&200);
    let vault_transfer = topic::new_node_topic(&101);
    let token_transfer = topic::new_node_topic(&201);

    for (t, name, kind, scope) in [
      (
        vault,
        "Vault",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container {
          container: domain::ProjectPath {
            file_path: "Vault.sol".to_string(),
          },
        },
      ),
      (
        token,
        "Token",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container {
          container: domain::ProjectPath {
            file_path: "Token.sol".to_string(),
          },
        },
      ),
      (
        vault_transfer,
        "transfer",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: domain::ProjectPath {
            file_path: "Vault.sol".to_string(),
          },
          component: vault,
        },
      ),
      (
        token_transfer,
        "transfer",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: domain::ProjectPath {
            file_path: "Token.sol".to_string(),
          },
          component: token,
        },
      ),
    ] {
      audit.topic_metadata.insert(
        t,
        TopicMetadata::NamedTopic {
          topic: t,
          scope,
          kind,
          name: name.to_string(),
          visibility: NamedTopicVisibility::Public,
          is_mutable: false,
          mutations: vec![],
          ancestors: vec![],
          descendants: vec![],
          relatives: vec![],
          transitive_topic: None,
          doc_references: vec![],
        },
      );
    }

    let section_id = 700;
    let section_topic = topic::new_documentation_topic(section_id);
    let phase_e_ident = DocumentationNode::CodeIdentifier {
      node_id: 901,
      value: "transfer".to_string(),
      // Phase E shape: referenced_topic stays None, candidates lists
      // both possible contracts' transfer functions.
      referenced_topic: None,
      kind: None,
      referenced_name: None,
      referenced_topic_candidates: vec![vault_transfer, token_transfer],
    };
    let section = DocumentationNode::Section {
      node_id: section_id,
      title: "Overview".to_string(),
      children: vec![DocumentationNode::Paragraph {
        node_id: section_id + 1,
        position: None,
        children: vec![phase_e_ident],
      }],
    };
    let doc_path = domain::ProjectPath {
      file_path: "README.md".to_string(),
    };
    audit.asts.insert(
      doc_path.clone(),
      domain::AST::Documentation(DocumentationAST {
        nodes: vec![section],
        project_path: doc_path,
        source_content: String::new(),
      }),
    );

    (audit, section_topic, vault, token)
  }

  /// Phase E candidates spanning two contracts: the consumer unions
  /// both contract topics into `section_to_contracts` and contributes
  /// nothing to `section_to_declarations`.
  #[test]
  fn phase_e_candidates_anchor_section_to_each_candidates_contract() {
    let (audit, section_topic, vault, token) =
      audit_with_phase_e_candidates_in_two_contracts();
    let result = mechanical_semantic_links(&audit);

    let mut anchored = result
      .section_to_contracts
      .get(&section_topic)
      .cloned()
      .expect("Phase E must populate section_to_contracts");
    anchored.sort_by_key(|t| t.id().to_string());
    let mut expected = vec![vault, token];
    expected.sort_by_key(|t| t.id().to_string());
    assert_eq!(
      anchored, expected,
      "Phase E unions both candidate contracts as anchors",
    );

    assert!(
      !result.section_to_declarations.contains_key(&section_topic),
      "Phase E must not contribute to section_to_declarations: {:?}",
      result.section_to_declarations.get(&section_topic),
    );
  }

  /// Multiple Phase E refs in the same section, each whose candidates
  /// pin distinct contracts, must union their contracts into the
  /// section's anchor set without duplication. Pin the union semantics
  /// across sibling Phase E refs.
  #[test]
  fn multiple_phase_e_refs_union_candidate_contracts_into_section_anchors() {
    use crate::documentation::ast::{DocumentationAST, DocumentationNode};
    let mut audit =
      domain::new_audit_data("test".to_string(), HashSet::new(), None);

    let alpha = topic::new_node_topic(&100);
    let beta = topic::new_node_topic(&200);
    let gamma = topic::new_node_topic(&300);
    let alpha_x = topic::new_node_topic(&101);
    let beta_x = topic::new_node_topic(&201);
    let gamma_y = topic::new_node_topic(&301);
    let alpha_y = topic::new_node_topic(&102);

    for (t, name, kind, scope) in [
      (
        alpha,
        "Alpha",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container {
          container: domain::ProjectPath {
            file_path: "Alpha.sol".into(),
          },
        },
      ),
      (
        beta,
        "Beta",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container {
          container: domain::ProjectPath {
            file_path: "Beta.sol".into(),
          },
        },
      ),
      (
        gamma,
        "Gamma",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container {
          container: domain::ProjectPath {
            file_path: "Gamma.sol".into(),
          },
        },
      ),
      (
        alpha_x,
        "x",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: domain::ProjectPath {
            file_path: "Alpha.sol".into(),
          },
          component: alpha,
        },
      ),
      (
        beta_x,
        "x",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: domain::ProjectPath {
            file_path: "Beta.sol".into(),
          },
          component: beta,
        },
      ),
      (
        gamma_y,
        "y",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: domain::ProjectPath {
            file_path: "Gamma.sol".into(),
          },
          component: gamma,
        },
      ),
      (
        alpha_y,
        "y",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: domain::ProjectPath {
            file_path: "Alpha.sol".into(),
          },
          component: alpha,
        },
      ),
    ] {
      audit.topic_metadata.insert(
        t,
        TopicMetadata::NamedTopic {
          topic: t,
          scope,
          kind,
          name: name.to_string(),
          visibility: NamedTopicVisibility::Public,
          is_mutable: false,
          mutations: vec![],
          ancestors: vec![],
          descendants: vec![],
          relatives: vec![],
          transitive_topic: None,
          doc_references: vec![],
        },
      );
    }

    // Two Phase E refs in the same section:
    //   - "x" candidates → {alpha_x, beta_x} → contracts {alpha, beta}
    //   - "y" candidates → {alpha_y, gamma_y} → contracts {alpha, gamma}
    // Union → {alpha, beta, gamma}. `alpha` appears twice in the
    // candidate union but only once in the anchor set (de-duplicated by
    // the `contains` check in the consumer).
    let section_id = 700;
    let section_topic = topic::new_documentation_topic(section_id);
    let mk_phase_e =
      |node_id: i32, value: &str, candidates: Vec<topic::Topic>| {
        DocumentationNode::CodeIdentifier {
          node_id,
          value: value.to_string(),
          referenced_topic: None,
          kind: None,
          referenced_name: None,
          referenced_topic_candidates: candidates,
        }
      };
    let section = DocumentationNode::Section {
      node_id: section_id,
      title: "Overview".to_string(),
      children: vec![DocumentationNode::Paragraph {
        node_id: section_id + 1,
        position: None,
        children: vec![
          mk_phase_e(901, "x", vec![alpha_x, beta_x]),
          mk_phase_e(902, "y", vec![alpha_y, gamma_y]),
        ],
      }],
    };
    let doc_path = domain::ProjectPath {
      file_path: "README.md".into(),
    };
    audit.asts.insert(
      doc_path.clone(),
      domain::AST::Documentation(DocumentationAST {
        nodes: vec![section],
        project_path: doc_path,
        source_content: String::new(),
      }),
    );

    let result = mechanical_semantic_links(&audit);
    let mut anchored = result
      .section_to_contracts
      .get(&section_topic)
      .cloned()
      .expect("union of Phase E refs must populate anchors");
    anchored.sort_by_key(|t| t.id().to_string());
    let mut expected = vec![alpha, beta, gamma];
    expected.sort_by_key(|t| t.id().to_string());
    assert_eq!(
      anchored, expected,
      "union of two Phase E refs' candidate contracts (de-duplicated)",
    );
    assert!(
      !result.section_to_declarations.contains_key(&section_topic),
      "still no declarations from Phase E",
    );
  }

  /// Phase E + Phase A coexist in the same section: Phase A contributes
  /// declaration + contract, Phase E contributes its candidates'
  /// contracts. Both effects compose; Phase E doesn't shadow or evict
  /// Phase A's contributions.
  #[test]
  fn phase_e_and_phase_a_refs_compose_in_same_section() {
    use crate::documentation::ast::{DocumentationAST, DocumentationNode};
    let mut audit =
      domain::new_audit_data("test".to_string(), HashSet::new(), None);

    let vault = topic::new_node_topic(&100);
    let token = topic::new_node_topic(&200);
    let vault_a = topic::new_node_topic(&101);
    let token_b = topic::new_node_topic(&201);
    let token_c = topic::new_node_topic(&202);

    for (t, name, kind, scope) in [
      (
        vault,
        "Vault",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container {
          container: domain::ProjectPath {
            file_path: "Vault.sol".into(),
          },
        },
      ),
      (
        token,
        "Token",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container {
          container: domain::ProjectPath {
            file_path: "Token.sol".into(),
          },
        },
      ),
      (
        vault_a,
        "doA",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: domain::ProjectPath {
            file_path: "Vault.sol".into(),
          },
          component: vault,
        },
      ),
      (
        token_b,
        "doB",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: domain::ProjectPath {
            file_path: "Token.sol".into(),
          },
          component: token,
        },
      ),
      (
        token_c,
        "doB",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: domain::ProjectPath {
            file_path: "Vault.sol".into(),
          },
          component: vault,
        },
      ),
    ] {
      audit.topic_metadata.insert(
        t,
        TopicMetadata::NamedTopic {
          topic: t,
          scope,
          kind,
          name: name.to_string(),
          visibility: NamedTopicVisibility::Public,
          is_mutable: false,
          mutations: vec![],
          ancestors: vec![],
          descendants: vec![],
          relatives: vec![],
          transitive_topic: None,
          doc_references: vec![],
        },
      );
    }

    let section_id = 700;
    let section_topic = topic::new_documentation_topic(section_id);
    let phase_a_ident = DocumentationNode::CodeIdentifier {
      node_id: 901,
      value: "doA".to_string(),
      referenced_topic: Some(vault_a),
      kind: Some(NamedTopicKind::Function(FunctionKind::Function)),
      referenced_name: Some("doA".to_string()),
      referenced_topic_candidates: vec![],
    };
    let phase_e_ident = DocumentationNode::CodeIdentifier {
      node_id: 902,
      value: "doB".to_string(),
      referenced_topic: None,
      kind: None,
      referenced_name: None,
      referenced_topic_candidates: vec![token_b, token_c],
    };
    let section = DocumentationNode::Section {
      node_id: section_id,
      title: "Overview".to_string(),
      children: vec![DocumentationNode::Paragraph {
        node_id: section_id + 1,
        position: None,
        children: vec![phase_a_ident, phase_e_ident],
      }],
    };
    let doc_path = domain::ProjectPath {
      file_path: "README.md".into(),
    };
    audit.asts.insert(
      doc_path.clone(),
      domain::AST::Documentation(DocumentationAST {
        nodes: vec![section],
        project_path: doc_path,
        source_content: String::new(),
      }),
    );

    let result = mechanical_semantic_links(&audit);

    // section_to_declarations: only Phase A contributes (vault_a).
    let decls = result
      .section_to_declarations
      .get(&section_topic)
      .expect("Phase A must contribute to declarations");
    assert_eq!(*decls, vec![vault_a]);

    // section_to_contracts: union of Phase A's contract (vault) and
    // Phase E candidates' contracts (token, vault). Vault appears in
    // both but is de-duplicated by the contains check.
    let mut anchored = result
      .section_to_contracts
      .get(&section_topic)
      .cloned()
      .expect("Phase A + Phase E must both anchor");
    anchored.sort_by_key(|t| t.id().to_string());
    let mut expected = vec![vault, token];
    expected.sort_by_key(|t| t.id().to_string());
    assert_eq!(
      anchored, expected,
      "Phase A and Phase E unite their contract anchors",
    );
  }

  /// Phase E refs nested under a child Section roll their anchors up to
  /// the top-level Section topic — same rollup behavior as Phase A,
  /// since both flow through the same `current_section` dispatch.
  #[test]
  fn phase_e_in_nested_section_rolls_up_to_top_level_anchor() {
    use crate::documentation::ast::{DocumentationAST, DocumentationNode};
    let mut audit =
      domain::new_audit_data("test".to_string(), HashSet::new(), None);

    let vault = topic::new_node_topic(&100);
    let token = topic::new_node_topic(&200);
    let vault_x = topic::new_node_topic(&101);
    let token_x = topic::new_node_topic(&201);

    for (t, name, kind, scope) in [
      (
        vault,
        "Vault",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container {
          container: domain::ProjectPath {
            file_path: "Vault.sol".into(),
          },
        },
      ),
      (
        token,
        "Token",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container {
          container: domain::ProjectPath {
            file_path: "Token.sol".into(),
          },
        },
      ),
      (
        vault_x,
        "x",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: domain::ProjectPath {
            file_path: "Vault.sol".into(),
          },
          component: vault,
        },
      ),
      (
        token_x,
        "x",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: domain::ProjectPath {
            file_path: "Token.sol".into(),
          },
          component: token,
        },
      ),
    ] {
      audit.topic_metadata.insert(
        t,
        TopicMetadata::NamedTopic {
          topic: t,
          scope,
          kind,
          name: name.to_string(),
          visibility: NamedTopicVisibility::Public,
          is_mutable: false,
          mutations: vec![],
          ancestors: vec![],
          descendants: vec![],
          relatives: vec![],
          transitive_topic: None,
          doc_references: vec![],
        },
      );
    }

    let outer_id = 700;
    let inner_id = 701;
    let outer_topic = topic::new_documentation_topic(outer_id);
    let inner_topic = topic::new_documentation_topic(inner_id);
    let phase_e_ident = DocumentationNode::CodeIdentifier {
      node_id: 901,
      value: "x".to_string(),
      referenced_topic: None,
      kind: None,
      referenced_name: None,
      referenced_topic_candidates: vec![vault_x, token_x],
    };
    let inner_section = DocumentationNode::Section {
      node_id: inner_id,
      title: "Inner".to_string(),
      children: vec![DocumentationNode::Paragraph {
        node_id: inner_id + 100,
        position: None,
        children: vec![phase_e_ident],
      }],
    };
    let outer_section = DocumentationNode::Section {
      node_id: outer_id,
      title: "Outer".to_string(),
      children: vec![inner_section],
    };
    let doc_path = domain::ProjectPath {
      file_path: "README.md".into(),
    };
    audit.asts.insert(
      doc_path.clone(),
      domain::AST::Documentation(DocumentationAST {
        nodes: vec![outer_section],
        project_path: doc_path,
        source_content: String::new(),
      }),
    );

    let result = mechanical_semantic_links(&audit);

    // Anchor lands on OUTER, not inner. Same rollup contract as
    // Phase A — `collect_mechanical_links_recursive` keeps
    // `current_section` pinned to the top-level section.
    let mut anchored = result
      .section_to_contracts
      .get(&outer_topic)
      .cloned()
      .expect("Phase E must anchor to top-level section");
    anchored.sort_by_key(|t| t.id().to_string());
    let mut expected = vec![vault, token];
    expected.sort_by_key(|t| t.id().to_string());
    assert_eq!(anchored, expected);

    assert!(
      !result.section_to_contracts.contains_key(&inner_topic),
      "inner section must NOT receive its own anchor entry — rollup goes to outer",
    );
  }

  /// A Phase E node whose candidate list is empty contributes nothing
  /// — the field is the gate. (Phase E itself only writes the field
  /// when candidates exist; a hand-constructed empty list mirrors the
  /// pre-pass / no-candidate state.)
  #[test]
  fn phase_e_empty_candidate_list_contributes_nothing() {
    use crate::documentation::ast::{DocumentationAST, DocumentationNode};
    let mut audit =
      domain::new_audit_data("test".to_string(), HashSet::new(), None);

    let section_id = 700;
    let section_topic = topic::new_documentation_topic(section_id);
    let empty_phase_e = DocumentationNode::CodeIdentifier {
      node_id: 901,
      value: "missing".to_string(),
      referenced_topic: None,
      kind: None,
      referenced_name: None,
      // Empty candidates → no Phase E contribution downstream.
      referenced_topic_candidates: vec![],
    };
    let section = DocumentationNode::Section {
      node_id: section_id,
      title: "Overview".to_string(),
      children: vec![DocumentationNode::Paragraph {
        node_id: section_id + 1,
        position: None,
        children: vec![empty_phase_e],
      }],
    };
    let doc_path = domain::ProjectPath {
      file_path: "README.md".to_string(),
    };
    audit.asts.insert(
      doc_path.clone(),
      domain::AST::Documentation(DocumentationAST {
        nodes: vec![section],
        project_path: doc_path,
        source_content: String::new(),
      }),
    );

    let result = mechanical_semantic_links(&audit);
    assert!(
      !result.section_to_contracts.contains_key(&section_topic),
      "empty candidate list ⇒ no contract anchors",
    );
    assert!(
      !result.section_to_declarations.contains_key(&section_topic),
      "empty candidate list ⇒ no declarations",
    );
  }
}

#[cfg(test)]
mod synthesis_render_tests {
  //! Tests for the synthesis-step renderers and prior-context helpers
  //! introduced for the 5-step semantic-linking pipeline. These exercise
  //! the scope-filtering logic without standing up full ASTs — every helper
  //! under test reads only `audit_data.topic_metadata`.
  use super::*;
  use crate::domain::{
    self, ContractKind, FunctionKind, NamedTopicKind, NamedTopicVisibility,
    Scope, TopicMetadata, new_audit_data,
  };
  use std::collections::HashSet;
  use topic::Topic;

  fn project_path(file: &str) -> domain::ProjectPath {
    domain::ProjectPath {
      file_path: file.to_string(),
    }
  }

  fn insert_named(
    audit: &mut domain::AuditData,
    topic: Topic,
    name: &str,
    kind: NamedTopicKind,
    visibility: NamedTopicVisibility,
    scope: Scope,
  ) {
    audit.topic_metadata.insert(
      topic,
      TopicMetadata::NamedTopic {
        topic,
        scope,
        kind,
        name: name.to_string(),
        visibility,
        is_mutable: false,
        mutations: vec![],
        ancestors: vec![],
        descendants: vec![],
        relatives: vec![],
        transitive_topic: None,
        doc_references: vec![],
      },
    );
  }

  /// Audit fixture: one Vault contract with one function `transfer` that
  /// has param `to`, return value `result`, and body local `temp`. Plus
  /// state var `balance` and event `Transfer` with arg `amount`.
  /// Returns the topics in the order:
  /// (vault, transfer, to, result, temp, balance, event_transfer, amount).
  #[allow(clippy::type_complexity)]
  fn build_vault_audit() -> (
    domain::AuditData,
    (Topic, Topic, Topic, Topic, Topic, Topic, Topic, Topic),
  ) {
    let mut audit = new_audit_data("test".to_string(), HashSet::new(), None);
    let path = project_path("Vault.sol");
    let vault = topic::new_node_topic(&100);
    let transfer = topic::new_node_topic(&110);
    let to = topic::new_node_topic(&111);
    let result = topic::new_node_topic(&112);
    let temp = topic::new_node_topic(&113);
    let balance = topic::new_node_topic(&120);
    let event_transfer = topic::new_node_topic(&130);
    let amount = topic::new_node_topic(&131);

    insert_named(
      &mut audit,
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      NamedTopicVisibility::Public,
      Scope::Container {
        container: path.clone(),
      },
    );
    insert_named(
      &mut audit,
      transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      NamedTopicVisibility::External,
      Scope::Component {
        container: path.clone(),
        component: vault,
      },
    );
    insert_named(
      &mut audit,
      to,
      "to",
      NamedTopicKind::LocalVariable,
      NamedTopicVisibility::Internal,
      Scope::Member {
        container: path.clone(),
        component: vault,
        member: transfer,
        signature_container: None,
      },
    );
    insert_named(
      &mut audit,
      result,
      "result",
      NamedTopicKind::LocalVariable,
      NamedTopicVisibility::Internal,
      Scope::Member {
        container: path.clone(),
        component: vault,
        member: transfer,
        signature_container: None,
      },
    );
    insert_named(
      &mut audit,
      temp,
      "temp",
      NamedTopicKind::LocalVariable,
      NamedTopicVisibility::Internal,
      Scope::ContainingBlock {
        container: path.clone(),
        component: vault,
        member: transfer,
        containing_blocks: vec![],
      },
    );
    insert_named(
      &mut audit,
      balance,
      "balance",
      NamedTopicKind::StateVariable(domain::VariableMutability::Mutable),
      NamedTopicVisibility::Public,
      Scope::Component {
        container: path.clone(),
        component: vault,
      },
    );
    insert_named(
      &mut audit,
      event_transfer,
      "Transfer",
      NamedTopicKind::Event,
      NamedTopicVisibility::Public,
      Scope::Component {
        container: path.clone(),
        component: vault,
      },
    );
    insert_named(
      &mut audit,
      amount,
      "amount",
      NamedTopicKind::LocalVariable,
      NamedTopicVisibility::Internal,
      Scope::Member {
        container: path.clone(),
        component: vault,
        member: event_transfer,
        signature_container: None,
      },
    );

    (
      audit,
      (
        vault,
        transfer,
        to,
        result,
        temp,
        balance,
        event_transfer,
        amount,
      ),
    )
  }

  fn json_topics(rendered: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(rendered).unwrap();
    v.as_array()
      .unwrap()
      .iter()
      .map(|d| d.get("topic").unwrap().as_str().unwrap().to_string())
      .collect()
  }

  #[test]
  fn signature_decls_include_member_and_params_but_exclude_body_locals() {
    let (audit, (_vault, transfer, to, result, temp, _, _, _)) =
      build_vault_audit();
    let rendered =
      render_member_signature_declarations_for_semantics(&[transfer], &audit);
    let topics = json_topics(&rendered);
    // Member topic itself plus its Scope::Member items (params/returns).
    assert!(topics.contains(&transfer.id().to_string()));
    assert!(topics.contains(&to.id().to_string()));
    assert!(topics.contains(&result.id().to_string()));
    // Body local must not appear here — that's step 5's job.
    assert!(!topics.contains(&temp.id().to_string()));
  }

  #[test]
  fn body_local_decls_include_only_containing_block_scope() {
    let (audit, (_vault, transfer, to, result, temp, _, _, _)) =
      build_vault_audit();
    let rendered =
      render_member_body_local_declarations_for_semantics(&[transfer], &audit);
    let topics = json_topics(&rendered);
    assert_eq!(topics, vec![temp.id().to_string()]);
    assert!(!topics.contains(&transfer.id().to_string()));
    assert!(!topics.contains(&to.id().to_string()));
    assert!(!topics.contains(&result.id().to_string()));
  }

  #[test]
  fn contract_level_decls_exclude_functions_and_modifiers() {
    let (audit, (vault, transfer, _, _, _, balance, event_transfer, _)) =
      build_vault_audit();
    let rendered =
      render_contract_level_declarations_for_semantics(&[vault], &audit);
    let topics = json_topics(&rendered);
    // State variables and events appear; functions do not.
    assert!(topics.contains(&balance.id().to_string()));
    assert!(topics.contains(&event_transfer.id().to_string()));
    assert!(!topics.contains(&transfer.id().to_string()));
  }
}

#[cfg(test)]
mod functional_property_render_tests {
  //! Tests for the helpers used by `render_batch_for_functional_properties`
  //! and the rest of pipeline step 5: `node_is_non_pure`,
  //! `walk_for_non_pure`, `lookup_member_feature`, `first_semantic`. These
  //! exercise behavior at the topic-metadata layer without requiring full
  //! AST construction where possible.
  use super::*;
  use crate::domain::{
    self, CallKind, FunctionModProperties, NamedTopicKind,
    NamedTopicVisibility, Scope, TopicMetadata, UnnamedTopicKind,
    new_audit_data,
  };
  use std::collections::HashSet;

  fn empty_audit() -> domain::AuditData {
    new_audit_data("test".to_string(), HashSet::new(), None)
  }

  fn add_unnamed_topic(
    audit: &mut domain::AuditData,
    id: i32,
    kind: UnnamedTopicKind,
  ) -> topic::Topic {
    let t = topic::new_node_topic(&id);
    audit.topic_metadata.insert(
      t,
      TopicMetadata::UnnamedTopic {
        topic: t,
        scope: Scope::Global,
        kind,
        transitive_topic: None,
      },
    );
    t
  }

  #[test]
  fn node_is_non_pure_recognizes_state_write() {
    let mut audit = empty_audit();
    let t =
      add_unnamed_topic(&mut audit, 10, UnnamedTopicKind::VariableMutation);
    assert!(node_is_non_pure(t.numeric_id(), &audit));
  }

  #[test]
  fn node_is_non_pure_recognizes_non_pure_call() {
    let mut audit = empty_audit();
    let t = add_unnamed_topic(
      &mut audit,
      10,
      UnnamedTopicKind::FunctionCall(CallKind::NonPure),
    );
    assert!(node_is_non_pure(t.numeric_id(), &audit));
  }

  #[test]
  fn node_is_non_pure_rejects_pure_call() {
    let mut audit = empty_audit();
    let t = add_unnamed_topic(
      &mut audit,
      10,
      UnnamedTopicKind::FunctionCall(CallKind::Pure),
    );
    assert!(!node_is_non_pure(t.numeric_id(), &audit));
  }

  #[test]
  fn node_is_non_pure_rejects_unknown_node_id() {
    let audit = empty_audit();
    assert!(!node_is_non_pure(999, &audit));
  }

  #[test]
  fn first_semantic_returns_only_present_description() {
    let mut audit = empty_audit();
    let decl = topic::new_node_topic(&5);
    let sem = topic::new_functional_property_topic(1);
    audit.topic_metadata.insert(
      sem,
      TopicMetadata::FunctionalSemanticTopic {
        topic: sem,
        description: "a balance".to_string(),
        declaration_topic: decl,
        documentation_topics: vec![],
        author: crate::collaborator::models::Author::System,
        created_at: None,
        match_source: None,
      },
    );
    audit.declaration_semantics.insert(decl, vec![sem]);

    assert_eq!(first_semantic(&decl, &audit), Some("a balance".to_string()));
  }

  #[test]
  fn first_semantic_returns_none_when_absent() {
    let audit = empty_audit();
    let decl = topic::new_node_topic(&5);
    assert_eq!(first_semantic(&decl, &audit), None);
  }

  #[test]
  fn first_semantic_warns_and_returns_first_when_multiple() {
    let mut audit = empty_audit();
    let decl = topic::new_node_topic(&5);
    let sem1 = topic::new_functional_property_topic(1);
    let sem2 = topic::new_functional_property_topic(2);
    for (sem, desc) in [(sem1, "first"), (sem2, "second")] {
      audit.topic_metadata.insert(
        sem,
        TopicMetadata::FunctionalSemanticTopic {
          topic: sem,
          description: desc.to_string(),
          declaration_topic: decl,
          documentation_topics: vec![],
          author: crate::collaborator::models::Author::System,
          created_at: None,
          match_source: None,
        },
      );
    }
    audit.declaration_semantics.insert(decl, vec![sem1, sem2]);

    // The function picks the first by iteration order — that's `sem1`.
    assert_eq!(first_semantic(&decl, &audit), Some("first".to_string()));
  }

  #[test]
  fn lookup_member_feature_returns_null_when_no_behaviors() {
    let audit = empty_audit();
    let member = topic::new_node_topic(&5);
    assert!(lookup_member_feature(&member, &audit).is_null());
  }

  #[test]
  fn lookup_member_feature_returns_null_when_no_link() {
    let mut audit = empty_audit();
    let member = topic::new_node_topic(&5);
    let beh = topic::new_behavior_topic(1);
    audit.topic_metadata.insert(
      beh,
      TopicMetadata::BehaviorTopic {
        topic: beh,
        description: "does X".to_string(),
        member_topic: member,
        author: crate::collaborator::models::Author::System,
        created_at: None,
      },
    );
    audit.member_behaviors.insert(member, vec![beh]);
    assert!(lookup_member_feature(&member, &audit).is_null());
  }

  #[test]
  fn lookup_member_feature_returns_feature_object_on_match() {
    let mut audit = empty_audit();
    let member = topic::new_node_topic(&5);
    let beh = topic::new_behavior_topic(1);
    let feat = topic::new_feature_topic(1);
    let req = topic::new_requirement_topic(1);
    audit.topic_metadata.insert(
      beh,
      TopicMetadata::BehaviorTopic {
        topic: beh,
        description: "does X".to_string(),
        member_topic: member,
        author: crate::collaborator::models::Author::System,
        created_at: None,
      },
    );
    audit.topic_metadata.insert(
      feat,
      TopicMetadata::FeatureTopic {
        topic: feat,
        name: "Vault".to_string(),
        description: "Vault feature".to_string(),
        author: crate::collaborator::models::Author::System,
        created_at: None,
      },
    );
    audit.topic_metadata.insert(
      req,
      TopicMetadata::RequirementTopic {
        topic: req,
        description: "must vault".to_string(),
        section_topic: topic::new_documentation_topic(1),
        author: crate::collaborator::models::Author::System,
        created_at: None,
      },
    );
    audit.member_behaviors.insert(member, vec![beh]);
    audit.feature_behavior_links.insert(feat, vec![beh]);
    audit.feature_requirement_links.insert(feat, vec![req]);

    let value = lookup_member_feature(&member, &audit);
    assert_eq!(value["topic"], feat.id());
    assert_eq!(value["name"], "Vault");
    assert_eq!(value["description"], "Vault feature");
    assert_eq!(value["requirements"][0], "must vault");
  }

  // ----- AST-driven walks -----

  fn dummy_loc() -> crate::solidity::ast::SourceLocation {
    crate::solidity::ast::SourceLocation {
      start: None,
      length: None,
      index: None,
    }
  }

  fn make_literal(node_id: i32) -> ASTNode {
    ASTNode::Literal {
      node_id,
      src_location: dummy_loc(),
      hex_value: String::new(),
      kind: crate::solidity::ast::LiteralKind::Number,
      type_descriptions: crate::solidity::ast::TypeDescriptions {
        type_identifier: String::new(),
        type_string: String::new(),
      },
      value: Some("1".to_string()),
    }
  }

  fn make_identifier(node_id: i32, name: &str, ref_decl: i32) -> ASTNode {
    ASTNode::Identifier {
      node_id,
      src_location: dummy_loc(),
      name: name.to_string(),
      overloaded_declarations: vec![],
      referenced_declaration: ref_decl,
    }
  }

  fn make_call(node_id: i32, callee_id: i32) -> ASTNode {
    ASTNode::FunctionCall {
      node_id,
      src_location: dummy_loc(),
      arguments: vec![],
      expression: Box::new(make_identifier(node_id + 1, "callee", callee_id)),
      name_locations: vec![],
      names: vec![],
      try_call: false,
      type_descriptions: crate::solidity::ast::TypeDescriptions {
        type_identifier: String::new(),
        type_string: String::new(),
      },
      referenced_return_declarations: vec![],
    }
  }

  fn make_assignment(node_id: i32, lhs: ASTNode, rhs: ASTNode) -> ASTNode {
    ASTNode::Assignment {
      node_id,
      src_location: dummy_loc(),
      operator: crate::solidity::ast::AssignmentOperator::Assign,
      right_hand_side: Box::new(rhs),
      left_hand_side: Box::new(lhs),
    }
  }

  #[test]
  fn walk_for_non_pure_collects_top_level_state_write() {
    let mut audit = empty_audit();
    let assignment =
      make_assignment(10, make_identifier(11, "x", 99), make_literal(12));
    add_unnamed_topic(&mut audit, 10, UnnamedTopicKind::VariableMutation);

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    walk_for_non_pure(&assignment, &audit, &mut out, &mut seen);
    assert_eq!(out, vec!["N10".to_string()]);
  }

  #[test]
  fn walk_for_non_pure_collects_nested_call_inside_state_write() {
    // `x = nonPureCall()` should yield BOTH the assignment (state write)
    // AND the function call topic in source order, deduped.
    let mut audit = empty_audit();
    let assignment =
      make_assignment(10, make_identifier(11, "x", 99), make_call(20, 100));
    add_unnamed_topic(&mut audit, 10, UnnamedTopicKind::VariableMutation);
    add_unnamed_topic(
      &mut audit,
      20,
      UnnamedTopicKind::FunctionCall(CallKind::NonPure),
    );

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    walk_for_non_pure(&assignment, &audit, &mut out, &mut seen);
    assert_eq!(out, vec!["N10".to_string(), "N20".to_string()]);
  }

  #[test]
  fn walk_for_non_pure_dedupes_repeated_topic() {
    // Walk the same subtree twice with a shared `seen` set — the second
    // walk must add no new entries even though the topic resolves
    // non-pure on each visit.
    let mut audit = empty_audit();
    let inner = ASTNode::ExpressionStatement {
      node_id: 20,
      src_location: dummy_loc(),
      expression: Box::new(make_identifier(21, "n", 0)),
    };
    add_unnamed_topic(
      &mut audit,
      20,
      UnnamedTopicKind::FunctionCall(CallKind::NonPure),
    );

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    walk_for_non_pure(&inner, &audit, &mut out, &mut seen);
    walk_for_non_pure(&inner, &audit, &mut out, &mut seen);
    assert_eq!(
      out,
      vec!["N20".to_string()],
      "second walk shares the same `seen` set and adds no new entries"
    );
  }

  #[test]
  fn collect_member_semantics_includes_scoped_locals_and_state_mutations() {
    let mut audit = empty_audit();
    let member = topic::new_node_topic(&100);
    let param = topic::new_node_topic(&101);
    let local = topic::new_node_topic(&102);
    let state_var = topic::new_node_topic(&103);
    let unrelated = topic::new_node_topic(&999);
    let container = domain::ProjectPath {
      file_path: "test.sol".to_string(),
    };
    let component = topic::new_node_topic(&50);

    // Parameter: Scope::Member { member }
    audit.topic_metadata.insert(
      param,
      TopicMetadata::NamedTopic {
        topic: param,
        scope: Scope::Member {
          container: container.clone(),
          component,
          member,
          signature_container: None,
        },
        kind: NamedTopicKind::LocalVariable,
        name: "amount".to_string(),
        visibility: NamedTopicVisibility::Public,
        is_mutable: false,
        mutations: vec![],
        ancestors: vec![],
        descendants: vec![],
        relatives: vec![],
        transitive_topic: None,
        doc_references: vec![],
      },
    );
    // Local: Scope::ContainingBlock { member }
    audit.topic_metadata.insert(
      local,
      TopicMetadata::NamedTopic {
        topic: local,
        scope: Scope::ContainingBlock {
          container: container.clone(),
          component,
          member,
          containing_blocks: vec![],
        },
        kind: NamedTopicKind::LocalVariable,
        name: "tmp".to_string(),
        visibility: NamedTopicVisibility::Public,
        is_mutable: false,
        mutations: vec![],
        ancestors: vec![],
        descendants: vec![],
        relatives: vec![],
        transitive_topic: None,
        doc_references: vec![],
      },
    );
    // Mutated state variable: lives at Component scope, not Member —
    // included via mutations list, not the scope walk.
    audit.topic_metadata.insert(
      state_var,
      TopicMetadata::NamedTopic {
        topic: state_var,
        scope: Scope::Component {
          container: container.clone(),
          component,
        },
        kind: NamedTopicKind::StateVariable(
          domain::VariableMutability::Mutable,
        ),
        name: "balance".to_string(),
        visibility: NamedTopicVisibility::Public,
        is_mutable: true,
        mutations: vec![],
        ancestors: vec![],
        descendants: vec![],
        relatives: vec![],
        transitive_topic: None,
        doc_references: vec![],
      },
    );
    // Unrelated: scoped to a different member entirely — must not appear.
    let other_member = topic::new_node_topic(&200);
    audit.topic_metadata.insert(
      unrelated,
      TopicMetadata::NamedTopic {
        topic: unrelated,
        scope: Scope::Member {
          container,
          component,
          member: other_member,
          signature_container: None,
        },
        kind: NamedTopicKind::LocalVariable,
        name: "other".to_string(),
        visibility: NamedTopicVisibility::Public,
        is_mutable: false,
        mutations: vec![],
        ancestors: vec![],
        descendants: vec![],
        relatives: vec![],
        transitive_topic: None,
        doc_references: vec![],
      },
    );

    audit.function_properties.insert(
      member,
      FunctionModProperties::FunctionProperties {
        reverts: vec![],
        calls: vec![],
        mutations: vec![state_var],
        events_emitted: vec![],
      },
    );

    let value = collect_member_semantics(&member, &audit);
    let map = value.as_object().expect("semantics is a JSON object");
    assert!(map.contains_key(&param.id()), "parameter must appear");
    assert!(map.contains_key(&local.id()), "local must appear");
    assert!(
      map.contains_key(&state_var.id()),
      "mutated state var must appear"
    );
    assert!(
      !map.contains_key(&unrelated.id()),
      "members from a different function must be excluded"
    );
  }

  #[test]
  fn collect_called_function_behaviors_emits_empty_for_out_of_scope() {
    let mut audit = empty_audit();
    let member = topic::new_node_topic(&100);
    let in_scope_callee = topic::new_node_topic(&200);
    let out_of_scope_callee = topic::new_node_topic(&300);
    audit.topic_metadata.insert(
      in_scope_callee,
      TopicMetadata::NamedTopic {
        topic: in_scope_callee,
        scope: Scope::Global,
        kind: NamedTopicKind::Function(crate::domain::FunctionKind::Function),
        name: "_update".to_string(),
        visibility: NamedTopicVisibility::Internal,
        is_mutable: false,
        mutations: vec![],
        ancestors: vec![],
        descendants: vec![],
        relatives: vec![],
        transitive_topic: None,
        doc_references: vec![],
      },
    );
    audit.topic_metadata.insert(
      out_of_scope_callee,
      TopicMetadata::NamedTopic {
        topic: out_of_scope_callee,
        scope: Scope::Global,
        kind: NamedTopicKind::Function(crate::domain::FunctionKind::Function),
        name: "transfer".to_string(),
        visibility: NamedTopicVisibility::External,
        is_mutable: false,
        mutations: vec![],
        ancestors: vec![],
        descendants: vec![],
        relatives: vec![],
        transitive_topic: None,
        doc_references: vec![],
      },
    );
    let beh = topic::new_behavior_topic(1);
    audit.topic_metadata.insert(
      beh,
      TopicMetadata::BehaviorTopic {
        topic: beh,
        description: "updates reserves".to_string(),
        member_topic: in_scope_callee,
        author: crate::collaborator::models::Author::System,
        created_at: None,
      },
    );
    audit.member_behaviors.insert(in_scope_callee, vec![beh]);

    audit.function_properties.insert(
      member,
      FunctionModProperties::FunctionProperties {
        reverts: vec![],
        calls: vec![in_scope_callee, out_of_scope_callee],
        mutations: vec![],
        events_emitted: vec![],
      },
    );

    let value = collect_called_function_behaviors(&member, &audit);
    let map = value
      .as_object()
      .expect("called_function_behaviors is an object");
    assert_eq!(
      map[&in_scope_callee.id()]["behaviors"][0],
      "updates reserves"
    );
    assert_eq!(
      map[&out_of_scope_callee.id()]["behaviors"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(99),
      0,
      "out-of-scope callee gets an empty behaviors array, not omission"
    );
  }
}

#[cfg(test)]
mod batch_render_integration_tests {
  //! Higher-level tests that drive `render_batch_for_behavior_extraction`
  //! and `render_batch_for_functional_properties` end-to-end. These check
  //! the JSON shapes that downstream LLM tasks actually consume — the
  //! `non_pure_subjects` list, the `is_non_pure: true` injection, the
  //! featureless-member skip, and the None-when-empty contract.
  use super::*;
  use crate::domain::{
    self, FunctionKind, FunctionModProperties, NamedTopicKind,
    NamedTopicVisibility, Scope, TopicMetadata, UnnamedTopicKind,
    new_audit_data,
  };
  use std::collections::HashSet;

  fn loc() -> crate::solidity::ast::SourceLocation {
    crate::solidity::ast::SourceLocation {
      start: None,
      length: None,
      index: None,
    }
  }

  fn empty_audit() -> domain::AuditData {
    new_audit_data("test".to_string(), HashSet::new(), None)
  }

  fn install_function(
    audit: &mut domain::AuditData,
    member: topic::Topic,
    name: &str,
    container: domain::ProjectPath,
    component: topic::Topic,
    body_node_id: i32,
    body_statements: Vec<ASTNode>,
  ) -> ASTNode {
    let function_node = ASTNode::FunctionDefinition {
      node_id: member.numeric_id(),
      src_location: loc(),
      implemented: true,
      signature: Box::new(ASTNode::FunctionSignature {
        node_id: member.numeric_id() + 100,
        src_location: loc(),
        documentation: None,
        kind: FunctionKind::Function,
        modifiers: Box::new(ASTNode::ModifierList {
          node_id: member.numeric_id() + 101,
          src_location: loc(),
          modifiers: vec![],
        }),
        name: name.to_string(),
        name_location: loc(),
        declaration_id: member.numeric_id(),
        parameters: Box::new(ASTNode::ParameterList {
          node_id: member.numeric_id() + 102,
          src_location: loc(),
          parameters: vec![],
          is_return_parameters: false,
        }),
        return_parameters: Box::new(ASTNode::ParameterList {
          node_id: member.numeric_id() + 103,
          src_location: loc(),
          parameters: vec![],
          is_return_parameters: true,
        }),
        scope: component.numeric_id(),
        state_mutability:
          crate::solidity::ast::FunctionStateMutability::NonPayable,
        virtual_: false,
        visibility: crate::solidity::ast::FunctionVisibility::External,
        implementation_declaration: None,
      }),
      body: Some(Box::new(ASTNode::Block {
        node_id: body_node_id,
        src_location: loc(),
        statements: body_statements,
      })),
    };
    audit
      .nodes
      .insert(member, domain::Node::Solidity(function_node.clone()));
    audit.topic_metadata.insert(
      member,
      TopicMetadata::NamedTopic {
        topic: member,
        scope: Scope::Component {
          container,
          component,
        },
        kind: NamedTopicKind::Function(FunctionKind::Function),
        name: name.to_string(),
        visibility: NamedTopicVisibility::External,
        is_mutable: false,
        mutations: vec![],
        ancestors: vec![],
        descendants: vec![],
        relatives: vec![],
        transitive_topic: None,
        doc_references: vec![],
      },
    );
    function_node
  }

  fn add_unnamed(
    audit: &mut domain::AuditData,
    id: i32,
    kind: UnnamedTopicKind,
  ) -> topic::Topic {
    let t = topic::new_node_topic(&id);
    audit.topic_metadata.insert(
      t,
      TopicMetadata::UnnamedTopic {
        topic: t,
        scope: Scope::Global,
        kind,
        transitive_topic: None,
      },
    );
    t
  }

  #[test]
  fn functional_property_render_emits_non_pure_subjects_and_flags() {
    // One in-scope function with a state write in its body. The render
    // must:
    //   - emit the state-write topic in the top-level non_pure_subjects
    //     list,
    //   - inject `is_non_pure: true` on that node in the rendered AST,
    //   - skip when the function has no feature link (we install one
    //     below to exercise the happy path).
    let mut audit = empty_audit();
    let container = domain::ProjectPath {
      file_path: "test.sol".to_string(),
    };
    let component = topic::new_node_topic(&1);
    audit.in_scope_files.insert(container.clone());

    let assignment_node_id = 50;
    let assignment_topic = add_unnamed(
      &mut audit,
      assignment_node_id,
      UnnamedTopicKind::VariableMutation,
    );

    let body = vec![ASTNode::Assignment {
      node_id: assignment_node_id,
      src_location: loc(),
      operator: crate::solidity::ast::AssignmentOperator::Assign,
      right_hand_side: Box::new(ASTNode::Literal {
        node_id: 51,
        src_location: loc(),
        hex_value: String::new(),
        kind: crate::solidity::ast::LiteralKind::Number,
        type_descriptions: crate::solidity::ast::TypeDescriptions {
          type_identifier: String::new(),
          type_string: String::new(),
        },
        value: Some("1".to_string()),
      }),
      left_hand_side: Box::new(ASTNode::Identifier {
        node_id: 52,
        src_location: loc(),
        name: "x".to_string(),
        overloaded_declarations: vec![],
        referenced_declaration: 99,
      }),
    }];

    let member = topic::new_node_topic(&100);
    install_function(
      &mut audit, member, "doThing", container, component, 200, body,
    );
    audit.function_properties.insert(
      member,
      FunctionModProperties::FunctionProperties {
        reverts: vec![],
        calls: vec![],
        mutations: vec![],
        events_emitted: vec![],
      },
    );

    // Set up the feature link so the renderer doesn't skip the member.
    let beh = topic::new_behavior_topic(1);
    let feat = topic::new_feature_topic(1);
    audit.topic_metadata.insert(
      beh,
      TopicMetadata::BehaviorTopic {
        topic: beh,
        description: "writes x".to_string(),
        member_topic: member,
        author: crate::collaborator::models::Author::System,
        created_at: None,
      },
    );
    audit.topic_metadata.insert(
      feat,
      TopicMetadata::FeatureTopic {
        topic: feat,
        name: "X".to_string(),
        description: "the X feature".to_string(),
        author: crate::collaborator::models::Author::System,
        created_at: None,
      },
    );
    audit.member_behaviors.insert(member, vec![beh]);
    audit.feature_behavior_links.insert(feat, vec![beh]);

    let rendered = render_batch_for_functional_properties(&[member], &audit)
      .expect("expected at least one renderable subject");

    let value: serde_json::Value =
      serde_json::from_str(&rendered.json).expect("batch JSON parses");

    // non_pure_subjects must list the assignment topic.
    let subjects = value
      .get("non_pure_subjects")
      .and_then(|v| v.as_array())
      .expect("non_pure_subjects array present");
    let subject_ids: Vec<&str> =
      subjects.iter().filter_map(|v| v.as_str()).collect();
    assert!(
      subject_ids.contains(&assignment_topic.id().as_str()),
      "expected non_pure_subjects to contain {}, got {:?}",
      assignment_topic.id(),
      subject_ids
    );

    // The feature must appear on the member object.
    let batch = value
      .get("batch")
      .and_then(|v| v.as_array())
      .expect("batch array present");
    assert_eq!(batch.len(), 1);
    let feat_obj = batch[0]
      .get("feature")
      .expect("feature field present on member");
    assert_eq!(feat_obj["name"], "X");

    // Behaviors are injected on the member.
    let behaviors = batch[0]
      .get("behaviors")
      .and_then(|v| v.as_array())
      .expect("behaviors field present on member");
    assert_eq!(behaviors[0], "writes x");

    // The is_non_pure flag must be set on the assignment node somewhere
    // in the rendered definition AST.
    let definition = batch[0]
      .get("definition")
      .expect("definition field present");
    assert!(
      contains_is_non_pure(definition, &assignment_topic.id()),
      "expected `is_non_pure: true` on node with id {} in definition",
      assignment_topic.id()
    );
  }

  /// Recursively search a JSON value for any object whose `id` matches
  /// `target_id` and that has `is_non_pure: true`. Used to assert the
  /// renderer's flag injection without depending on AST shape.
  fn contains_is_non_pure(value: &serde_json::Value, target_id: &str) -> bool {
    match value {
      serde_json::Value::Object(map) => {
        if map.get("id").and_then(|v| v.as_str()) == Some(target_id)
          && map.get("is_non_pure").and_then(|v| v.as_bool()) == Some(true)
        {
          return true;
        }
        map.values().any(|v| contains_is_non_pure(v, target_id))
      }
      serde_json::Value::Array(arr) => {
        arr.iter().any(|v| contains_is_non_pure(v, target_id))
      }
      _ => false,
    }
  }

  #[test]
  fn functional_property_render_returns_none_for_pure_only_member() {
    // A function with no non-pure subjects in its body should produce
    // None — the LLM has nothing to ask about.
    let mut audit = empty_audit();
    let container = domain::ProjectPath {
      file_path: "test.sol".to_string(),
    };
    let component = topic::new_node_topic(&1);
    audit.in_scope_files.insert(container.clone());

    let member = topic::new_node_topic(&100);
    install_function(
      &mut audit,
      member,
      "purely",
      container,
      component,
      200,
      vec![],
    );
    audit.function_properties.insert(
      member,
      FunctionModProperties::FunctionProperties {
        reverts: vec![],
        calls: vec![],
        mutations: vec![],
        events_emitted: vec![],
      },
    );

    // Set up a feature link so the skip is purity-driven, not feature-
    // driven.
    let beh = topic::new_behavior_topic(1);
    let feat = topic::new_feature_topic(1);
    audit.topic_metadata.insert(
      beh,
      TopicMetadata::BehaviorTopic {
        topic: beh,
        description: "does nothing".to_string(),
        member_topic: member,
        author: crate::collaborator::models::Author::System,
        created_at: None,
      },
    );
    audit.topic_metadata.insert(
      feat,
      TopicMetadata::FeatureTopic {
        topic: feat,
        name: "F".to_string(),
        description: "f".to_string(),
        author: crate::collaborator::models::Author::System,
        created_at: None,
      },
    );
    audit.member_behaviors.insert(member, vec![beh]);
    audit.feature_behavior_links.insert(feat, vec![beh]);

    let rendered = render_batch_for_functional_properties(&[member], &audit);
    assert!(
      rendered.is_none(),
      "pure-only batch must produce no rendered work"
    );
  }

  #[test]
  fn functional_property_render_returns_none_when_no_feature_link() {
    // A function with non-pure subjects but no feature link — renderer
    // skips the member, leaving no eligible work in the batch.
    let mut audit = empty_audit();
    let container = domain::ProjectPath {
      file_path: "test.sol".to_string(),
    };
    let component = topic::new_node_topic(&1);
    audit.in_scope_files.insert(container.clone());

    let member = topic::new_node_topic(&100);
    let assignment_node_id = 50;
    add_unnamed(
      &mut audit,
      assignment_node_id,
      UnnamedTopicKind::VariableMutation,
    );
    let body = vec![ASTNode::Assignment {
      node_id: assignment_node_id,
      src_location: loc(),
      operator: crate::solidity::ast::AssignmentOperator::Assign,
      right_hand_side: Box::new(ASTNode::Literal {
        node_id: 51,
        src_location: loc(),
        hex_value: String::new(),
        kind: crate::solidity::ast::LiteralKind::Number,
        type_descriptions: crate::solidity::ast::TypeDescriptions {
          type_identifier: String::new(),
          type_string: String::new(),
        },
        value: Some("1".to_string()),
      }),
      left_hand_side: Box::new(ASTNode::Identifier {
        node_id: 52,
        src_location: loc(),
        name: "x".to_string(),
        overloaded_declarations: vec![],
        referenced_declaration: 99,
      }),
    }];
    install_function(
      &mut audit, member, "doThing", container, component, 200, body,
    );
    audit.function_properties.insert(
      member,
      FunctionModProperties::FunctionProperties {
        reverts: vec![],
        calls: vec![],
        mutations: vec![],
        events_emitted: vec![],
      },
    );
    // Note: no behavior, no feature link.

    let rendered = render_batch_for_functional_properties(&[member], &audit);
    assert!(
      rendered.is_none(),
      "featureless member must skip and produce no batch"
    );
  }

  #[test]
  fn behavior_render_returns_none_when_all_members_unresolvable() {
    // Members not in audit_data.nodes can't be rendered. The batch
    // render must return None rather than emitting an empty batch.
    let audit = empty_audit();
    let phantom = topic::new_node_topic(&999);
    let rendered = render_batch_for_behavior_extraction(&[phantom], &audit);
    assert!(rendered.is_none());
  }
}

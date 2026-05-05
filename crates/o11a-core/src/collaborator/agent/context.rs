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

/// Controls which parts of the AST tree are expanded vs. stubbed.
struct ASTRenderContext {
  /// The topic the agent requested context for.
  /// Used to decide whether to expand function bodies.
  target_topic: topic::Topic,
  /// When true, function/modifier bodies are omitted.
  /// Set to true when converting ContractDefinition members.
  omit_function_and_modifier_bodies: bool,
  /// Whether source-derived (untrusted) comments — inline `//` dev comments
  /// and NatSpec docstrings — should appear in the rendered output. Set to
  /// `false` when the rendering feeds an agent task that must operate only on
  /// trusted, pipeline-generated content (behavior extraction, where only
  /// `functional_semantics` annotations are trusted). Set to `true` when the
  /// developer's prose is useful context (semantic linking, topic views).
  /// Auditor-authored `Info` comments are always included regardless.
  include_untrusted_comments: bool,
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

fn lookup_doc_node_comments(
  node_id: i32,
  audit_data: &AuditData,
  include_untrusted: bool,
) -> Vec<String> {
  let doc_topic = topic::new_documentation_topic(node_id);
  lookup_topic_comments(&doc_topic, audit_data, include_untrusted)
}

/// Build a JSON object for a node, attaching comments if present.
fn make_node_json(
  mut obj: serde_json::Value,
  comments: Vec<String>,
) -> serde_json::Value {
  if !comments.is_empty() {
    obj["comments"] = json!(comments);
  }
  obj
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
    return make_node_json(
      json!({
        "type": "topic_ref",
        "id": topic.id(),
        "name": name,
      }),
      comments,
    );
  }

  let node_id = resolved.node_id();
  let id = topic::new_node_topic(&node_id).id().to_string();
  let comments = lookup_node_comments(
    node_id,
    audit_data,
    render_ctx.include_untrusted_comments,
  );

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

  make_node_json(obj, comments)
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

  make_node_json(obj, comments)
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
    | TopicMetadata::ThreatTopic { .. }
    | TopicMetadata::InvariantTopic { .. } => {
      let kind = match metadata {
        TopicMetadata::FeatureTopic { .. } => "Feature",
        TopicMetadata::RequirementTopic { .. } => "Requirement",
        TopicMetadata::BehaviorTopic { .. } => "Behavior",
        TopicMetadata::FunctionalSemanticTopic { .. } => "Semantic",
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
// Behavior Extraction: Contract rendering with semantics
// ============================================================================

/// A pre-rendered contract with full member bodies and functional semantics
/// for behavior extraction.
pub struct ContractForBehaviorExtraction {
  pub contract_topic: topic::Topic,
  pub contract_name: String,
  pub json: String,
}

/// Render a contract's members with full bodies and functional semantics
/// annotated on declarations, for the behavior extraction LLM task.
pub fn render_contract_for_behavior_extraction(
  contract_node: &ASTNode,
  audit_data: &AuditData,
) -> Option<ContractForBehaviorExtraction> {
  let name = match contract_node {
    ASTNode::ContractDefinition { signature, .. } => {
      let resolved_sig = signature.resolve(&audit_data.nodes);
      match resolved_sig {
        ASTNode::ContractSignature { name, .. } => name.clone(),
        _ => {
          let ct = topic::new_node_topic(&contract_node.node_id());
          audit_data
            .topic_metadata
            .get(&ct)
            .and_then(|m| m.name())
            .unwrap_or("unknown")
            .to_string()
        }
      }
    }
    _ => return None,
  };

  let members = contract_members(contract_node);

  let contract_topic = topic::new_node_topic(&contract_node.node_id());

  // Render with bodies included. Behavior extraction runs against trusted,
  // pipeline-generated content only — `functional_semantics` below is the
  // trusted channel. The developer's own inline comments and NatSpec must
  // not leak into this render.
  let render_ctx = ASTRenderContext {
    target_topic: contract_topic,
    omit_function_and_modifier_bodies: false,
    include_untrusted_comments: false,
  };

  // Build semantic annotations for declarations in this contract
  let mut semantics: Vec<serde_json::Value> = Vec::new();
  for (decl_topic, sem_topics) in &audit_data.declaration_semantics {
    // Check if this declaration is scoped to this contract
    let Some(metadata) = audit_data.topic_metadata.get(decl_topic) else {
      continue;
    };
    let in_contract = match metadata.scope() {
      domain::Scope::Component { component, .. } => {
        *component == contract_topic
      }
      domain::Scope::Member { component, .. } => *component == contract_topic,
      domain::Scope::ContainingBlock { component, .. } => {
        *component == contract_topic
      }
      _ => false,
    };
    if !in_contract {
      continue;
    }
    let name = metadata.name().unwrap_or("");
    for sem_topic in sem_topics {
      if let Some(domain::TopicMetadata::FunctionalSemanticTopic {
        description,
        ..
      }) = audit_data.topic_metadata.get(sem_topic)
      {
        semantics.push(json!({
          "declaration_topic": decl_topic.id(),
          "name": name,
          "semantic": description,
        }));
      }
    }
  }

  // Filter to functions and modifiers, excluding transitive members (e.g.,
  // interface functions with an in-scope implementation). Behaviors for
  // those will be extracted from the implementation contract instead.
  let member_snippets: Vec<serde_json::Value> = members
    .iter()
    .filter(|m| {
      let resolved = m.resolve(&audit_data.nodes);
      if !matches!(
        resolved,
        ASTNode::FunctionDefinition { .. } | ASTNode::ModifierDefinition { .. }
      ) {
        return false;
      }
      let member_topic = topic::new_node_topic(&resolved.node_id());
      audit_data
        .topic_metadata
        .get(&member_topic)
        .and_then(|m| m.transitive_topic())
        .is_none()
    })
    .map(|m| render_solidity_ast_snippet(m, &render_ctx, audit_data))
    .collect();

  let obj = json!({
    "contract_topic": contract_topic.id(),
    "name": name,
    "members": member_snippets,
    "functional_semantics": semantics,
  });

  Some(ContractForBehaviorExtraction {
    contract_topic,
    contract_name: name,
    json: serde_json::to_string(&obj).unwrap_or_default(),
  })
}

/// Collect all contracts rendered for behavior extraction.
pub fn collect_contracts_for_behavior_extraction(
  audit_data: &AuditData,
) -> Vec<ContractForBehaviorExtraction> {
  let mut results = Vec::new();
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
        if !audit_data.in_scope_files.contains(path) {
          continue;
        }

        if let Some(rendered) =
          render_contract_for_behavior_extraction(resolved, audit_data)
        {
          results.push(rendered);
        }
      }
    }
  }
  results
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

/// Step 4 (member-scoped batch) — render the signature source for each
/// member (function/modifier body stripped). Used as the disambiguation
/// payload alongside `render_member_signature_declarations_for_semantics`.
pub fn render_member_signature_sources_for_semantics(
  member_topics: &[topic::Topic],
  audit_data: &AuditData,
) -> String {
  let mut parts = Vec::new();
  for mt in member_topics {
    if let Some(source) = render_member_signature_for_semantics(mt, audit_data)
    {
      parts.push(source);
    }
  }
  parts.join("\n\n")
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

/// Step 4 (contract-scoped batch) — render the non-function members of each
/// contract as Solidity signatures (state-variable declarations, event
/// signatures, struct/enum definitions). Pairs with
/// `render_contract_level_declarations_for_semantics` as the disambiguation
/// payload.
pub fn render_contract_level_signatures_for_semantics(
  contract_topics: &[topic::Topic],
  audit_data: &AuditData,
) -> String {
  let mut parts = Vec::new();
  for ct in contract_topics {
    let sigs = render_contract_declaration_signatures(ct, audit_data);
    if sigs != "[]" && !sigs.is_empty() {
      parts.push(sigs);
    }
  }
  parts.join("\n\n")
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

/// Step 5 — render the full body source for each member (function/modifier
/// body included). Pairs with
/// `render_member_body_local_declarations_for_semantics` as the
/// disambiguation payload.
pub fn render_member_body_sources_for_semantics(
  member_topics: &[topic::Topic],
  audit_data: &AuditData,
) -> String {
  let mut parts = Vec::new();
  for mt in member_topics {
    if let Some(source) = render_member_source_for_semantics(mt, audit_data) {
      parts.push(source);
    }
  }
  parts.join("\n\n")
}

/// Render the prior-step semantic links relevant to the current synthesis
/// batch. `topics` is the set of declaration topics whose accumulated
/// semantics should be surfaced (e.g., for step 4: the containing contracts
/// of every member in the batch; for step 5: every topic that lives within
/// any of those contracts — see `topics_within_contracts`). After per-step
/// condensation, each topic has at most one link.
///
/// Lines are emitted in deterministic order (sorted by declaration topic)
/// so prompt text is stable across runs. Returns an empty string when no
/// relevant prior semantics exist, so callers can use `if !block.is_empty()`
/// to decide whether to emit the "Previously derived semantics" prompt
/// section.
pub fn render_prior_semantics_block(
  topics: &[topic::Topic],
  links: &[domain::SemanticLink],
  audit_data: &AuditData,
) -> String {
  use std::collections::HashSet;
  let topic_set: HashSet<&topic::Topic> = topics.iter().collect();
  let mut filtered: Vec<&domain::SemanticLink> = links
    .iter()
    .filter(|link| topic_set.contains(&link.declaration_topic))
    .collect();
  filtered.sort_by_key(|l| l.declaration_topic);
  let mut lines: Vec<String> = Vec::with_capacity(filtered.len());
  for link in filtered {
    let name = audit_data
      .topic_metadata
      .get(&link.declaration_topic)
      .and_then(|m| m.name())
      .unwrap_or("");
    lines.push(format!(
      "- {} ({}): {}",
      link.declaration_topic.id(),
      name,
      link.description
    ));
  }
  lines.join("\n")
}

/// Returns every topic that "belongs to" any of the listed contracts —
/// the contracts themselves plus everything in their `Component` /
/// `Member` / `ContainingBlock` scope, including struct fields and enum
/// members reached through one parent hop.
///
/// Used by step 5 to assemble the prior-semantics topic set: any topic
/// step 2 or step 4 may have produced a semantic for that's reachable
/// from a body local should appear in the step 5 prompt's prior-context
/// block.
pub fn topics_within_contracts(
  contract_topics: &[topic::Topic],
  audit_data: &AuditData,
) -> Vec<topic::Topic> {
  use std::collections::HashSet;
  let contract_set: HashSet<&topic::Topic> = contract_topics.iter().collect();
  let mut out: Vec<topic::Topic> = contract_topics.to_vec();
  for (decl_topic, metadata) in &audit_data.topic_metadata {
    if contract_set.contains(decl_topic) {
      continue;
    }
    let component = match metadata.scope() {
      domain::Scope::Component { component, .. }
      | domain::Scope::Member { component, .. }
      | domain::Scope::ContainingBlock { component, .. } => component,
      _ => continue,
    };
    if contract_set.contains(component) {
      out.push(*decl_topic);
      continue;
    }
    // Struct field / enum member: walk one parent hop. The decl's scope
    // points at the struct/enum; that struct/enum's scope points at the
    // contract.
    if let Some(parent_scope) = audit_data
      .topic_metadata
      .get(component)
      .map(TopicMetadata::scope)
    {
      let grandparent = match parent_scope {
        domain::Scope::Component { component, .. }
        | domain::Scope::Member { component, .. }
        | domain::Scope::ContainingBlock { component, .. } => component,
        _ => continue,
      };
      if contract_set.contains(grandparent) {
        out.push(*decl_topic);
      }
    }
  }
  out
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

/// Render a member's full source code as a JSON snippet for synthesis-step
/// disambiguation context (currently used by step 5).
pub fn render_member_source_for_semantics(
  member_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Option<String> {
  // Find the AST node for this member
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
            let render_ctx = ASTRenderContext {
              target_topic: *member_topic,
              omit_function_and_modifier_bodies: false,
              include_untrusted_comments: true,
            };
            let rendered = render_solidity_ast_snippet(
              resolved_member,
              &render_ctx,
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

/// Like `render_member_source_for_semantics` but with function/modifier
/// bodies stripped — useful for BM25 corpora that want signatures only.
pub fn render_member_signature_for_semantics(
  member_topic: &topic::Topic,
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
            let render_ctx = ASTRenderContext {
              target_topic: *member_topic,
              omit_function_and_modifier_bodies: true,
              include_untrusted_comments: true,
            };
            let rendered = render_solidity_ast_snippet(
              resolved_member,
              &render_ctx,
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

/// Render contract-scoped declaration signatures as source context for the
/// step 4 contract-scoped batch. Returns a compact JSON of state-variable
/// declarations, event/error signatures, struct/enum definitions — i.e.,
/// the non-function members of the contract.
pub fn render_contract_declaration_signatures(
  contract_topic: &topic::Topic,
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
        let render_ctx = ASTRenderContext {
          target_topic: *contract_topic,
          omit_function_and_modifier_bodies: true,
          include_untrusted_comments: true,
        };
        // Filter to non-function/modifier members (state vars, events, structs, etc.)
        // Resolve each member before checking its type
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
          .map(|n| render_solidity_ast_snippet(n, &render_ctx, audit_data))
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
      .insert(group_topic.clone(), Node::Solidity(group_node));

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

    let rendered =
      render_contract_for_behavior_extraction(&contract_node, &audit_data)
        .expect("behavior extraction returned None");

    let value: serde_json::Value = serde_json::from_str(&rendered.json)
      .expect("behavior extraction produced invalid JSON");
    let members = value
      .get("members")
      .and_then(|m| m.as_array())
      .expect("members field missing");
    assert_eq!(members.len(), 1, "expected one rendered function member");

    let comments: Vec<&str> = members[0]
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

    let semantics = value
      .get("functional_semantics")
      .and_then(|s| s.as_array())
      .expect("functional_semantics field missing");
    assert!(
      semantics.iter().any(|s| s
        .get("semantic")
        .and_then(|v| v.as_str())
        .is_some_and(|text| text.contains("admin role"))),
      "expected trusted semantic annotation on function, got: {:?}",
      semantics
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
    self, ContractKind, FunctionKind, MatchSource, NamedTopicKind,
    NamedTopicVisibility, Scope, SemanticLink, TopicMetadata, new_audit_data,
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

  #[test]
  fn topics_within_contracts_walks_member_and_block_scope() {
    let (audit, (vault, transfer, to, result, temp, balance, _, _)) =
      build_vault_audit();
    let topics = topics_within_contracts(&[vault], &audit);
    let set: HashSet<Topic> = topics.into_iter().collect();
    // Contract itself, plus everything scoped within it.
    assert!(set.contains(&vault));
    assert!(set.contains(&transfer));
    assert!(set.contains(&to));
    assert!(set.contains(&result));
    assert!(set.contains(&temp));
    assert!(set.contains(&balance));
  }

  #[test]
  fn topics_within_contracts_walks_struct_field_one_hop() {
    // Vault contract holds a struct Receipt { uint256 amount; }. The
    // amount field's scope's component is the struct, whose component is
    // the contract. The 1-hop walk should reach the contract.
    let mut audit = new_audit_data("test".to_string(), HashSet::new(), None);
    let path = project_path("Vault.sol");
    let vault = topic::new_node_topic(&100);
    let receipt = topic::new_node_topic(&140);
    let receipt_amount = topic::new_node_topic(&141);

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
      receipt,
      "Receipt",
      NamedTopicKind::Struct,
      NamedTopicVisibility::Public,
      Scope::Component {
        container: path.clone(),
        component: vault,
      },
    );
    insert_named(
      &mut audit,
      receipt_amount,
      "amount",
      NamedTopicKind::LocalVariable,
      NamedTopicVisibility::Internal,
      Scope::Component {
        container: path.clone(),
        component: receipt,
      },
    );

    let topics = topics_within_contracts(&[vault], &audit);
    let set: HashSet<Topic> = topics.into_iter().collect();
    assert!(
      set.contains(&receipt_amount),
      "struct field should roll up to its enclosing contract"
    );
  }

  #[test]
  fn prior_semantics_block_filters_and_sorts_deterministically() {
    let (audit, (vault, transfer, to, _result, _temp, _balance, _, _)) =
      build_vault_audit();
    // Build links in deliberately non-sorted order so we exercise sorting.
    let links = vec![
      SemanticLink {
        documentation_topics: vec![],
        declaration_topic: transfer,
        description: "atomic balance transfer".to_string(),
        match_source: MatchSource::Mechanical,
      },
      SemanticLink {
        documentation_topics: vec![],
        declaration_topic: vault,
        description: "the canonical staking vault".to_string(),
        match_source: MatchSource::Mechanical,
      },
      // `to` is in the audit but NOT requested in the topic filter — must
      // be excluded.
      SemanticLink {
        documentation_topics: vec![],
        declaration_topic: to,
        description: "irrelevant".to_string(),
        match_source: MatchSource::Bm25,
      },
    ];
    let block =
      render_prior_semantics_block(&[vault, transfer], &links, &audit);
    let lines: Vec<&str> = block.lines().collect();
    assert_eq!(lines.len(), 2, "the unrequested `to` link must be excluded");
    // Deterministic sort by declaration topic — Topic ordering is by
    // numeric_id, so vault (100) precedes transfer (110).
    assert!(lines[0].contains("Vault"));
    assert!(lines[0].contains("the canonical staking vault"));
    assert!(lines[1].contains("transfer"));
    assert!(lines[1].contains("atomic balance transfer"));
  }

  #[test]
  fn prior_semantics_block_returns_empty_string_when_no_matches() {
    let (audit, (vault, _, _, _, _, _, _, _)) = build_vault_audit();
    let links = vec![SemanticLink {
      documentation_topics: vec![],
      declaration_topic: topic::new_node_topic(&999),
      description: "orphan".to_string(),
      match_source: MatchSource::Mechanical,
    }];
    let block = render_prior_semantics_block(&[vault], &links, &audit);
    assert!(block.is_empty());
  }
}

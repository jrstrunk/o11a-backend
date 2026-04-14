use std::collections::HashSet;

use serde::Serialize;
use serde_json::json;

use crate::collaborator::formatter as comment_formatter;
use crate::core::{
  self, AuditData, BlockAnnotationKind, ContractKind, ControlFlowStatementKind,
  FunctionKind, NamedTopicKind, NamedTopicVisibility, Node, Reference,
  SourceChild, SourceContext, TitledTopicKind, TopicMetadata, UnnamedTopicKind,
  VariableMutability, topic,
};

use crate::documentation::parser::DocumentationNode;
use crate::solidity::parser::ASTNode;

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

/// A source child is a raw JSON value — either an AST snippet (for
/// references) or an annotated block wrapper.

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
      comment_type.clone()
    }
    Some(TopicMetadata::FeatureTopic { name, .. }) => name.clone(),
    Some(TopicMetadata::RequirementTopic { description, .. }) => {
      description.clone()
    }
    Some(TopicMetadata::BehaviorTopic { description, .. }) => {
      description.clone()
    }
    Some(TopicMetadata::ThreatTopic { description, .. }) => {
      description.clone()
    }
    Some(TopicMetadata::InvariantTopic { description, .. }) => {
      description.clone()
    }
    Some(TopicMetadata::DocumentationTopic { .. }) => {
      topic.id().to_string()
    }
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
    TopicMetadata::CommentTopic { comment_type, .. } => comment_type.clone(),
    TopicMetadata::FeatureTopic { name, .. } => name.clone(),
    TopicMetadata::RequirementTopic { description, .. } => description.clone(),
    TopicMetadata::BehaviorTopic { description, .. } => description.clone(),
    TopicMetadata::ThreatTopic { description, .. } => description.clone(),
    TopicMetadata::InvariantTopic { description, .. } => description.clone(),
    TopicMetadata::DocumentationTopic { .. } => {
      metadata.topic().id().to_string()
    }
  }
}

/// Build an `AgentScopeTitle` for a topic: plaintext name, topic id, and
/// any info comments targeting that topic.
fn build_scope_title(
  topic: &topic::Topic,
  audit_data: &AuditData,
) -> AgentScopeTitle {
  let name = plaintext_name(topic, audit_data);
  let comments = lookup_topic_comments(topic, audit_data);
  AgentScopeTitle {
    name,
    topic: topic.id().to_string(),
    comments,
  }
}

/// Look up info comments targeting a topic from the CommentIndex.
fn lookup_topic_comments(
  topic: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<String> {
  let comment_topics = audit_data
    .comment_index
    .get(topic)
    .map(|v| v.as_slice())
    .unwrap_or(&[]);
  comment_topics
    .iter()
    .filter_map(|comment_topic| {
      let is_info = matches!(audit_data.topic_metadata.get(comment_topic),
        Some(TopicMetadata::CommentTopic { comment_type, .. })
          if comment_type == "info"
      );
      if !is_info {
        return None;
      }
      let content = match audit_data.nodes.get(comment_topic) {
        Some(Node::Comment(nodes)) => {
          comment_formatter::render_comment_plain_text(nodes)
        }
        _ => return None,
      };
      let content = content.trim().to_string();
      if content.is_empty() {
        return None;
      }
      Some(content)
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
  source_text_cache: &std::collections::HashMap<String, String>,
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
        target_topic: target_topic.clone(),
        omit_function_and_modifier_bodies: false,
      };
      Some(render_solidity_ast_snippet(
        node,
        &render_ctx,
        audit_data,
        source_text_cache,
      ))
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
    BlockAnnotationKind::If(core::ControlFlowBranch::True) => "if_true",
    BlockAnnotationKind::If(core::ControlFlowBranch::False) => "if_false",
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

/// Look up info comments targeting a node from the CommentIndex.
fn lookup_node_comments(node_id: i32, audit_data: &AuditData) -> Vec<String> {
  let node_topic = topic::new_node_topic(&node_id);
  lookup_topic_comments(&node_topic, audit_data)
}

fn lookup_doc_node_comments(
  node_id: i32,
  audit_data: &AuditData,
) -> Vec<String> {
  let doc_topic = topic::new_documentation_topic(node_id);
  lookup_topic_comments(&doc_topic, audit_data)
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
  source_text_cache: &std::collections::HashMap<String, String>,
) -> serde_json::Value {
  let resolved = node.resolve(&audit_data.nodes);

  // Unresolved stub → TopicRef
  if let ASTNode::Stub { node_id, topic, .. } = resolved {
    let name = resolve_topic_name(topic, audit_data);
    let comments = lookup_node_comments(*node_id, audit_data);
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
  let comments = lookup_node_comments(node_id, audit_data);

  // Helper closure for recursive conversion
  let recurse = |child: &ASTNode| -> serde_json::Value {
    render_solidity_ast_snippet(
      child,
      render_ctx,
      audit_data,
      source_text_cache,
    )
  };

  // Flatten comment-less SemanticBlocks when rendering statement lists
  let recurse_statements = |stmts: &[ASTNode]| -> Vec<serde_json::Value> {
    stmts
      .iter()
      .flat_map(|s| {
        let resolved_s = s.resolve(&audit_data.nodes);
        if let ASTNode::SemanticBlock { statements, .. } = resolved_s {
          let node_id = resolved_s.node_id();
          let comments = lookup_node_comments(node_id, audit_data);
          if comments.is_empty() {
            // Flatten: recurse into the inner statements directly
            return statements
              .iter()
              .map(|inner| {
                render_solidity_ast_snippet(
                  inner,
                  render_ctx,
                  audit_data,
                  source_text_cache,
                )
              })
              .collect::<Vec<_>>();
          }
        }
        vec![render_solidity_ast_snippet(
          s,
          render_ctx,
          audit_data,
          source_text_cache,
        )]
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
        return vec![render_solidity_ast_snippet(
          body,
          render_ctx,
          audit_data,
          source_text_cache,
        )];
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
      "arguments": arguments.iter().map(|a| recurse(a)).collect::<Vec<_>>(),
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
      "arguments": arguments.iter().map(|a| recurse(a)).collect::<Vec<_>>(),
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
      "components": components.iter().map(|c| recurse(c)).collect::<Vec<_>>(),
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
        "declarations": declarations.iter().map(|d| recurse(d)).collect::<Vec<_>>(),
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
        target_topic: render_ctx.target_topic.clone(),
        omit_function_and_modifier_bodies: true,
      };
      let members: Vec<serde_json::Value> = nodes
        .iter()
        .map(|n| {
          render_solidity_ast_snippet(
            n,
            &member_ctx,
            audit_data,
            source_text_cache,
          )
        })
        .collect();

      json!({
        "type": "contract_definition",
        "id": id,
        "name": name,
        "kind": kind,
        "signature": render_solidity_ast_snippet(signature, render_ctx, audit_data, source_text_cache),
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

      let sig_json = render_solidity_ast_snippet(
        signature,
        render_ctx,
        audit_data,
        source_text_cache,
      );

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

      let sig_json = render_solidity_ast_snippet(
        signature,
        render_ctx,
        audit_data,
        source_text_cache,
      );

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
      "members": members.iter().map(|m| recurse(m)).collect::<Vec<_>>(),
    }),

    ASTNode::EnumDefinition { name, members, .. } => json!({
      "type": "enum_definition",
      "id": id,
      "name": name,
      "members": members.iter().map(|m| recurse(m)).collect::<Vec<_>>(),
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
        obj["base_contracts"] = json!(
          base_contracts
            .iter()
            .map(|b| recurse(b))
            .collect::<Vec<_>>()
        );
      }
      if !directives.is_empty() {
        obj["directives"] =
          json!(directives.iter().map(|d| recurse(d)).collect::<Vec<_>>());
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
      "parameters": parameters.iter().map(|p| recurse(p)).collect::<Vec<_>>(),
    }),

    ASTNode::ModifierList { modifiers, .. } => json!({
      "type": "modifier_list",
      "id": id,
      "modifiers": modifiers.iter().map(|m| recurse(m)).collect::<Vec<_>>(),
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
        obj["arguments"] =
          json!(args.iter().map(|a| recurse(a)).collect::<Vec<_>>());
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
      "options": options.iter().map(|o| recurse(o)).collect::<Vec<_>>(),
    }),

    ASTNode::IndexRangeAccess { nodes, body, .. } => {
      let mut obj = json!({
        "type": "index_range_access",
        "id": id,
        "nodes": nodes.iter().map(|n| recurse(n)).collect::<Vec<_>>(),
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
      "clauses": clauses.iter().map(|c| recurse(c)).collect::<Vec<_>>(),
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
      "nodes": nodes.iter().map(|n| recurse(n)).collect::<Vec<_>>(),
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
            if let Some(sems) = audit_data.functional_semantics.get(t) {
              if let Some(sem) = sems.first() {
                text.push_str(" (");
                text.push_str(&sem.text);
                if sems.len() > 1 {
                  for s in &sems[1..] {
                    text.push_str("; ");
                    text.push_str(&s.text);
                  }
                }
                text.push(')');
              }
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
          if let Some(sems) = audit_data.functional_semantics.get(t) {
            let joined: Vec<&str> = sems.iter().map(|s| s.text.as_str()).collect();
            if !joined.is_empty() {
              text.push_str(" (");
              text.push_str(&joined.join("; "));
              text.push(')');
            }
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
    let comments = lookup_doc_node_comments(*node_id, audit_data);
    return make_node_json(
      json!({"type": "topic_ref", "id": topic.id(), "name": name}),
      comments,
    );
  }

  let node_id = resolved.node_id();
  let id = topic::new_documentation_topic(node_id).id().to_string();
  let comments = lookup_doc_node_comments(node_id, audit_data);

  let recurse = |child: &DocumentationNode,
                 ctx: Option<&DocRenderContext>|
   -> serde_json::Value {
    render_documentation_ast_snippet(child, audit_data, ctx)
  };

  let render_children =
    |children: &[DocumentationNode],
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
          if let Some(sems) = audit_data.functional_semantics.get(t) {
            let joined: Vec<&str> = sems.iter().map(|s| s.text.as_str()).collect();
            if !joined.is_empty() {
              semantic_suffix = format!(" ({})", joined.join("; "));
            }
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

    DocumentationNode::Image { alt, .. } => return json!(format!("[image: {}]", alt)),
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
  source_text_cache: &std::collections::HashMap<String, String>,
) -> Vec<AgentSourceGroup> {
  groups
    .iter()
    .map(|group| {
      convert_source_group(group, target_topic, audit_data, source_text_cache)
    })
    .collect()
}

fn convert_source_group(
  group: &SourceContext,
  target_topic: &topic::Topic,
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) -> AgentSourceGroup {
  let scope = build_scope_title(group.scope(), audit_data);

  let scope_references = group
    .scope_references()
    .iter()
    .map(|r| convert_reference(r, target_topic, audit_data, source_text_cache))
    .collect();

  let nested_references = group
    .nested_references()
    .iter()
    .map(|nested| {
      let subscope = build_scope_title(nested.subscope(), audit_data);
      let children = convert_source_children(
        nested.children(),
        target_topic,
        audit_data,
        source_text_cache,
      );
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
  source_text_cache: &std::collections::HashMap<String, String>,
) -> Vec<serde_json::Value> {
  children
    .iter()
    .flat_map(|child| match child {
      SourceChild::Reference(reference) => {
        let snippet = convert_reference(
          reference,
          target_topic,
          audit_data,
          source_text_cache,
        );
        // Flatten comment-less semantic blocks
        if snippet.get("kind").and_then(|v| v.as_str()) == Some("semantic")
          && snippet.get("type").and_then(|v| v.as_str()) == Some("block")
          && snippet.get("comments").is_none()
        {
          if let Some(stmts) =
            snippet.get("statements").and_then(|v| v.as_array())
          {
            return stmts.clone();
          }
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
          source_text_cache,
        );
        let children = convert_source_children(
          block.children(),
          target_topic,
          audit_data,
          source_text_cache,
        );
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
  source_text_cache: &std::collections::HashMap<String, String>,
) -> serde_json::Value {
  let ref_topic = reference.reference_topic();

  let mut snippet = match audit_data.nodes.get(ref_topic) {
    Some(Node::Solidity(solidity_node)) => {
      let render_ctx = ASTRenderContext {
        target_topic: target_topic.clone(),
        omit_function_and_modifier_bodies: false,
      };
      render_solidity_ast_snippet(
        solidity_node,
        &render_ctx,
        audit_data,
        source_text_cache,
      )
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
          if comment_type == "info"
      );
      if !is_info {
        continue;
      }
      let content = match audit_data.nodes.get(mention_topic) {
        Some(Node::Comment(nodes)) => {
          comment_formatter::render_comment_plain_text(nodes)
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
  scope: &core::Scope,
  audit_data: &AuditData,
) -> Vec<AgentSourceGroup> {
  let ancestors = scope.ancestor_topics();

  // Find the outermost ancestor that is a documentation section.
  // Ancestors are ordered [component, member, ...containing_blocks].
  let root_ancestor = ancestors.iter().find(|t| {
    matches!(
      t.kind(),
      Some(topic::TopicKind::Documentation)
    ) && matches!(
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
      let rendered =
        render_documentation_ast_snippet(node, audit_data, None);
      let scope_title = build_scope_title(topic, audit_data);
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
        t.underlying_id().ok()
      } else {
        None
      }
    })
    .collect();

  let target_node_id = match topic.underlying_id() {
    Ok(id) => id,
    Err(_) => return vec![],
  };

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

  let scope_title = build_scope_title(root_ancestor, audit_data);
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
  source_text_cache: &std::collections::HashMap<String, String>,
  include_expanded_context: bool,
) -> Option<AgentTopicContext> {
  let topic = topic::new_topic(topic_id);
  let metadata = audit_data.topic_metadata.get(&topic)?;

  let topic_id_string = topic_id.to_string();
  let name = resolve_topic_name(&topic, audit_data);

  let empty_ctx: Vec<crate::core::SourceContext> = vec![];
  let topic_ctx = audit_data.topic_context.get(&topic).unwrap_or(&empty_ctx);
  let context = convert_source_groups(
    topic_ctx,
    &topic,
    audit_data,
    source_text_cache,
  );
  let mentions: Vec<String> = audit_data
    .mentions_index
    .get(&topic)
    .map(|topics| topics.iter().map(|t| t.id.clone()).collect())
    .unwrap_or_default();

  match metadata {
    TopicMetadata::NamedTopic { kind, .. } => {
      let (kind_str, sub_kind) = named_kind_to_string(kind);

      let expanded = if include_expanded_context {
        Some(convert_source_groups(
          metadata.expanded_context(),
          &topic,
          audit_data,
          source_text_cache,
        ))
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
      mentions,
    }),

    TopicMetadata::ControlFlow {
      kind, condition, ..
    } => {
      let condition_snippet = match audit_data.nodes.get(condition) {
        Some(Node::Solidity(node)) => {
          let render_ctx = ASTRenderContext {
            target_topic: topic.clone(),
            omit_function_and_modifier_bodies: false,
          };
          Some(render_solidity_ast_snippet(
            node,
            &render_ctx,
            audit_data,
            source_text_cache,
          ))
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
        mentions,
      })
    }

    TopicMetadata::CommentTopic { comment_type, .. } => {
      Some(AgentTopicContext {
        topic: topic_id_string,
        name,
        kind: "Comment".to_string(),
        sub_kind: Some(comment_type.clone()),
        condition: None,
        context,
        expanded_context: None,
        mentions,
      })
    }

    TopicMetadata::FeatureTopic { .. } => Some(AgentTopicContext {
      topic: topic_id_string,
      name,
      kind: "Feature".to_string(),
      sub_kind: None,
      condition: None,
      context,
      expanded_context: None,
      mentions,
    }),

    TopicMetadata::RequirementTopic { .. } => Some(AgentTopicContext {
      topic: topic_id_string,
      name,
      kind: "Requirement".to_string(),
      sub_kind: None,
      condition: None,
      context,
      expanded_context: None,
      mentions,
    }),

    TopicMetadata::BehaviorTopic { .. } => Some(AgentTopicContext {
      topic: topic_id_string,
      name,
      kind: "Behavior".to_string(),
      sub_kind: None,
      condition: None,
      context,
      expanded_context: None,
      mentions,
    }),

    TopicMetadata::ThreatTopic { .. } => Some(AgentTopicContext {
      topic: topic_id_string,
      name,
      kind: "Threat".to_string(),
      sub_kind: None,
      condition: None,
      context,
      expanded_context: None,
      mentions,
    }),

    TopicMetadata::InvariantTopic { .. } => Some(AgentTopicContext {
      topic: topic_id_string,
      name,
      kind: "Invariant".to_string(),
      sub_kind: None,
      condition: None,
      context,
      expanded_context: None,
      mentions,
    }),

    TopicMetadata::DocumentationTopic {
      is_technical, ..
    } => Some(AgentTopicContext {
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
      mentions,
    }),
  }
}

/// Render a contract's members (signatures only, no bodies) as a JSON object
/// with N-prefixed topic IDs. Used by semantic linking pass 1.
pub fn render_contract_members_for_linking(
  contract_node: &crate::solidity::parser::ASTNode,
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) -> Option<String> {
  use crate::solidity::parser::ASTNode;

  let (name, kind, members) = match contract_node {
    ASTNode::ContractDefinition {
      signature, nodes, ..
    } => {
      let resolved_sig = signature.resolve(&audit_data.nodes);
      let (name, kind) = match resolved_sig {
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
      };
      (name, kind, nodes)
    }
    _ => return None,
  };

  let render_ctx = ASTRenderContext {
    target_topic: topic::new_node_topic(&-1),
    omit_function_and_modifier_bodies: true,
  };

  let member_snippets: Vec<serde_json::Value> = members
    .iter()
    .map(|m| render_solidity_ast_snippet(m, &render_ctx, audit_data, source_text_cache))
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
  source_text_cache: &std::collections::HashMap<String, String>,
) -> Option<ContractForBehaviorExtraction> {
  let (name, _kind, members) = match contract_node {
    ASTNode::ContractDefinition {
      signature, nodes, ..
    } => {
      let resolved_sig = signature.resolve(&audit_data.nodes);
      let (name, kind) = match resolved_sig {
        ASTNode::ContractSignature {
          name,
          contract_kind,
          ..
        } => (name.clone(), format!("{:?}", contract_kind).to_lowercase()),
        _ => {
          let ct = topic::new_node_topic(&contract_node.node_id());
          let name = audit_data
            .topic_metadata
            .get(&ct)
            .and_then(|m| m.name())
            .unwrap_or("unknown")
            .to_string();
          (name, "contract".to_string())
        }
      };
      (name, kind, nodes)
    }
    _ => return None,
  };

  let contract_topic = topic::new_node_topic(&contract_node.node_id());

  // Render with bodies included
  let render_ctx = ASTRenderContext {
    target_topic: contract_topic.clone(),
    omit_function_and_modifier_bodies: false,
  };

  // Build semantic annotations for declarations in this contract
  let mut semantics: Vec<serde_json::Value> = Vec::new();
  for (decl_topic, sems) in &audit_data.functional_semantics {
    // Check if this declaration is scoped to this contract
    if let Some(metadata) = audit_data.topic_metadata.get(decl_topic) {
      let in_contract = match metadata.scope() {
        core::Scope::Component { component, .. } => *component == contract_topic,
        core::Scope::Member { component, .. } => *component == contract_topic,
        core::Scope::ContainingBlock { component, .. } => *component == contract_topic,
        _ => false,
      };
      if in_contract {
        for sem in sems {
          semantics.push(json!({
            "declaration_topic": decl_topic.id(),
            "name": metadata.name().unwrap_or(""),
            "semantic": sem.text,
          }));
        }
      }
    }
  }

  // Filter to only functions and modifiers — state variables are excluded
  // from behavior extraction. Resolve stubs before checking type.
  let member_snippets: Vec<serde_json::Value> = members
    .iter()
    .filter(|m| {
      let resolved = m.resolve(&audit_data.nodes);
      matches!(
        resolved,
        ASTNode::FunctionDefinition { .. } | ASTNode::ModifierDefinition { .. }
      )
    })
    .map(|m| render_solidity_ast_snippet(m, &render_ctx, audit_data, source_text_cache))
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
  source_text_cache: &std::collections::HashMap<String, String>,
) -> Vec<ContractForBehaviorExtraction> {
  let mut results = Vec::new();
  for (path, ast) in &audit_data.asts {
    // Only include contracts from in-scope files
    if !audit_data.in_scope_files.contains(path) {
      continue;
    }
    let sol_ast = match ast {
      core::AST::Solidity(sol_ast) => sol_ast,
      _ => continue,
    };
    for node in &sol_ast.nodes {
      let resolved = node.resolve(&audit_data.nodes);
      if let ASTNode::ContractDefinition { .. } = resolved {
        if !audit_data.in_scope_files.contains(path) {
          continue;
        }

        if let Some(rendered) = render_contract_for_behavior_extraction(
          resolved,
          audit_data,
          source_text_cache,
        ) {
          results.push(rendered);
        }
      }
    }
  }
  results
}

// ============================================================================
// Semantic Linking: Pass 3 Context Rendering
// ============================================================================

/// Render a list of declarations within a member that need semantic assignment.
/// Returns a JSON string of declaration names and topic IDs.
pub fn render_member_declarations_for_semantics(
  member_topic: &topic::Topic,
  audit_data: &AuditData,
) -> String {
  let mut declarations: Vec<serde_json::Value> = Vec::new();

  // Include the member itself as a candidate
  if let Some(metadata) = audit_data.topic_metadata.get(member_topic) {
    if let Some(name) = metadata.name() {
      declarations.push(json!({
        "topic": member_topic.id(),
        "name": name,
        "kind": "member",
      }));
    }
  }

  // Collect declarations scoped to this member
  for (decl_topic, metadata) in &audit_data.topic_metadata {
    let in_member = match metadata.scope() {
      core::Scope::Member { member, .. } => member == member_topic,
      core::Scope::ContainingBlock { member, .. } => member == member_topic,
      _ => false,
    };
    if !in_member {
      continue;
    }
    if let Some(name) = metadata.name() {
      let kind = match metadata {
        TopicMetadata::NamedTopic { kind, .. } => format!("{:?}", kind),
        _ => continue,
      };
      declarations.push(json!({
        "topic": decl_topic.id(),
        "name": name,
        "kind": kind,
      }));
    }
  }

  serde_json::to_string(&declarations).unwrap_or_else(|_| "[]".to_string())
}

/// Render declarations for multiple members in one JSON array for batched pass 3.
pub fn render_batched_member_declarations_for_semantics(
  member_topics: &[topic::Topic],
  audit_data: &AuditData,
) -> String {
  let mut all_declarations: Vec<serde_json::Value> = Vec::new();
  for mt in member_topics {
    let single = render_member_declarations_for_semantics(mt, audit_data);
    if let Ok(decls) = serde_json::from_str::<Vec<serde_json::Value>>(&single) {
      all_declarations.extend(decls);
    }
  }
  serde_json::to_string(&all_declarations).unwrap_or_else(|_| "[]".to_string())
}

/// Render source code for multiple members as a combined string for batched pass 3.
pub fn render_batched_member_sources_for_semantics(
  member_topics: &[topic::Topic],
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) -> String {
  let mut parts = Vec::new();
  for mt in member_topics {
    if let Some(source) = render_member_source_for_semantics(mt, audit_data, source_text_cache) {
      parts.push(source);
    }
  }
  parts.join("\n\n")
}

/// Render contract-level declarations for multiple contracts in one JSON array.
pub fn render_batched_contract_declarations_for_semantics(
  contract_topics: &[topic::Topic],
  audit_data: &AuditData,
) -> String {
  let mut all_declarations: Vec<serde_json::Value> = Vec::new();
  for ct in contract_topics {
    let single = render_contract_declarations_for_semantics(ct, audit_data);
    if let Ok(decls) = serde_json::from_str::<Vec<serde_json::Value>>(&single) {
      all_declarations.extend(decls);
    }
  }
  serde_json::to_string(&all_declarations).unwrap_or_else(|_| "[]".to_string())
}

/// Render contract-level declaration signatures for multiple contracts.
pub fn render_batched_contract_declaration_signatures(
  contract_topics: &[topic::Topic],
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) -> String {
  let mut parts = Vec::new();
  for ct in contract_topics {
    let sigs = render_contract_declaration_signatures(ct, audit_data, source_text_cache);
    if sigs != "[]" && !sigs.is_empty() {
      parts.push(sigs);
    }
  }
  parts.join("\n\n")
}

/// Render a member's full source code as a JSON snippet for pass 3 context.
pub fn render_member_source_for_semantics(
  member_topic: &topic::Topic,
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) -> Option<String> {
  // Find the AST node for this member
  for (_path, ast) in &audit_data.asts {
    let sol_ast = match ast {
      core::AST::Solidity(sol_ast) => sol_ast,
      _ => continue,
    };
    for contract_node in &sol_ast.nodes {
      let resolved_contract = contract_node.resolve(&audit_data.nodes);
      if let ASTNode::ContractDefinition { nodes, .. } = resolved_contract {
        for member_node in nodes {
          let resolved_member = member_node.resolve(&audit_data.nodes);
          let node_topic = topic::new_node_topic(&resolved_member.node_id());
          if node_topic == *member_topic {
            let render_ctx = ASTRenderContext {
              target_topic: member_topic.clone(),
              omit_function_and_modifier_bodies: false,
            };
            let rendered = render_solidity_ast_snippet(
              resolved_member,
              &render_ctx,
              audit_data,
              source_text_cache,
            );
            return Some(
              serde_json::to_string(&rendered).unwrap_or_default(),
            );
          }
        }
      }
    }
  }
  None
}

/// For pass 2 mechanical step: given a section's resolved declarations,
/// find the containing members. For declarations scoped at component level
/// (state variables), find members that read/write them.
/// Render component-scoped declarations (state variables, events, structs, enums)
/// for a contract that need semantic assignment. These are declarations at
/// the contract level, not inside any function or modifier.
pub fn render_contract_declarations_for_semantics(
  contract_topic: &topic::Topic,
  audit_data: &AuditData,
) -> String {
  let mut declarations: Vec<serde_json::Value> = Vec::new();

  for (decl_topic, metadata) in &audit_data.topic_metadata {
    let at_component = match metadata.scope() {
      core::Scope::Component { component, .. } => component == contract_topic,
      _ => false,
    };
    if !at_component {
      continue;
    }
    if let TopicMetadata::NamedTopic { name, kind, .. } = metadata {
      declarations.push(json!({
        "topic": decl_topic.id(),
        "name": name,
        "kind": format!("{:?}", kind),
      }));
    }
  }

  serde_json::to_string(&declarations).unwrap_or_else(|_| "[]".to_string())
}

/// Render contract-scoped declaration signatures as source context for pass 3.
/// Returns a compact JSON of state variable declarations, event signatures, etc.
pub fn render_contract_declaration_signatures(
  contract_topic: &topic::Topic,
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) -> String {
  for (_path, ast) in &audit_data.asts {
    let sol_ast = match ast {
      core::AST::Solidity(sol_ast) => sol_ast,
      _ => continue,
    };
    for contract_node in &sol_ast.nodes {
      let resolved = contract_node.resolve(&audit_data.nodes);
      let node_topic = topic::new_node_topic(&resolved.node_id());
      if node_topic != *contract_topic {
        continue;
      }
      if let ASTNode::ContractDefinition { nodes, .. } = resolved {
        let render_ctx = ASTRenderContext {
          target_topic: contract_topic.clone(),
          omit_function_and_modifier_bodies: true,
        };
        // Filter to non-function/modifier members (state vars, events, structs, etc.)
        // Resolve each member before checking its type
        let snippets: Vec<serde_json::Value> = nodes
          .iter()
          .filter(|n| {
            let resolved_n = n.resolve(&audit_data.nodes);
            !matches!(
              resolved_n,
              ASTNode::FunctionDefinition { .. }
                | ASTNode::ModifierDefinition { .. }
            )
          })
          .map(|n| {
            render_solidity_ast_snippet(n, &render_ctx, audit_data, source_text_cache)
          })
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
        core::Scope::Member { member, component, .. }
        | core::Scope::ContainingBlock { member, component, .. }
          if component == contract_topic =>
        {
          if !members.contains(member) {
            members.push(member.clone());
          }
        }
        // Declaration is at component level (state variable) — find members that use it
        core::Scope::Component { component, .. }
          if component == contract_topic =>
        {
          // Check function properties for mutations and calls referencing this variable
          for (fn_topic, props) in &audit_data.function_properties {
            let (mutations, _calls) = match props {
              core::FunctionModProperties::FunctionProperties {
                mutations,
                calls,
                ..
              } => (mutations, calls),
              core::FunctionModProperties::ModifierProperties {
                mutations,
                calls,
                ..
              } => (mutations, calls),
            };
            if mutations.contains(decl_topic) {
              if !members.contains(fn_topic) {
                members.push(fn_topic.clone());
              }
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
  pub section_to_contracts: std::collections::HashMap<topic::Topic, Vec<topic::Topic>>,
  /// Maps D-prefixed section topics to the specific N-prefixed declaration topics
  /// that were resolved from inline code references
  pub section_to_declarations: std::collections::HashMap<topic::Topic, Vec<topic::Topic>>,
}

/// Walk the documentation ASTs and resolve inline code references to find
/// confirmed section→contract associations. This is the mechanical layer
/// of semantic linking — perfect confidence because the documentation
/// literally names the declaration.
pub fn mechanical_semantic_links(
  audit_data: &AuditData,
) -> MechanicalLinkResult {
  let mut section_to_contracts: std::collections::HashMap<topic::Topic, Vec<topic::Topic>> =
    std::collections::HashMap::new();
  let mut section_to_declarations: std::collections::HashMap<topic::Topic, Vec<topic::Topic>> =
    std::collections::HashMap::new();

  for (_path, ast) in &audit_data.asts {
    let doc_ast = match ast {
      core::AST::Documentation(doc_ast) => doc_ast,
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

/// Recursively walk documentation nodes, tracking the top-level section.
/// When a CodeIdentifier with a resolved reference is found, walk up
/// the reference's scope to find the containing contract and record
/// the section→contract and section→declaration associations.
///
/// Only the first (top-level) section sets `current_section`; nested
/// child sections inherit the parent so that all mechanical links roll
/// up to the top-level section that the pipeline actually processes.
fn collect_mechanical_links_recursive(
  node: &crate::documentation::parser::DocumentationNode,
  current_section: Option<&topic::Topic>,
  audit_data: &AuditData,
  section_to_contracts: &mut std::collections::HashMap<topic::Topic, Vec<topic::Topic>>,
  section_to_declarations: &mut std::collections::HashMap<topic::Topic, Vec<topic::Topic>>,
) {
  // Resolve Stubs through audit_data.nodes — the AST contains stubs after analysis
  let node = node.resolve(&audit_data.nodes);
  match node {
    DocumentationNode::Section { node_id, children, .. } => {
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

    DocumentationNode::Heading { section, children, .. } => {
      // Process heading text with current section context
      for child in children {
        collect_mechanical_links_recursive(
          child, current_section, audit_data,
          section_to_contracts, section_to_declarations,
        );
      }
      // Process section content
      if let Some(sec) = section {
        collect_mechanical_links_recursive(
          sec, current_section, audit_data,
          section_to_contracts, section_to_declarations,
        );
      }
    }

    DocumentationNode::CodeIdentifier {
      referenced_topic: Some(ref_topic),
      ..
    } => {
      if let Some(section_topic) = current_section {
        // Record section → declaration
        let decls = section_to_declarations
          .entry(section_topic.clone())
          .or_default();
        if !decls.contains(ref_topic) {
          decls.push(ref_topic.clone());
        }

        // Walk up the declaration's scope to find the containing contract.
        // If the reference IS a contract, use it directly.
        if let Some(metadata) = audit_data.topic_metadata.get(ref_topic) {
          let contract_topic = match metadata {
            TopicMetadata::NamedTopic {
              kind: core::NamedTopicKind::Contract(_),
              ..
            } => Some(ref_topic.clone()),
            _ => match metadata.scope() {
              core::Scope::Component { component, .. } => Some(component.clone()),
              core::Scope::Member { component, .. } => Some(component.clone()),
              core::Scope::ContainingBlock { component, .. } => Some(component.clone()),
              _ => None,
            },
          };
          if let Some(ct) = contract_topic {
            let contracts = section_to_contracts
              .entry(section_topic.clone())
              .or_default();
            if !contracts.contains(&ct) {
              contracts.push(ct.clone());
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
          child, current_section, audit_data,
          section_to_contracts, section_to_declarations,
        );
      }
    }

    // Leaf nodes and nodes without relevant children
    _ => {}
  }
}

/// Render a list of in-scope contracts with their names and topic IDs
/// for LLM pass 1 of semantic linking. Only contracts from files listed
/// in scope.txt are included — dependencies are excluded.
pub fn render_contract_list_for_semantic_linking(
  audit_data: &AuditData,
  source_text_cache: &std::collections::HashMap<String, String>,
) -> Vec<(topic::Topic, String)> {
  use crate::solidity::parser::ASTNode;

  let mut contracts = Vec::new();
  for (path, ast) in &audit_data.asts {
    // Only include contracts from in-scope files
    if !audit_data.in_scope_files.contains(path) {
      continue;
    }
    let sol_ast = match ast {
      core::AST::Solidity(sol_ast) => sol_ast,
      _ => continue,
    };
    for node in &sol_ast.nodes {
      let resolved = node.resolve(&audit_data.nodes);
      if let ASTNode::ContractDefinition { .. } = resolved {
        let contract_topic = topic::new_node_topic(&resolved.node_id());
        if let Some(json) = render_contract_members_for_linking(
          resolved,
          audit_data,
          source_text_cache,
        ) {
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
  let node_id = section_topic.numeric_id()? as i32;
  let doc_topic = topic::new_documentation_topic(node_id);

  // Find the section's title from metadata
  let title = match audit_data.topic_metadata.get(&doc_topic) {
    Some(TopicMetadata::TitledTopic { title, .. }) => Some(title.as_str()),
    _ => {
      eprintln!(
        "render_section_text: no TitledTopic metadata for {} (node_id={})",
        section_topic.id(), node_id
      );
      None
    }
  };

  // Render the section content from the documentation AST
  let doc_node = find_doc_node_by_id(audit_data, node_id);
  if doc_node.is_none() {
    eprintln!(
      "render_section_text: find_doc_node_by_id returned None for node_id={}",
      node_id
    );
    return None;
  }

  let rendered = render_documentation_ast_snippet(
    doc_node?,
    audit_data,
    None,
  );

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
fn find_doc_node_by_id<'a>(
  audit_data: &'a AuditData,
  target_id: i32,
) -> Option<&'a crate::documentation::parser::DocumentationNode> {
  fn search_node<'a>(
    node: &'a crate::documentation::parser::DocumentationNode,
    target_id: i32,
    nodes_map: &'a std::collections::BTreeMap<topic::Topic, core::Node>,
  ) -> Option<&'a crate::documentation::parser::DocumentationNode> {
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

  for (_path, ast) in &audit_data.asts {
    let doc_ast = match ast {
      core::AST::Documentation(doc_ast) => doc_ast,
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

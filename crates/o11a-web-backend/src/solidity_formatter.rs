use crate::formatting;
use o11a_core::core::topic::{self, new_node_topic};
use o11a_core::core::{self, TopicMetadata};
use o11a_core::core::{ContractKind, FunctionKind, VariableMutability};
use o11a_core::solidity::ast::{
  self, ASTNode, AssignmentOperator, BinaryOperator, FunctionStateMutability,
  FunctionVisibility, LiteralKind, StorageLocation, StubKind, UnaryOperator,
  VariableVisibility,
};
use std::collections::BTreeMap;

/// Context passed through the recursive formatting process.
pub struct Context {
  /// The topic that is being formatted (the target of `node_to_source_text`).
  /// This allows recursive formatting logic to distinguish between the target
  /// node and recursively formatted child nodes.
  pub target_topic: topic::Topic,
  pub omit_variable_declaration_let: bool,
  /// When true, a parameter variable that is the target topic will be formatted
  /// as its parent signature node. Set to false when recursively formatting the
  /// signature to prevent infinite recursion.
  pub format_parameter_variable_as_signature: bool,
  pub omit_function_and_modifier_bodies: bool,
}

pub fn global_to_source_text(topic: &topic::Topic) -> Option<String> {
  match topic {
    topic::Topic::Node(-8) => Some(formatting::format_topic_token(
      &new_node_topic(&-8),
      "keccak256",
      "global",
      topic,
    )),
    topic::Topic::Node(-27) => Some(formatting::format_topic_token(
      &new_node_topic(&-27),
      "type",
      "global",
      topic,
    )),
    topic::Topic::Node(-28) => Some(formatting::format_topic_token(
      &new_node_topic(&-28),
      "this",
      "global",
      topic,
    )),
    _ => None,
  }
}

/// Converts an AST node and all its children to a formatted HTML string with syntax highlighting.
///
/// This function recursively processes nodes and uses div-based indentation with padding.
/// Binary operators are formatted with the left-hand side and operator on one line,
/// and the right-hand side indented on the next line.
pub fn node_to_source_text(
  node: &ASTNode,
  nodes_map: &BTreeMap<topic::Topic, core::Node>,
  topic_metadata: &BTreeMap<topic::Topic, core::TopicMetadata>,
) -> String {
  let ctx = Context {
    target_topic: new_node_topic(&node.node_id()),
    omit_variable_declaration_let: false,
    format_parameter_variable_as_signature: false,
    omit_function_and_modifier_bodies: false,
  };

  format!(
    "<pre><code>{}</code></pre>",
    do_node_to_source_text(node, 0, nodes_map, topic_metadata, &ctx)
  )
}

/// Extracts the StubKind from an ASTNode. For Stub nodes this is free (the
/// kind field is already populated). For real nodes this classifies on the fly.
fn get_stub_kind(node: &ASTNode) -> StubKind {
  match node {
    ASTNode::Stub { kind, .. } => kind.clone(),
    _ => ast::classify_node_stub_kind(node),
  }
}

/// Returns the topic to use for an identifier placeholder. For Identifier
/// nodes, this is the referenced declaration's topic (not the identifier
/// node's own topic). For TypeConversion nodes, this is the argument's topic.
/// For all other nodes, this is the node's own topic.
fn placeholder_topic(node: &ASTNode) -> topic::Topic {
  match node {
    ASTNode::Stub { kind, topic, .. } => {
      // If the stub carries a placeholder topic (Identifier → referenced
      // declaration, TypeConversion → argument), use it.
      kind
        .placeholder_topic()
        .cloned()
        .unwrap_or_else(|| *topic)
    }
    ASTNode::Identifier {
      referenced_declaration,
      ..
    }
    | ASTNode::IdentifierPath {
      referenced_declaration,
      ..
    } => new_node_topic(referenced_declaration),
    ASTNode::MemberAccess { expression, .. } => {
      // Recurse into the base expression
      placeholder_topic(expression)
    }
    ASTNode::TypeConversion { argument, .. } => {
      // Recurse into the argument to get its referenced declaration topic
      placeholder_topic(argument)
    }
    _ => new_node_topic(&node.node_id()),
  }
}

/// If the node is identifier-like, returns an identifier placeholder string
/// for it. For compound expressions (FunctionCall, StructConstructor), recurses
/// into the callee expression to find the root identifier placeholder. This
/// allows callers to hoist the placeholder above operators (e.g. `?`, `:`)
/// rather than having it appear inside the compound expression's output.
fn maybe_identifier_placeholder(node: &ASTNode) -> String {
  let kind = get_stub_kind(node);
  if kind.is_identifier_like() {
    formatting::format_identifier_placeholder(&placeholder_topic(node))
  } else {
    // Recurse into compound expression wrappers to find the root identifier.
    match node {
      ASTNode::FunctionCall { expression, .. }
      | ASTNode::StructConstructor { expression, .. }
      | ASTNode::FunctionCallOptions { expression, .. }
      | ASTNode::MemberAccess { expression, .. }
      | ASTNode::IndexAccess {
        base_expression: expression,
        ..
      } => maybe_identifier_placeholder(expression),
      _ => String::new(),
    }
  }
}

fn do_node_to_source_text(
  node: &ASTNode,
  indent_level: usize,
  nodes_map: &BTreeMap<topic::Topic, core::Node>,
  topic_metadata: &BTreeMap<topic::Topic, core::TopicMetadata>,
  ctx: &Context,
) -> String {
  let node_str = match node.resolve(nodes_map) {
    ASTNode::Assignment {
      node_id,
      operator,
      left_hand_side,
      right_hand_side,
      ..
    } => {
      let lhs_placeholder = maybe_identifier_placeholder(left_hand_side);
      let rhs_placeholder = maybe_identifier_placeholder(right_hand_side);
      let lhs = do_node_to_source_text(
        left_hand_side,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      let op = assignment_operator_to_string(operator);

      let indent_level = indent_level + 1;
      let rhs = do_node_to_source_text(
        right_hand_side,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      format!(
        "{}{} {} {}{}",
        lhs_placeholder,
        formatting::format_keyword("mut"),
        lhs,
        formatting::format_topic_operator(
          &new_node_topic(node_id),
          &op,
          &new_node_topic(node_id)
        ),
        formatting::indent(
          &format!("{}{}", rhs_placeholder, rhs),
          indent_level,
        )
      )
    }

    ASTNode::BinaryOperation {
      node_id,
      left_expression,
      operator,
      right_expression,
      ..
    } => {
      let lhs_placeholder = maybe_identifier_placeholder(left_expression);
      let rhs_placeholder = maybe_identifier_placeholder(right_expression);
      let lhs = do_node_to_source_text(
        left_expression,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      let op = binary_operator_to_string(operator);

      if is_boolean_and_or_operator(operator) || is_math_operator(operator) {
        // Boolean &&, ||, and math operators do not indent, but place the
        // operator and rhs on the next line with the same indentation level
        // of the lhs
        let rhs = do_node_to_source_text(
          right_expression,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        );
        format!(
          "{}{}\n{}{} {}",
          lhs_placeholder,
          lhs,
          rhs_placeholder,
          formatting::format_topic_operator(
            &new_node_topic(node_id),
            &op,
            &new_node_topic(node_id)
          ),
          &rhs,
        )
      } else {
        let indent_level = indent_level + 1;
        let rhs = do_node_to_source_text(
          right_expression,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        );
        format!(
          "{}{}{}",
          lhs_placeholder,
          lhs,
          formatting::indent(
            &format!(
              "{}{} {}",
              rhs_placeholder,
              formatting::format_topic_operator(
                &new_node_topic(node_id),
                &op,
                &new_node_topic(node_id)
              ),
              &rhs
            ),
            indent_level
          )
        )
      }
    }

    ASTNode::Conditional {
      node_id,
      condition,
      true_expression,
      false_expression,
      ..
    } => {
      let cond = do_node_to_source_text(
        condition,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );

      let indent_level = indent_level + 1;
      let true_placeholder = maybe_identifier_placeholder(true_expression);
      let part = if let Some(false_expr) = false_expression {
        let false_placeholder = maybe_identifier_placeholder(false_expr);
        format!(
          "\n{}{} {}\n{}{} {}",
          true_placeholder,
          formatting::format_topic_operator(
            &new_node_topic(node_id),
            "?",
            &new_node_topic(node_id)
          ),
          do_node_to_source_text(
            true_expression,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx
          ),
          false_placeholder,
          formatting::format_operator(":"),
          do_node_to_source_text(
            false_expr,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx
          ),
        )
      } else {
        format!(
          "\n{}{} {}",
          true_placeholder,
          formatting::format_topic_operator(
            &new_node_topic(node_id),
            "?",
            &new_node_topic(node_id)
          ),
          do_node_to_source_text(
            true_expression,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx
          )
        )
      };

      format!("{}{}", cond, part)
    }

    ASTNode::ElementaryTypeNameExpression { type_name, .. } => {
      do_node_to_source_text(
        type_name,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      )
    }

    ASTNode::FunctionCall {
      expression,
      arguments,
      ..
    } => {
      // The caller is responsible for emitting the root identifier placeholder
      // (via maybe_identifier_placeholder, which recurses into FunctionCall).
      let expression = expression.resolve(nodes_map);

      let expr = do_node_to_source_text(
        expression,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );

      let indent_level = indent_level + 1;

      // Format arguments (Argument nodes handle parameter name formatting)
      let args = arguments
        .iter()
        .map(|arg| {
          do_node_to_source_text(
            arg,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx,
          )
        })
        .collect::<Vec<_>>()
        .join("\n");

      if arguments.is_empty() {
        format!("{}()", expr)
      } else {
        let indented_args = match expression {
          // If the expression is a member access, indent the arguments twice to
          // match the member access indent
          ASTNode::MemberAccess { .. } => {
            format!(
              "({}",
              formatting::indent(
                &format!(
                  "{}\n)",
                  formatting::inline_indent(&args, indent_level)
                ),
                indent_level
              )
            )
          }
          _ => format!("({}\n)", formatting::indent(&args, indent_level)),
        };

        format!("{}{}", expr, indented_args)
      }
    }

    ASTNode::Argument {
      parameter,
      argument,
      ..
    } => {
      let arg_placeholder = maybe_identifier_placeholder(argument);
      let arg_str = do_node_to_source_text(
        argument,
        indent_level + 1,
        nodes_map,
        topic_metadata,
        ctx,
      );

      // Format as "parameter:\n\targument" if parameter is available
      match parameter {
        Some(param) => {
          match param.as_ref() {
            ASTNode::Identifier {
              name,
              referenced_declaration,
              ..
            } if !name.is_empty() => {
              // Placeholder for the parameter name, targeting the referenced declaration
              let param_placeholder = formatting::format_identifier_placeholder(
                &new_node_topic(referenced_declaration),
              );
              // Only include parameter name if it's not empty
              let param_str = do_node_to_source_text(
                param,
                indent_level,
                nodes_map,
                topic_metadata,
                ctx,
              );
              format!(
                "{}{}:{}",
                param_placeholder,
                param_str,
                formatting::indent(
                  &format!("{}{}", arg_placeholder, arg_str),
                  indent_level + 1,
                )
              )
            }

            // Fallback: just format the argument without parameter name
            _ => format!("{}{}", arg_placeholder, arg_str),
          }
        }

        // Fallback: just format the argument without parameter name
        _ => format!("{}{}", arg_placeholder, arg_str),
      }
    }

    ASTNode::TypeConversion {
      node_id,
      expression,
      argument,
      ..
    } => {
      let expr = do_node_to_source_text(
        expression,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );

      let arg_str = do_node_to_source_text(
        argument,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );

      format!(
        "{}{}{}",
        arg_str,
        formatting::format_topic_operator(
          &new_node_topic(node_id),
          "@",
          &new_node_topic(node_id)
        ),
        expr,
      )
    }

    ASTNode::StructConstructor {
      expression,
      arguments,
      ..
    } => {
      let expr = do_node_to_source_text(
        expression,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );

      let indent_level = indent_level + 1;
      let args = arguments
        .iter()
        .map(|arg| {
          do_node_to_source_text(
            arg,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx,
          )
        })
        .collect::<Vec<_>>()
        .join("\n");

      format!("{}({}\n)", expr, formatting::indent(&args, indent_level))
    }

    ASTNode::FunctionCallOptions {
      expression,
      options,
      ..
    } => {
      let expr = do_node_to_source_text(
        expression,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );

      let indent_level = indent_level + 1;
      let opts = options
        .iter()
        .map(|opt| {
          do_node_to_source_text(
            opt,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx,
          )
        })
        .collect::<Vec<_>>()
        .join("\n");
      let trailing_newline = if !opts.is_empty() { "\n" } else { "" };
      format!(
        "{}{{{}{}}}",
        expr,
        formatting::indent(&opts, indent_level),
        trailing_newline
      )
    }

    ASTNode::Identifier {
      node_id,
      name,
      referenced_declaration,
      ..
    }
    | ASTNode::IdentifierPath {
      node_id,
      name,
      referenced_declaration,
      ..
    } => format_identifier(
      &new_node_topic(node_id),
      name,
      &new_node_topic(referenced_declaration),
      topic_metadata,
    ),

    ASTNode::IndexAccess {
      base_expression,
      index_expression,
      ..
    } => {
      // The caller is responsible for emitting the root identifier placeholder
      // (via maybe_identifier_placeholder, which recurses into IndexAccess).
      let base = do_node_to_source_text(
        base_expression,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      if let Some(idx) = index_expression {
        // If the base is a MemberAccess, the last line of its output is
        // indented one level deeper (MemberAccess adds indent_level + 1).
        // The brackets should match that level.
        let bracket_indent = if matches!(
          base_expression.resolve(nodes_map),
          ASTNode::MemberAccess {
            referenced_declaration: Some(_),
            ..
          }
        ) {
          indent_level + 1
        } else {
          indent_level
        };
        let idx_placeholder = maybe_identifier_placeholder(idx);
        let idx_content = format!(
          "{}{}",
          idx_placeholder,
          do_node_to_source_text(
            idx,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx
          )
        );
        format!(
          "{}[{}\n]",
          base,
          formatting::indent(&idx_content, bracket_indent + 1),
        )
      } else {
        base
      }
    }

    ASTNode::IndexRangeAccess { .. } => {
      "IndexRangeAccess placeholder".to_owned()
    }

    ASTNode::Literal {
      kind,
      value,
      hex_value,
      ..
    } => match kind {
      LiteralKind::String => formatting::format_string(&format!(
        "\"{}\"",
        formatting::html_escape(value.as_ref().unwrap_or(hex_value))
      )),
      LiteralKind::HexString => formatting::format_number(&format!(
        "0x{}",
        value.as_ref().unwrap_or(hex_value)
      )),
      LiteralKind::Number | LiteralKind::Bool => {
        formatting::format_number(value.as_ref().unwrap_or(hex_value))
      }
    },

    ASTNode::MemberAccess {
      node_id,
      expression,
      member_name,
      referenced_declaration,
      ..
    } => {
      // Check if this is a special case like block.timestamp or msg.sender
      let resolved_expression = expression.resolve(nodes_map);
      let is_special_case =
        if let ASTNode::Identifier { name, .. } = resolved_expression {
          matches!(
            (name.as_str(), member_name.as_str()),
            ("block", "timestamp")
              | ("block", "number")
              | ("block", "prevrandao")
              | ("block", "gaslimit")
              | ("block", "difficulty")
              | ("block", "coinbase")
              | ("block", "chainid")
              | ("block", "blobbasefee")
              | ("block", "basefee")
              | ("msg", "sender")
              | ("msg", "value")
              | ("msg", "data")
              | ("msg", "sig")
              | ("tx", "gasprice")
              | ("tx", "origin")
              | ("abi", "encode")
              | ("abi", "encodePacked")
          )
        } else {
          false
        };

      if is_special_case {
        // Format as a single token with "global" class
        if let ASTNode::Identifier { name, .. } = resolved_expression {
          format!(
            "{}.{}",
            formatting::format_global(name),
            formatting::format_global(member_name)
          )
        } else {
          unreachable!()
        }
      } else {
        // Default formatting for other member accesses
        let expr = do_node_to_source_text(
          expression,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        );
        if let Some(member_node_id) = referenced_declaration {
          let member = format_identifier(
            &new_node_topic(node_id),
            member_name,
            &new_node_topic(member_node_id),
            topic_metadata,
          );

          let indent_level = indent_level + 1;
          // Placeholder for the member, targeting the referenced declaration
          let member_placeholder = formatting::format_identifier_placeholder(
            &new_node_topic(member_node_id),
          );
          let member_expr = format!(
            "{}{}{}",
            member_placeholder,
            formatting::format_operator("."),
            member
          );
          format!("{}{}", expr, formatting::indent(&member_expr, indent_level),)
        } else {
          let member = formatting::format_member(member_name);

          let member_expr =
            format!("{}{}", formatting::format_operator("."), member);
          format!("{}{}", expr, member_expr)
        }
      }
    }

    ASTNode::NewExpression {
      node_id, type_name, ..
    } => {
      format!(
        "{} {}",
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "new",
          &new_node_topic(node_id)
        ),
        do_node_to_source_text(
          type_name,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx
        )
      )
    }

    ASTNode::TupleExpression { components, .. } => {
      let indent_level = indent_level + 1;
      let comps = components
        .iter()
        .map(|c| {
          let comp_placeholder = maybe_identifier_placeholder(c);
          format!(
            "{}{}",
            comp_placeholder,
            do_node_to_source_text(
              c,
              indent_level,
              nodes_map,
              topic_metadata,
              ctx,
            )
          )
        })
        .collect::<Vec<_>>()
        .join(",\n");
      let trailing_newline = if !comps.is_empty() { "\n" } else { "" };
      format!(
        "{}{}{}{}",
        formatting::format_brace("(", indent_level),
        formatting::indent(&comps, indent_level),
        trailing_newline,
        formatting::format_brace(")", indent_level)
      )
    }

    ASTNode::UnaryOperation {
      node_id,
      prefix,
      operator,
      sub_expression,
      ..
    } => {
      let op = unary_operator_to_string(operator);
      let sub_placeholder = maybe_identifier_placeholder(sub_expression);
      let expr = do_node_to_source_text(
        sub_expression,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      let keyword = match operator {
        UnaryOperator::Increment => {
          format!("{} ", formatting::format_keyword("mut"))
        }
        UnaryOperator::Decrement => {
          format!("{} ", formatting::format_keyword("mut"))
        }
        UnaryOperator::Plus => String::new(),
        UnaryOperator::Minus => String::new(),
        UnaryOperator::BitwiseNot => String::new(),
        UnaryOperator::Not => String::new(),
        UnaryOperator::Delete => {
          format!("{} ", formatting::format_keyword("mut"))
        }
      };

      if *prefix {
        format!(
          "{}{}{}{}",
          sub_placeholder,
          keyword,
          formatting::format_topic_operator(
            &new_node_topic(node_id),
            &op,
            &new_node_topic(node_id)
          ),
          expr
        )
      } else {
        format!(
          "{}{}{}{}",
          sub_placeholder,
          keyword,
          expr,
          formatting::format_topic_operator(
            &new_node_topic(node_id),
            &op,
            &new_node_topic(node_id)
          )
        )
      }
    }

    ASTNode::EnumValue { node_id, name, .. } => {
      formatting::format_topic_enum_value(
        &new_node_topic(node_id),
        name,
        &new_node_topic(node_id),
      )
    }

    ASTNode::Block { statements, .. } => {
      if statements.is_empty() {
        formatting::format_brace("{}", indent_level)
      } else {
        let indent_level = indent_level + 1;
        let stmts = statements
          .iter()
          .map(|s| {
            do_node_to_source_text(
              s,
              indent_level,
              nodes_map,
              topic_metadata,
              ctx,
            )
          })
          .collect::<Vec<_>>()
          .join("\n\n");
        let trailing_newline = if !stmts.is_empty() { "\n" } else { "" };
        format!(
          "{}{}{}{}",
          formatting::format_brace("{", indent_level),
          formatting::indent(&stmts, indent_level),
          trailing_newline,
          formatting::format_brace("}", indent_level),
        )
      }
    }

    ASTNode::SemanticBlock {
      node_id,
      statements,
      documentation: _,
      ..
    } => {
      let statements = statements
        .iter()
        .map(|s| {
          do_node_to_source_text(
            s,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx,
          )
        })
        .collect::<Vec<_>>()
        .join("\n");

      let topic = topic::new_node_topic(node_id);
      let block_placeholder =
        formatting::format_containing_block_placeholder(&topic);

      // TODO: eventually we will want transitive semantic blocks if there
      // is only one statement within them
      if ctx.target_topic == topic {
        // When the semantic block is the target topic, render it as the
        // target topic, so that the UI can not render it, when it is redundant
        // with its container. This is especially impactful for the references
        // panel, where it can be annoying to have to pass over each semantic
        // block containing each reference.
        format!(
          "{}{}",
          block_placeholder,
          formatting::format_topic_block(
            &new_node_topic(node_id),
            &statements,
            "semantic-block target-topic",
            &topic,
          )
        )
      } else {
        format!(
          "{}{}",
          block_placeholder,
          formatting::format_topic_block(
            &new_node_topic(node_id),
            &statements,
            "semantic-block",
            &topic,
          )
        )
      }
    }

    ASTNode::ContractMemberGroup {
      node_id,
      members,
      documentation: _,
      ..
    } => {
      let members_str = members
        .iter()
        .map(|m| {
          do_node_to_source_text(
            m,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx,
          )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

      let topic = topic::new_node_topic(node_id);
      let block_placeholder =
        formatting::format_containing_block_placeholder(&topic);

      let class = if ctx.target_topic == topic {
        "contract-member-group target-topic"
      } else {
        "contract-member-group"
      };

      format!(
        "{}{}",
        block_placeholder,
        formatting::format_topic_block(
          &new_node_topic(node_id),
          &members_str,
          class,
          &topic,
        )
      )
    }

    ASTNode::Break { node_id, .. } => {
      format!(
        "{}{}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        formatting::format_keyword("break"),
      )
    }

    ASTNode::Continue { node_id, .. } => {
      format!(
        "{}{}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        formatting::format_keyword("continue"),
      )
    }

    ASTNode::DoWhileStatement {
      node_id,
      condition,
      body,
      ..
    } => {
      let body_str = if let Some(b) = body {
        do_node_to_source_text(b, indent_level, nodes_map, topic_metadata, ctx)
      } else {
        String::new()
      };
      let cond_placeholder = maybe_identifier_placeholder(condition);
      let cond = do_node_to_source_text(
        condition,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      format!(
        "{}{} {} {} ({}{})",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "do",
          &new_node_topic(node_id)
        ),
        body_str,
        formatting::format_keyword("while"),
        cond_placeholder,
        cond
      )
    }

    ASTNode::EmitStatement {
      node_id,
      event_call,
      ..
    } => {
      let call_placeholder = maybe_identifier_placeholder(event_call);
      format!(
        "{}{}{} {}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        call_placeholder,
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "emit",
          &new_node_topic(node_id)
        ),
        do_node_to_source_text(
          event_call,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx
        )
      )
    }

    ASTNode::ExpressionStatement {
      node_id,
      expression,
      ..
    } => {
      let expr_placeholder = maybe_identifier_placeholder(expression);
      format!(
        "{}{}{}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        expr_placeholder,
        do_node_to_source_text(
          expression,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        )
      )
    }

    ASTNode::ForStatement {
      node_id,
      condition,
      body,
      ..
    } => {
      let _loop_expr = do_node_to_source_text(
        condition,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      let body_str = do_node_to_source_text(
        body,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      format!(
        "{}{} (LoopExpr) {}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "for",
          &new_node_topic(node_id)
        ),
        body_str
      )
    }

    ASTNode::LoopExpression {
      initialization_expression,
      condition,
      loop_expression,
      ..
    } => {
      let init = if let Some(init_expr) = initialization_expression {
        do_node_to_source_text(
          init_expr,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        )
      } else {
        String::new()
      };
      let cond = if let Some(cond_expr) = condition {
        do_node_to_source_text(
          cond_expr,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        )
      } else {
        String::new()
      };
      let loop_expr = if let Some(l_expr) = loop_expression {
        do_node_to_source_text(
          l_expr,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        )
      } else {
        String::new()
      };
      format!("{}; {}; {}", init, cond, loop_expr)
    }

    ASTNode::IfStatement {
      node_id,
      condition,
      true_body,
      false_body,
      ..
    } => {
      let cond_placeholder = maybe_identifier_placeholder(condition);
      let cond = do_node_to_source_text(
        condition,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      let true_b = do_node_to_source_text(
        true_body,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      let false_part = if let Some(false_b) = false_body {
        format!(
          " {} {}",
          formatting::format_keyword("else"),
          do_node_to_source_text(
            false_b,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx
          )
        )
      } else {
        String::new()
      };
      format!(
        "{}{} ({}{}\n) {}{}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "if",
          &new_node_topic(node_id)
        ),
        cond_placeholder,
        formatting::indent(&cond, indent_level + 1),
        true_b,
        false_part
      )
    }

    ASTNode::InlineAssembly { .. } => formatting::format_keyword("assembly"),

    ASTNode::PlaceholderStatement { node_id, .. } => {
      format!(
        "{}{}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "placeholder",
          &new_node_topic(node_id),
        )
      )
    }

    ASTNode::Return {
      node_id,
      expression,
      ..
    } => {
      let stmt_placeholder =
        formatting::format_statement_placeholder(&new_node_topic(node_id));
      if let Some(expr) = expression {
        let expr_placeholder = maybe_identifier_placeholder(expr);
        format!(
          "{}{}{} {}",
          stmt_placeholder,
          expr_placeholder,
          formatting::format_topic_keyword(
            &new_node_topic(node_id),
            "return",
            &new_node_topic(node_id)
          ),
          do_node_to_source_text(
            expr,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx
          )
        )
      } else {
        format!(
          "{}{}",
          stmt_placeholder,
          formatting::format_topic_keyword(
            &new_node_topic(node_id),
            "return",
            &new_node_topic(node_id),
          )
        )
      }
    }

    ASTNode::RevertStatement {
      node_id,
      error_call,
      ..
    } => {
      let call_placeholder = maybe_identifier_placeholder(error_call);
      format!(
        "{}{}{} {}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        call_placeholder,
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "revert",
          &new_node_topic(node_id)
        ),
        do_node_to_source_text(
          error_call,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx
        )
      )
    }

    ASTNode::TryStatement { .. } => "TryStatement placeholder".to_owned(),

    ASTNode::UncheckedBlock { statements, .. } => {
      let indent_level = indent_level + 1;

      let stmts = statements
        .iter()
        .map(|s| {
          do_node_to_source_text(
            s,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx,
          )
        })
        .collect::<Vec<_>>()
        .join("\n");
      let trailing_newline = if !stmts.is_empty() { "\n" } else { "" };
      format!(
        "{} {}{}{}{}",
        formatting::format_keyword("unchecked"),
        formatting::format_brace("{", indent_level),
        formatting::indent(&stmts, indent_level),
        trailing_newline,
        formatting::format_brace("}", indent_level)
      )
    }

    ASTNode::VariableDeclarationStatement {
      declarations,
      initial_value,
      ..
    } => {
      let indent_level = indent_level + 1;

      if let Some(initial_val) = initial_value {
        if declarations.len() == 1 {
          // Identifier placeholder for the declared variable (above the let line)
          let decl_placeholder = formatting::format_identifier_placeholder(
            &new_node_topic(&declarations[0].node_id()),
          );
          // Identifier placeholder for the initial value (if identifier-like)
          let val_placeholder = maybe_identifier_placeholder(initial_val);
          // Single declaration: use simple format
          format!(
            "{}{} {}{}",
            decl_placeholder,
            do_node_to_source_text(
              &declarations[0],
              indent_level,
              nodes_map,
              topic_metadata,
              ctx
            ),
            formatting::format_operator("="),
            formatting::indent(
              &format!(
                "{}{}",
                val_placeholder,
                do_node_to_source_text(
                  initial_val,
                  indent_level,
                  nodes_map,
                  topic_metadata,
                  ctx
                )
              ),
              indent_level
            ),
          )
        } else {
          // Multiple declarations: use tuple syntax
          // Use a context that omits the "let" keyword for each declaration
          let tuple_ctx = Context {
            target_topic: ctx.target_topic,
            omit_variable_declaration_let: true,
            format_parameter_variable_as_signature: ctx
              .format_parameter_variable_as_signature,
            omit_function_and_modifier_bodies: ctx
              .omit_function_and_modifier_bodies,
          };
          let declarations_str = declarations
            .iter()
            .map(|declaration| {
              do_node_to_source_text(
                declaration,
                indent_level + 1,
                nodes_map,
                topic_metadata,
                &tuple_ctx,
              )
            })
            .collect::<Vec<_>>()
            .join("\n");

          let initial_val_str = do_node_to_source_text(
            initial_val,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx,
          );

          format!(
            "{} ({}\n) {} {}",
            formatting::format_keyword("let"),
            formatting::indent(&declarations_str, indent_level + 1),
            formatting::format_operator("="),
            &initial_val_str
          )
        }
      } else {
        declarations
          .iter()
          .map(|d| {
            do_node_to_source_text(
              d,
              indent_level,
              nodes_map,
              topic_metadata,
              ctx,
            )
          })
          .collect::<Vec<_>>()
          .join("\n")
      }
    }

    ASTNode::VariableDeclaration {
      node_id,
      type_name,
      storage_location,
      visibility,
      mutability,
      value,
      name,
      constant,
      parameter_variable,
      implementation_declaration,
      struct_field,
      ..
    } => {
      // If this is a parameter variable and it's the target topic, format the
      // parent signature node instead
      let signature_node = parameter_variable.and_then(|signature_node_id| {
        let is_target = ctx.target_topic == new_node_topic(node_id);
        if is_target {
          let signature_topic = new_node_topic(&signature_node_id);
          if let Some(core::Node::Solidity(sig_node)) =
            nodes_map.get(&signature_topic)
          {
            Some(sig_node)
          } else {
            None
          }
        } else {
          None
        }
      });

      if let Some(sig_node) = signature_node
        && ctx.format_parameter_variable_as_signature
      {
        // Set format_parameter_variable_as_signature to false to prevent
        // infinite recursion when the signature contains this variable
        let sig_ctx = Context {
          target_topic: ctx.target_topic,
          omit_variable_declaration_let: false,
          format_parameter_variable_as_signature: false,
          omit_function_and_modifier_bodies: ctx
            .omit_function_and_modifier_bodies,
        };
        do_node_to_source_text(
          sig_node,
          indent_level,
          nodes_map,
          topic_metadata,
          &sig_ctx,
        )
      } else {
        let storage = storage_location_to_string(storage_location);
        let visibility_str = variable_visibility_to_string(visibility);

        // If there are no mutations for the variable, set the mutability
        // to "immutable", otherwise set it to "mutable"
        let mutability = if *visibility == VariableVisibility::Internal {
          // Check if this variable has mutations
          let has_mutations = topic_metadata
            .get(&new_node_topic(node_id))
            .map(|meta| meta.is_mutable())
            .unwrap_or(false);
          if has_mutations {
            &VariableMutability::Mutable
          } else {
            &VariableMutability::Immutable
          }
        } else {
          mutability
        };
        let mutability_str = variable_mutability_to_string(mutability);

        let mut parts = vec![];
        // Do not render a visibility modifier for internal variables (they are
        // assumed to be internal)
        if *visibility != VariableVisibility::Internal {
          parts.push(formatting::format_keyword(&visibility_str));
        }
        // Only render "let" for local variables that are not parameters or struct fields
        if *visibility == VariableVisibility::Internal
          && !ctx.omit_variable_declaration_let
          && !struct_field
          && parameter_variable.is_none()
        {
          parts.push(formatting::format_keyword("let"));
        }
        // Do not render an immutable modifier for internal variables (they are
        // assumed to be immutable unless mutated)
        if !(*visibility == VariableVisibility::Internal
          && *mutability == VariableMutability::Immutable)
        {
          parts.push(formatting::format_keyword(&mutability_str));
        }
        if !storage.is_empty() {
          parts.push(formatting::format_keyword(&storage));
        }
        let type_str = do_node_to_source_text(
          type_name,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        );
        let topic =
          new_node_topic(&implementation_declaration.unwrap_or(*node_id));
        if !name.is_empty() {
          parts.push(format!(
            "{}:",
            format_identifier(
              &new_node_topic(node_id),
              name,
              &topic,
              topic_metadata
            )
          ));

          parts.push(type_str);
        } else {
          // Format the type as an identifier
          parts.push(formatting::format_topic_token(
            &new_node_topic(node_id),
            &type_str,
            "unnamed-parameter",
            &topic,
          ));
        };

        let decl = parts.join(" ");
        if let Some(val) = value
          && !constant
        {
          let val = do_node_to_source_text(
            val,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx,
          );
          format!(
            "{} {} {}",
            decl,
            formatting::format_operator("="),
            formatting::indent(&val, indent_level)
          )
        } else {
          decl
        }
      }
    }

    ASTNode::WhileStatement {
      node_id,
      condition,
      body,
      ..
    } => {
      let cond_placeholder = maybe_identifier_placeholder(condition);
      let cond = do_node_to_source_text(
        condition,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      let body_str = if let Some(b) = body {
        do_node_to_source_text(b, indent_level, nodes_map, topic_metadata, ctx)
      } else {
        formatting::format_brace("{}", indent_level)
      };
      format!(
        "{}{} ({}{}) {}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "while",
          &new_node_topic(node_id)
        ),
        cond_placeholder,
        formatting::indent(&cond, indent_level),
        body_str
      )
    }

    ASTNode::ContractSignature {
      node_id,
      contract_kind,
      name,
      declaration_id,
      base_contracts,
      abstract_,
      directives,
      documentation,
      ..
    } => {
      let documentation = documentation
        .clone()
        .map(|doc| match doc.as_ref().resolve(nodes_map) {
          ASTNode::StructuredDocumentation { text, .. } => {
            format!("<span>{}</span>", text.clone())
          }
          _ => {
            println!("Unexpected documentation node: {:?}", doc);
            String::new()
          }
        })
        .unwrap_or_default();
      let kind = contract_kind_to_string(contract_kind);
      let abstract_str = if *abstract_ {
        format!("{} ", formatting::format_keyword("abstract"))
      } else {
        String::new()
      };
      let base_indent_level = indent_level + 1;
      let bases = if !base_contracts.is_empty() {
        let first_base = do_node_to_source_text(
          &base_contracts[0],
          base_indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        );
        let remaining_bases = if base_contracts.len() > 1 {
          let remaining = base_contracts[1..]
            .iter()
            .map(|b| {
              do_node_to_source_text(
                b,
                base_indent_level,
                nodes_map,
                topic_metadata,
                ctx,
              )
            })
            .collect::<Vec<_>>()
            .join("\n");
          format!(
            "\n{}",
            formatting::inline_indent(&remaining, base_indent_level)
          )
        } else {
          String::new()
        };
        format!(
          "{} {}{}",
          formatting::indent(
            &formatting::format_keyword("is"),
            base_indent_level
          ),
          first_base,
          remaining_bases
        )
      } else {
        String::new()
      };

      let directives = if directives.is_empty() {
        String::new()
      } else {
        let directives_str = directives
          .iter()
          .map(|d| {
            do_node_to_source_text(
              d,
              base_indent_level,
              nodes_map,
              topic_metadata,
              ctx,
            )
          })
          .collect::<Vec<_>>()
          .join("\n");
        format!(
          "\n{}",
          formatting::inline_indent(&directives_str, base_indent_level)
        )
      };

      format!(
        "{}{}{} {}{}{}",
        documentation,
        abstract_str,
        formatting::format_keyword(&kind),
        format_identifier(
          &new_node_topic(node_id),
          name,
          &new_node_topic(declaration_id),
          topic_metadata
        ),
        bases,
        directives
      )
    }

    ASTNode::ContractDefinition {
      signature, nodes, ..
    } => {
      let sig_str = do_node_to_source_text(
        signature,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );

      if nodes.is_empty() {
        format!(
          "{} {}",
          sig_str,
          formatting::format_brace("{}", indent_level)
        )
      } else {
        let contract_ctx = Context {
          target_topic: ctx.target_topic,
          omit_variable_declaration_let: ctx.omit_variable_declaration_let,
          format_parameter_variable_as_signature: ctx
            .format_parameter_variable_as_signature,
          omit_function_and_modifier_bodies: true,
        };
        // Members are always wrapped in ContractMemberGroup nodes, each
        // starting with a block-level placeholder div that introduces an
        // implicit line break. Joining with a single `\n` here plus the
        // `\n\n` used between members within a group gives consistent
        // two-line visual separation between every top-level member.
        let members = nodes
          .iter()
          .map(|n| {
            do_node_to_source_text(
              n,
              indent_level + 1,
              nodes_map,
              topic_metadata,
              &contract_ctx,
            )
          })
          .collect::<Vec<_>>()
          .join("\n");
        let trailing_newline = if !members.is_empty() { "\n" } else { "" };
        format!(
          "{} {}{}{}{}",
          sig_str,
          formatting::format_brace("{", indent_level),
          formatting::indent(&members, indent_level + 1),
          trailing_newline,
          formatting::format_brace("}", indent_level)
        )
      }
    }

    ASTNode::FunctionSignature {
      node_id,
      kind,
      name,
      declaration_id,
      parameters,
      return_parameters,
      modifiers,
      visibility,
      state_mutability,
      virtual_,
      implementation_declaration,
      documentation,
      ..
    } => {
      let documentation = documentation
        .clone()
        .map(|doc| match doc.as_ref().resolve(nodes_map) {
          ASTNode::StructuredDocumentation { text, .. } => {
            format!("<span>{}</span>", text.clone())
          }
          _ => {
            println!("Unexpected documentation node: {:?}", doc);
            String::new()
          }
        })
        .unwrap_or_default();
      let virtual_str = if *virtual_ {
        format!("{} ", formatting::format_keyword("virtual"))
      } else {
        String::new()
      };
      let visibility_str =
        formatting::format_keyword(&function_visibility_to_string(visibility));
      let mutability = format!(
        " {}",
        formatting::format_keyword(&function_mutability_to_string(
          state_mutability
        ))
      );
      let kind_name_str = if name.is_empty() {
        format!(
          " {}",
          formatting::format_topic_keyword(
            &new_node_topic(node_id),
            &function_kind_to_string(kind),
            &new_node_topic(declaration_id)
          )
        )
      } else {
        let definition_id = if let Some(impl_id) = implementation_declaration {
          impl_id
        } else {
          declaration_id
        };
        format!(
          " {} {}",
          formatting::format_keyword(&function_kind_to_string(kind)),
          formatting::format_topic_function_name(
            &new_node_topic(node_id),
            name,
            &new_node_topic(definition_id)
          )
        )
      };
      let params = format!(
        "{} ",
        do_node_to_source_text(
          parameters,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx
        )
      );
      let modifiers_str = {
        do_node_to_source_text(
          modifiers,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        )
      };
      let returns = match kind {
        FunctionKind::Constructor
        | FunctionKind::Fallback
        | FunctionKind::Receive => String::new(),
        _ => format!(
          "\n{} {} ",
          formatting::format_keyword("returns"),
          do_node_to_source_text(
            return_parameters,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx
          )
        ),
      };

      format!(
        "{}{}{}{}{}{}{}{}",
        documentation,
        virtual_str,
        visibility_str,
        mutability,
        kind_name_str,
        params,
        modifiers_str,
        returns,
      )
    }

    ASTNode::FunctionDefinition {
      signature, body, ..
    } => {
      let sig_str = do_node_to_source_text(
        signature,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );

      if ctx.omit_function_and_modifier_bodies {
        sig_str.to_string()
      } else {
        let body_str = if let Some(b) = body {
          do_node_to_source_text(
            b,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx,
          )
        } else {
          String::new()
        };

        format!("{}{}", sig_str, body_str)
      }
    }

    ASTNode::EventDefinition {
      node_id,
      parameters,
      name,
      ..
    } => {
      let params = do_node_to_source_text(
        parameters,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      format!(
        "{} {}{}",
        formatting::format_keyword("event"),
        format_identifier(
          &new_node_topic(node_id),
          name,
          &new_node_topic(node_id),
          topic_metadata
        ),
        params
      )
    }

    ASTNode::ErrorDefinition {
      node_id,
      parameters,
      name,
      ..
    } => {
      let params = do_node_to_source_text(
        parameters,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      format!(
        "{} {}{}",
        formatting::format_keyword("error"),
        format_identifier(
          &new_node_topic(node_id),
          name,
          &new_node_topic(node_id),
          topic_metadata
        ),
        params
      )
    }

    ASTNode::ModifierSignature {
      node_id,
      name,
      declaration_id,
      parameters,
      virtual_,
      visibility,
      ..
    } => {
      let params = do_node_to_source_text(
        parameters,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      let virtual_str = if *virtual_ {
        format!("{} ", formatting::format_keyword("virtual"))
      } else {
        String::new()
      };
      let visibility_str =
        formatting::format_keyword(&function_visibility_to_string(visibility));

      format!(
        "{}{} {} {}{}",
        virtual_str,
        visibility_str,
        formatting::format_keyword("mod"),
        formatting::format_topic_function_name(
          &new_node_topic(node_id),
          name,
          &new_node_topic(declaration_id)
        ),
        params,
      )
    }

    ASTNode::ModifierDefinition {
      signature, body, ..
    } => {
      let sig_str = do_node_to_source_text(
        signature,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );

      if ctx.omit_function_and_modifier_bodies {
        sig_str.to_string()
      } else {
        let body_str = do_node_to_source_text(
          body,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        );

        format!("{} {}", sig_str, body_str)
      }
    }

    ASTNode::StructDefinition {
      node_id,
      members,
      visibility,
      name,
      ..
    } => {
      let visibility_str =
        formatting::format_keyword(&variable_visibility_to_string(visibility));
      let indent_level = indent_level + 1;
      let member_ctx = Context {
        target_topic: ctx.target_topic,
        omit_variable_declaration_let: true,
        format_parameter_variable_as_signature: ctx
          .format_parameter_variable_as_signature,
        omit_function_and_modifier_bodies: ctx
          .omit_function_and_modifier_bodies,
      };
      let members_str = members
        .iter()
        .map(|m| {
          do_node_to_source_text(
            m,
            indent_level,
            nodes_map,
            topic_metadata,
            &member_ctx,
          )
        })
        .collect::<Vec<_>>()
        .join("\n");
      let trailing_newline = if !members_str.is_empty() { "\n" } else { "" };
      format!(
        "{} {} {} {{{}{}}}",
        visibility_str,
        formatting::format_keyword("struct"),
        format_identifier(
          &new_node_topic(node_id),
          name,
          &new_node_topic(node_id),
          topic_metadata
        ),
        formatting::indent(&members_str, indent_level),
        trailing_newline
      )
    }

    ASTNode::EnumDefinition {
      node_id,
      members,
      name,
      ..
    } => {
      let indent_level = indent_level + 1;

      let members_str = members
        .iter()
        .map(|m| {
          do_node_to_source_text(
            m,
            indent_level,
            nodes_map,
            topic_metadata,
            ctx,
          )
        })
        .collect::<Vec<_>>()
        .join("\n");
      let trailing_newline = if !members_str.is_empty() { "\n" } else { "" };
      format!(
        "{} {} {{{}{}}}",
        formatting::format_keyword("enum"),
        format_identifier(
          &new_node_topic(node_id),
          name,
          &new_node_topic(node_id),
          topic_metadata
        ),
        formatting::indent(&members_str, indent_level),
        trailing_newline
      )
    }

    ASTNode::UserDefinedValueTypeDefinition {
      node_id,
      underlying_type,
      name,
      ..
    } => {
      format!(
        "type {} is {}",
        format_identifier(
          &new_node_topic(node_id),
          name,
          &new_node_topic(node_id),
          topic_metadata
        ),
        do_node_to_source_text(
          underlying_type,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx
        )
      )
    }

    ASTNode::PragmaDirective { literals, .. } => {
      format!(
        "{} {}",
        formatting::format_keyword("pragma"),
        formatting::format_number(&literals.join("."))
      )
    }

    ASTNode::ImportDirective { file, .. } => {
      format!(
        "{} {}",
        formatting::format_keyword("import"),
        formatting::format_string(file)
      )
    }

    ASTNode::UsingForDirective {
      library_name,
      type_name,
      ..
    } => {
      let lib = if let Some(lib_node) = library_name {
        do_node_to_source_text(
          lib_node,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx,
        )
      } else {
        String::new()
      };
      let type_str = if let Some(type_node) = type_name {
        let for_indent_level = indent_level + 1;
        format!(
          "{} {}",
          formatting::indent(
            &formatting::format_keyword("for"),
            for_indent_level
          ),
          do_node_to_source_text(
            type_node,
            for_indent_level,
            nodes_map,
            topic_metadata,
            ctx
          )
        )
      } else {
        String::new()
      };
      format!(
        "{} {}{}",
        formatting::format_keyword("using"),
        lib,
        type_str
      )
    }

    ASTNode::SourceUnit { nodes, .. } => nodes
      .iter()
      .map(|n| {
        do_node_to_source_text(n, indent_level, nodes_map, topic_metadata, ctx)
      })
      .collect::<Vec<_>>()
      .join("\n\n"),

    ASTNode::InheritanceSpecifier { base_name, .. } => do_node_to_source_text(
      base_name,
      indent_level,
      nodes_map,
      topic_metadata,
      ctx,
    ),

    ASTNode::ElementaryTypeName { name, .. } => formatting::format_type(name),

    ASTNode::FunctionTypeName {
      parameter_types,
      return_parameter_types,
      visibility,
      state_mutability,
      ..
    } => {
      let params = do_node_to_source_text(
        parameter_types,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      let visibility_str = function_visibility_to_string(visibility);
      let mutability_str = function_mutability_to_string(state_mutability);
      let returns_str = format!(
        "{} {}",
        formatting::format_keyword("returns"),
        do_node_to_source_text(
          return_parameter_types,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx
        )
      );

      format!(
        "{}{}{}{}{}",
        formatting::format_keyword("fn"),
        params,
        formatting::format_keyword(&visibility_str),
        formatting::format_keyword(&mutability_str),
        returns_str
      )
    }

    ASTNode::ParameterList { parameters, .. } => {
      if parameters.is_empty() {
        String::from("()")
      } else {
        let indent_level = indent_level + 1;
        let param_ctx = Context {
          target_topic: ctx.target_topic,
          omit_variable_declaration_let: true,
          format_parameter_variable_as_signature: ctx
            .format_parameter_variable_as_signature,
          omit_function_and_modifier_bodies: ctx
            .omit_function_and_modifier_bodies,
        };
        let params = parameters
          .iter()
          .map(|p| {
            do_node_to_source_text(
              p,
              indent_level,
              nodes_map,
              topic_metadata,
              &param_ctx,
            )
          })
          .collect::<Vec<_>>()
          .join("\n");
        let trailing_newline = if !params.is_empty() { "\n" } else { "" };
        format!(
          "({}{})",
          formatting::indent(&params, indent_level),
          trailing_newline
        )
      }
    }

    ASTNode::TryCatchClause { .. } => "TryCatchClause placeholder".to_owned(),

    ASTNode::ModifierInvocation {
      modifier_name,
      arguments,
      ..
    } => {
      let name = do_node_to_source_text(
        modifier_name,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      if let Some(args) = arguments {
        if args.is_empty() {
          format!("{}()", name)
        } else {
          let args_str = args
            .iter()
            .map(|a| {
              do_node_to_source_text(
                a,
                indent_level,
                nodes_map,
                topic_metadata,
                ctx,
              )
            })
            .collect::<Vec<_>>()
            .join("\n");
          format!(
            "{}({}\n)",
            name,
            formatting::indent(&args_str, indent_level + 1)
          )
        }
      } else {
        name
      }
    }

    ASTNode::UserDefinedTypeName { path_node, .. } => do_node_to_source_text(
      path_node,
      indent_level,
      nodes_map,
      topic_metadata,
      ctx,
    ),

    ASTNode::ArrayTypeName { base_type, .. } => {
      format!(
        "{}[]",
        do_node_to_source_text(
          base_type,
          indent_level,
          nodes_map,
          topic_metadata,
          ctx
        )
      )
    }

    ASTNode::Mapping {
      key_type,
      key_name,
      value_type,
      value_name,
      ..
    } => {
      let mut key = do_node_to_source_text(
        key_type,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      if let Some(name) = key_name {
        key = format!("{} {}", key, name)
      }
      let mut value = do_node_to_source_text(
        value_type,
        indent_level,
        nodes_map,
        topic_metadata,
        ctx,
      );
      if let Some(name) = value_name {
        value = format!("{} {}", value, name)
      }
      format!(
        "{} ({}\n)",
        formatting::format_keyword("map"),
        formatting::indent(
          &format!("{} {} {}", key, formatting::format_operator("=>"), value),
          indent_level + 1
        )
      )
    }

    ASTNode::StructuredDocumentation { .. } => String::new(),

    ASTNode::Stub { topic, .. } => format!("NodeStub-{}", topic.id()),

    ASTNode::ModifierList { modifiers, .. } => {
      if modifiers.is_empty() {
        String::new()
      } else {
        let indent_level = indent_level + 1;
        let mods = modifiers
          .iter()
          .map(|m| {
            do_node_to_source_text(
              m,
              indent_level,
              nodes_map,
              topic_metadata,
              ctx,
            )
          })
          .collect::<Vec<_>>()
          .join("\n");
        format!(
          "{}{}",
          formatting::format_keyword("mods"),
          formatting::indent(&mods, indent_level)
        )
      }
    }

    ASTNode::Other { .. } => formatting::format_comment("Unknown"),
  };

  formatting::format_node(&node_str, &new_node_topic(&node.node_id()), "node")
}

/// Delimiter pair for a container topic: opening and optional closing HTML strings.
pub struct Delimiter {
  pub opening: String,
  pub closing: Option<String>,
}

/// Returns the formatted delimiter pair for a container topic, or `None` if
/// the node type does not have meaningful token-bearing delimiters.
///
/// Only nodes whose delimiters contain child topic tokens are included:
/// - IfStatement: `if (cond)`
/// - ForStatement: `for (LoopExpr)`
/// - WhileStatement: `while (cond)`
/// - DoWhileStatement: `do` (opening) / `while (cond)` (closing)
pub fn node_to_delimiter(
  node: &ASTNode,
  nodes_map: &BTreeMap<topic::Topic, core::Node>,
  topic_metadata: &BTreeMap<topic::Topic, core::TopicMetadata>,
) -> Option<Delimiter> {
  let ctx = Context {
    target_topic: new_node_topic(&node.node_id()),
    omit_variable_declaration_let: false,
    format_parameter_variable_as_signature: false,
    omit_function_and_modifier_bodies: false,
  };
  do_node_to_delimiter(node, nodes_map, topic_metadata, &ctx)
}

fn do_node_to_delimiter(
  node: &ASTNode,
  nodes_map: &BTreeMap<topic::Topic, core::Node>,
  topic_metadata: &BTreeMap<topic::Topic, core::TopicMetadata>,
  ctx: &Context,
) -> Option<Delimiter> {
  let delimiter = match node.resolve(nodes_map) {
    ASTNode::IfStatement {
      node_id, condition, ..
    } => {
      let cond_placeholder = maybe_identifier_placeholder(condition);
      let cond =
        do_node_to_source_text(condition, 0, nodes_map, topic_metadata, ctx);
      let opening = format!(
        "{}{} ({}{}\n) {}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "if",
          &new_node_topic(node_id)
        ),
        cond_placeholder,
        formatting::indent(&cond, 1),
        formatting::format_brace("{", 0),
      );
      Some(Delimiter {
        opening,
        closing: Some(formatting::format_brace("}", 0)),
      })
    }

    ASTNode::ForStatement {
      node_id, condition, ..
    } => {
      let loop_expr =
        do_node_to_source_text(condition, 0, nodes_map, topic_metadata, ctx);
      let opening = format!(
        "{}{} ({}\n) {}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "for",
          &new_node_topic(node_id)
        ),
        formatting::indent(&loop_expr, 1),
        formatting::format_brace("{", 0),
      );
      Some(Delimiter {
        opening,
        closing: Some(formatting::format_brace("}", 0)),
      })
    }

    ASTNode::WhileStatement {
      node_id, condition, ..
    } => {
      let cond_placeholder = maybe_identifier_placeholder(condition);
      let cond =
        do_node_to_source_text(condition, 0, nodes_map, topic_metadata, ctx);
      let opening = format!(
        "{}{} ({}\n) {}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "while",
          &new_node_topic(node_id)
        ),
        formatting::indent(&format!("{}{}", cond_placeholder, &cond), 1),
        formatting::format_brace("{", 0),
      );
      Some(Delimiter {
        opening,
        closing: Some(formatting::format_brace("}", 0)),
      })
    }

    ASTNode::DoWhileStatement {
      node_id, condition, ..
    } => {
      let cond_placeholder = maybe_identifier_placeholder(condition);
      let cond =
        do_node_to_source_text(condition, 0, nodes_map, topic_metadata, ctx);
      let opening = format!(
        "{}{} {}",
        formatting::format_statement_placeholder(&new_node_topic(node_id)),
        formatting::format_topic_keyword(
          &new_node_topic(node_id),
          "do",
          &new_node_topic(node_id),
        ),
        formatting::format_brace("{", 0),
      );
      let closing = format!(
        "{} {} ({}\n)",
        formatting::format_brace("}", 0),
        formatting::format_keyword("while"),
        formatting::indent(&format!("{}{}", cond_placeholder, cond), 1),
      );
      Some(Delimiter {
        opening,
        closing: Some(closing),
      })
    }

    _ => None,
  };

  delimiter.map(|d| Delimiter {
    opening: format!("<pre><code>{}</code></pre>", d.opening),
    closing: d.closing.map(|c| format!("<pre><code>{}</code></pre>", c)),
  })
}

// This function is used in many places where the node type is already known
// because this function has to exist for the times when the identifier is a
// reference and the node type is not already known, and duplicating
// identifier formatting logic could lead to inconsistencies.
fn format_identifier(
  id_topic: &topic::Topic,
  name: &str,
  ref_topic: &topic::Topic,
  topic_metadata: &BTreeMap<topic::Topic, core::TopicMetadata>,
) -> String {
  match topic_metadata.get(ref_topic) {
    Some(TopicMetadata::NamedTopic { kind, .. }) => {
      formatting::format_named_identifier(id_topic, name, ref_topic, kind)
    }
    _ => formatting::format_topic_token(id_topic, name, "unknown", ref_topic),
  }
}

fn binary_operator_to_string(op: &BinaryOperator) -> String {
  match op {
    BinaryOperator::Add => "+",
    BinaryOperator::Subtract => "-",
    BinaryOperator::Multiply => "*",
    BinaryOperator::Divide => "/",
    BinaryOperator::Modulo => "%",
    BinaryOperator::Power => "**",
    BinaryOperator::Equal => "==",
    BinaryOperator::NotEqual => "!=",
    BinaryOperator::LessThan => "<",
    BinaryOperator::LessThanOrEqual => "<=",
    BinaryOperator::GreaterThan => ">",
    BinaryOperator::GreaterThanOrEqual => ">=",
    BinaryOperator::And => "&&",
    BinaryOperator::Or => "||",
    BinaryOperator::BitwiseAnd => "&",
    BinaryOperator::BitwiseOr => "|",
    BinaryOperator::BitwiseXor => "^",
    BinaryOperator::LeftShift => "<<",
    BinaryOperator::RightShift => ">>",
  }
  .to_string()
}

fn is_math_operator(op: &BinaryOperator) -> bool {
  matches!(
    op,
    BinaryOperator::Add
      | BinaryOperator::Subtract
      | BinaryOperator::Multiply
      | BinaryOperator::Divide
      | BinaryOperator::Modulo
      | BinaryOperator::Power
      | BinaryOperator::BitwiseAnd
      | BinaryOperator::BitwiseOr
      | BinaryOperator::BitwiseXor
      | BinaryOperator::LeftShift
      | BinaryOperator::RightShift
  )
}

fn is_boolean_and_or_operator(op: &BinaryOperator) -> bool {
  matches!(op, BinaryOperator::And | BinaryOperator::Or)
}

fn assignment_operator_to_string(op: &AssignmentOperator) -> String {
  match op {
    AssignmentOperator::Assign => "=",
    AssignmentOperator::AddAssign => "+=",
    AssignmentOperator::SubtractAssign => "-=",
    AssignmentOperator::MultiplyAssign => "*=",
    AssignmentOperator::DivideAssign => "/=",
    AssignmentOperator::ModuloAssign => "%=",
    AssignmentOperator::BitwiseAndAssign => "&=",
    AssignmentOperator::BitwiseOrAssign => "|=",
    AssignmentOperator::BitwiseXorAssign => "^=",
    AssignmentOperator::LeftShiftAssign => "<<=",
    AssignmentOperator::RightShiftAssign => ">>=",
  }
  .to_string()
}

fn unary_operator_to_string(op: &UnaryOperator) -> String {
  match op {
    UnaryOperator::Increment => "++",
    UnaryOperator::Decrement => "--",
    UnaryOperator::Plus => "+",
    UnaryOperator::Minus => "-",
    UnaryOperator::BitwiseNot => "~",
    UnaryOperator::Not => "!",
    UnaryOperator::Delete => "del ",
  }
  .to_string()
}

fn contract_kind_to_string(kind: &ContractKind) -> String {
  match kind {
    ContractKind::Contract => "contract",
    ContractKind::Library => "library",
    ContractKind::Abstract => "abstract",
    ContractKind::Interface => "interface",
  }
  .to_string()
}

fn function_kind_to_string(kind: &FunctionKind) -> String {
  match kind {
    FunctionKind::Constructor => "constructor",
    FunctionKind::Function => "fn",
    FunctionKind::Fallback => "fallback",
    FunctionKind::Receive => "receive",
    FunctionKind::FreeFunction => "fn",
  }
  .to_string()
}

fn function_visibility_to_string(visibility: &FunctionVisibility) -> String {
  match visibility {
    FunctionVisibility::Public => "pub",
    FunctionVisibility::Private => "priv",
    FunctionVisibility::Internal => "int",
    FunctionVisibility::External => "ext",
  }
  .to_string()
}

fn variable_visibility_to_string(visibility: &VariableVisibility) -> String {
  match visibility {
    VariableVisibility::Public => "pub",
    VariableVisibility::Private => "priv",
    VariableVisibility::Internal => "int",
  }
  .to_string()
}

fn function_mutability_to_string(
  mutability: &FunctionStateMutability,
) -> String {
  match mutability {
    FunctionStateMutability::Pure => "pure",
    FunctionStateMutability::View => "view",
    FunctionStateMutability::NonPayable => "nonpay",
    FunctionStateMutability::Payable => "pay",
  }
  .to_string()
}

fn variable_mutability_to_string(mutability: &VariableMutability) -> String {
  match mutability {
    VariableMutability::Mutable => "mut",
    VariableMutability::Immutable => "immut",
    VariableMutability::Constant => "const",
  }
  .to_string()
}

fn storage_location_to_string(location: &StorageLocation) -> String {
  match location {
    StorageLocation::Default => "",
    StorageLocation::Storage => "storage",
    StorageLocation::Memory => "memory",
    StorageLocation::Calldata => "calldata",
  }
  .to_string()
}

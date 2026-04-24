//! Structural delimiter data for control-flow statements.
//!
//! Exposes the data the UI layer needs to render opening/closing delimiters
//! (for `if`, `for`, `while`, `do-while`) without requiring any HTML
//! knowledge inside this crate.

use crate::domain::topic::{self, new_node_topic};
use crate::solidity::ast::ASTNode;
use serde::Serialize;

/// Kind of control-flow statement that carries renderable delimiters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelimiterKind {
  If,
  For,
  While,
  DoWhile,
}

/// Structural information needed to render a control-flow statement's
/// delimiters. Topic fields point at the statement itself and its
/// condition expression, leaving the presentation choice (tokens,
/// whitespace, HTML) to the consumer.
#[derive(Debug, Clone, Serialize)]
pub struct DelimiterInfo {
  pub kind: DelimiterKind,
  pub node_topic: topic::Topic,
  pub condition_topic: topic::Topic,
}

/// Returns the structural delimiter info for a control-flow AST node, or
/// `None` for nodes that don't carry meaningful delimiters.
pub fn delimiter_info_for_node(node: &ASTNode) -> Option<DelimiterInfo> {
  let (kind, node_id, condition) = match node {
    ASTNode::IfStatement {
      node_id, condition, ..
    } => (DelimiterKind::If, node_id, condition),
    ASTNode::ForStatement {
      node_id, condition, ..
    } => (DelimiterKind::For, node_id, condition),
    ASTNode::WhileStatement {
      node_id, condition, ..
    } => (DelimiterKind::While, node_id, condition),
    ASTNode::DoWhileStatement {
      node_id, condition, ..
    } => (DelimiterKind::DoWhile, node_id, condition),
    _ => return None,
  };

  Some(DelimiterInfo {
    kind,
    node_topic: new_node_topic(node_id),
    condition_topic: new_node_topic(&condition.node_id()),
  })
}

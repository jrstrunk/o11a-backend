use crate::formatting;
use o11a_core::collaborator::parser::{self, CommentNode};
use o11a_core::core;
use o11a_core::core::topic;
use std::collections::BTreeMap;

/// Renders comment AST nodes to HTML, wrapped in a topic block with the
/// comment's topic data attribute.
pub fn render_comment_html(
  nodes: &[CommentNode],
  comment_topic: &topic::Topic,
  nodes_map: &BTreeMap<topic::Topic, core::Node>,
) -> String {
  let content: String = nodes
    .iter()
    .map(|node| node_to_html(node, comment_topic, nodes_map))
    .collect();

  formatting::format_topic_block(
    comment_topic,
    &content,
    "comment-root target-topic",
    comment_topic,
  )
}

fn node_to_html(
  node: &CommentNode,
  comment_topic: &topic::Topic,
  nodes_map: &BTreeMap<topic::Topic, core::Node>,
) -> String {
  match node {
    CommentNode::Text { value } => formatting::html_escape(value),

    CommentNode::InlineCode { children, .. } => {
      let inner: String = children
        .iter()
        .map(|c| node_to_html(c, comment_topic, nodes_map))
        .collect();
      formatting::format_inline_code(&inner)
    }

    CommentNode::CodeKeyword { value } => {
      formatting::format_keyword(&formatting::html_escape(value))
    }

    CommentNode::CodeOperator { value } => formatting::format_operator(value),

    CommentNode::CodeIdentifier {
      value,
      referenced_topic,
      kind,
      referenced_name,
      ..
    } => {
      let display_value = referenced_name.as_deref().unwrap_or(value);
      let class = kind
        .as_ref()
        .map(formatting::named_topic_kind_to_class)
        .unwrap_or("unknown");

      match referenced_topic {
        Some(ref_topic) => format!(
          "`{}`",
          formatting::format_topic_token(
            comment_topic,
            &formatting::html_escape(display_value),
            class,
            ref_topic,
          )
        ),
        None => formatting::format_token(
          &formatting::html_escape(display_value),
          class,
        ),
      }
    }

    CommentNode::CodeText { value } => formatting::html_escape(value),

    CommentNode::Emphasis { text } => {
      formatting::format_emphasis(&formatting::html_escape(text))
    }

    CommentNode::Strong { text } => {
      formatting::format_strong(&formatting::html_escape(text))
    }

    CommentNode::Link { url, text } => {
      formatting::format_link(url, None, &formatting::html_escape(text))
    }
  }
}

/// Parse a description string with the comment markdown parser and render
/// to inline HTML with code reference resolution. Does not wrap in a block.
pub fn render_description_html(
  text: &str,
  owner_topic: &topic::Topic,
  audit_data: &core::AuditData,
) -> String {
  let (_referenced_topics, nodes) = parser::parse_comment(text, audit_data);
  nodes
    .iter()
    .map(|node| node_to_html(node, owner_topic, &audit_data.nodes))
    .collect()
}

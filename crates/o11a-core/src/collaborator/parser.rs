use crate::code_refs as doc_parser;
use crate::core;
use crate::core::topic::Topic;
use serde::{Deserialize, Serialize};

/// Inline-only AST node for comment content.
/// Only supports: text, inline code (with tokenization), emphasis, strong, links.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CommentNode {
  Text {
    value: String,
  },
  InlineCode {
    value: String,
    children: Vec<CommentNode>,
  },
  CodeKeyword {
    value: String,
  },
  CodeOperator {
    value: String,
  },
  CodeIdentifier {
    value: String,
    referenced_topic: Option<Topic>,
    kind: Option<core::NamedTopicKind>,
    referenced_name: Option<String>,
  },
  CodeText {
    value: String,
  },
  Emphasis {
    text: String,
  },
  Strong {
    text: String,
  },
  Link {
    url: String,
    text: String,
  },
}

/// Parses comment content into inline-only AST nodes and extracts mentions.
///
/// Uses a custom character-by-character scanner that recognizes:
/// - Backtick-delimited inline code (with code reference resolution)
/// - `**bold**`
/// - `*italic*`
/// - `[text](url)` links
///
/// Anything that doesn't match these patterns is preserved as-is in Text nodes.
pub fn parse_comment(
  content: &str,
  audit_data: &core::AuditData,
) -> (Vec<Topic>, Vec<CommentNode>) {
  let nodes = scan_inline_nodes(content, audit_data);
  let nodes = doc_parser::split_text_code_references(
    nodes,
    |node| match node {
      CommentNode::Text { value } => Some(value.as_str()),
      _ => None,
    },
    |value| CommentNode::Text { value },
    |code_str| CommentNode::InlineCode {
      value: code_str.to_string(),
      children: tokenize_comment_code(code_str, audit_data),
    },
  );

  let mut mentions = Vec::new();
  for node in &nodes {
    collect_mentions(node, &mut mentions);
  }
  mentions.sort_unstable();
  mentions.dedup();

  (mentions, nodes)
}

/// Collects referenced topics from CodeIdentifier nodes.
fn collect_mentions(node: &CommentNode, out: &mut Vec<Topic>) {
  match node {
    CommentNode::CodeIdentifier {
      referenced_topic: Some(t),
      ..
    } => {
      out.push(*t);
    }
    CommentNode::InlineCode { children, .. } => {
      for child in children {
        collect_mentions(child, out);
      }
    }
    _ => {}
  }
}

/// Scans input text left-to-right, producing inline CommentNode tokens.
fn scan_inline_nodes(
  input: &str,
  audit_data: &core::AuditData,
) -> Vec<CommentNode> {
  let mut nodes = Vec::new();
  let mut text_buf = String::new();
  let bytes = input.as_bytes();
  let len = bytes.len();
  let mut i = 0;

  while i < len {
    let b = bytes[i];

    // Backtick: inline code
    if b == b'`'
      && let Some((code, end)) = scan_backtick(input, i)
    {
      flush_text(&mut text_buf, &mut nodes);
      let children = tokenize_comment_code(code, audit_data);
      nodes.push(CommentNode::InlineCode {
        value: code.to_string(),
        children,
      });
      i = end;
      continue;
    }

    // `**`: strong
    if b == b'*'
      && i + 1 < len
      && bytes[i + 1] == b'*'
      && let Some((inner, end)) = scan_double_star(input, i)
      && !inner.is_empty()
    {
      flush_text(&mut text_buf, &mut nodes);
      nodes.push(CommentNode::Strong {
        text: inner.to_string(),
      });
      i = end;
      continue;
    }

    // `*`: emphasis (single, not `**`)
    if b == b'*'
      && !(i + 1 < len && bytes[i + 1] == b'*')
      && let Some((inner, end)) = scan_single_star(input, i)
      && !inner.is_empty()
    {
      flush_text(&mut text_buf, &mut nodes);
      nodes.push(CommentNode::Emphasis {
        text: inner.to_string(),
      });
      i = end;
      continue;
    }

    // `[text](url)`: link
    if b == b'['
      && let Some((text, url, end)) = scan_link(input, i)
    {
      flush_text(&mut text_buf, &mut nodes);
      nodes.push(CommentNode::Link {
        text: text.to_string(),
        url: url.to_string(),
      });
      i = end;
      continue;
    }

    // Default: accumulate text
    // Advance by one character (not one byte)
    let ch = input[i..].chars().next().unwrap();
    text_buf.push(ch);
    i += ch.len_utf8();
  }

  flush_text(&mut text_buf, &mut nodes);
  nodes
}

/// Flush the text buffer into a Text node if non-empty.
fn flush_text(buf: &mut String, nodes: &mut Vec<CommentNode>) {
  if !buf.is_empty() {
    nodes.push(CommentNode::Text { value: buf.clone() });
    buf.clear();
  }
}

/// Scan for closing backtick starting at position `start` (which is the opening backtick).
/// Returns (inner_text, position_after_closing_backtick).
fn scan_backtick(input: &str, start: usize) -> Option<(&str, usize)> {
  let after = start + 1;
  let rest = &input[after..];
  let close = rest.find('`')?;
  Some((&rest[..close], after + close + 1))
}

/// Scan for closing `**` starting at position `start` (which is the first `*` of `**`).
/// Returns (inner_text, position_after_closing_double_star).
fn scan_double_star(input: &str, start: usize) -> Option<(&str, usize)> {
  let after = start + 2;
  if after > input.len() {
    return None;
  }
  let rest = &input[after..];
  let close = rest.find("**")?;
  Some((&rest[..close], after + close + 2))
}

/// Scan for closing `*` (single) starting at position `start`.
/// The closing `*` must not be followed by another `*`.
/// Returns (inner_text, position_after_closing_star).
fn scan_single_star(input: &str, start: usize) -> Option<(&str, usize)> {
  let after = start + 1;
  if after > input.len() {
    return None;
  }
  let rest = &input[after..];
  let bytes = rest.as_bytes();
  for j in 0..bytes.len() {
    if bytes[j] == b'*' {
      // Make sure this isn't part of `**`
      if j + 1 < bytes.len() && bytes[j + 1] == b'*' {
        continue;
      }
      // Also make sure the previous char isn't `*` (i.e. we're not at the second star of `**`)
      if j > 0 && bytes[j - 1] == b'*' {
        continue;
      }
      return Some((&rest[..j], after + j + 1));
    }
  }
  None
}

/// Scan for `[text](url)` pattern starting at position `start` (the `[`).
/// Returns (text, url, position_after_closing_paren).
fn scan_link(input: &str, start: usize) -> Option<(&str, &str, usize)> {
  let after_bracket = start + 1;
  let rest = &input[after_bracket..];

  // Find closing `]`
  let close_bracket = rest.find(']')?;
  let text = &rest[..close_bracket];

  // `(` must immediately follow `]`
  let paren_start = after_bracket + close_bracket + 1;
  if paren_start >= input.len() || input.as_bytes()[paren_start] != b'(' {
    return None;
  }

  // Find closing `)`
  let url_start = paren_start + 1;
  let url_rest = &input[url_start..];
  let close_paren = url_rest.find(')')?;
  let url = &url_rest[..close_paren];

  Some((text, url, url_start + close_paren + 1))
}

/// Tokenizes code content into keywords, operators, identifiers, and text.
/// Reuses shared tokenization helpers from the documentation parser.
fn tokenize_comment_code(
  code: &str,
  audit_data: &core::AuditData,
) -> Vec<CommentNode> {
  let mut tokens = Vec::new();
  let mut chars = code.char_indices().peekable();
  let mut text_buffer = String::new();

  while let Some((idx, c)) = chars.next() {
    let remaining = &code[idx..];

    // Check for operator
    if let Some(op) = doc_parser::match_operator(remaining) {
      if !text_buffer.is_empty() {
        tokens.push(CommentNode::CodeText {
          value: text_buffer.clone(),
        });
        text_buffer.clear();
      }
      tokens.push(CommentNode::CodeOperator {
        value: op.to_string(),
      });
      for _ in 1..op.len() {
        chars.next();
      }
      continue;
    }

    // Check for identifier start
    if c.is_ascii_alphabetic() || c == '_' {
      if !text_buffer.is_empty() {
        tokens.push(CommentNode::CodeText {
          value: text_buffer.clone(),
        });
        text_buffer.clear();
      }

      let mut ident = String::new();
      ident.push(c);
      while let Some(&(_, next_c)) = chars.peek() {
        if next_c.is_ascii_alphanumeric() || next_c == '_' {
          ident.push(next_c);
          chars.next();
        } else {
          break;
        }
      }

      if doc_parser::is_keyword(&ident) {
        tokens.push(CommentNode::CodeKeyword { value: ident });
      } else {
        let (referenced_topic, kind, referenced_name) = if let Some(metadata) =
          doc_parser::find_declaration_by_name(audit_data, &ident)
        {
          (
            Some(*metadata.topic()),
            doc_parser::get_named_topic_kind(metadata),
            metadata.name().map(|n| n.to_string()),
          )
        } else {
          (None, None, None)
        };

        tokens.push(CommentNode::CodeIdentifier {
          value: ident,
          referenced_topic,
          kind,
          referenced_name,
        });
      }
      continue;
    }

    text_buffer.push(c);
  }

  if !text_buffer.is_empty() {
    tokens.push(CommentNode::CodeText { value: text_buffer });
  }

  tokens
}

/// Renders comment AST nodes to plain text (raw markdown without semantics).
/// This is the non-HTML rendering path used by agent prompt construction and
/// other core consumers that need a string form of the comment content.
pub fn render_comment_plain_text(nodes: &[CommentNode]) -> String {
  nodes.iter().map(node_to_plain_text).collect()
}

fn node_to_plain_text(node: &CommentNode) -> String {
  match node {
    CommentNode::Text { value } => value.clone(),
    CommentNode::InlineCode { value, .. } => format!("`{}`", value),
    CommentNode::CodeKeyword { value }
    | CommentNode::CodeOperator { value }
    | CommentNode::CodeText { value } => value.clone(),
    CommentNode::CodeIdentifier {
      value,
      referenced_name,
      ..
    } => referenced_name.as_deref().unwrap_or(value).to_string(),
    CommentNode::Emphasis { text } => format!("*{}*", text),
    CommentNode::Strong { text } => format!("**{}**", text),
    CommentNode::Link { text, url } => format!("[{}]({})", text, url),
  }
}

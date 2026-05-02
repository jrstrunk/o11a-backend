//! Shared text/code-reference helpers used by both comment parsing
//! (in `collaborator::parser`) and documentation parsing (in
//! `o11a-analyze`'s documentation parser). These live in core because
//! the comment parser, which runs at server-time on user-authored
//! prose, depends on them.

use crate::domain;
use crate::domain::topic;
use regex::Regex;
use std::sync::LazyLock;

/// Solidity keywords for syntax highlighting
pub const SOLIDITY_KEYWORDS: &[&str] = &[
  // Control flow
  "if",
  "else",
  "for",
  "while",
  "do",
  "break",
  "continue",
  "return",
  "try",
  "catch",
  "revert",
  "throw",
  // Function/modifier
  "function",
  "modifier",
  "constructor",
  "fallback",
  "receive",
  "returns",
  // Visibility
  "public",
  "private",
  "internal",
  "external",
  // Mutability
  "pure",
  "view",
  "payable",
  "constant",
  "immutable",
  // Storage
  "memory",
  "storage",
  "calldata",
  // Contract structure
  "contract",
  "interface",
  "library",
  "abstract",
  "is",
  "using",
  "import",
  "pragma",
  // Types
  "mapping",
  "struct",
  "enum",
  "event",
  "error",
  "type",
  // Literals/values
  "true",
  "false",
  // Other
  "new",
  "delete",
  "emit",
  "indexed",
  "anonymous",
  "virtual",
  "override",
  "assembly",
];

/// Rust keywords for syntax highlighting
pub const RUST_KEYWORDS: &[&str] = &[
  // Control flow
  "if",
  "else",
  "for",
  "while",
  "loop",
  "break",
  "continue",
  "return",
  "match",
  // Function/module
  "fn",
  "mod",
  "use",
  "pub",
  "crate",
  "self",
  "super",
  "impl",
  "trait",
  "where",
  // Types
  "struct",
  "enum",
  "type",
  "const",
  "static",
  "let",
  "mut",
  "ref",
  "move",
  // Async
  "async",
  "await",
  // Other
  "as",
  "dyn",
  "extern",
  "in",
  "unsafe",
  "macro_rules",
];

/// Operators for syntax highlighting (multi-character first, then single-character)
pub const OPERATORS: &[&str] = &[
  // Multi-character (longest first)
  "<<=", ">>=", "..=", "...", "==", "!=", "<=", ">=", "&&", "||", "<<", ">>",
  "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "++", "--", "**", "=>", "::",
  "->", "..", // Single-character
  "+", "-", "*", "/", "%", "=", "<", ">", "&", "|", "^", "!", "~", "?", ":",
  ";", ".", ",",
];

/// Extracts the NamedTopicKind from topic metadata if it's a named topic
pub fn get_named_topic_kind(
  metadata: &domain::TopicMetadata,
) -> Option<domain::NamedTopicKind> {
  match metadata {
    domain::TopicMetadata::NamedTopic { kind, .. } => Some(kind.clone()),
    domain::TopicMetadata::UnnamedTopic { .. }
    | domain::TopicMetadata::ControlFlow { .. }
    | domain::TopicMetadata::TitledTopic { .. }
    | domain::TopicMetadata::CommentTopic { .. }
    | domain::TopicMetadata::FeatureTopic { .. }
    | domain::TopicMetadata::RequirementTopic { .. }
    | domain::TopicMetadata::BehaviorTopic { .. }
    | domain::TopicMetadata::FunctionalSemanticTopic { .. }
    | domain::TopicMetadata::ThreatTopic { .. }
    | domain::TopicMetadata::InvariantTopic { .. }
    | domain::TopicMetadata::DocumentationTopic { .. } => None,
  }
}

/// Checks if a string is a keyword (Solidity or Rust)
pub fn is_keyword(s: &str) -> bool {
  SOLIDITY_KEYWORDS.contains(&s) || RUST_KEYWORDS.contains(&s)
}

/// Tries to match an operator at the current position
pub fn match_operator(s: &str) -> Option<&'static str> {
  OPERATORS
    .iter()
    .find(|&op| s.starts_with(op))
    .map(|v| v as _)
}

/// Regex matching code-like identifiers in prose text:
///   - camelCase (e.g., balanceOf, getOwner)
///   - snake_case (e.g., total_supply, _owner)
///   - SCREAMING_SNAKE_CASE (e.g., MAX_SUPPLY, ADMIN_ROLE)
///
/// Optionally followed by () to capture function-call style references.
static CODE_REF_RE: LazyLock<Regex> = LazyLock::new(|| {
  Regex::new(
    r"\b(?:[a-z][a-zA-Z0-9]*[A-Z][a-zA-Z0-9]*|[a-z_][a-z0-9]*(?:_[a-z0-9]+)+|[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+)(?:\(\))?",
  )
  .unwrap()
});

/// Finds code references in a text string.
/// Returns (start, end) byte offsets for each match.
pub fn find_code_references(text: &str) -> Vec<(usize, usize)> {
  CODE_REF_RE
    .find_iter(text)
    .map(|m| (m.start(), m.end()))
    .collect()
}

/// Splits nodes containing text with code references into alternating
/// text and inline-code nodes. Generic over any node type via
/// extractor and constructor closures.
pub fn split_text_code_references<T>(
  nodes: Vec<T>,
  get_text: impl Fn(&T) -> Option<&str>,
  make_text: impl Fn(String) -> T,
  make_inline_code: impl Fn(&str) -> T,
) -> Vec<T> {
  let mut result = Vec::new();
  for node in nodes {
    let text_value = get_text(&node).map(|s| s.to_string());
    match text_value {
      Some(value) => {
        let refs = find_code_references(&value);
        if refs.is_empty() {
          result.push(node);
        } else {
          let mut last_end = 0;
          for (start, end) in refs {
            if start > last_end {
              let before = &value[last_end..start];
              if !before.is_empty() {
                result.push(make_text(before.to_string()));
              }
            }
            result.push(make_inline_code(&value[start..end]));
            last_end = end;
          }
          if last_end < value.len() {
            let after = &value[last_end..];
            if !after.is_empty() {
              result.push(make_text(after.to_string()));
            }
          }
        }
      }
      None => result.push(node),
    }
  }
  result
}

/// Searches the AuditData for a declaration with the given value.
/// Search order: topic ID, qualified name, then simple name.
/// Used to resolve inline code references to solidity declarations.
pub fn find_declaration_by_name<'a>(
  audit_data: &'a domain::AuditData,
  value: &str,
) -> Option<&'a domain::TopicMetadata> {
  match topic::parse_topic(value) {
    // If this is a valid topic string, use it directly
    Ok(topic) => audit_data.topic_metadata.get(&topic),
    // Otherwise, fall back to searching by qualified or simple name
    Err(..) => audit_data
      .name_index
      .get_by_qualified_name(value)
      .and_then(|t| audit_data.topic_metadata.get(t))
      .or_else(|| {
        audit_data
          .name_index
          .get_by_simple_name(value)
          .and_then(|t| audit_data.topic_metadata.get(t))
      }),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  // -- helpers for split_text_code_references tests --

  #[derive(Debug, Clone, PartialEq)]
  enum TestNode {
    Text(String),
    Code(String),
    Other,
  }

  fn split(nodes: Vec<TestNode>) -> Vec<TestNode> {
    split_text_code_references(
      nodes,
      |n| match n {
        TestNode::Text(s) => Some(s.as_str()),
        _ => None,
      },
      TestNode::Text,
      |s| TestNode::Code(s.to_string()),
    )
  }

  // -- find_code_references: camelCase --

  #[test]
  fn camel_case_basic() {
    let refs = find_code_references("The balanceOf function");
    assert_eq!(refs.len(), 1);
    assert_eq!(&"The balanceOf function"[refs[0].0..refs[0].1], "balanceOf");
  }

  #[test]
  fn camel_case_with_parens() {
    let refs = find_code_references("Call balanceOf() here");
    assert_eq!(refs.len(), 1);
    assert_eq!(
      &"Call balanceOf() here"[refs[0].0..refs[0].1],
      "balanceOf()"
    );
  }

  #[test]
  fn camel_case_multiple() {
    let refs = find_code_references("Use getOwner and balanceOf for this");
    assert_eq!(refs.len(), 2);
    let text = "Use getOwner and balanceOf for this";
    assert_eq!(&text[refs[0].0..refs[0].1], "getOwner");
    assert_eq!(&text[refs[1].0..refs[1].1], "balanceOf");
  }

  // -- find_code_references: snake_case --

  #[test]
  fn snake_case_basic() {
    let refs = find_code_references("The total_supply is stored");
    assert_eq!(refs.len(), 1);
    assert_eq!(
      &"The total_supply is stored"[refs[0].0..refs[0].1],
      "total_supply"
    );
  }

  #[test]
  fn snake_case_with_leading_underscore() {
    let refs = find_code_references("The _internal_value is private");
    assert_eq!(refs.len(), 1);
    assert_eq!(
      &"The _internal_value is private"[refs[0].0..refs[0].1],
      "_internal_value"
    );
  }

  #[test]
  fn snake_case_with_parens() {
    let refs = find_code_references("The collect_fees() function");
    assert_eq!(refs.len(), 1);
    assert_eq!(
      &"The collect_fees() function"[refs[0].0..refs[0].1],
      "collect_fees()"
    );
  }

  // -- find_code_references: SCREAMING_SNAKE_CASE --

  #[test]
  fn screaming_snake_basic() {
    let refs = find_code_references("The ADMIN_ROLE is required");
    assert_eq!(refs.len(), 1);
    assert_eq!(
      &"The ADMIN_ROLE is required"[refs[0].0..refs[0].1],
      "ADMIN_ROLE"
    );
  }

  #[test]
  fn screaming_snake_with_numbers() {
    let refs = find_code_references("Use MAX_UINT256 as the cap");
    assert_eq!(refs.len(), 1);
    assert_eq!(
      &"Use MAX_UINT256 as the cap"[refs[0].0..refs[0].1],
      "MAX_UINT256"
    );
  }

  #[test]
  fn screaming_snake_multiple_underscores() {
    let refs = find_code_references("The DEFAULT_ADMIN_ROLE is special");
    assert_eq!(refs.len(), 1);
    assert_eq!(
      &"The DEFAULT_ADMIN_ROLE is special"[refs[0].0..refs[0].1],
      "DEFAULT_ADMIN_ROLE"
    );
  }

  // -- find_code_references: non-matches --

  #[test]
  fn plain_english_no_match() {
    assert!(find_code_references("The quick brown fox").is_empty());
  }

  #[test]
  fn single_word_no_match() {
    // All-lowercase single word
    assert!(find_code_references("hello").is_empty());
    // All-uppercase single word (no underscore)
    assert!(find_code_references("HELLO").is_empty());
    // Capitalized word (PascalCase without a second hump)
    assert!(find_code_references("Hello").is_empty());
  }

  #[test]
  fn all_caps_no_underscore_no_match() {
    // "API", "URL", etc. should not match
    assert!(find_code_references("The API returns JSON").is_empty());
  }

  #[test]
  fn number_only_no_match() {
    assert!(find_code_references("Use 12345 as input").is_empty());
  }

  // -- find_code_references: edge cases --

  #[test]
  fn reference_at_start_of_string() {
    let refs = find_code_references("balanceOf is a function");
    assert_eq!(refs.len(), 1);
    assert_eq!(
      &"balanceOf is a function"[refs[0].0..refs[0].1],
      "balanceOf"
    );
  }

  #[test]
  fn reference_at_end_of_string() {
    let refs = find_code_references("Check the ADMIN_ROLE");
    assert_eq!(refs.len(), 1);
    assert_eq!(&"Check the ADMIN_ROLE"[refs[0].0..refs[0].1], "ADMIN_ROLE");
  }

  #[test]
  fn reference_is_entire_string() {
    let refs = find_code_references("balanceOf");
    assert_eq!(refs.len(), 1);
  }

  #[test]
  fn empty_string() {
    assert!(find_code_references("").is_empty());
  }

  #[test]
  fn adjacent_references() {
    // Separated only by a space
    let text = "balanceOf ADMIN_ROLE";
    let refs = find_code_references(text);
    assert_eq!(refs.len(), 2);
    assert_eq!(&text[refs[0].0..refs[0].1], "balanceOf");
    assert_eq!(&text[refs[1].0..refs[1].1], "ADMIN_ROLE");
  }

  #[test]
  fn reference_next_to_punctuation() {
    let text = "Use balanceOf, not getBalance.";
    let refs = find_code_references(text);
    assert_eq!(refs.len(), 2);
    assert_eq!(&text[refs[0].0..refs[0].1], "balanceOf");
    assert_eq!(&text[refs[1].0..refs[1].1], "getBalance");
  }

  #[test]
  fn parentheses_with_args_not_captured() {
    // Only empty () is captured; non-empty parens are not part of the match
    let text = "Call balanceOf(owner) here";
    let refs = find_code_references(text);
    assert_eq!(refs.len(), 1);
    assert_eq!(&text[refs[0].0..refs[0].1], "balanceOf");
  }

  // -- split_text_code_references --

  #[test]
  fn split_no_references() {
    let nodes = vec![TestNode::Text("Hello world".into())];
    assert_eq!(split(nodes), vec![TestNode::Text("Hello world".into())]);
  }

  #[test]
  fn split_single_reference_middle() {
    let nodes = vec![TestNode::Text("The ADMIN_ROLE is required".into())];
    assert_eq!(
      split(nodes),
      vec![
        TestNode::Text("The ".into()),
        TestNode::Code("ADMIN_ROLE".into()),
        TestNode::Text(" is required".into()),
      ]
    );
  }

  #[test]
  fn split_reference_at_start() {
    let nodes = vec![TestNode::Text("balanceOf returns uint".into())];
    assert_eq!(
      split(nodes),
      vec![
        TestNode::Code("balanceOf".into()),
        TestNode::Text(" returns uint".into()),
      ]
    );
  }

  #[test]
  fn split_reference_at_end() {
    let nodes = vec![TestNode::Text("Check the total_supply".into())];
    assert_eq!(
      split(nodes),
      vec![
        TestNode::Text("Check the ".into()),
        TestNode::Code("total_supply".into()),
      ]
    );
  }

  #[test]
  fn split_multiple_references() {
    let nodes = vec![TestNode::Text(
      "The collect_fees() function updates feeBalance and TOTAL_FEES".into(),
    )];
    assert_eq!(
      split(nodes),
      vec![
        TestNode::Text("The ".into()),
        TestNode::Code("collect_fees()".into()),
        TestNode::Text(" function updates ".into()),
        TestNode::Code("feeBalance".into()),
        TestNode::Text(" and ".into()),
        TestNode::Code("TOTAL_FEES".into()),
      ]
    );
  }

  #[test]
  fn split_non_text_nodes_pass_through() {
    let nodes = vec![
      TestNode::Code("existing_code".into()),
      TestNode::Text("The ADMIN_ROLE is set".into()),
      TestNode::Other,
    ];
    assert_eq!(
      split(nodes),
      vec![
        TestNode::Code("existing_code".into()),
        TestNode::Text("The ".into()),
        TestNode::Code("ADMIN_ROLE".into()),
        TestNode::Text(" is set".into()),
        TestNode::Other,
      ]
    );
  }

  #[test]
  fn split_plain_text_unchanged() {
    let nodes = vec![
      TestNode::Text("No code here".into()),
      TestNode::Text("Or here either".into()),
    ];
    assert_eq!(split(nodes.clone()), nodes,);
  }

  #[test]
  fn split_empty_input() {
    let nodes: Vec<TestNode> = vec![];
    assert_eq!(split(nodes), Vec::<TestNode>::new());
  }

  #[test]
  fn split_reference_is_entire_text() {
    let nodes = vec![TestNode::Text("balanceOf".into())];
    assert_eq!(split(nodes), vec![TestNode::Code("balanceOf".into())]);
  }
}

use crate::core;
use crate::core::topic;
use markdown::ParseOptions;
use markdown::mdast::Node as MdNode;
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};

/// Global counter for documentation node IDs
/// This can be used later when processing user-submitted docs
static NEXT_DOC_NODE_ID: AtomicI32 = AtomicI32::new(1);

/// Gets the next documentation node ID
pub fn next_node_id() -> i32 {
  NEXT_DOC_NODE_ID.fetch_add(1, Ordering::SeqCst)
}

/// Solidity keywords for syntax highlighting
pub(crate) const SOLIDITY_KEYWORDS: &[&str] = &[
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
pub(crate) const RUST_KEYWORDS: &[&str] = &[
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
pub(crate) const OPERATORS: &[&str] = &[
  // Multi-character (longest first)
  "<<=", ">>=", "..=", "...", "==", "!=", "<=", ">=", "&&", "||", "<<", ">>",
  "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "++", "--", "**", "=>", "::",
  "->", "..", // Single-character
  "+", "-", "*", "/", "%", "=", "<", ">", "&", "|", "^", "!", "~", "?", ":",
  ";", ".", ",",
];

/// Extracts the NamedTopicKind from topic metadata if it's a named topic
pub(crate) fn get_named_topic_kind(
  metadata: &core::TopicMetadata,
) -> Option<core::NamedTopicKind> {
  match metadata {
    core::TopicMetadata::NamedTopic { kind, .. } => Some(kind.clone()),
    core::TopicMetadata::UnnamedTopic { .. }
    | core::TopicMetadata::ControlFlow { .. }
    | core::TopicMetadata::TitledTopic { .. }
    | core::TopicMetadata::CommentTopic { .. }
    | core::TopicMetadata::FeatureTopic { .. }
    | core::TopicMetadata::RequirementTopic { .. }
    | core::TopicMetadata::BehaviorTopic { .. }
    | core::TopicMetadata::ThreatTopic { .. }
    | core::TopicMetadata::InvariantTopic { .. }
    | core::TopicMetadata::DocumentationTopic { .. } => None,
  }
}

/// Checks if a string is a keyword (Solidity or Rust)
pub(crate) fn is_keyword(s: &str) -> bool {
  SOLIDITY_KEYWORDS.contains(&s) || RUST_KEYWORDS.contains(&s)
}

/// Tries to match an operator at the current position
pub(crate) fn match_operator(s: &str) -> Option<&'static str> {
  for op in OPERATORS {
    if s.starts_with(op) {
      return Some(op);
    }
  }
  None
}

/// Tokenizes code into CodeKeyword, CodeOperator, CodeIdentifier, and CodeText nodes
fn tokenize_code(
  code: &str,
  audit_data: &core::AuditData,
  next_id: &dyn Fn() -> i32,
) -> Vec<DocumentationNode> {
  let mut tokens = Vec::new();
  let mut chars = code.char_indices().peekable();
  let mut text_buffer = String::new();

  while let Some((idx, c)) = chars.next() {
    // Check for operator
    let remaining = &code[idx..];
    if let Some(op) = match_operator(remaining) {
      // Flush text buffer
      if !text_buffer.is_empty() {
        tokens.push(DocumentationNode::CodeText {
          node_id: next_id(),
          value: text_buffer.clone(),
        });
        text_buffer.clear();
      }

      tokens.push(DocumentationNode::CodeOperator {
        node_id: next_id(),
        value: op.to_string(),
      });

      // Skip the operator characters
      for _ in 1..op.len() {
        chars.next();
      }
      continue;
    }

    // Check for identifier start
    if c.is_ascii_alphabetic() || c == '_' {
      // Flush text buffer
      if !text_buffer.is_empty() {
        tokens.push(DocumentationNode::CodeText {
          node_id: next_id(),
          value: text_buffer.clone(),
        });
        text_buffer.clear();
      }

      // Collect the full identifier
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

      if is_keyword(&ident) {
        tokens.push(DocumentationNode::CodeKeyword {
          node_id: next_id(),
          value: ident,
        });
      } else {
        // Try to find a matching declaration
        let (referenced_topic, kind, referenced_name) = if let Some(metadata) =
          find_declaration_by_name(audit_data, &ident)
        {
          (
            Some(metadata.topic().clone()),
            get_named_topic_kind(metadata),
            metadata.name().map(|n| n.to_string()),
          )
        } else {
          (None, None, None)
        };

        tokens.push(DocumentationNode::CodeIdentifier {
          node_id: next_id(),
          value: ident,
          referenced_topic,
          kind,
          referenced_name,
        });
      }
      continue;
    }

    // Everything else goes to text buffer
    text_buffer.push(c);
  }

  // Flush remaining text buffer
  if !text_buffer.is_empty() {
    tokens.push(DocumentationNode::CodeText {
      node_id: next_id(),
      value: text_buffer,
    });
  }

  tokens
}

/// Processes markdown files from src/ and docs/ directories
pub fn process_files(
  project_root: &Path,
  document_files: &[core::ProjectPath],
  audit_data: &core::AuditData,
) -> Result<
  std::collections::BTreeMap<core::ProjectPath, Vec<DocumentationAST>>,
  String,
> {
  let mut ast_map = std::collections::BTreeMap::new();

  for project_path in document_files {
    let file_path = project_root.join(&project_path.file_path);

    if !file_path.exists() || !file_path.is_file() {
      return Err(format!(
        "Document file not found: {} (listed in documents.txt)",
        project_path.file_path
      ));
    }

    let content = std::fs::read_to_string(&file_path).map_err(|e| {
      format!("Failed to read document file {:?}: {}", file_path, e)
    })?;

    let ast =
      ast_from_markdown(&content, project_path, audit_data, &next_node_id)?;

    ast_map
      .entry(project_path.clone())
      .or_insert_with(Vec::new)
      .push(ast);
  }

  Ok(ast_map)
}

pub fn ast_from_markdown(
  content: &str,
  project_path: &core::ProjectPath,
  audit_data: &core::AuditData,
  next_id: &dyn Fn() -> i32,
) -> Result<DocumentationAST, String> {
  // Parse markdown to mdast
  let md_ast = markdown::to_mdast(content, &ParseOptions::default())
    .map_err(|e| format!("Failed to parse markdown: {}", e))?;

  // Convert mdast to our DocumentationNode format
  let nodes = convert_mdast_node(&md_ast, audit_data, next_id)?;

  Ok(DocumentationAST {
    nodes: vec![nodes],
    project_path: project_path.clone(),
    source_content: content.to_string(),
  })
}

pub struct DocumentationAST {
  pub nodes: Vec<DocumentationNode>,
  pub project_path: core::ProjectPath,
  pub source_content: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DocumentationNode {
  Root {
    node_id: i32,
    position: Option<usize>,
    children: Vec<DocumentationNode>,
  },

  // Heading: contains its text content and a section child with content until the next heading
  Heading {
    node_id: i32,
    position: Option<usize>,
    level: u8,
    children: Vec<DocumentationNode>, // Text content of the heading
    section: Option<Box<DocumentationNode>>, // Section containing content until next heading
  },

  // Section: groups all content under a heading until the next heading
  // This node is created by the parser, not present in markdown AST
  // The section is a child of the Heading, sharing its title
  Section {
    node_id: i32,
    title: String, // Text content of the heading (copied from parent)
    children: Vec<DocumentationNode>, // Content until next heading
  },

  Paragraph {
    node_id: i32,
    position: Option<usize>,
    children: Vec<DocumentationNode>,
  },

  // Sentence: created by parser from paragraph content
  Sentence {
    node_id: i32,
    children: Vec<DocumentationNode>,
  },

  Text {
    node_id: i32,
    position: Option<usize>,
    value: String,
  },

  // Inline code (potential references to source code declarations)
  InlineCode {
    node_id: i32,
    position: Option<usize>,
    value: String,
    children: Vec<DocumentationNode>,
  },

  CodeBlock {
    node_id: i32,
    position: Option<usize>,
    lang: Option<String>,
    value: String,
    children: Vec<DocumentationNode>,
  },

  // Code token types for syntax highlighting and reference resolution
  // Created by parser during tokenization, not from mdast
  CodeKeyword {
    node_id: i32,
    value: String,
  },

  CodeOperator {
    node_id: i32,
    value: String,
  },

  CodeIdentifier {
    node_id: i32,
    value: String,
    referenced_topic: Option<topic::Topic>,
    kind: Option<core::NamedTopicKind>,
    referenced_name: Option<String>,
  },

  CodeText {
    node_id: i32,
    value: String,
  },

  List {
    node_id: i32,
    position: Option<usize>,
    ordered: bool,
    children: Vec<DocumentationNode>,
  },

  ListItem {
    node_id: i32,
    position: Option<usize>,
    children: Vec<DocumentationNode>,
  },

  Emphasis {
    node_id: i32,
    position: Option<usize>,
    children: Vec<DocumentationNode>,
  },

  Strong {
    node_id: i32,
    position: Option<usize>,
    children: Vec<DocumentationNode>,
  },

  Link {
    node_id: i32,
    position: Option<usize>,
    url: String,
    title: Option<String>,
    children: Vec<DocumentationNode>,
  },

  BlockQuote {
    node_id: i32,
    position: Option<usize>,
    children: Vec<DocumentationNode>,
  },

  ThematicBreak {
    node_id: i32,
    position: Option<usize>,
  },

  Break {
    node_id: i32,
    position: Option<usize>,
  },

  Delete {
    node_id: i32,
    position: Option<usize>,
    children: Vec<DocumentationNode>,
  },

  Image {
    node_id: i32,
    position: Option<usize>,
    alt: String,
    url: String,
    title: Option<String>,
  },

  Table {
    node_id: i32,
    position: Option<usize>,
    children: Vec<DocumentationNode>,
  },

  TableRow {
    node_id: i32,
    position: Option<usize>,
    children: Vec<DocumentationNode>,
  },

  TableCell {
    node_id: i32,
    position: Option<usize>,
    children: Vec<DocumentationNode>,
  },

  Html {
    node_id: i32,
    position: Option<usize>,
    value: String,
  },

  FootnoteDefinition {
    node_id: i32,
    position: Option<usize>,
    identifier: String,
    label: Option<String>,
    children: Vec<DocumentationNode>,
  },

  FootnoteReference {
    node_id: i32,
    position: Option<usize>,
    identifier: String,
    label: Option<String>,
  },

  LinkReference {
    node_id: i32,
    position: Option<usize>,
    identifier: String,
    label: Option<String>,
    children: Vec<DocumentationNode>,
  },

  ImageReference {
    node_id: i32,
    position: Option<usize>,
    alt: String,
    identifier: String,
    label: Option<String>,
  },

  Definition {
    node_id: i32,
    position: Option<usize>,
    url: String,
    title: Option<String>,
    identifier: String,
    label: Option<String>,
  },

  Frontmatter {
    node_id: i32,
    position: Option<usize>,
    value: String,
  },

  Math {
    node_id: i32,
    position: Option<usize>,
    value: String,
  },

  InlineMath {
    node_id: i32,
    position: Option<usize>,
    value: String,
  },

  // Placeholder for a node (similar to Solidity's Stub)
  Stub {
    node_id: i32,
    topic: topic::Topic,
  },
}

impl DocumentationNode {
  pub fn node_id(&self) -> i32 {
    match self {
      DocumentationNode::Root { node_id, .. } => *node_id,
      DocumentationNode::Section { node_id, .. } => *node_id,
      DocumentationNode::Heading { node_id, .. } => *node_id,
      DocumentationNode::Paragraph { node_id, .. } => *node_id,
      DocumentationNode::Sentence { node_id, .. } => *node_id,
      DocumentationNode::Text { node_id, .. } => *node_id,
      DocumentationNode::InlineCode { node_id, .. } => *node_id,
      DocumentationNode::CodeBlock { node_id, .. } => *node_id,
      DocumentationNode::CodeKeyword { node_id, .. } => *node_id,
      DocumentationNode::CodeOperator { node_id, .. } => *node_id,
      DocumentationNode::CodeIdentifier { node_id, .. } => *node_id,
      DocumentationNode::CodeText { node_id, .. } => *node_id,
      DocumentationNode::List { node_id, .. } => *node_id,
      DocumentationNode::ListItem { node_id, .. } => *node_id,
      DocumentationNode::Emphasis { node_id, .. } => *node_id,
      DocumentationNode::Strong { node_id, .. } => *node_id,
      DocumentationNode::Link { node_id, .. } => *node_id,
      DocumentationNode::BlockQuote { node_id, .. } => *node_id,
      DocumentationNode::ThematicBreak { node_id, .. } => *node_id,
      DocumentationNode::Break { node_id, .. } => *node_id,
      DocumentationNode::Delete { node_id, .. } => *node_id,
      DocumentationNode::Image { node_id, .. } => *node_id,
      DocumentationNode::Table { node_id, .. } => *node_id,
      DocumentationNode::TableRow { node_id, .. } => *node_id,
      DocumentationNode::TableCell { node_id, .. } => *node_id,
      DocumentationNode::Html { node_id, .. } => *node_id,
      DocumentationNode::FootnoteDefinition { node_id, .. } => *node_id,
      DocumentationNode::FootnoteReference { node_id, .. } => *node_id,
      DocumentationNode::LinkReference { node_id, .. } => *node_id,
      DocumentationNode::ImageReference { node_id, .. } => *node_id,
      DocumentationNode::Definition { node_id, .. } => *node_id,
      DocumentationNode::Frontmatter { node_id, .. } => *node_id,
      DocumentationNode::Math { node_id, .. } => *node_id,
      DocumentationNode::InlineMath { node_id, .. } => *node_id,
      DocumentationNode::Stub { node_id, .. } => *node_id,
    }
  }

  /// Returns the source position (start offset) for nodes that have one.
  /// Returns None for nodes created by the parser (Section, Sentence, Code tokens, Stub).
  pub fn position(&self) -> Option<usize> {
    match self {
      DocumentationNode::Root { position, .. }
      | DocumentationNode::Heading { position, .. }
      | DocumentationNode::Paragraph { position, .. }
      | DocumentationNode::Text { position, .. }
      | DocumentationNode::InlineCode { position, .. }
      | DocumentationNode::CodeBlock { position, .. }
      | DocumentationNode::List { position, .. }
      | DocumentationNode::ListItem { position, .. }
      | DocumentationNode::Emphasis { position, .. }
      | DocumentationNode::Strong { position, .. }
      | DocumentationNode::Link { position, .. }
      | DocumentationNode::BlockQuote { position, .. }
      | DocumentationNode::ThematicBreak { position, .. }
      | DocumentationNode::Break { position, .. }
      | DocumentationNode::Delete { position, .. }
      | DocumentationNode::Image { position, .. }
      | DocumentationNode::Table { position, .. }
      | DocumentationNode::TableRow { position, .. }
      | DocumentationNode::TableCell { position, .. }
      | DocumentationNode::Html { position, .. }
      | DocumentationNode::FootnoteDefinition { position, .. }
      | DocumentationNode::FootnoteReference { position, .. }
      | DocumentationNode::LinkReference { position, .. }
      | DocumentationNode::ImageReference { position, .. }
      | DocumentationNode::Definition { position, .. }
      | DocumentationNode::Frontmatter { position, .. }
      | DocumentationNode::Math { position, .. }
      | DocumentationNode::InlineMath { position, .. } => *position,
      // Nodes created by parser don't have position
      DocumentationNode::Section { .. }
      | DocumentationNode::Sentence { .. }
      | DocumentationNode::CodeKeyword { .. }
      | DocumentationNode::CodeOperator { .. }
      | DocumentationNode::CodeIdentifier { .. }
      | DocumentationNode::CodeText { .. }
      | DocumentationNode::Stub { .. } => None,
    }
  }

  pub fn children(&self) -> Vec<&DocumentationNode> {
    match self {
      DocumentationNode::Root { children, .. }
      | DocumentationNode::Section { children, .. }
      | DocumentationNode::Paragraph { children, .. }
      | DocumentationNode::Sentence { children, .. }
      | DocumentationNode::InlineCode { children, .. }
      | DocumentationNode::CodeBlock { children, .. }
      | DocumentationNode::List { children, .. }
      | DocumentationNode::ListItem { children, .. }
      | DocumentationNode::Emphasis { children, .. }
      | DocumentationNode::Strong { children, .. }
      | DocumentationNode::Link { children, .. }
      | DocumentationNode::BlockQuote { children, .. }
      | DocumentationNode::Delete { children, .. }
      | DocumentationNode::Table { children, .. }
      | DocumentationNode::TableRow { children, .. }
      | DocumentationNode::TableCell { children, .. }
      | DocumentationNode::FootnoteDefinition { children, .. }
      | DocumentationNode::LinkReference { children, .. } => {
        children.iter().collect()
      }
      // Heading has text children and optionally a section child
      DocumentationNode::Heading {
        children, section, ..
      } => {
        let mut result: Vec<&DocumentationNode> = children.iter().collect();
        if let Some(sec) = section {
          result.push(sec.as_ref());
        }
        result
      }
      _ => vec![],
    }
  }

  /// Extracts the text content from a node by recursively collecting Text node values.
  /// Useful for getting the plain text of a heading.
  pub fn extract_text(&self) -> String {
    match self {
      DocumentationNode::Text { value, .. } => value.clone(),
      DocumentationNode::CodeText { value, .. } => value.clone(),
      DocumentationNode::CodeKeyword { value, .. } => value.clone(),
      DocumentationNode::CodeOperator { value, .. } => value.clone(),
      DocumentationNode::CodeIdentifier { value, .. } => value.clone(),
      DocumentationNode::InlineCode { value, .. } => value.clone(),
      _ => {
        // Recursively collect text from children
        self
          .children()
          .into_iter()
          .map(|child| child.extract_text())
          .collect::<Vec<_>>()
          .join("")
      }
    }
  }

  /// Resolves a node, looking up Stub nodes from the nodes_map
  pub fn resolve<'a>(
    &'a self,
    nodes_map: &'a std::collections::BTreeMap<
      crate::core::topic::Topic,
      crate::core::Node,
    >,
  ) -> &'a DocumentationNode {
    match self {
      DocumentationNode::Stub { topic, .. } => {
        if let Some(crate::core::Node::Documentation(doc_node)) =
          nodes_map.get(topic)
        {
          doc_node
        } else {
          self
        }
      }
      _ => self,
    }
  }
}

/// Splits paragraph children into sentence nodes based on periods
/// Each sentence contains all inline nodes (Text, InlineCode, Emphasis, Strong, Link) until a period
fn split_into_sentences(
  children: Vec<DocumentationNode>,
  next_id: &dyn Fn() -> i32,
) -> Vec<DocumentationNode> {
  let mut sentences = Vec::new();
  let mut current_sentence_nodes = Vec::new();

  for node in children {
    match &node {
      DocumentationNode::Text { value, .. } => {
        // Split text by periods, keeping track of which nodes go into which sentence
        let mut remaining_text = value.as_str();

        loop {
          if let Some(period_idx) = remaining_text.find('.') {
            // Found a period
            let before_period = &remaining_text[..=period_idx]; // Include the period

            if !before_period.trim().is_empty() {
              // Add text up to and including the period
              current_sentence_nodes.push(DocumentationNode::Text {
                node_id: next_id(),
                position: None, // Created by parser, no mdast position
                value: before_period.to_string(),
              });

              // Complete this sentence
              if !current_sentence_nodes.is_empty() {
                sentences.push(DocumentationNode::Sentence {
                  node_id: next_id(),
                  children: current_sentence_nodes.drain(..).collect(),
                });
              }
            }

            // Move past the period
            remaining_text = &remaining_text[period_idx + 1..];
          } else {
            // No more periods in this text node
            if !remaining_text.trim().is_empty() {
              current_sentence_nodes.push(DocumentationNode::Text {
                node_id: next_id(),
                position: None, // Created by parser, no mdast position
                value: remaining_text.to_string(),
              });
            }
            break;
          }
        }
      }

      // For non-text inline nodes, add them to the current sentence
      DocumentationNode::InlineCode { .. }
      | DocumentationNode::Emphasis { .. }
      | DocumentationNode::Strong { .. }
      | DocumentationNode::Link { .. } => {
        current_sentence_nodes.push(node);
      }

      // Other node types shouldn't appear as direct children of paragraphs,
      // but handle them gracefully by ending the current sentence
      _ => {
        // End the current sentence if there is one
        if !current_sentence_nodes.is_empty() {
          sentences.push(DocumentationNode::Sentence {
            node_id: next_id(),
            children: current_sentence_nodes.drain(..).collect(),
          });
        }
        // The unexpected node is not added to any sentence
      }
    }
  }

  // Add any remaining nodes as the final sentence
  if !current_sentence_nodes.is_empty() {
    sentences.push(DocumentationNode::Sentence {
      node_id: next_id(),
      children: current_sentence_nodes,
    });
  }

  sentences
}

/// Groups nodes into sections based on headings with proper nesting
/// Each heading creates a section that contains all content until the next heading
/// of the same or higher level (lower number). Deeper headings become nested sections.
fn group_into_sections(
  nodes: Vec<DocumentationNode>,
  next_id: &dyn Fn() -> i32,
) -> Vec<DocumentationNode> {
  // Find the minimum heading level in the nodes to start grouping from there
  let min_level = nodes
    .iter()
    .filter_map(|n| match n {
      DocumentationNode::Heading { level, .. } => Some(*level),
      _ => None,
    })
    .min()
    .unwrap_or(1);

  group_into_sections_at_level(nodes, min_level, next_id)
}

/// Recursively groups nodes into sections at the specified heading level
/// Headings at exactly `level` create sections at this depth
/// Deeper headings (higher numbers) become nested sections within the content
fn group_into_sections_at_level(
  nodes: Vec<DocumentationNode>,
  level: u8,
  next_id: &dyn Fn() -> i32,
) -> Vec<DocumentationNode> {
  // Find the minimum heading level in these nodes
  let min_level = nodes
    .iter()
    .filter_map(|n| match n {
      DocumentationNode::Heading { level, .. } => Some(*level),
      _ => None,
    })
    .min();

  // If no headings or min level is deeper than current level, process at min level
  let effective_level = match min_level {
    Some(min) if min > level => min,
    Some(_) => level,
    None => return nodes, // No headings, return as-is
  };

  let mut result = Vec::new();
  let mut current_heading: Option<DocumentationNode> = None;
  let mut current_content = Vec::new();

  for node in nodes {
    match &node {
      DocumentationNode::Heading { level: h_level, .. } => {
        if *h_level == effective_level {
          // Same level heading - finalize previous section if any
          if let Some(heading) = current_heading.take() {
            // Recursively group the content at deeper levels
            let nested_children = group_into_sections_at_level(
              current_content.drain(..).collect(),
              effective_level + 1,
              next_id,
            );
            // Create heading with section as child
            result.push(create_heading_with_section(
              heading,
              nested_children,
              next_id,
            ));
          }
          // Start a new section at this level
          current_heading = Some(node);
        } else if *h_level > effective_level {
          // Deeper heading - add to current content (will be nested later)
          if current_heading.is_some() {
            current_content.push(node);
          } else {
            // No section started yet, add directly to result
            result.push(node);
          }
        } else {
          // Shallower heading (h_level < effective_level) - shouldn't happen
          // but handle gracefully by finalizing current and adding to result
          if let Some(heading) = current_heading.take() {
            let nested_children = group_into_sections_at_level(
              current_content.drain(..).collect(),
              effective_level + 1,
              next_id,
            );
            // Create heading with section as child
            result.push(create_heading_with_section(
              heading,
              nested_children,
              next_id,
            ));
          }
          result.push(node);
        }
      }
      _ => {
        // Non-heading content
        if current_heading.is_some() {
          current_content.push(node);
        } else {
          result.push(node);
        }
      }
    }
  }

  // Handle the last section if there is one
  if let Some(heading) = current_heading {
    let nested_children = group_into_sections_at_level(
      current_content,
      effective_level + 1,
      next_id,
    );
    // Create heading with section as child
    result.push(create_heading_with_section(
      heading,
      nested_children,
      next_id,
    ));
  }

  result
}

/// Creates a Heading node with a Section child containing the given content.
/// The heading's existing data is preserved, and a new Section node is created
/// with the heading's title and the provided children.
fn create_heading_with_section(
  heading: DocumentationNode,
  section_children: Vec<DocumentationNode>,
  next_id: &dyn Fn() -> i32,
) -> DocumentationNode {
  match heading {
    DocumentationNode::Heading {
      node_id,
      position,
      level,
      children,
      section: _, // Ignore any existing section
    } => {
      let section_title = children
        .iter()
        .map(|c| c.extract_text())
        .collect::<Vec<_>>()
        .join("");
      let section_node_id = next_id();
      let section = DocumentationNode::Section {
        node_id: section_node_id,
        title: section_title,
        children: section_children,
      };
      DocumentationNode::Heading {
        node_id,
        position,
        level,
        children,
        section: Some(Box::new(section)),
      }
    }
    // If not a heading, just return as-is (shouldn't happen)
    other => other,
  }
}

/// Extracts the start offset from an mdast node's position
fn get_mdast_position(node: &MdNode) -> Option<usize> {
  node.position().map(|p| p.start.offset)
}

fn convert_mdast_node(
  node: &MdNode,
  audit_data: &core::AuditData,
  next_id: &dyn Fn() -> i32,
) -> Result<DocumentationNode, String> {
  let node_id = next_id();
  let position = get_mdast_position(node);

  match node {
    MdNode::Root(root) => {
      // Convert all children first
      let children = root
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      // Group children into sections
      let sections = group_into_sections(children, next_id);

      Ok(DocumentationNode::Root {
        node_id,
        position,
        children: sections,
      })
    }

    MdNode::Heading(heading) => {
      let children = heading
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::Heading {
        node_id,
        position,
        level: heading.depth,
        children,
        section: None, // Section is added later by group_into_sections
      })
    }

    MdNode::Paragraph(paragraph) => {
      let children = paragraph
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      // Split the paragraph's children into sentences
      let sentences = split_into_sentences(children, next_id);

      if sentences.len() == 1 {
        // If there is only one sentence, return it directly as a sentence
        // node without a containing paragraph node
        Ok(sentences.get(0).unwrap().clone())
      } else {
        Ok(DocumentationNode::Paragraph {
          node_id,
          position,
          children: sentences,
        })
      }
    }

    MdNode::Text(text) => Ok(DocumentationNode::Text {
      node_id,
      position,
      value: text.value.clone(),
    }),

    MdNode::InlineCode(code) => {
      let children = tokenize_code(&code.value, audit_data, next_id);

      Ok(DocumentationNode::InlineCode {
        node_id,
        position,
        value: code.value.clone(),
        children,
      })
    }

    MdNode::Code(code) => {
      let children = tokenize_code(&code.value, audit_data, next_id);

      Ok(DocumentationNode::CodeBlock {
        node_id,
        position,
        lang: code.lang.clone(),
        value: code.value.clone(),
        children,
      })
    }

    MdNode::List(list) => {
      let children = list
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::List {
        node_id,
        position,
        ordered: list.ordered,
        children,
      })
    }

    MdNode::ListItem(item) => {
      let children = item
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::ListItem {
        node_id,
        position,
        children,
      })
    }

    MdNode::Emphasis(emphasis) => {
      let children = emphasis
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::Emphasis {
        node_id,
        position,
        children,
      })
    }

    MdNode::Strong(strong) => {
      let children = strong
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::Strong {
        node_id,
        position,
        children,
      })
    }

    MdNode::Link(link) => {
      let children = link
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::Link {
        node_id,
        position,
        url: link.url.clone(),
        title: link.title.clone(),
        children,
      })
    }

    MdNode::BlockQuote(quote) => {
      let children = quote
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::BlockQuote {
        node_id,
        position,
        children,
      })
    }

    MdNode::ThematicBreak(_) => {
      Ok(DocumentationNode::ThematicBreak { node_id, position })
    }

    MdNode::Break(_) => {
      Ok(DocumentationNode::Break { node_id, position })
    }

    MdNode::Delete(delete) => {
      let children = delete
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::Delete {
        node_id,
        position,
        children,
      })
    }

    MdNode::Image(image) => Ok(DocumentationNode::Image {
      node_id,
      position,
      alt: image.alt.clone(),
      url: image.url.clone(),
      title: image.title.clone(),
    }),

    MdNode::Table(table) => {
      let children = table
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::Table {
        node_id,
        position,
        children,
      })
    }

    MdNode::TableRow(row) => {
      let children = row
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::TableRow {
        node_id,
        position,
        children,
      })
    }

    MdNode::TableCell(cell) => {
      let children = cell
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::TableCell {
        node_id,
        position,
        children,
      })
    }

    MdNode::Html(html) => Ok(DocumentationNode::Html {
      node_id,
      position,
      value: html.value.clone(),
    }),

    MdNode::FootnoteDefinition(def) => {
      let children = def
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::FootnoteDefinition {
        node_id,
        position,
        identifier: def.identifier.clone(),
        label: def.label.clone(),
        children,
      })
    }

    MdNode::FootnoteReference(ref_node) => {
      Ok(DocumentationNode::FootnoteReference {
        node_id,
        position,
        identifier: ref_node.identifier.clone(),
        label: ref_node.label.clone(),
      })
    }

    MdNode::LinkReference(link_ref) => {
      let children = link_ref
        .children
        .iter()
        .map(|child| convert_mdast_node(child, audit_data, next_id))
        .collect::<Result<Vec<_>, _>>()?;

      Ok(DocumentationNode::LinkReference {
        node_id,
        position,
        identifier: link_ref.identifier.clone(),
        label: link_ref.label.clone(),
        children,
      })
    }

    MdNode::ImageReference(img_ref) => {
      Ok(DocumentationNode::ImageReference {
        node_id,
        position,
        alt: img_ref.alt.clone(),
        identifier: img_ref.identifier.clone(),
        label: img_ref.label.clone(),
      })
    }

    MdNode::Definition(def) => Ok(DocumentationNode::Definition {
      node_id,
      position,
      url: def.url.clone(),
      title: def.title.clone(),
      identifier: def.identifier.clone(),
      label: def.label.clone(),
    }),

    MdNode::Yaml(yaml) => Ok(DocumentationNode::Frontmatter {
      node_id,
      position,
      value: yaml.value.clone(),
    }),

    MdNode::Toml(toml) => Ok(DocumentationNode::Frontmatter {
      node_id,
      position,
      value: toml.value.clone(),
    }),

    MdNode::Math(math) => Ok(DocumentationNode::Math {
      node_id,
      position,
      value: math.value.clone(),
    }),

    MdNode::InlineMath(math) => Ok(DocumentationNode::InlineMath {
      node_id,
      position,
      value: math.value.clone(),
    }),

    // MDX-specific nodes are not supported
    _ => Ok(DocumentationNode::Text {
      node_id,
      position,
      value: "[UNSUPPORTED]".to_string(),
    }),
  }
}

/// Searches the AuditData for a declaration with the given value
/// Search order: topic ID, qualified name, then simple name
/// This is used to resolve inline code references to solidity declarations
pub(crate) fn find_declaration_by_name<'a>(
  audit_data: &'a core::AuditData,
  value: &str,
) -> Option<&'a crate::core::TopicMetadata> {
  // First try to find by topic ID (most specific)
  audit_data
    .topic_metadata
    .get(&topic::new_topic(value))
    .or_else(|| {
      // If not found by topic ID, try to find by qualified name
      audit_data
        .name_index
        .get_by_qualified_name(value)
        .and_then(|t| audit_data.topic_metadata.get(t))
        .or_else(|| {
          // If not found by qualified name, try to find by simple name
          // Returns None for ambiguous names (multiple topics share the name)
          audit_data
            .name_index
            .get_by_simple_name(value)
            .and_then(|t| audit_data.topic_metadata.get(t))
        })
    })
}

/// Converts children nodes to stubs for storage optimization
pub fn children_to_stubs(node: DocumentationNode) -> DocumentationNode {
  match node {
    DocumentationNode::Root {
      node_id,
      position,
      children,
    } => DocumentationNode::Root {
      node_id,
      position,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::Section {
      node_id,
      title,
      children,
    } => DocumentationNode::Section {
      node_id,
      title,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::Heading {
      node_id,
      position,
      level,
      children,
      section,
    } => DocumentationNode::Heading {
      node_id,
      position,
      level,
      children: children.into_iter().map(node_to_stub).collect(),
      section: section.map(|s| Box::new(node_to_stub(*s))),
    },
    DocumentationNode::Paragraph {
      node_id,
      position,
      children,
    } => DocumentationNode::Paragraph {
      node_id,
      position,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::Sentence { node_id, children } => {
      DocumentationNode::Sentence {
        node_id,
        children: children.into_iter().map(node_to_stub).collect(),
      }
    }
    DocumentationNode::List {
      node_id,
      position,
      ordered,
      children,
    } => DocumentationNode::List {
      node_id,
      position,
      ordered,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::ListItem {
      node_id,
      position,
      children,
    } => DocumentationNode::ListItem {
      node_id,
      position,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::Emphasis {
      node_id,
      position,
      children,
    } => DocumentationNode::Emphasis {
      node_id,
      position,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::Strong {
      node_id,
      position,
      children,
    } => DocumentationNode::Strong {
      node_id,
      position,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::Link {
      node_id,
      position,
      url,
      title,
      children,
    } => DocumentationNode::Link {
      node_id,
      position,
      url,
      title,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::BlockQuote {
      node_id,
      position,
      children,
    } => DocumentationNode::BlockQuote {
      node_id,
      position,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::InlineCode {
      node_id,
      position,
      value,
      children,
    } => DocumentationNode::InlineCode {
      node_id,
      position,
      value,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::CodeBlock {
      node_id,
      position,
      lang,
      value,
      children,
    } => DocumentationNode::CodeBlock {
      node_id,
      position,
      lang,
      value,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::Delete {
      node_id,
      position,
      children,
    } => DocumentationNode::Delete {
      node_id,
      position,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::Table {
      node_id,
      position,
      children,
    } => DocumentationNode::Table {
      node_id,
      position,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::TableRow {
      node_id,
      position,
      children,
    } => DocumentationNode::TableRow {
      node_id,
      position,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::TableCell {
      node_id,
      position,
      children,
    } => DocumentationNode::TableCell {
      node_id,
      position,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::FootnoteDefinition {
      node_id,
      position,
      identifier,
      label,
      children,
    } => DocumentationNode::FootnoteDefinition {
      node_id,
      position,
      identifier,
      label,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    DocumentationNode::LinkReference {
      node_id,
      position,
      identifier,
      label,
      children,
    } => DocumentationNode::LinkReference {
      node_id,
      position,
      identifier,
      label,
      children: children.into_iter().map(node_to_stub).collect(),
    },
    // Leaf nodes remain unchanged
    other => other,
  }
}

fn node_to_stub(node: DocumentationNode) -> DocumentationNode {
  DocumentationNode::Stub {
    node_id: node.node_id(),
    topic: topic::new_documentation_topic(node.node_id()),
  }
}

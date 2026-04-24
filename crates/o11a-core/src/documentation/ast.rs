use crate::core;
use crate::core::topic;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentationAST {
  pub nodes: Vec<DocumentationNode>,
  pub project_path: core::ProjectPath,
  pub source_content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

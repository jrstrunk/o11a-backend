//! Per-contract member corpus assembly for BM25 scoring.
//!
//! Each indexable declaration inside a contract becomes one BM25 "document".
//! "Indexable" means: top-level contract members (functions, modifiers,
//! events, errors, structs, enums, state variables) PLUS deeper declarations
//! that carry semantic weight: function parameters, named locals inside
//! function bodies, struct fields, enum members. Each document is the
//! concatenation of the declaration's name with any inline NatSpec / `[dev]`
//! comments attached to it.

use crate::collaborator::agent::context;
use crate::collaborator::parser::CommentNode;
use crate::domain::{
  self, AuditData, NamedTopicKind, Scope, TopicMetadata, topic,
};

/// Which textual content goes into a contract's BM25 Pass 1 summary
/// document. Both variants include the contract name + NatSpec + each
/// indexable member's name + NatSpec — they differ only in whether they
/// also include member source code, and if so, whether function bodies
/// are stripped.
#[derive(Copy, Clone, Debug)]
pub enum SummaryCorpusVariant {
  /// Adds rendered signature text per member (function/modifier bodies
  /// stripped). Smaller corpus, less noise from implementation details.
  Signatures,
  /// Adds full member source per member (function/modifier bodies
  /// included). Bigger corpus, more lexical surface — may catch matches
  /// against expression-level identifiers but adds keyword noise.
  Body,
}

/// One declaration's BM25 document, pre-tokenized.
pub struct MemberDoc {
  pub member_topic: topic::Topic,
  pub tokens: Vec<String>,
}

/// Build the BM25 corpus for a single contract: one document per indexable
/// declaration inside that contract. Returns an empty vec if the contract
/// topic isn't found or has no indexable declarations.
///
/// Includes top-level members (functions/modifiers/events/errors/structs/
/// enums/state vars) plus parameters, named locals, struct fields, and enum
/// members — anything declared inside a member that the tokenizer can turn
/// into useful tokens.
pub fn build_contract_member_corpus(
  contract_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<MemberDoc> {
  let mut docs: Vec<MemberDoc> = Vec::new();

  for (member_topic, metadata) in &audit_data.topic_metadata {
    let TopicMetadata::NamedTopic {
      kind, name, scope, ..
    } = metadata
    else {
      continue;
    };

    if !is_indexable_kind(kind) {
      continue;
    }

    if !belongs_to_contract(scope, contract_topic, audit_data) {
      continue;
    }

    let mut text = String::with_capacity(name.len() * 2);
    text.push_str(name);

    if let Some(comment_topics) = audit_data.comment_index.get(member_topic) {
      for ct in comment_topics {
        if let Some(domain::Node::Comment(comment_nodes)) =
          audit_data.nodes.get(ct)
        {
          for node in comment_nodes {
            text.push(' ');
            append_comment_text(node, &mut text);
          }
        }
      }
    }

    let tokens = super::tokenize::tokenize_code_text(&text);
    if !tokens.is_empty() {
      docs.push(MemberDoc {
        member_topic: *member_topic,
        tokens,
      });
    }
  }

  docs
}

fn is_indexable_kind(kind: &NamedTopicKind) -> bool {
  matches!(
    kind,
    NamedTopicKind::Function(_)
      | NamedTopicKind::Modifier
      | NamedTopicKind::Event
      | NamedTopicKind::Error
      | NamedTopicKind::Struct
      | NamedTopicKind::Enum
      | NamedTopicKind::EnumMember
      | NamedTopicKind::StateVariable(_)
      | NamedTopicKind::LocalVariable
  )
}

/// True when `scope` is `contract_topic`, a member of it, an item nested in
/// one of those members (locals, parameters), or a field of a struct/enum
/// declared inside the contract. The recursive parent walk is bounded
/// (depth ≤ 3) since Solidity's nesting is shallow.
fn belongs_to_contract(
  scope: &Scope,
  contract_topic: &topic::Topic,
  audit_data: &AuditData,
) -> bool {
  match scope {
    Scope::Member { component, .. } => component == contract_topic,
    Scope::ContainingBlock { component, .. } => component == contract_topic,
    Scope::Component { component, .. } => {
      if component == contract_topic {
        return true;
      }
      // Struct field / enum member: walk one level up via the parent
      // component's scope. Capped at one extra hop to avoid loops; this
      // covers nested structs declared at contract level but not
      // arbitrarily-deep type declarations.
      audit_data
        .topic_metadata
        .get(component)
        .map(TopicMetadata::scope)
        .map(|parent_scope| match parent_scope {
          Scope::Member { component: c, .. }
          | Scope::ContainingBlock { component: c, .. }
          | Scope::Component { component: c, .. } => c == contract_topic,
          _ => false,
        })
        .unwrap_or(false)
    }
    _ => false,
  }
}

// ---------------------------------------------------------------------------
// Contract-summary corpus (BM25 Pass 1 input)
// ---------------------------------------------------------------------------

/// One contract's BM25 document for Pass 1 contract discovery: contract name
/// plus the names + NatSpec of all its indexable declarations, tokenized as
/// one bag-of-words document.
pub struct ContractDoc {
  pub contract_topic: topic::Topic,
  pub tokens: Vec<String>,
}

/// Build a BM25 corpus where each contract is one document. The document
/// text is the contract name + its NatSpec + every indexable declaration's
/// name + NatSpec, plus per-variant additions (signature text or full
/// source). Used by BM25 Pass 1 to discover relevant contracts.
pub fn build_contract_summary_corpus(
  audit_data: &AuditData,
  variant: SummaryCorpusVariant,
) -> Vec<ContractDoc> {
  let mut docs: Vec<ContractDoc> = Vec::new();

  // First pass: collect contract topics.
  let contract_topics: Vec<topic::Topic> = audit_data
    .topic_metadata
    .iter()
    .filter_map(|(t, meta)| match meta {
      TopicMetadata::NamedTopic {
        kind: NamedTopicKind::Contract(_),
        ..
      } => Some(*t),
      _ => None,
    })
    .collect();

  for ct in contract_topics {
    let Some(TopicMetadata::NamedTopic { name, .. }) =
      audit_data.topic_metadata.get(&ct)
    else {
      continue;
    };

    let mut text = String::new();
    text.push_str(name);

    // Contract-level NatSpec.
    if let Some(comment_topics) = audit_data.comment_index.get(&ct) {
      for c in comment_topics {
        if let Some(domain::Node::Comment(nodes)) = audit_data.nodes.get(c) {
          for node in nodes {
            text.push(' ');
            append_comment_text(node, &mut text);
          }
        }
      }
    }

    // Member surface: reuse build_contract_member_corpus' filtering and
    // collect each member's name + NatSpec + signature/body into the
    // contract document.
    for (mt, meta) in &audit_data.topic_metadata {
      let TopicMetadata::NamedTopic {
        kind,
        name: mname,
        scope,
        ..
      } = meta
      else {
        continue;
      };
      if !is_indexable_kind(kind) {
        continue;
      }
      if !belongs_to_contract(scope, &ct, audit_data) {
        continue;
      }

      text.push(' ');
      text.push_str(mname);

      if let Some(comment_topics) = audit_data.comment_index.get(mt) {
        for c in comment_topics {
          if let Some(domain::Node::Comment(nodes)) = audit_data.nodes.get(c) {
            for node in nodes {
              text.push(' ');
              append_comment_text(node, &mut text);
            }
          }
        }
      }

      // Append signature or full body source per variant. The renderers
      // produce JSON; the tokenizer treats it as text so the JSON
      // wrappers contribute almost nothing once stop-words / common
      // tokens are pruned.
      let extra = match variant {
        SummaryCorpusVariant::Signatures => {
          context::render_member_signature_for_semantics(mt, audit_data)
        }
        SummaryCorpusVariant::Body => {
          context::render_member_source_for_semantics(mt, audit_data)
        }
      };
      if let Some(s) = extra {
        text.push(' ');
        text.push_str(&s);
      }
    }

    let tokens = super::tokenize::tokenize_code_text(&text);
    if !tokens.is_empty() {
      docs.push(ContractDoc {
        contract_topic: ct,
        tokens,
      });
    }
  }

  docs
}

/// Walk a `CommentNode` and append its surface text into `out` separated by
/// spaces. Drops formatting markers and link URLs; keeps inline-code
/// identifier values so they tokenize alongside the surrounding prose.
fn append_comment_text(node: &CommentNode, out: &mut String) {
  match node {
    CommentNode::Text { value }
    | CommentNode::CodeText { value }
    | CommentNode::CodeKeyword { value }
    | CommentNode::CodeOperator { value } => {
      out.push(' ');
      out.push_str(value);
    }
    CommentNode::CodeIdentifier { value, .. } => {
      out.push(' ');
      out.push_str(value);
    }
    CommentNode::InlineCode { children, .. } => {
      for child in children {
        append_comment_text(child, out);
      }
    }
    CommentNode::Emphasis { text } | CommentNode::Strong { text } => {
      out.push(' ');
      out.push_str(text);
    }
    CommentNode::Link { text, .. } => {
      // Drop the URL; keep only the human-readable link text.
      out.push(' ');
      out.push_str(text);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn append_comment_text_extracts_surface_text() {
    let node = CommentNode::Text {
      value: "the user balance".to_string(),
    };
    let mut out = String::new();
    append_comment_text(&node, &mut out);
    assert_eq!(out.trim(), "the user balance");
  }

  #[test]
  fn append_comment_text_recurses_into_inline_code() {
    let node = CommentNode::InlineCode {
      value: "foo()".to_string(),
      children: vec![CommentNode::CodeIdentifier {
        value: "foo".to_string(),
        referenced_topic: None,
        kind: None,
        referenced_name: None,
        referenced_topic_candidates: Vec::new(),
      }],
    };
    let mut out = String::new();
    append_comment_text(&node, &mut out);
    assert!(out.contains("foo"));
  }

  #[test]
  fn append_comment_text_drops_link_urls() {
    let node = CommentNode::Link {
      url: "https://example.com/dont-include-this-url".to_string(),
      text: "see the spec".to_string(),
    };
    let mut out = String::new();
    append_comment_text(&node, &mut out);
    assert!(out.contains("see the spec"));
    assert!(!out.contains("example.com"));
  }
}

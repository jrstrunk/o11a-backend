//! Synthetic developer-comment factory.
//!
//! Builds `CommentTopic` entries from raw documentation text (NatSpec
//! comments, semantic-block doc strings, etc.) and inserts them into
//! `AuditData`. Lives in core because both the analysis pipeline (in
//! `o11a-analyze`) and core's collaboration tests rely on it; the
//! comment parser the function calls is also in core.

use crate::collaborator::models::Author;
use crate::collaborator::parser as comment_parser;
use crate::domain::topic;
use crate::domain::{AuditData, CommentType, Node, Scope, TopicMetadata};
use std::sync::atomic::{AtomicI32, Ordering};

/// Global counter for synthetic developer documentation comment IDs.
/// Uses negative IDs starting from -10 to avoid collision with
/// real DB comments (positive auto-increment). The topic prefix system
/// (C vs N) prevents collision with generated AST node IDs.
static NEXT_DEV_DOC_COMMENT_ID: AtomicI32 = AtomicI32::new(-10);

fn next_dev_doc_comment_id() -> i32 {
  NEXT_DEV_DOC_COMMENT_ID.fetch_sub(1, Ordering::SeqCst)
}

/// Create a synthetic developer CommentTopic targeting the given topic.
/// Parses the text through the comment parser to resolve code references.
pub fn create_synthetic_dev_comment(
  target_topic: &topic::Topic,
  doc_text: &str,
  comment_type: CommentType,
  author: Author,
  audit_data: &mut AuditData,
) {
  let comment_id = next_dev_doc_comment_id();
  let comment_topic = topic::new_comment_topic(comment_id);

  // Parse the documentation text through the comment parser to resolve
  // code references (mentions) in the developer's prose.
  let (mentions, comment_nodes) =
    comment_parser::parse_comment(doc_text, audit_data);

  audit_data
    .nodes
    .insert(comment_topic, Node::Comment(comment_nodes));

  let mut mentioned_topics = mentions;
  mentioned_topics.sort_unstable();
  mentioned_topics.dedup();

  let scope = audit_data
    .topic_metadata
    .get(target_topic)
    .map(|m| m.scope().clone())
    .unwrap_or(Scope::Global);

  audit_data.topic_metadata.insert(
    comment_topic,
    TopicMetadata::CommentTopic {
      topic: comment_topic,
      target_topic: *target_topic,
      comment_type,
      author,
      created_at: String::new(), // Synthetic — no real timestamp
      scope,
      mentioned_topics: mentioned_topics.clone(),
    },
  );

  audit_data
    .comment_index
    .entry(*target_topic)
    .or_default()
    .push(comment_topic);

  for mention in &mentioned_topics {
    audit_data
      .mentions_index
      .entry(*mention)
      .or_default()
      .push(comment_topic);
  }
}

use crate::collaborator::models::*;
use crate::collaborator::parser;
use crate::domain::{self, DataContext, topic};
use sqlx::SqlitePool;

pub mod user_entities;

pub use user_entities::{
  apply_user_entities_snapshot, load_user_entities_snapshot,
};

// ============================================================================
// Startup loading
// ============================================================================

/// Ingest a `Comment` into in-memory state: parse markdown, render HTML, insert
/// the AST node, register topic metadata + reverse indexes, and cache the
/// rendered HTML on the data context. Returns the parsed mention topics so
/// callers can broadcast follow-up events. No-op if the audit is unknown.
pub fn ingest_comment(
  data_context: &mut DataContext,
  comment: &Comment,
  scope: &ScopeInfo,
) -> Vec<topic::Topic> {
  let Some(audit_data) = data_context.get_audit_mut(&comment.audit_id) else {
    return Vec::new();
  };

  let comment_topic = comment.comment_topic();
  let target_topic = topic::new_topic(&comment.topic_id);

  let (mentions, nodes) =
    parser::parse_comment(&comment.content_markdown, audit_data);

  audit_data
    .nodes
    .insert(comment_topic, domain::Node::Comment(nodes));

  let mut mentioned_topics: Vec<topic::Topic> = mentions.clone();
  mentioned_topics.sort_unstable();
  mentioned_topics.dedup();

  audit_data.topic_metadata.insert(
    comment_topic,
    domain::TopicMetadata::CommentTopic {
      topic: comment_topic,
      author: comment.author,
      comment_type: domain::CommentType::parse_str(&comment.comment_type)
        .unwrap_or_else(|| {
          panic!(
            "Unknown comment type '{}' in comment {}",
            comment.comment_type, comment.id
          )
        }),
      target_topic,
      created_at: comment.created_at.clone(),
      scope: scope.to_scope(),
      mentioned_topics,
    },
  );

  let comments = audit_data.comment_index.entry(target_topic).or_default();
  if !comments.contains(&comment_topic) {
    comments.push(comment_topic);
  }

  for mention in &mentions {
    let entries = audit_data.mentions_index.entry(*mention).or_default();
    if !entries.contains(&comment_topic) {
      entries.push(comment_topic);
    }
  }

  mentions
}

/// Load all visible comments for ingestion. Pure I/O; pair with
/// `ingest_loaded_comments` so callers can hold a sync mutex around the
/// mutation without crossing an `.await`.
pub async fn load_visible_comments(
  pool: &SqlitePool,
) -> Result<Vec<Comment>, sqlx::Error> {
  sqlx::query_as::<_, Comment>(
    "SELECT * FROM comments WHERE status != 'hidden'",
  )
  .fetch_all(pool)
  .await
}

/// Ingest a batch of pre-loaded comments. Synchronous so it composes with
/// `std::sync::Mutex` guards. Returns the number ingested.
pub fn ingest_loaded_comments(
  data_context: &mut DataContext,
  comments: &[Comment],
) -> usize {
  for comment in comments {
    let scope: ScopeInfo =
      serde_json::from_str(&comment.scope).unwrap_or_default();
    ingest_comment(data_context, comment, &scope);
  }
  comments.len()
}

// ============================================================================
// Comment CRUD operations
// ============================================================================

/// Creates a new comment
pub async fn create_comment(
  pool: &SqlitePool,
  audit_id: &str,
  request: &CreateCommentRequest,
  scope: &ScopeInfo,
) -> Result<Comment, sqlx::Error> {
  let comment_type = request.comment_type.as_str();
  let status = request.comment_type.default_status().as_str();
  let scope_json =
    serde_json::to_string(scope).unwrap_or_else(|_| "{}".to_string());

  let result = sqlx::query(
        r#"
        INSERT INTO comments (audit_id, topic_id, content_markdown, author_id, comment_type, status, scope)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(audit_id)
    .bind(&request.topic_id)
    .bind(&request.content)
    .bind(request.author)
    .bind(comment_type)
    .bind(status)
    .bind(scope_json)
    .execute(pool)
    .await?;

  let comment_id = result.last_insert_rowid();
  get_comment_raw(pool, comment_id).await
}

/// Gets a single comment by ID (raw database row)
pub async fn get_comment_raw(
  pool: &SqlitePool,
  comment_id: i64,
) -> Result<Comment, sqlx::Error> {
  sqlx::query_as::<_, Comment>("SELECT * FROM comments WHERE id = ?")
    .bind(comment_id)
    .fetch_one(pool)
    .await
}

/// Gets all comments for an audit filtered by type and status
pub async fn get_comments_by_type_and_status(
  pool: &SqlitePool,
  audit_id: &str,
  comment_type: &str,
  status: &str,
) -> Result<Vec<Comment>, sqlx::Error> {
  sqlx::query_as::<_, Comment>(
        "SELECT * FROM comments WHERE audit_id = ? AND comment_type = ? AND status = ? ORDER BY created_at DESC",
    )
    .bind(audit_id)
    .bind(comment_type)
    .bind(status)
    .fetch_all(pool)
    .await
}

/// Gets comments by IDs (for mention lookups)
pub async fn get_comments_by_ids(
  pool: &SqlitePool,
  comment_ids: &[i64],
) -> Result<Vec<Comment>, sqlx::Error> {
  if comment_ids.is_empty() {
    return Ok(vec![]);
  }

  let placeholders = comment_ids
    .iter()
    .map(|_| "?")
    .collect::<Vec<_>>()
    .join(",");
  let query = format!(
    "SELECT * FROM comments WHERE id IN ({}) AND status != 'hidden' ORDER BY created_at DESC",
    placeholders
  );

  let mut q = sqlx::query_as::<_, Comment>(&query);
  for id in comment_ids {
    q = q.bind(id);
  }
  q.fetch_all(pool).await
}

// ============================================================================
// Status operations
// ============================================================================

/// Updates comment status
pub async fn update_status(
  pool: &SqlitePool,
  comment_id: i64,
  status: &CommentStatus,
) -> Result<CommentStatusResponse, sqlx::Error> {
  let status_str = status.as_str();

  sqlx::query("UPDATE comments SET status = ? WHERE id = ?")
    .bind(status_str)
    .bind(comment_id)
    .execute(pool)
    .await?;

  get_comment_status(pool, comment_id).await
}

/// Gets status for a comment
pub async fn get_comment_status(
  pool: &SqlitePool,
  comment_id: i64,
) -> Result<CommentStatusResponse, sqlx::Error> {
  let comment = get_comment_raw(pool, comment_id).await?;

  Ok(CommentStatusResponse {
    comment_topic_id: comment.comment_topic_id(),
    status: comment.get_status(),
  })
}

// ============================================================================
// Vote operations
// ============================================================================

/// Vote info result
#[derive(Debug, Default)]
pub struct VoteInfo {
  pub score: i64,
  pub upvotes: i64,
  pub downvotes: i64,
  pub user_vote: Option<VoteValue>,
}

/// Upsert a vote (insert or update existing)
pub async fn upsert_vote(
  pool: &SqlitePool,
  comment_id: i64,
  user_id: i64,
  vote: i32,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    r#"
        INSERT INTO comment_votes (comment_id, user_id, vote)
        VALUES (?, ?, ?)
        ON CONFLICT(comment_id, user_id) DO UPDATE SET vote = excluded.vote
        "#,
  )
  .bind(comment_id)
  .bind(user_id)
  .bind(vote)
  .execute(pool)
  .await?;
  Ok(())
}

/// Delete a vote
pub async fn delete_vote(
  pool: &SqlitePool,
  comment_id: i64,
  user_id: i64,
) -> Result<(), sqlx::Error> {
  sqlx::query("DELETE FROM comment_votes WHERE comment_id = ? AND user_id = ?")
    .bind(comment_id)
    .bind(user_id)
    .execute(pool)
    .await?;
  Ok(())
}

/// Get vote information for a comment
pub async fn get_vote_info(
  pool: &SqlitePool,
  comment_id: i64,
  user_id: Option<i64>,
) -> Result<VoteInfo, sqlx::Error> {
  let score: i64 = sqlx::query_scalar(
    "SELECT COALESCE(SUM(vote), 0) FROM comment_votes WHERE comment_id = ?",
  )
  .bind(comment_id)
  .fetch_one(pool)
  .await?;

  let upvotes: i64 = sqlx::query_scalar(
    "SELECT COUNT(*) FROM comment_votes WHERE comment_id = ? AND vote = 1",
  )
  .bind(comment_id)
  .fetch_one(pool)
  .await?;

  let downvotes: i64 = sqlx::query_scalar(
    "SELECT COUNT(*) FROM comment_votes WHERE comment_id = ? AND vote = -1",
  )
  .bind(comment_id)
  .fetch_one(pool)
  .await?;

  let user_vote = if let Some(uid) = user_id {
    sqlx::query_scalar::<_, i32>(
      "SELECT vote FROM comment_votes WHERE comment_id = ? AND user_id = ?",
    )
    .bind(comment_id)
    .bind(uid)
    .fetch_optional(pool)
    .await?
    .map(VoteValue::from_i32)
  } else {
    None
  };

  Ok(VoteInfo {
    score,
    upvotes,
    downvotes,
    user_vote,
  })
}

/// Get comment IDs that a user has not voted on
pub async fn get_unvoted_comment_ids(
  pool: &SqlitePool,
  audit_id: &str,
  user_id: i64,
) -> Result<Vec<i64>, sqlx::Error> {
  sqlx::query_scalar::<_, i64>(
    r#"
        SELECT c.id FROM comments c
        WHERE c.audit_id = ?
          AND c.status != 'hidden'
          AND NOT EXISTS (
              SELECT 1 FROM comment_votes v
              WHERE v.comment_id = c.id AND v.user_id = ?
          )
        ORDER BY c.created_at DESC
        "#,
  )
  .bind(audit_id)
  .bind(user_id)
  .fetch_all(pool)
  .await
}

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

// ============================================================================
// Impact analysis (threat-feature link) operations
// ============================================================================

#[derive(Debug, sqlx::FromRow)]
pub struct ThreatFeatureLinkRow {
  pub id: i64,
  pub audit_id: String,
  pub threat_id: i64,
  pub feature_id: i64,
  pub relation: String,
  pub severity: String,
}

/// Creates a threat-feature link (impact analysis)
pub async fn create_threat_feature_link(
  pool: &SqlitePool,
  audit_id: &str,
  threat_id: i64,
  feature_id: i64,
  relation: &str,
  severity: &str,
) -> Result<ThreatFeatureLinkRow, sqlx::Error> {
  let result = sqlx::query(
    r#"
        INSERT INTO threat_feature_links (audit_id, threat_id, feature_id, relation, severity)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(threat_id, feature_id) DO UPDATE SET relation = ?, severity = ?
        "#,
  )
  .bind(audit_id)
  .bind(threat_id)
  .bind(feature_id)
  .bind(relation)
  .bind(severity)
  .bind(relation)
  .bind(severity)
  .execute(pool)
  .await?;

  let id = result.last_insert_rowid();
  sqlx::query_as::<_, ThreatFeatureLinkRow>(
    "SELECT * FROM threat_feature_links WHERE id = ?",
  )
  .bind(id)
  .fetch_one(pool)
  .await
}

/// Deletes a threat-feature link
pub async fn delete_threat_feature_link(
  pool: &SqlitePool,
  threat_id: i64,
  feature_id: i64,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    "DELETE FROM threat_feature_links WHERE threat_id = ? AND feature_id = ?",
  )
  .bind(threat_id)
  .bind(feature_id)
  .execute(pool)
  .await?;
  Ok(())
}

/// Gets all threat-feature links for a threat
pub async fn get_threat_feature_links(
  pool: &SqlitePool,
  threat_id: i64,
) -> Result<Vec<ThreatFeatureLinkRow>, sqlx::Error> {
  sqlx::query_as::<_, ThreatFeatureLinkRow>(
    "SELECT * FROM threat_feature_links WHERE threat_id = ? ORDER BY id",
  )
  .bind(threat_id)
  .fetch_all(pool)
  .await
}

// ============================================================================
// Condition CRUD operations
// ============================================================================

/// Database row for a condition
#[derive(Debug, sqlx::FromRow)]
pub struct ConditionRow {
  pub id: i64,
  pub audit_id: String,
  pub subject_topic: String,
  pub condition_type: String,
  pub description: String,
  #[sqlx(rename = "author_id")]
  pub author: Author,
  pub created_at: String,
}

/// Database row for a condition evaluation
#[derive(Debug, sqlx::FromRow)]
pub struct ConditionEvaluationRow {
  pub id: i64,
  pub condition_id: i64,
  pub question: String,
  pub answer: String,
}

/// Creates a new condition and returns the row
pub async fn create_condition(
  pool: &SqlitePool,
  audit_id: &str,
  subject_topic: &str,
  condition_type: &str,
  description: &str,
  author_id: Author,
) -> Result<ConditionRow, sqlx::Error> {
  let result = sqlx::query(
    r#"
        INSERT INTO conditions (audit_id, subject_topic, condition_type, description, author_id)
        VALUES (?, ?, ?, ?, ?)
        "#,
  )
  .bind(audit_id)
  .bind(subject_topic)
  .bind(condition_type)
  .bind(description)
  .bind(author_id)
  .execute(pool)
  .await?;

  let id = result.last_insert_rowid();
  sqlx::query_as::<_, ConditionRow>("SELECT * FROM conditions WHERE id = ?")
    .bind(id)
    .fetch_one(pool)
    .await
}

/// Adds an evaluation (question/answer pair) to a condition
pub async fn add_condition_evaluation(
  pool: &SqlitePool,
  condition_id: i64,
  question: &str,
  answer: &str,
) -> Result<ConditionEvaluationRow, sqlx::Error> {
  let result = sqlx::query(
    r#"
        INSERT INTO condition_evaluations (condition_id, question, answer)
        VALUES (?, ?, ?)
        "#,
  )
  .bind(condition_id)
  .bind(question)
  .bind(answer)
  .execute(pool)
  .await?;

  let id = result.last_insert_rowid();
  sqlx::query_as::<_, ConditionEvaluationRow>(
    "SELECT * FROM condition_evaluations WHERE id = ?",
  )
  .bind(id)
  .fetch_one(pool)
  .await
}

/// Deletes a condition and its evaluations
pub async fn delete_condition(
  pool: &SqlitePool,
  condition_id: i64,
) -> Result<(), sqlx::Error> {
  sqlx::query("DELETE FROM condition_evaluations WHERE condition_id = ?")
    .bind(condition_id)
    .execute(pool)
    .await?;
  sqlx::query("DELETE FROM conditions WHERE id = ?")
    .bind(condition_id)
    .execute(pool)
    .await?;
  Ok(())
}

/// Gets all conditions for a subject topic
pub async fn get_conditions_for_subject(
  pool: &SqlitePool,
  audit_id: &str,
  subject_topic: &str,
) -> Result<Vec<ConditionRow>, sqlx::Error> {
  sqlx::query_as::<_, ConditionRow>(
    "SELECT * FROM conditions WHERE audit_id = ? AND subject_topic = ? ORDER BY id",
  )
  .bind(audit_id)
  .bind(subject_topic)
  .fetch_all(pool)
  .await
}

/// Gets all evaluations for a condition
pub async fn get_condition_evaluations(
  pool: &SqlitePool,
  condition_id: i64,
) -> Result<Vec<ConditionEvaluationRow>, sqlx::Error> {
  sqlx::query_as::<_, ConditionEvaluationRow>(
    "SELECT * FROM condition_evaluations WHERE condition_id = ? ORDER BY id",
  )
  .bind(condition_id)
  .fetch_all(pool)
  .await
}

// ============================================================================
// Threat CRUD operations
// ============================================================================

/// Database row for a threat
#[derive(Debug, sqlx::FromRow)]
pub struct ThreatRow {
  pub id: i64,
  pub audit_id: String,
  pub subject_topic: String,
  pub description: String,
  #[sqlx(rename = "author_id")]
  pub author: Author,
  pub created_at: String,
  pub severity: Option<String>,
}

/// Creates a new threat and returns the row
pub async fn create_threat(
  pool: &SqlitePool,
  audit_id: &str,
  subject_topic: &str,
  description: &str,
  author_id: Author,
  severity: Option<&str>,
) -> Result<ThreatRow, sqlx::Error> {
  let result = sqlx::query(
    r#"
        INSERT INTO threats (audit_id, subject_topic, description, author_id, severity)
        VALUES (?, ?, ?, ?, ?)
        "#,
  )
  .bind(audit_id)
  .bind(subject_topic)
  .bind(description)
  .bind(author_id)
  .bind(severity)
  .execute(pool)
  .await?;

  let id = result.last_insert_rowid();
  sqlx::query_as::<_, ThreatRow>("SELECT * FROM threats WHERE id = ?")
    .bind(id)
    .fetch_one(pool)
    .await
}

/// Deletes all threats and invariants for an audit, leaving features and
/// requirements intact.
pub async fn delete_all_threats_for_audit(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<(), sqlx::Error> {
  // Delete invariant source topic associations
  sqlx::query(
    r#"
        DELETE FROM invariant_source_topics WHERE invariant_id IN (
            SELECT i.id FROM invariants i
            JOIN threats t ON i.threat_id = t.id
            WHERE t.audit_id = ?
        )
        "#,
  )
  .bind(audit_id)
  .execute(pool)
  .await?;

  // Delete invariants
  sqlx::query(
    r#"
        DELETE FROM invariants WHERE threat_id IN (
            SELECT id FROM threats WHERE audit_id = ?
        )
        "#,
  )
  .bind(audit_id)
  .execute(pool)
  .await?;

  // Delete threats
  sqlx::query("DELETE FROM threats WHERE audit_id = ?")
    .bind(audit_id)
    .execute(pool)
    .await?;

  Ok(())
}

/// Deletes a threat and its associated invariants
pub async fn delete_threat(
  pool: &SqlitePool,
  threat_id: i64,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    r#"
        DELETE FROM invariant_source_topics WHERE invariant_id IN (
            SELECT id FROM invariants WHERE threat_id = ?
        )
        "#,
  )
  .bind(threat_id)
  .execute(pool)
  .await?;
  sqlx::query("DELETE FROM invariants WHERE threat_id = ?")
    .bind(threat_id)
    .execute(pool)
    .await?;
  sqlx::query("DELETE FROM threats WHERE id = ?")
    .bind(threat_id)
    .execute(pool)
    .await?;
  Ok(())
}

// ============================================================================
// Invariant CRUD operations
// ============================================================================

/// Database row for an invariant
#[derive(Debug, sqlx::FromRow)]
pub struct InvariantRow {
  pub id: i64,
  pub threat_id: i64,
  pub description: String,
  #[sqlx(rename = "author_id")]
  pub author: Author,
  pub created_at: String,
  pub severity: Option<String>,
}

/// Creates a new invariant and returns the row
pub async fn create_invariant(
  pool: &SqlitePool,
  threat_id: i64,
  description: &str,
  author_id: Author,
  severity: Option<&str>,
) -> Result<InvariantRow, sqlx::Error> {
  let result = sqlx::query(
    r#"
        INSERT INTO invariants (threat_id, description, author_id, severity)
        VALUES (?, ?, ?, ?)
        "#,
  )
  .bind(threat_id)
  .bind(description)
  .bind(author_id)
  .bind(severity)
  .execute(pool)
  .await?;

  let id = result.last_insert_rowid();
  sqlx::query_as::<_, InvariantRow>("SELECT * FROM invariants WHERE id = ?")
    .bind(id)
    .fetch_one(pool)
    .await
}

/// Deletes an invariant and its source topic associations
pub async fn delete_invariant(
  pool: &SqlitePool,
  invariant_id: i64,
) -> Result<(), sqlx::Error> {
  sqlx::query("DELETE FROM invariant_source_topics WHERE invariant_id = ?")
    .bind(invariant_id)
    .execute(pool)
    .await?;
  sqlx::query("DELETE FROM invariants WHERE id = ?")
    .bind(invariant_id)
    .execute(pool)
    .await?;
  Ok(())
}

/// Adds a source topic to an invariant
pub async fn add_invariant_source_topic(
  pool: &SqlitePool,
  invariant_id: i64,
  topic_id: &str,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    r#"
        INSERT OR IGNORE INTO invariant_source_topics (invariant_id, topic_id)
        VALUES (?, ?)
        "#,
  )
  .bind(invariant_id)
  .bind(topic_id)
  .execute(pool)
  .await?;
  Ok(())
}

/// Removes a source topic from an invariant
pub async fn remove_invariant_source_topic(
  pool: &SqlitePool,
  invariant_id: i64,
  topic_id: &str,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    "DELETE FROM invariant_source_topics WHERE invariant_id = ? AND topic_id = ?",
  )
  .bind(invariant_id)
  .bind(topic_id)
  .execute(pool)
  .await?;
  Ok(())
}

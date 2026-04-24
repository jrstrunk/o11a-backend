use crate::collaborator::models::*;
use crate::collaborator::parser;
use crate::core::{self, DataContext, Requirement, topic};
use sqlx::SqlitePool;

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
    .insert(comment_topic.clone(), core::Node::Comment(nodes));

  let mut mentioned_topics: Vec<topic::Topic> = mentions.clone();
  mentioned_topics.sort_unstable();
  mentioned_topics.dedup();

  audit_data.topic_metadata.insert(
    comment_topic.clone(),
    core::TopicMetadata::CommentTopic {
      topic: comment_topic.clone(),
      author_id: comment.author_id,
      comment_type: core::CommentType::from_str(&comment.comment_type).expect(
        &format!(
          "Unknown comment type '{}' in comment {}",
          comment.comment_type, comment.id
        ),
      ),
      target_topic: target_topic.clone(),
      created_at: comment.created_at.clone(),
      scope: scope.to_scope(),
      mentioned_topics,
    },
  );

  let comments = audit_data.comment_index.entry(target_topic).or_default();
  if !comments.contains(&comment_topic) {
    comments.push(comment_topic.clone());
  }

  for mention in &mentions {
    let entries = audit_data
      .mentions_index
      .entry(mention.clone())
      .or_default();
    if !entries.contains(&comment_topic) {
      entries.push(comment_topic.clone());
    }
  }

  mentions
}

/// Load and parse all comments on server startup. Returns the number loaded.
pub async fn load_and_parse_all_comments(
  pool: &SqlitePool,
  data_context: &mut DataContext,
) -> Result<usize, sqlx::Error> {
  let comments = sqlx::query_as::<_, Comment>(
    "SELECT * FROM comments WHERE status != 'hidden'",
  )
  .fetch_all(pool)
  .await?;

  let count = comments.len();

  for comment in &comments {
    let scope: ScopeInfo =
      serde_json::from_str(&comment.scope).unwrap_or_default();
    ingest_comment(data_context, comment, &scope);
  }

  Ok(count)
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
    .bind(request.author_id)
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
// Feature CRUD operations
// ============================================================================

/// Database row for a feature
#[derive(Debug, sqlx::FromRow)]
pub struct FeatureRow {
  pub id: i64,
  pub audit_id: String,
  pub name: String,
  pub description: String,
  pub author_id: i64,
  pub created_at: String,
}

/// Creates a new feature and returns the row
pub async fn create_feature(
  pool: &SqlitePool,
  audit_id: &str,
  name: &str,
  description: &str,
  author_id: i64,
) -> Result<FeatureRow, sqlx::Error> {
  let result = sqlx::query(
    r#"
        INSERT INTO features (audit_id, name, description, author_id)
        VALUES (?, ?, ?, ?)
        "#,
  )
  .bind(audit_id)
  .bind(name)
  .bind(description)
  .bind(author_id)
  .execute(pool)
  .await?;

  let id = result.last_insert_rowid();
  sqlx::query_as::<_, FeatureRow>("SELECT * FROM features WHERE id = ?")
    .bind(id)
    .fetch_one(pool)
    .await
}

/// Deletes all features and their associated data for an audit.
/// Cascades all feature-related data for an audit.
pub async fn delete_all_features_for_audit(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<(), sqlx::Error> {
  // Delete requirement documentation topic associations via link table
  sqlx::query(
    r#"
        DELETE FROM requirement_documentation_topics WHERE requirement_id IN (
            SELECT requirement_id FROM feature_requirement_links WHERE audit_id = ?
        )
        "#,
  )
  .bind(audit_id)
  .execute(pool)
  .await?;

  // Delete requirements via link table
  sqlx::query(
    r#"
        DELETE FROM requirements WHERE id IN (
            SELECT requirement_id FROM feature_requirement_links WHERE audit_id = ?
        )
        "#,
  )
  .bind(audit_id)
  .execute(pool)
  .await?;

  // Delete feature-requirement and feature-behavior links
  delete_all_feature_links_for_audit(pool, audit_id).await?;

  // Delete features
  sqlx::query("DELETE FROM features WHERE audit_id = ?")
    .bind(audit_id)
    .execute(pool)
    .await?;

  Ok(())
}

/// Load all features and requirements from the database.
/// Returns the number of features loaded.
pub async fn load_all_features(
  pool: &SqlitePool,
  data_context: &mut DataContext,
) -> Result<usize, sqlx::Error> {
  let features = sqlx::query_as::<_, FeatureRow>("SELECT * FROM features")
    .fetch_all(pool)
    .await?;

  let requirements =
    sqlx::query_as::<_, RequirementRow>("SELECT * FROM requirements")
      .fetch_all(pool)
      .await?;

  let req_doc_topics = sqlx::query_as::<_, RequirementDocumentationTopicRow>(
    "SELECT * FROM requirement_documentation_topics",
  )
  .fetch_all(pool)
  .await?;

  let threats = sqlx::query_as::<_, ThreatRow>("SELECT * FROM threats")
    .fetch_all(pool)
    .await?;

  let invariants =
    sqlx::query_as::<_, InvariantRow>("SELECT * FROM invariants")
      .fetch_all(pool)
      .await?;

  let inv_source_topics = sqlx::query_as::<_, InvariantSourceTopicRow>(
    "SELECT * FROM invariant_source_topics",
  )
  .fetch_all(pool)
  .await?;

  // Group requirement documentation topics by requirement_id
  let mut doc_by_req: std::collections::HashMap<
    i64,
    Vec<&RequirementDocumentationTopicRow>,
  > = std::collections::HashMap::new();
  for rdt in &req_doc_topics {
    doc_by_req.entry(rdt.requirement_id).or_default().push(rdt);
  }

  // Group invariants by threat_id
  let mut invs_by_threat: std::collections::HashMap<i64, Vec<&InvariantRow>> =
    std::collections::HashMap::new();
  for inv in &invariants {
    invs_by_threat.entry(inv.threat_id).or_default().push(inv);
  }

  // Group invariant source topics by invariant_id
  let mut src_by_inv: std::collections::HashMap<
    i64,
    Vec<&InvariantSourceTopicRow>,
  > = std::collections::HashMap::new();
  for ist in &inv_source_topics {
    src_by_inv.entry(ist.invariant_id).or_default().push(ist);
  }

  let count = features.len();

  // Build a mapping from requirement_id -> audit_id via feature_requirement_links
  // so we can load requirements into the correct audit
  let mut req_audit_map: std::collections::HashMap<i64, String> =
    std::collections::HashMap::new();
  let frl_rows_for_mapping = sqlx::query_as::<_, FeatureRequirementLinkRow>(
    "SELECT * FROM feature_requirement_links",
  )
  .fetch_all(pool)
  .await?;
  for frl in &frl_rows_for_mapping {
    req_audit_map.insert(frl.requirement_id, frl.audit_id.clone());
  }

  // Load features
  for row in &features {
    if let Some(audit_data) = data_context.get_audit_mut(&row.audit_id) {
      let feature_topic = topic::new_feature_topic(row.id as i32);
      audit_data.topic_metadata.insert(
        feature_topic.clone(),
        core::TopicMetadata::FeatureTopic {
          topic: feature_topic,
          name: row.name.clone(),
          description: row.description.clone(),
          author_id: row.author_id,
          created_at: row.created_at.clone(),
        },
      );
    }
  }

  // Load requirements (independent of features — links come from feature_requirement_links)
  for req in &requirements {
    let audit_id = match req_audit_map.get(&req.id) {
      Some(id) => id.as_str(),
      None => continue,
    };
    if let Some(audit_data) = data_context.get_audit_mut(audit_id) {
      let req_topic = topic::new_requirement_topic(req.id as i32);

      let mut documentation_topics = Vec::new();
      if let Some(docs) = doc_by_req.get(&req.id) {
        for d in docs {
          documentation_topics.push(topic::new_topic(&d.topic_id));
        }
      }

      audit_data.topic_metadata.insert(
        req_topic.clone(),
        core::TopicMetadata::RequirementTopic {
          topic: req_topic.clone(),
          description: req.description.clone(),
          section_topic: topic::new_topic(
            req.section_topic.as_deref().unwrap_or(""),
          ),
          author_id: req.author_id,
          created_at: req.created_at.clone(),
        },
      );

      audit_data.requirements.insert(
        req_topic,
        Requirement {
          documentation_topics,
        },
      );
    }
  }

  // Load threats (independent of features, keyed by subject_topic)
  for th in &threats {
    if let Some(audit_data) = data_context.get_audit_mut(&th.audit_id) {
      let threat_topic = topic::new_attack_vector_topic(th.id as i32);
      let subject_topic = topic::new_topic(&th.subject_topic);

      // Load invariants for this threat
      let mut invariant_topics = Vec::new();
      if let Some(inv_rows) = invs_by_threat.get(&th.id) {
        for inv in inv_rows {
          let inv_topic = topic::new_invariant_topic(inv.id as i32);
          invariant_topics.push(inv_topic.clone());

          let mut source_topics = Vec::new();
          if let Some(srcs) = src_by_inv.get(&inv.id) {
            for s in srcs {
              source_topics.push(topic::new_topic(&s.topic_id));
            }
          }

          let severity = inv
            .severity
            .as_deref()
            .and_then(core::ThreatSeverity::from_str);

          audit_data.topic_metadata.insert(
            inv_topic.clone(),
            core::TopicMetadata::InvariantTopic {
              topic: inv_topic.clone(),
              description: inv.description.clone(),
              threat_topic: threat_topic.clone(),
              author_id: inv.author_id,
              created_at: inv.created_at.clone(),
              severity,
            },
          );

          audit_data
            .invariants
            .insert(inv_topic, core::Invariant { source_topics });
        }
      }

      let severity = th
        .severity
        .as_deref()
        .and_then(core::ThreatSeverity::from_str);

      audit_data.topic_metadata.insert(
        threat_topic.clone(),
        core::TopicMetadata::ThreatTopic {
          topic: threat_topic.clone(),
          description: th.description.clone(),
          subject_topic: subject_topic.clone(),
          author_id: th.author_id,
          created_at: th.created_at.clone(),
          severity,
        },
      );

      audit_data
        .threats
        .insert(threat_topic, core::Threat { invariant_topics });
    }
  }

  // Load feature-requirement links
  let frl_rows = sqlx::query_as::<_, FeatureRequirementLinkRow>(
    "SELECT * FROM feature_requirement_links",
  )
  .fetch_all(pool)
  .await?;

  for row in &frl_rows {
    if let Some(audit_data) = data_context.get_audit_mut(&row.audit_id) {
      let feature_topic = topic::new_feature_topic(row.feature_id as i32);
      let req_topic = topic::new_requirement_topic(row.requirement_id as i32);
      let reqs = audit_data
        .feature_requirement_links
        .entry(feature_topic)
        .or_default();
      if !reqs.contains(&req_topic) {
        reqs.push(req_topic);
      }
    }
  }

  // Load feature-behavior links
  let fbl_rows = sqlx::query_as::<_, FeatureBehaviorLinkRow>(
    "SELECT * FROM feature_behavior_links",
  )
  .fetch_all(pool)
  .await?;

  for row in &fbl_rows {
    if let Some(audit_data) = data_context.get_audit_mut(&row.audit_id) {
      let feature_topic = topic::new_feature_topic(row.feature_id as i32);
      let beh_topic = topic::new_behavior_topic(row.behavior_id as i32);
      let behs = audit_data
        .feature_behavior_links
        .entry(feature_topic)
        .or_default();
      if !behs.contains(&beh_topic) {
        behs.push(beh_topic);
      }
    }
  }

  // Load semantic links
  let semantic_link_rows =
    sqlx::query_as::<_, SemanticLinkRow>("SELECT * FROM semantic_links")
      .fetch_all(pool)
      .await?;

  // Load semantic link doc topics
  let semantic_doc_rows =
    sqlx::query_as::<_, SemanticLinkDocRow>("SELECT * FROM semantic_link_docs")
      .fetch_all(pool)
      .await?;

  // Group doc topics by semantic_link_id
  let mut docs_by_link: std::collections::HashMap<i64, Vec<String>> =
    std::collections::HashMap::new();
  for row in &semantic_doc_rows {
    docs_by_link
      .entry(row.semantic_link_id)
      .or_default()
      .push(row.documentation_topic.clone());
  }

  for row in &semantic_link_rows {
    if let Some(audit_data) = data_context.get_audit_mut(&row.audit_id) {
      let documentation_topics: Vec<topic::Topic> = docs_by_link
        .get(&row.id)
        .map(|dts| dts.iter().map(|dt| topic::new_topic(dt)).collect())
        .unwrap_or_default();

      // declaration_topic is already the base topic (transitive topics
      // are resolved before persisting), so no redirect needed.
      let decl_topic = topic::new_topic(&row.declaration_topic);
      let sem_topic = topic::new_functional_property_topic(row.id as i32);

      audit_data.topic_metadata.insert(
        sem_topic.clone(),
        core::TopicMetadata::FunctionalSemanticTopic {
          topic: sem_topic,
          description: row.semantic_text.clone(),
          declaration_topic: decl_topic,
          documentation_topics,
          author_id: row.author_id,
          created_at: row.created_at.clone(),
        },
      );
    }
  }

  // Load threat-feature links (impact analysis)
  let tfl_rows = sqlx::query_as::<_, ThreatFeatureLinkRow>(
    "SELECT * FROM threat_feature_links",
  )
  .fetch_all(pool)
  .await?;

  for row in &tfl_rows {
    if let Some(audit_data) = data_context.get_audit_mut(&row.audit_id) {
      let relation = match core::ThreatFeatureRelation::from_str(&row.relation)
      {
        Some(r) => r,
        None => continue,
      };
      let severity = match core::ThreatSeverity::from_str(&row.severity) {
        Some(s) => s,
        None => continue,
      };
      audit_data
        .threat_feature_links
        .push(core::ThreatFeatureLink {
          threat_topic: topic::new_attack_vector_topic(row.threat_id as i32),
          feature_topic: topic::new_feature_topic(row.feature_id as i32),
          relation,
          severity,
        });
    }
  }

  // Load conditions
  let condition_rows =
    sqlx::query_as::<_, ConditionRow>("SELECT * FROM conditions")
      .fetch_all(pool)
      .await?;

  let cond_eval_rows = sqlx::query_as::<_, ConditionEvaluationRow>(
    "SELECT * FROM condition_evaluations",
  )
  .fetch_all(pool)
  .await?;

  let mut evals_by_cond: std::collections::HashMap<
    i64,
    Vec<&ConditionEvaluationRow>,
  > = std::collections::HashMap::new();
  for eval in &cond_eval_rows {
    evals_by_cond
      .entry(eval.condition_id)
      .or_default()
      .push(eval);
  }

  for row in &condition_rows {
    if let Some(audit_data) = data_context.get_audit_mut(&row.audit_id) {
      let evaluations = evals_by_cond
        .get(&row.id)
        .map(|evals| {
          evals
            .iter()
            .map(|e| core::ConditionEvaluation {
              question: e.question.clone(),
              answer: e.answer.clone(),
            })
            .collect()
        })
        .unwrap_or_default();

      let condition_type = match row.condition_type.as_str() {
        "state_write" => core::NonPureSubjectType::StateWrite,
        "state_read" => core::NonPureSubjectType::StateRead,
        "external_call" => core::NonPureSubjectType::ExternalCall,
        "delegate_call" => core::NonPureSubjectType::DelegateCall,
        "inline_assembly" => core::NonPureSubjectType::InlineAssembly,
        "create" => core::NonPureSubjectType::Create,
        _ => continue,
      };

      audit_data.conditions.push(core::Condition {
        subject_topic: topic::new_topic(&row.subject_topic),
        condition_type,
        description: row.description.clone(),
        evaluations,
      });
    }
  }

  // Load behaviors
  let behavior_rows =
    sqlx::query_as::<_, BehaviorRow>("SELECT * FROM behaviors")
      .fetch_all(pool)
      .await?;

  for row in &behavior_rows {
    if let Some(audit_data) = data_context.get_audit_mut(&row.audit_id) {
      let beh_topic = topic::new_behavior_topic(row.id as i32);
      let member_topic = topic::new_topic(&row.member_topic);

      audit_data.topic_metadata.insert(
        beh_topic.clone(),
        core::TopicMetadata::BehaviorTopic {
          topic: beh_topic,
          description: row.description.clone(),
          member_topic: member_topic.clone(),
          author_id: row.author_id,
          created_at: row.created_at.clone(),
        },
      );
    }
  }

  Ok(count)
}

// ============================================================================
// Semantic link operations
// ============================================================================

#[derive(Debug, sqlx::FromRow)]
pub struct SemanticLinkRow {
  pub id: i64,
  pub audit_id: String,
  pub declaration_topic: String,
  pub semantic_text: String,
  pub author_id: i64,
  pub created_at: String,
}

#[derive(Debug, sqlx::FromRow)]
struct SemanticLinkDocRow {
  semantic_link_id: i64,
  documentation_topic: String,
}

/// Adds a semantic link with its documentation topics.
pub async fn add_semantic_link(
  pool: &SqlitePool,
  audit_id: &str,
  declaration_topic: &str,
  semantic_text: &str,
  author_id: i64,
  documentation_topics: &[&str],
) -> Result<i64, sqlx::Error> {
  let result = sqlx::query(
    r#"
        INSERT INTO semantic_links (audit_id, declaration_topic, semantic_text, author_id)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(audit_id, declaration_topic, semantic_text) DO NOTHING
        "#,
  )
  .bind(audit_id)
  .bind(declaration_topic)
  .bind(semantic_text)
  .bind(author_id)
  .execute(pool)
  .await?;

  // Get the ID (either from insert or existing row)
  let link_id = if result.rows_affected() > 0 {
    result.last_insert_rowid()
  } else {
    sqlx::query_scalar::<_, i64>(
      "SELECT id FROM semantic_links WHERE audit_id = ? AND declaration_topic = ? AND semantic_text = ?",
    )
    .bind(audit_id)
    .bind(declaration_topic)
    .bind(semantic_text)
    .fetch_one(pool)
    .await?
  };

  for dt in documentation_topics {
    sqlx::query(
      "INSERT OR IGNORE INTO semantic_link_docs (semantic_link_id, documentation_topic) VALUES (?, ?)",
    )
    .bind(link_id)
    .bind(dt)
    .execute(pool)
    .await?;
  }
  Ok(link_id)
}

/// Deletes all semantic links for an audit
pub async fn delete_all_semantic_links(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<(), sqlx::Error> {
  // Delete junction table rows first (FK cascade may not be enabled)
  sqlx::query(
    "DELETE FROM semantic_link_docs WHERE semantic_link_id IN (SELECT id FROM semantic_links WHERE audit_id = ?)",
  )
  .bind(audit_id)
  .execute(pool)
  .await?;
  sqlx::query("DELETE FROM semantic_links WHERE audit_id = ?")
    .bind(audit_id)
    .execute(pool)
    .await?;
  Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct FeatureRequirementLinkRow {
  #[allow(dead_code)]
  id: i64,
  audit_id: String,
  feature_id: i64,
  requirement_id: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct FeatureBehaviorLinkRow {
  #[allow(dead_code)]
  id: i64,
  audit_id: String,
  feature_id: i64,
  behavior_id: i64,
}

// ============================================================================
// Feature-requirement and feature-behavior link operations
// ============================================================================

pub async fn add_feature_requirement_link(
  pool: &SqlitePool,
  audit_id: &str,
  feature_id: i64,
  requirement_id: i64,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    "INSERT OR IGNORE INTO feature_requirement_links (audit_id, feature_id, requirement_id) VALUES (?, ?, ?)",
  )
  .bind(audit_id)
  .bind(feature_id)
  .bind(requirement_id)
  .execute(pool)
  .await?;
  Ok(())
}

pub async fn add_feature_behavior_link(
  pool: &SqlitePool,
  audit_id: &str,
  feature_id: i64,
  behavior_id: i64,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    "INSERT OR IGNORE INTO feature_behavior_links (audit_id, feature_id, behavior_id) VALUES (?, ?, ?)",
  )
  .bind(audit_id)
  .bind(feature_id)
  .bind(behavior_id)
  .execute(pool)
  .await?;
  Ok(())
}

pub async fn delete_all_feature_links_for_audit(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<(), sqlx::Error> {
  sqlx::query("DELETE FROM feature_requirement_links WHERE audit_id = ?")
    .bind(audit_id)
    .execute(pool)
    .await?;
  sqlx::query("DELETE FROM feature_behavior_links WHERE audit_id = ?")
    .bind(audit_id)
    .execute(pool)
    .await?;
  Ok(())
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
  pub author_id: i64,
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
  author_id: i64,
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
// Behavior CRUD operations
// ============================================================================

/// Database row for a behavior
#[derive(Debug, sqlx::FromRow)]
pub struct BehaviorRow {
  pub id: i64,
  pub audit_id: String,
  pub member_topic: String,
  pub description: String,
  pub author_id: i64,
  pub created_at: String,
}

/// Creates a new behavior and returns the row
pub async fn create_behavior(
  pool: &SqlitePool,
  audit_id: &str,
  member_topic: &str,
  description: &str,
  author_id: i64,
) -> Result<BehaviorRow, sqlx::Error> {
  let result = sqlx::query(
    r#"
        INSERT INTO behaviors (audit_id, member_topic, description, author_id)
        VALUES (?, ?, ?, ?)
        "#,
  )
  .bind(audit_id)
  .bind(member_topic)
  .bind(description)
  .bind(author_id)
  .execute(pool)
  .await?;

  let id = result.last_insert_rowid();
  sqlx::query_as::<_, BehaviorRow>("SELECT * FROM behaviors WHERE id = ?")
    .bind(id)
    .fetch_one(pool)
    .await
}

// ============================================================================
// Requirement CRUD operations
// ============================================================================

/// Database row for a requirement
#[derive(Debug, sqlx::FromRow)]
pub struct RequirementRow {
  pub id: i64,
  pub section_topic: Option<String>,
  pub description: String,
  pub author_id: i64,
  pub created_at: String,
}

/// Creates a new requirement and returns the row
pub async fn create_requirement(
  pool: &SqlitePool,
  description: &str,
  author_id: i64,
) -> Result<RequirementRow, sqlx::Error> {
  let result = sqlx::query(
    r#"
        INSERT INTO requirements (description, author_id)
        VALUES (?, ?)
        "#,
  )
  .bind(description)
  .bind(author_id)
  .execute(pool)
  .await?;

  let id = result.last_insert_rowid();
  sqlx::query_as::<_, RequirementRow>("SELECT * FROM requirements WHERE id = ?")
    .bind(id)
    .fetch_one(pool)
    .await
}

/// Database row for a requirement documentation topic association
#[derive(Debug, sqlx::FromRow)]
pub struct RequirementDocumentationTopicRow {
  pub id: i64,
  pub requirement_id: i64,
  pub topic_id: String,
}

/// Adds a documentation topic to a requirement
pub async fn add_requirement_documentation_topic(
  pool: &SqlitePool,
  requirement_id: i64,
  topic_id: &str,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    r#"
        INSERT OR IGNORE INTO requirement_documentation_topics (requirement_id, topic_id)
        VALUES (?, ?)
        "#,
  )
  .bind(requirement_id)
  .bind(topic_id)
  .execute(pool)
  .await?;
  Ok(())
}

/// Removes a documentation topic from a requirement
/// Sets the section_topic on a requirement
pub async fn set_requirement_section(
  pool: &SqlitePool,
  requirement_id: i64,
  section_topic: &str,
) -> Result<(), sqlx::Error> {
  sqlx::query("UPDATE requirements SET section_topic = ? WHERE id = ?")
    .bind(section_topic)
    .bind(requirement_id)
    .execute(pool)
    .await?;
  Ok(())
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
  pub author_id: i64,
  pub created_at: String,
  pub severity: Option<String>,
}

/// Creates a new threat and returns the row
pub async fn create_threat(
  pool: &SqlitePool,
  audit_id: &str,
  subject_topic: &str,
  description: &str,
  author_id: i64,
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
  pub author_id: i64,
  pub created_at: String,
  pub severity: Option<String>,
}

/// Database row for an invariant source topic association
#[derive(Debug, sqlx::FromRow)]
pub struct InvariantSourceTopicRow {
  pub id: i64,
  pub invariant_id: i64,
  pub topic_id: String,
}

/// Creates a new invariant and returns the row
pub async fn create_invariant(
  pool: &SqlitePool,
  threat_id: i64,
  description: &str,
  author_id: i64,
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

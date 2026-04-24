use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::str::FromStr;

pub async fn create_pool(
  database_url: &str,
) -> Result<SqlitePool, sqlx::Error> {
  let options =
    SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);

  let pool = SqlitePoolOptions::new()
    .max_connections(5)
    .connect_with(options)
    .await?;

  Ok(pool)
}

/// Creates the full database schema. Uses CREATE TABLE IF NOT EXISTS so it is
/// safe to call on an existing database (no data loss). All tables and indices
/// used by the application are defined here.
pub async fn init_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
  // ── Comments ────────────────────────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS comments (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        topic_id TEXT NOT NULL,
        content_markdown TEXT NOT NULL,
        author_id INTEGER NOT NULL,
        comment_type TEXT NOT NULL DEFAULT 'note',
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        status TEXT NOT NULL DEFAULT 'active',
        scope TEXT NOT NULL DEFAULT '{}'
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_comments_audit_topic ON comments(audit_id, topic_id)",
  )
  .execute(pool)
  .await?;
  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_comments_audit_status ON comments(audit_id, status)",
  )
  .execute(pool)
  .await?;
  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_comments_author ON comments(author_id)",
  )
  .execute(pool)
  .await?;
  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_comments_type ON comments(audit_id, comment_type)",
  )
  .execute(pool)
  .await?;

  // ── Votes ───────────────────────────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS comment_votes (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        comment_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        vote INTEGER NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        UNIQUE(comment_id, user_id)
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_votes_comment ON comment_votes(comment_id)",
  )
  .execute(pool)
  .await?;
  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_votes_user ON comment_votes(user_id)",
  )
  .execute(pool)
  .await?;

  // ── Features ────────────────────────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS features (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        name TEXT NOT NULL,
        description TEXT NOT NULL,
        author_id INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_features_audit ON features(audit_id)",
  )
  .execute(pool)
  .await?;

  // ── Requirements ────────────────────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS requirements (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        description TEXT NOT NULL,
        section_topic TEXT,
        author_id INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS requirement_source_topics (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        requirement_id INTEGER NOT NULL,
        topic_id TEXT NOT NULL,
        UNIQUE(requirement_id, topic_id)
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_req_source_topics_req ON requirement_source_topics(requirement_id)",
  )
  .execute(pool)
  .await?;

  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS requirement_documentation_topics (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        requirement_id INTEGER NOT NULL,
        topic_id TEXT NOT NULL,
        UNIQUE(requirement_id, topic_id)
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_req_doc_topics_req ON requirement_documentation_topics(requirement_id)",
  )
  .execute(pool)
  .await?;

  // ── Threats ─────────────────────────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS threats (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        subject_topic TEXT NOT NULL,
        description TEXT NOT NULL,
        author_id INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        severity TEXT NOT NULL DEFAULT 'medium'
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_threats_audit ON threats(audit_id)",
  )
  .execute(pool)
  .await?;
  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_threats_subject ON threats(subject_topic)",
  )
  .execute(pool)
  .await?;

  // ── Invariants ──────────────────────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS invariants (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        threat_id INTEGER NOT NULL,
        description TEXT NOT NULL,
        author_id INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        severity TEXT NOT NULL DEFAULT 'medium'
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_invariants_threat ON invariants(threat_id)",
  )
  .execute(pool)
  .await?;

  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS invariant_source_topics (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        invariant_id INTEGER NOT NULL,
        topic_id TEXT NOT NULL,
        UNIQUE(invariant_id, topic_id)
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_inv_source_topics_inv ON invariant_source_topics(invariant_id)",
  )
  .execute(pool)
  .await?;

  // ── Semantic links ──────────────────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS semantic_links (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        declaration_topic TEXT NOT NULL,
        semantic_text TEXT NOT NULL,
        author_id INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        UNIQUE(audit_id, declaration_topic, semantic_text)
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS semantic_link_docs (
        semantic_link_id INTEGER NOT NULL,
        documentation_topic TEXT NOT NULL,
        UNIQUE(semantic_link_id, documentation_topic),
        FOREIGN KEY (semantic_link_id) REFERENCES semantic_links(id) ON DELETE CASCADE
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_semantic_links_audit ON semantic_links(audit_id)",
  )
  .execute(pool)
  .await?;

  // ── Threat-feature links ────────────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS threat_feature_links (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        threat_id INTEGER NOT NULL,
        feature_id INTEGER NOT NULL,
        relation TEXT NOT NULL,
        severity TEXT NOT NULL,
        UNIQUE(threat_id, feature_id)
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_tfl_threat ON threat_feature_links(threat_id)",
  )
  .execute(pool)
  .await?;
  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_tfl_feature ON threat_feature_links(feature_id)",
  )
  .execute(pool)
  .await?;

  // ── Conditions ──────────────────────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS conditions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        subject_topic TEXT NOT NULL,
        condition_type TEXT NOT NULL,
        description TEXT NOT NULL,
        author_id INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_conditions_audit ON conditions(audit_id)",
  )
  .execute(pool)
  .await?;
  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_conditions_subject ON conditions(subject_topic)",
  )
  .execute(pool)
  .await?;

  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS condition_evaluations (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        condition_id INTEGER NOT NULL,
        question TEXT NOT NULL,
        answer TEXT NOT NULL
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_cond_evals_cond ON condition_evaluations(condition_id)",
  )
  .execute(pool)
  .await?;

  // ── Behaviors ───────────────────────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS behaviors (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        member_topic TEXT NOT NULL,
        description TEXT NOT NULL,
        author_id INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_behaviors_audit ON behaviors(audit_id)",
  )
  .execute(pool)
  .await?;

  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS behavior_source_topics (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        behavior_id INTEGER NOT NULL,
        topic_id TEXT NOT NULL,
        UNIQUE(behavior_id, topic_id)
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_beh_source_topics_beh ON behavior_source_topics(behavior_id)",
  )
  .execute(pool)
  .await?;

  // ── Feature-requirement links ───────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS feature_requirement_links (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        feature_id INTEGER NOT NULL,
        requirement_id INTEGER NOT NULL,
        UNIQUE(feature_id, requirement_id)
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_frl_audit ON feature_requirement_links(audit_id)",
  )
  .execute(pool)
  .await?;

  // ── Feature-behavior links ──────────────────────────────────────────────
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS feature_behavior_links (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        feature_id INTEGER NOT NULL,
        behavior_id INTEGER NOT NULL,
        UNIQUE(feature_id, behavior_id)
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_fbl_audit ON feature_behavior_links(audit_id)",
  )
  .execute(pool)
  .await?;

  Ok(())
}

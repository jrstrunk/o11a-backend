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

  // ── User features ───────────────────────────────────────────────────────
  // Mutable, user-authored companion to the pipeline's `features` output
  // (which now lives in `audit.json`). Column shapes mirror
  // `TopicMetadata::FeatureTopic`.
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS user_features (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        name TEXT NOT NULL,
        description TEXT NOT NULL,
        author_id INTEGER NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_user_features_audit ON user_features(audit_id)",
  )
  .execute(pool)
  .await?;

  // ── User requirements ───────────────────────────────────────────────────
  // Column shapes mirror `TopicMetadata::RequirementTopic`.
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS user_requirements (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        description TEXT NOT NULL,
        section_topic TEXT,
        author_id INTEGER NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_user_requirements_audit ON user_requirements(audit_id)",
  )
  .execute(pool)
  .await?;

  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS user_requirement_documentation_topics (
        user_requirement_id INTEGER NOT NULL,
        documentation_topic TEXT NOT NULL,
        PRIMARY KEY (user_requirement_id, documentation_topic)
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

  // ── User functional semantics ───────────────────────────────────────────
  // Column shapes mirror `TopicMetadata::FunctionalSemanticTopic`.
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS user_functional_semantics (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        description TEXT NOT NULL,
        declaration_topic TEXT NOT NULL,
        author_id INTEGER NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_user_functional_semantics_audit ON user_functional_semantics(audit_id)",
  )
  .execute(pool)
  .await?;

  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS user_functional_semantic_documentation_topics (
        user_functional_semantic_id INTEGER NOT NULL,
        documentation_topic TEXT NOT NULL,
        PRIMARY KEY (user_functional_semantic_id, documentation_topic)
    )
    "#,
  )
  .execute(pool)
  .await?;

  // ── User behaviors ──────────────────────────────────────────────────────
  // Column shapes mirror `TopicMetadata::BehaviorTopic`.
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS user_behaviors (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        description TEXT NOT NULL,
        member_topic TEXT NOT NULL,
        author_id INTEGER NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_user_behaviors_audit ON user_behaviors(audit_id)",
  )
  .execute(pool)
  .await?;

  // ── User characteristics ────────────────────────────────────────────────
  // Column shapes mirror `TopicMetadata::CharacteristicTopic`. The `kind`
  // column stores `SystemCharacteristicKind::as_str()` (currently only
  // "Security"); the `section_topic` column is nullable for characteristics
  // whose only source is the raw `security.md`. User-authored creation is
  // deferred — this table lands empty in Phase 2.
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS user_characteristics (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        audit_id TEXT NOT NULL,
        description TEXT NOT NULL,
        kind TEXT NOT NULL,
        section_topic TEXT,
        author_id INTEGER NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )
    "#,
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_user_characteristics_audit ON user_characteristics(audit_id)",
  )
  .execute(pool)
  .await?;

  sqlx::query(
    "CREATE INDEX IF NOT EXISTS idx_user_characteristics_section ON user_characteristics(section_topic)",
  )
  .execute(pool)
  .await?;

  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS user_characteristic_documentation_topics (
        user_characteristic_id INTEGER NOT NULL,
        documentation_topic TEXT NOT NULL,
        PRIMARY KEY (user_characteristic_id, documentation_topic)
    )
    "#,
  )
  .execute(pool)
  .await?;

  // ── User feature-requirement links ──────────────────────────────────────
  // `requirement_topic` may reference either a pipeline or a user requirement.
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS user_feature_requirement_links (
        user_feature_id INTEGER NOT NULL,
        requirement_topic TEXT NOT NULL,
        PRIMARY KEY (user_feature_id, requirement_topic)
    )
    "#,
  )
  .execute(pool)
  .await?;

  // ── User feature-behavior links ─────────────────────────────────────────
  // `behavior_topic` may reference either a pipeline or a user behavior.
  sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS user_feature_behavior_links (
        user_feature_id INTEGER NOT NULL,
        behavior_topic TEXT NOT NULL,
        PRIMARY KEY (user_feature_id, behavior_topic)
    )
    "#,
  )
  .execute(pool)
  .await?;

  Ok(())
}

//! User-created (or user-agent-created) companions to the pipeline's
//! feature/requirement/behavior/functional-semantic outputs.
//!
//! The pipeline's outputs are rewritten wholesale on every analysis run and
//! live in `audit.json`. User-created entities persist across runs and are
//! stored in the collaboration SQLite DB. At startup, they are loaded here
//! *after* `apply_report` has reseeded the ID counters, so user-entity IDs
//! occupy the same `i32` space as pipeline entities without collision.

use crate::collaborator::models::Author;
use crate::domain::{
  AuditData, Characteristic, Requirement, SystemCharacteristicKind,
  TopicMetadata, topic,
};
use sqlx::SqlitePool;

// ============================================================================
// Row types
// ============================================================================

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserFeatureRow {
  pub id: i64,
  pub audit_id: String,
  pub name: String,
  pub description: String,
  #[sqlx(rename = "author_id")]
  pub author: Author,
  pub created_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRequirementRow {
  pub id: i64,
  pub audit_id: String,
  pub description: String,
  pub section_topic: Option<String>,
  #[sqlx(rename = "author_id")]
  pub author: Author,
  pub created_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserBehaviorRow {
  pub id: i64,
  pub audit_id: String,
  pub description: String,
  pub member_topic: String,
  #[sqlx(rename = "author_id")]
  pub author: Author,
  pub created_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserFunctionalSemanticRow {
  pub id: i64,
  pub audit_id: String,
  pub description: String,
  pub declaration_topic: String,
  #[sqlx(rename = "author_id")]
  pub author: Author,
  pub created_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserCharacteristicRow {
  pub id: i64,
  pub audit_id: String,
  pub description: String,
  /// `SystemCharacteristicKind::as_str()` form (currently only "Security").
  pub kind: String,
  pub section_topic: Option<String>,
  #[sqlx(rename = "author_id")]
  pub author: Author,
  pub created_at: String,
}

// ============================================================================
// Create
// ============================================================================

/// Creates a user feature with the given pre-allocated ID. The caller is
/// responsible for obtaining `id` via `o11a_core::ids::allocate_spec_id`
/// so the in-memory counter, not SQLite, owns ID allocation.
pub async fn create_user_feature(
  pool: &SqlitePool,
  id: i32,
  audit_id: &str,
  name: &str,
  description: &str,
  author_id: Author,
  created_at: &str,
) -> Result<UserFeatureRow, sqlx::Error> {
  sqlx::query(
    r#"
    INSERT INTO user_features (id, audit_id, name, description, author_id, created_at)
    VALUES (?, ?, ?, ?, ?, ?)
    "#,
  )
  .bind(id)
  .bind(audit_id)
  .bind(name)
  .bind(description)
  .bind(author_id)
  .bind(created_at)
  .execute(pool)
  .await?;

  sqlx::query_as::<_, UserFeatureRow>(
    "SELECT * FROM user_features WHERE id = ?",
  )
  .bind(id)
  .fetch_one(pool)
  .await
}

pub async fn create_user_requirement(
  pool: &SqlitePool,
  id: i32,
  audit_id: &str,
  description: &str,
  section_topic: Option<&str>,
  author_id: Author,
  created_at: &str,
) -> Result<UserRequirementRow, sqlx::Error> {
  sqlx::query(
    r#"
    INSERT INTO user_requirements (id, audit_id, description, section_topic, author_id, created_at)
    VALUES (?, ?, ?, ?, ?, ?)
    "#,
  )
  .bind(id)
  .bind(audit_id)
  .bind(description)
  .bind(section_topic)
  .bind(author_id)
  .bind(created_at)
  .execute(pool)
  .await?;

  sqlx::query_as::<_, UserRequirementRow>(
    "SELECT * FROM user_requirements WHERE id = ?",
  )
  .bind(id)
  .fetch_one(pool)
  .await
}

pub async fn create_user_behavior(
  pool: &SqlitePool,
  id: i32,
  audit_id: &str,
  description: &str,
  member_topic: &str,
  author_id: Author,
  created_at: &str,
) -> Result<UserBehaviorRow, sqlx::Error> {
  sqlx::query(
    r#"
    INSERT INTO user_behaviors (id, audit_id, description, member_topic, author_id, created_at)
    VALUES (?, ?, ?, ?, ?, ?)
    "#,
  )
  .bind(id)
  .bind(audit_id)
  .bind(description)
  .bind(member_topic)
  .bind(author_id)
  .bind(created_at)
  .execute(pool)
  .await?;

  sqlx::query_as::<_, UserBehaviorRow>(
    "SELECT * FROM user_behaviors WHERE id = ?",
  )
  .bind(id)
  .fetch_one(pool)
  .await
}

#[allow(clippy::too_many_arguments)]
pub async fn create_user_characteristic(
  pool: &SqlitePool,
  id: i32,
  audit_id: &str,
  description: &str,
  kind: SystemCharacteristicKind,
  section_topic: Option<&str>,
  author_id: Author,
  created_at: &str,
) -> Result<UserCharacteristicRow, sqlx::Error> {
  sqlx::query(
    r#"
    INSERT INTO user_characteristics (id, audit_id, description, kind, section_topic, author_id, created_at)
    VALUES (?, ?, ?, ?, ?, ?, ?)
    "#,
  )
  .bind(id)
  .bind(audit_id)
  .bind(description)
  .bind(kind.as_str())
  .bind(section_topic)
  .bind(author_id)
  .bind(created_at)
  .execute(pool)
  .await?;

  sqlx::query_as::<_, UserCharacteristicRow>(
    "SELECT * FROM user_characteristics WHERE id = ?",
  )
  .bind(id)
  .fetch_one(pool)
  .await
}

pub async fn add_user_characteristic_documentation_topic(
  pool: &SqlitePool,
  user_characteristic_id: i32,
  documentation_topic: &str,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    r#"
    INSERT OR IGNORE INTO user_characteristic_documentation_topics (user_characteristic_id, documentation_topic)
    VALUES (?, ?)
    "#,
  )
  .bind(user_characteristic_id)
  .bind(documentation_topic)
  .execute(pool)
  .await?;
  Ok(())
}

pub async fn create_user_functional_semantic(
  pool: &SqlitePool,
  id: i32,
  audit_id: &str,
  description: &str,
  declaration_topic: &str,
  author_id: Author,
  created_at: &str,
) -> Result<UserFunctionalSemanticRow, sqlx::Error> {
  sqlx::query(
    r#"
    INSERT INTO user_functional_semantics (id, audit_id, description, declaration_topic, author_id, created_at)
    VALUES (?, ?, ?, ?, ?, ?)
    "#,
  )
  .bind(id)
  .bind(audit_id)
  .bind(description)
  .bind(declaration_topic)
  .bind(author_id)
  .bind(created_at)
  .execute(pool)
  .await?;

  sqlx::query_as::<_, UserFunctionalSemanticRow>(
    "SELECT * FROM user_functional_semantics WHERE id = ?",
  )
  .bind(id)
  .fetch_one(pool)
  .await
}

pub async fn add_user_feature_requirement_link(
  pool: &SqlitePool,
  user_feature_id: i32,
  requirement_topic: &str,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    r#"
    INSERT OR IGNORE INTO user_feature_requirement_links (user_feature_id, requirement_topic)
    VALUES (?, ?)
    "#,
  )
  .bind(user_feature_id)
  .bind(requirement_topic)
  .execute(pool)
  .await?;
  Ok(())
}

pub async fn add_user_feature_behavior_link(
  pool: &SqlitePool,
  user_feature_id: i32,
  behavior_topic: &str,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    r#"
    INSERT OR IGNORE INTO user_feature_behavior_links (user_feature_id, behavior_topic)
    VALUES (?, ?)
    "#,
  )
  .bind(user_feature_id)
  .bind(behavior_topic)
  .execute(pool)
  .await?;
  Ok(())
}

pub async fn add_user_requirement_documentation_topic(
  pool: &SqlitePool,
  user_requirement_id: i32,
  documentation_topic: &str,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    r#"
    INSERT OR IGNORE INTO user_requirement_documentation_topics (user_requirement_id, documentation_topic)
    VALUES (?, ?)
    "#,
  )
  .bind(user_requirement_id)
  .bind(documentation_topic)
  .execute(pool)
  .await?;
  Ok(())
}

pub async fn add_user_functional_semantic_documentation_topic(
  pool: &SqlitePool,
  user_functional_semantic_id: i32,
  documentation_topic: &str,
) -> Result<(), sqlx::Error> {
  sqlx::query(
    r#"
    INSERT OR IGNORE INTO user_functional_semantic_documentation_topics (user_functional_semantic_id, documentation_topic)
    VALUES (?, ?)
    "#,
  )
  .bind(user_functional_semantic_id)
  .bind(documentation_topic)
  .execute(pool)
  .await?;
  Ok(())
}

// ============================================================================
// Load (all rows for an audit)
// ============================================================================

pub async fn load_user_features(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<Vec<UserFeatureRow>, sqlx::Error> {
  sqlx::query_as::<_, UserFeatureRow>(
    "SELECT * FROM user_features WHERE audit_id = ? ORDER BY id",
  )
  .bind(audit_id)
  .fetch_all(pool)
  .await
}

pub async fn load_user_requirements(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<Vec<UserRequirementRow>, sqlx::Error> {
  sqlx::query_as::<_, UserRequirementRow>(
    "SELECT * FROM user_requirements WHERE audit_id = ? ORDER BY id",
  )
  .bind(audit_id)
  .fetch_all(pool)
  .await
}

pub async fn load_user_behaviors(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<Vec<UserBehaviorRow>, sqlx::Error> {
  sqlx::query_as::<_, UserBehaviorRow>(
    "SELECT * FROM user_behaviors WHERE audit_id = ? ORDER BY id",
  )
  .bind(audit_id)
  .fetch_all(pool)
  .await
}

pub async fn load_user_functional_semantics(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<Vec<UserFunctionalSemanticRow>, sqlx::Error> {
  sqlx::query_as::<_, UserFunctionalSemanticRow>(
    "SELECT * FROM user_functional_semantics WHERE audit_id = ? ORDER BY id",
  )
  .bind(audit_id)
  .fetch_all(pool)
  .await
}

pub async fn load_user_characteristics(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<Vec<UserCharacteristicRow>, sqlx::Error> {
  sqlx::query_as::<_, UserCharacteristicRow>(
    "SELECT * FROM user_characteristics WHERE audit_id = ? ORDER BY id",
  )
  .bind(audit_id)
  .fetch_all(pool)
  .await
}

/// Load the D-prefixed documentation topics associated with a user requirement.
async fn load_user_requirement_documentation_topics(
  pool: &SqlitePool,
  user_requirement_id: i64,
) -> Result<Vec<String>, sqlx::Error> {
  sqlx::query_scalar::<_, String>(
    "SELECT documentation_topic FROM user_requirement_documentation_topics WHERE user_requirement_id = ? ORDER BY documentation_topic",
  )
  .bind(user_requirement_id)
  .fetch_all(pool)
  .await
}

/// Load the D-prefixed documentation topics associated with a user functional semantic.
async fn load_user_functional_semantic_documentation_topics(
  pool: &SqlitePool,
  user_functional_semantic_id: i64,
) -> Result<Vec<String>, sqlx::Error> {
  sqlx::query_scalar::<_, String>(
    "SELECT documentation_topic FROM user_functional_semantic_documentation_topics WHERE user_functional_semantic_id = ? ORDER BY documentation_topic",
  )
  .bind(user_functional_semantic_id)
  .fetch_all(pool)
  .await
}

/// Load the D-prefixed documentation topics associated with a user characteristic.
async fn load_user_characteristic_documentation_topics(
  pool: &SqlitePool,
  user_characteristic_id: i64,
) -> Result<Vec<String>, sqlx::Error> {
  sqlx::query_scalar::<_, String>(
    "SELECT documentation_topic FROM user_characteristic_documentation_topics WHERE user_characteristic_id = ? ORDER BY documentation_topic",
  )
  .bind(user_characteristic_id)
  .fetch_all(pool)
  .await
}

/// Load all `(user_feature_id, requirement_topic)` rows for an audit. The
/// feature id here refers to rows in `user_features`; the requirement topic
/// may point at either a pipeline or user requirement.
async fn load_user_feature_requirement_links(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<Vec<(i64, String)>, sqlx::Error> {
  sqlx::query_as::<_, (i64, String)>(
    r#"
    SELECT l.user_feature_id, l.requirement_topic
    FROM user_feature_requirement_links l
    JOIN user_features f ON f.id = l.user_feature_id
    WHERE f.audit_id = ?
    "#,
  )
  .bind(audit_id)
  .fetch_all(pool)
  .await
}

async fn load_user_feature_behavior_links(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<Vec<(i64, String)>, sqlx::Error> {
  sqlx::query_as::<_, (i64, String)>(
    r#"
    SELECT l.user_feature_id, l.behavior_topic
    FROM user_feature_behavior_links l
    JOIN user_features f ON f.id = l.user_feature_id
    WHERE f.audit_id = ?
    "#,
  )
  .bind(audit_id)
  .fetch_all(pool)
  .await
}

// ============================================================================
// Apply
// ============================================================================

/// All rows for an audit's user-created entities, loaded in one go so the
/// caller can apply them under a synchronous mutex without holding the lock
/// across `.await` points.
pub struct UserEntitiesSnapshot {
  pub features: Vec<UserFeatureRow>,
  pub requirements: Vec<(UserRequirementRow, Vec<String>)>,
  pub behaviors: Vec<UserBehaviorRow>,
  pub characteristics: Vec<(UserCharacteristicRow, Vec<String>)>,
  pub functional_semantics: Vec<(UserFunctionalSemanticRow, Vec<String>)>,
  pub feature_requirement_links: Vec<(i64, String)>,
  pub feature_behavior_links: Vec<(i64, String)>,
}

/// Load every user-entity row for `audit_id` in one pass. Pure I/O; no
/// mutation of `AuditData`. Pair with `apply_user_entities_snapshot`.
pub async fn load_user_entities_snapshot(
  pool: &SqlitePool,
  audit_id: &str,
) -> Result<UserEntitiesSnapshot, sqlx::Error> {
  let features = load_user_features(pool, audit_id).await?;
  let requirement_rows = load_user_requirements(pool, audit_id).await?;
  let behaviors = load_user_behaviors(pool, audit_id).await?;
  let characteristic_rows = load_user_characteristics(pool, audit_id).await?;
  let semantic_rows = load_user_functional_semantics(pool, audit_id).await?;

  let mut requirements = Vec::with_capacity(requirement_rows.len());
  for r in requirement_rows {
    let docs = load_user_requirement_documentation_topics(pool, r.id).await?;
    requirements.push((r, docs));
  }

  let mut characteristics = Vec::with_capacity(characteristic_rows.len());
  for c in characteristic_rows {
    let docs =
      load_user_characteristic_documentation_topics(pool, c.id).await?;
    characteristics.push((c, docs));
  }

  let mut functional_semantics = Vec::with_capacity(semantic_rows.len());
  for s in semantic_rows {
    let docs =
      load_user_functional_semantic_documentation_topics(pool, s.id).await?;
    functional_semantics.push((s, docs));
  }

  let feature_requirement_links =
    load_user_feature_requirement_links(pool, audit_id).await?;
  let feature_behavior_links =
    load_user_feature_behavior_links(pool, audit_id).await?;

  Ok(UserEntitiesSnapshot {
    features,
    requirements,
    behaviors,
    characteristics,
    functional_semantics,
    feature_requirement_links,
    feature_behavior_links,
  })
}

/// Hydrate `audit_data` from a snapshot. Synchronous so callers can hold a
/// `std::sync::Mutex` guard while calling without crossing an await point.
/// Must be called *after* `report::apply_report` so pipeline IDs have already
/// reseeded the counters; user-entity IDs loaded here share the same `i32`
/// space as pipeline IDs (pipeline IDs own 1..=N, user IDs continue from N+1).
/// Reseeds the `S`- and `P`-prefixed counters at the end so subsequent
/// in-memory allocations skip past every user-entity row this call installed.
///
/// Callers should invoke `crate::domain::rebuild_feature_context` after this
/// so reverse indexes pick up the new topic metadata.
pub fn apply_user_entities_snapshot(
  audit_data: &mut AuditData,
  snapshot: UserEntitiesSnapshot,
) {
  let UserEntitiesSnapshot {
    features,
    requirements,
    behaviors,
    characteristics,
    functional_semantics,
    feature_requirement_links,
    feature_behavior_links,
  } = snapshot;

  for f in &features {
    let topic = topic::new_spec_topic(f.id as i32);
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::FeatureTopic {
        topic,
        name: f.name.clone(),
        description: f.description.clone(),
        author: f.author,
        created_at: Some(f.created_at.clone()),
      },
    );
  }

  for (r, doc_ids) in &requirements {
    let topic = topic::new_spec_topic(r.id as i32);
    let section_topic =
      topic::new_topic(r.section_topic.as_deref().unwrap_or(""));

    let documentation_topics: Vec<topic::Topic> =
      doc_ids.iter().map(|id| topic::new_topic(id)).collect();

    audit_data.requirements.insert(
      topic,
      Requirement {
        documentation_topics,
      },
    );

    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::RequirementTopic {
        topic,
        description: r.description.clone(),
        section_topic,
        author: r.author,
        created_at: Some(r.created_at.clone()),
      },
    );
  }

  for b in &behaviors {
    let topic = topic::new_spec_topic(b.id as i32);
    let member_topic = topic::new_topic(&b.member_topic);
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::BehaviorTopic {
        topic,
        description: b.description.clone(),
        member_topic,
        author: b.author,
        created_at: Some(b.created_at.clone()),
      },
    );
  }

  for (c, doc_ids) in &characteristics {
    let topic = topic::new_spec_topic(c.id as i32);
    let kind = SystemCharacteristicKind::parse_str(&c.kind).unwrap_or_else(
      || {
        panic!(
          "Unknown system characteristic kind '{}' in user_characteristics row {}",
          c.kind, c.id
        )
      },
    );
    let section_topic = c.section_topic.as_deref().map(topic::new_topic);
    let documentation_topics: Vec<topic::Topic> =
      doc_ids.iter().map(|id| topic::new_topic(id)).collect();

    audit_data.characteristics.insert(
      topic,
      Characteristic {
        documentation_topics,
      },
    );

    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::CharacteristicTopic {
        topic,
        description: c.description.clone(),
        kind,
        section_topic,
        author: c.author,
        created_at: Some(c.created_at.clone()),
      },
    );
  }

  for (s, doc_ids) in &functional_semantics {
    let topic = topic::new_functional_property_topic(s.id as i32);
    let declaration_topic = topic::new_topic(&s.declaration_topic);
    let documentation_topics: Vec<topic::Topic> =
      doc_ids.iter().map(|id| topic::new_topic(id)).collect();
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::FunctionalSemanticTopic {
        topic,
        description: s.description.clone(),
        declaration_topic,
        documentation_topics,
        author: s.author,
        created_at: Some(s.created_at.clone()),
        // User-authored semantics carry no workflow provenance.
        match_source: None,
      },
    );
  }

  for (user_feature_id, requirement_topic) in feature_requirement_links {
    let feature_topic = topic::new_spec_topic(user_feature_id as i32);
    audit_data
      .feature_requirement_links
      .entry(feature_topic)
      .or_default()
      .push(topic::new_topic(&requirement_topic));
  }

  for (user_feature_id, behavior_topic) in feature_behavior_links {
    let feature_topic = topic::new_spec_topic(user_feature_id as i32);
    audit_data
      .feature_behavior_links
      .entry(feature_topic)
      .or_default()
      .push(topic::new_topic(&behavior_topic));
  }

  // Reseed the S- and P-counters past the highest user-entity ID just
  // installed. `apply_report` already reseeded once based on the pipeline
  // output; this second call covers any user-entity row whose DB ID exceeds
  // the pipeline's max (e.g. a user-authored entity allocated by a previous
  // server process). Scanning the merged `topic_metadata` keys is safe
  // because `reseed_*` performs an unconditional store, and the post-merge
  // max is never lower than `apply_report`'s.
  let (max_spec, max_func_prop) =
    topic_metadata_max_spec_and_functional_property(audit_data);
  crate::ids::reseed_spec_id(max_spec);
  crate::ids::reseed_functional_property_id(max_func_prop);
}

/// Highest numeric ID across `Topic::Spec` and `Topic::FunctionalProperty`
/// keys in `topic_metadata`, returned as `(max_spec, max_functional_property)`.
/// Either component is 0 when no key of that variant exists.
fn topic_metadata_max_spec_and_functional_property(
  audit_data: &AuditData,
) -> (i32, i32) {
  let mut max_spec = 0;
  let mut max_func_prop = 0;
  for key in audit_data.topic_metadata.keys() {
    match key {
      topic::Topic::Spec(id) if *id > max_spec => max_spec = *id,
      topic::Topic::FunctionalProperty(id) if *id > max_func_prop => {
        max_func_prop = *id
      }
      _ => {}
    }
  }
  (max_spec, max_func_prop)
}

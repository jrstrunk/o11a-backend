use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sqlx::FromRow;

pub use crate::collaborator::scope_info::ScopeInfo;
pub use crate::domain::CommentType;
use crate::domain::topic;

/// Reserved author IDs — retained only for the enum conversion table below.
/// All domain/API/handler code should use `Author` variants.
pub(crate) const AUTHOR_SYSTEM: i64 = 1;
pub(crate) const AUTHOR_DEV_TECHNICAL: i64 = 2;
pub(crate) const AUTHOR_DEV_DOCUMENTATION: i64 = 3;
pub(crate) const AUTHOR_AGENT_MICRO: i64 = 4;
pub(crate) const AUTHOR_AGENT_SMALL: i64 = 5;
pub(crate) const AUTHOR_AGENT_MEDIUM: i64 = 6;
pub(crate) const AUTHOR_AGENT_LARGE: i64 = 7;

/// Typed authorship marker.
///
/// Wire format is preserved: serializes as a plain `i64` and decodes from an
/// SQL `INTEGER` column. Reserved variants (`1..=7`) stay in sync with the
/// `AUTHOR_*` constants; user IDs (`>= 8`) live in `Author::User(n)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Author {
  System,
  DevTechnical,
  DevDocumentation,
  AgentMicro,
  AgentSmall,
  AgentMedium,
  AgentLarge,
  User(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidAuthorId(pub i64);

impl std::fmt::Display for InvalidAuthorId {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "invalid author id: {}", self.0)
  }
}

impl std::error::Error for InvalidAuthorId {}

impl Author {
  pub fn as_i64(self) -> i64 {
    match self {
      Author::System => AUTHOR_SYSTEM,
      Author::DevTechnical => AUTHOR_DEV_TECHNICAL,
      Author::DevDocumentation => AUTHOR_DEV_DOCUMENTATION,
      Author::AgentMicro => AUTHOR_AGENT_MICRO,
      Author::AgentSmall => AUTHOR_AGENT_SMALL,
      Author::AgentMedium => AUTHOR_AGENT_MEDIUM,
      Author::AgentLarge => AUTHOR_AGENT_LARGE,
      Author::User(n) => n as i64,
    }
  }

  pub fn from_id(id: i64) -> Result<Self, InvalidAuthorId> {
    match id {
      AUTHOR_SYSTEM => Ok(Author::System),
      AUTHOR_DEV_TECHNICAL => Ok(Author::DevTechnical),
      AUTHOR_DEV_DOCUMENTATION => Ok(Author::DevDocumentation),
      AUTHOR_AGENT_MICRO => Ok(Author::AgentMicro),
      AUTHOR_AGENT_SMALL => Ok(Author::AgentSmall),
      AUTHOR_AGENT_MEDIUM => Ok(Author::AgentMedium),
      AUTHOR_AGENT_LARGE => Ok(Author::AgentLarge),
      n if n >= 8 => Ok(Author::User(n as u64)),
      n => Err(InvalidAuthorId(n)),
    }
  }
}

/// Fallible parser — alias for `Author::from_id` for readability at boundaries.
pub fn parse_author(i: i64) -> Result<Author, InvalidAuthorId> {
  Author::from_id(i)
}

impl From<Author> for i64 {
  fn from(a: Author) -> Self {
    a.as_i64()
  }
}

impl TryFrom<i64> for Author {
  type Error = InvalidAuthorId;
  fn try_from(v: i64) -> Result<Self, Self::Error> {
    Author::from_id(v)
  }
}

impl Serialize for Author {
  fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_i64(self.as_i64())
  }
}

impl<'de> Deserialize<'de> for Author {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    let n = i64::deserialize(d)?;
    Author::from_id(n).map_err(serde::de::Error::custom)
  }
}

impl sqlx::Type<sqlx::Sqlite> for Author {
  fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
    <i64 as sqlx::Type<sqlx::Sqlite>>::type_info()
  }

  fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
    <i64 as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
  }
}

impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for Author {
  fn decode(
    value: sqlx::sqlite::SqliteValueRef<'r>,
  ) -> Result<Self, sqlx::error::BoxDynError> {
    let n = <i64 as sqlx::Decode<sqlx::Sqlite>>::decode(value)?;
    Ok(Author::from_id(n)?)
  }
}

impl<'q> sqlx::Encode<'q, sqlx::Sqlite> for Author {
  fn encode_by_ref(
    &self,
    buf: &mut Vec<sqlx::sqlite::SqliteArgumentValue<'q>>,
  ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
    <i64 as sqlx::Encode<'_, sqlx::Sqlite>>::encode_by_ref(&self.as_i64(), buf)
  }
}

/// Comment status - controls visibility and state
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommentStatus {
  // General statuses (all comment types)
  Active,   // Visible normally (default for most types)
  Hidden,   // Soft-deleted, hidden from default view
  Resolved, // Marked as addressed/completed

  // Question-specific statuses
  Unanswered, // Question awaiting answer (default for questions)
  Answered,   // Question has been answered

  // Finding lead-specific statuses
  Unconfirmed, // Finding lead awaiting review (default for finding_lead)
  Confirmed,   // Finding lead confirmed as valid
  Rejected,    // Finding lead rejected as invalid
}

impl CommentStatus {
  pub fn as_str(&self) -> &'static str {
    match self {
      CommentStatus::Active => "active",
      CommentStatus::Hidden => "hidden",
      CommentStatus::Resolved => "resolved",
      CommentStatus::Unanswered => "unanswered",
      CommentStatus::Answered => "answered",
      CommentStatus::Unconfirmed => "unconfirmed",
      CommentStatus::Confirmed => "confirmed",
      CommentStatus::Rejected => "rejected",
    }
  }

  pub fn parse_str(s: &str) -> Self {
    match s {
      "hidden" => CommentStatus::Hidden,
      "resolved" => CommentStatus::Resolved,
      "unanswered" => CommentStatus::Unanswered,
      "answered" => CommentStatus::Answered,
      "unconfirmed" => CommentStatus::Unconfirmed,
      "confirmed" => CommentStatus::Confirmed,
      "rejected" => CommentStatus::Rejected,
      _ => CommentStatus::Active,
    }
  }
}

impl CommentType {
  /// Returns the default status for this comment type
  pub fn default_status(&self) -> CommentStatus {
    match self {
      CommentType::Question => CommentStatus::Unanswered,
      CommentType::FindingLead => CommentStatus::Unconfirmed,
      _ => CommentStatus::Active,
    }
  }
}

/// Vote value
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VoteValue {
  Up,
  Down,
}

impl VoteValue {
  pub fn to_i32(&self) -> i32 {
    match self {
      VoteValue::Up => 1,
      VoteValue::Down => -1,
    }
  }

  pub fn from_i32(v: i32) -> Self {
    if v >= 0 {
      VoteValue::Up
    } else {
      VoteValue::Down
    }
  }
}

/// Core comment structure (database row)
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Comment {
  pub id: i64,
  pub audit_id: String,
  pub topic_id: String, // Topic being commented on (N123, D45, C99 for replies)

  // Immutable fields
  pub content_markdown: String, // Raw markdown content
  #[sqlx(rename = "author_id")]
  #[serde(rename = "author_id")]
  pub author: Author, // Reserved variants = 1..=7, users >= 8.
  pub comment_type: String,     // Stored as string, convert to CommentType
  pub created_at: String,

  // Mutable field
  pub status: String, // Stored as string, convert to CommentStatus

  // Scope copied from target topic at creation time (stored as JSON)
  pub scope: String,
}

impl Comment {
  /// Returns this comment's topic ID (e.g., "C42" for id=42)
  pub fn comment_topic_id(&self) -> String {
    format!("C{}", self.id)
  }

  pub fn comment_topic(&self) -> topic::Topic {
    topic::new_comment_topic(self.id.try_into().unwrap())
  }

  /// Returns the parsed comment type
  ///
  /// Panics if the stored type string is not a known variant. This indicates
  /// a data integrity issue in the database.
  pub fn get_comment_type(&self) -> CommentType {
    CommentType::parse_str(&self.comment_type).unwrap_or_else(|| {
      panic!(
        "Unknown comment type '{}' in comment {}",
        self.comment_type, self.id
      )
    })
  }

  /// Returns the parsed comment status
  pub fn get_status(&self) -> CommentStatus {
    CommentStatus::parse_str(&self.status)
  }
}

/// Vote record (database row)
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct CommentVote {
  pub id: i64,
  pub comment_id: i64,
  pub user_id: i64,
  pub vote: i32, // 1 or -1
  pub created_at: String,
}

// ============================================================================
// Request types
// ============================================================================

fn default_comment_type() -> CommentType {
  CommentType::Note
}

/// Request to create a new comment
#[derive(Debug, Deserialize)]
pub struct CreateCommentRequest {
  pub topic_id: String, // Topic to comment on (N123, D45, or C99 for replies)
  pub content: String,  // Markdown content
  #[serde(rename = "author_id")]
  pub author: Author,
  #[serde(default = "default_comment_type")]
  pub comment_type: CommentType, // Defaults to Note when omitted
}

/// Request to update comment status
#[derive(Debug, Deserialize)]
pub struct UpdateStatusRequest {
  pub status: CommentStatus,
}

/// Request to cast a vote
#[derive(Debug, Deserialize)]
pub struct VoteRequest {
  pub user_id: i64,
  pub vote: VoteValue,
}

// ============================================================================
// Response types
// ============================================================================

/// Response for create operations (returns the comment's topic ID)
#[derive(Debug, Clone, Serialize)]
pub struct CommentCreatedResponse {
  pub comment_topic_id: String, // "C{id}" - use this to fetch full metadata
}

/// Response for listing comment topic IDs
#[derive(Debug, Clone, Serialize)]
pub struct CommentListResponse {
  pub comment_topic_ids: Vec<String>, // ["C1", "C2", "C3"]
}

/// Response for status queries
#[derive(Debug, Clone, Serialize)]
pub struct CommentStatusResponse {
  pub comment_topic_id: String,
  pub status: CommentStatus,
}

/// Vote summary for a single comment
#[derive(Debug, Clone, Serialize)]
pub struct CommentVoteSummary {
  pub comment_id: i64,
  pub comment_topic_id: String, // "C{id}"
  pub score: i64,               // Sum of all votes
  pub upvotes: i64,
  pub downvotes: i64,
  pub user_vote: Option<VoteValue>, // Current user's vote if requested
}

// ============================================================================
// WebSocket event types
// ============================================================================

/// Audit event types for the real-time event stream. Carries comment activity
/// today and is the envelope for future audit-scoped events (pipeline refresh,
/// user-created entity, etc.).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditEvent {
  /// A conversation entry was added to a topic's conversation. Carries only
  /// topic identifiers — rendered representations are fetched on demand by
  /// the client, keeping this event payload free of any presentation format.
  /// - `topic_id`: the topic whose conversation was updated.
  /// - `comment_topic_id`: the new comment that triggered the update.
  /// - `invalidated_thread_ids`: parent comment topic IDs whose threads are
  ///   now stale and should be refetched by the client.
  TopicUpdated {
    audit_id: String,
    topic_id: String,
    comment_topic_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    invalidated_thread_ids: Vec<String>,
  },
  /// Status updated - includes new status directly (no need to refetch)
  StatusUpdated {
    audit_id: String,
    comment_topic_id: String,
    status: CommentStatus,
  },
  /// Vote updated - includes current vote counts
  VoteUpdated {
    audit_id: String,
    comment_topic_id: String,
    score: i64,
    upvotes: i64,
    downvotes: i64,
  },
}

impl AuditEvent {
  pub fn audit_id(&self) -> &str {
    match self {
      AuditEvent::TopicUpdated { audit_id, .. }
      | AuditEvent::StatusUpdated { audit_id, .. }
      | AuditEvent::VoteUpdated { audit_id, .. } => audit_id,
    }
  }
}

#[cfg(test)]
mod author_tests {
  use super::*;

  #[test]
  fn author_serializes_as_integer() {
    let cases = [
      (Author::System, "1"),
      (Author::DevTechnical, "2"),
      (Author::DevDocumentation, "3"),
      (Author::AgentMicro, "4"),
      (Author::AgentSmall, "5"),
      (Author::AgentMedium, "6"),
      (Author::AgentLarge, "7"),
      (Author::User(42), "42"),
    ];
    for (author, expected) in cases {
      assert_eq!(serde_json::to_string(&author).unwrap(), expected);
    }
  }

  #[test]
  fn author_deserializes_from_integer() {
    for n in [1i64, 2, 3, 4, 5, 6, 7, 8, 42, 1_000_000] {
      let s = n.to_string();
      let author: Author = serde_json::from_str(&s).unwrap();
      assert_eq!(author.as_i64(), n);
    }
  }

  #[test]
  fn author_rejects_invalid_ids() {
    assert!(Author::from_id(0).is_err());
    assert!(Author::from_id(-1).is_err());
    assert!(serde_json::from_str::<Author>("0").is_err());
    assert!(serde_json::from_str::<Author>("-5").is_err());
  }

  #[test]
  fn author_roundtrips_through_id() {
    for variant in [
      Author::System,
      Author::DevTechnical,
      Author::DevDocumentation,
      Author::AgentMicro,
      Author::AgentSmall,
      Author::AgentMedium,
      Author::AgentLarge,
      Author::User(8),
      Author::User(9999),
    ] {
      assert_eq!(Author::from_id(variant.as_i64()).unwrap(), variant);
    }
  }
}

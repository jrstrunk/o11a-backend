use serde::{Deserialize, Serialize};
use sqlx::FromRow;

pub use crate::collaborator::scope_info::ScopeInfo;
pub use crate::core::CommentType;
use crate::core::topic;

/// Reserved author IDs
pub const AUTHOR_SYSTEM: i64 = 1;
pub const AUTHOR_DEV_TECHNICAL: i64 = 2;
pub const AUTHOR_DEV_DOCUMENTATION: i64 = 3;
pub const AUTHOR_AGENT_MICRO: i64 = 4;
pub const AUTHOR_AGENT_SMALL: i64 = 5;
pub const AUTHOR_AGENT_MEDIUM: i64 = 6;
pub const AUTHOR_AGENT_LARGE: i64 = 7;

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
  pub author_id: i64,           // 1=system, 2=agent, 3+=users
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
  pub author_id: i64,   // 1=system, 2=agent, 3+=users
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

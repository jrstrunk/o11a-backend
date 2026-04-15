use axum::{
  Json,
  extract::{Path, Query, State},
  http::StatusCode,
  response::{Html, IntoResponse},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::api::AppState;
use crate::collaborator::{db, formatter, models::*, parser, store};
use crate::core::{
  self, Feature, project,
  topic::{self, TopicKind, new_topic},
};

/// Parse a topic ID string from a URL path parameter into a numeric database ID.
/// Accepts both prefixed (e.g. "F7") and bare numeric (e.g. "7") formats.
fn parse_path_id(input: &str, expected_kind: TopicKind) -> Result<i64, StatusCode> {
  topic::parse_topic_id(input, expected_kind).map_err(|e| {
    eprintln!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })
}

// Health check handler
pub async fn health_check() -> StatusCode {
  println!("GET /health");
  StatusCode::OK
}

// DataContext response (placeholder structure - will be populated from analyzer)
#[derive(Debug, Serialize)]
pub struct DataContextResponse {
  pub in_scope_files: Vec<String>,
  pub nodes: serde_json::Value,
  pub declarations: serde_json::Value,
}

// Get DataContext for a specific audit
pub async fn get_data_context(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<Json<DataContextResponse>, StatusCode> {
  println!("GET /api/v1/audits/{}/data-context", audit_id);
  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_data_context: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  // Convert in_scope_files to Vec<String>
  let in_scope_files: Vec<String> = audit_data
    .in_scope_files
    .iter()
    .map(|p| p.file_path.clone())
    .collect();

  Ok(Json(DataContextResponse {
    in_scope_files,
    nodes: serde_json::json!({}),
    declarations: serde_json::json!({}),
  }))
}

// Chat model
#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct Chat {
  pub id: i64,
  pub content: String,
  pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateChatRequest {
  pub content: String,
}

// Get all chats
pub async fn get_chats(
  State(state): State<AppState>,
) -> Result<Json<Vec<Chat>>, StatusCode> {
  println!("GET /api/v1/chats");
  let chats = sqlx::query_as::<_, Chat>(
    "SELECT id, content, created_at FROM chats ORDER BY created_at DESC",
  )
  .fetch_all(&state.db)
  .await
  .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  Ok(Json(chats))
}

// Create a new chat
pub async fn create_chat(
  State(state): State<AppState>,
  Json(payload): Json<CreateChatRequest>,
) -> Result<Json<Chat>, StatusCode> {
  println!("POST /api/v1/chats");
  let result = sqlx::query("INSERT INTO chats (content) VALUES (?)")
    .bind(&payload.content)
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  let chat = sqlx::query_as::<_, Chat>(
    "SELECT id, content, created_at FROM chats WHERE id = ?",
  )
  .bind(result.last_insert_rowid())
  .fetch_one(&state.db)
  .await
  .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  Ok(Json(chat))
}

// Boundaries response (placeholder for future implementation)
#[derive(Debug, Serialize)]
pub struct BoundariesResponse {
  pub boundaries: Vec<String>,
}

// Get boundaries for a specific audit
pub async fn get_boundaries(
  State(_state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<Json<BoundariesResponse>, StatusCode> {
  println!("GET /api/v1/audits/{}/boundaries", audit_id);
  // TODO: Implement actual boundaries from checker
  Ok(Json(BoundariesResponse { boundaries: vec![] }))
}

#[derive(Debug, Serialize)]
pub struct InScopeFilesResponse {
  pub in_scope_files: Vec<String>,
}

// Get in scope files for a specific audit
pub async fn get_in_scope_files(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<Json<InScopeFilesResponse>, StatusCode> {
  println!("GET /api/v1/audits/{}/in_scope_files", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_in_scope_files: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let in_scope_files: Vec<String> = audit_data
    .in_scope_files
    .iter()
    .map(|p| p.file_path.clone())
    .collect();

  Ok(Json(InScopeFilesResponse { in_scope_files }))
}

// Audit management handlers

#[derive(Debug, Serialize)]
pub struct AuditInfo {
  pub audit_id: String,
}

#[derive(Debug, Serialize)]
pub struct AuditsListResponse {
  pub audits: Vec<AuditInfo>,
}

// List all audits
pub async fn list_audits(
  State(state): State<AppState>,
) -> Result<Json<AuditsListResponse>, StatusCode> {
  println!("GET /api/v1/audits");
  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in list_audits: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audits = ctx
    .list_audits()
    .into_iter()
    .map(|audit_id| AuditInfo { audit_id })
    .collect();

  Ok(Json(AuditsListResponse { audits }))
}

#[derive(Debug, Deserialize)]
pub struct CreateAuditRequest {
  pub audit_id: String,
  pub project_root: String,
}

#[derive(Debug, Serialize)]
pub struct CreateAuditResponse {
  pub audit_id: String,
  pub message: String,
}

// Create a new audit
pub async fn create_audit(
  State(state): State<AppState>,
  Json(payload): Json<CreateAuditRequest>,
) -> Result<Json<CreateAuditResponse>, StatusCode> {
  println!("POST /api/v1/audits");
  let project_root = std::path::Path::new(&payload.project_root);

  // Load the project for this audit
  project::load_project(project_root, &payload.audit_id, &state.data_context)
    .map_err(|e| {
    eprintln!(
      "Failed to load project for audit '{}': {}",
      payload.audit_id, e
    );
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  Ok(Json(CreateAuditResponse {
    audit_id: payload.audit_id.clone(),
    message: format!("Audit '{}' created successfully", payload.audit_id),
  }))
}

#[derive(Debug, Serialize)]
pub struct DeleteAuditResponse {
  pub message: String,
}

// Delete an audit
pub async fn delete_audit(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<Json<DeleteAuditResponse>, StatusCode> {
  println!("DELETE /api/v1/audits/{}", audit_id);
  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in delete_audit: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  if ctx.delete_audit(&audit_id) {
    Ok(Json(DeleteAuditResponse {
      message: format!("Audit '{}' deleted successfully", audit_id),
    }))
  } else {
    Err(StatusCode::NOT_FOUND)
  }
}

#[derive(Debug, Serialize)]
pub struct ContractsResponse {
  pub contracts: Vec<TopicMetadataResponse>,
}

#[derive(Debug, Serialize)]
pub struct DocumentsResponse {
  pub documents: Vec<TopicMetadataResponse>,
}

// Get all documents for an audit
pub async fn get_documents(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<Json<DocumentsResponse>, StatusCode> {
  println!("GET /api/v1/audits/{}/documents", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_documents: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let mut documents = Vec::new();

  // Iterate through all topic metadata and filter for documentation roots
  for (topic, metadata) in &audit_data.topic_metadata {
    if matches!(
      metadata,
      crate::core::TopicMetadata::DocumentationTopic { .. }
    ) {
      documents.push(topic_metadata_to_response(topic, metadata));
    }
  }

  Ok(Json(DocumentsResponse { documents }))
}

// Get all contracts for an audit
pub async fn get_contracts(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<Json<ContractsResponse>, StatusCode> {
  println!("GET /api/v1/audits/{}/contracts", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_contracts: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let mut contracts = Vec::new();

  // Iterate through all topic metadata and filter for contracts in scope files
  for (topic, metadata) in &audit_data.topic_metadata {
    let is_contract = match metadata {
      crate::core::TopicMetadata::NamedTopic { kind, .. } => {
        matches!(kind, crate::core::NamedTopicKind::Contract(_))
      }
      crate::core::TopicMetadata::UnnamedTopic { .. }
      | crate::core::TopicMetadata::ControlFlow { .. }
      | crate::core::TopicMetadata::TitledTopic { .. }
      | crate::core::TopicMetadata::CommentTopic { .. }
      | crate::core::TopicMetadata::FeatureTopic { .. }
      | crate::core::TopicMetadata::RequirementTopic { .. }
      | crate::core::TopicMetadata::BehaviorTopic { .. }
      | crate::core::TopicMetadata::ThreatTopic { .. }
      | crate::core::TopicMetadata::InvariantTopic { .. }
      | crate::core::TopicMetadata::DocumentationTopic { .. } => false,
    };

    if is_contract {
      // Check if the contract is in an in-scope file
      let is_in_scope = match metadata.scope() {
        crate::core::Scope::Container { container } => {
          audit_data.in_scope_files.contains(container)
        }
        _ => false,
      };

      if !is_in_scope {
        continue;
      }

      contracts.push(topic_metadata_to_response(topic, metadata));
    }
  }

  Ok(Json(ContractsResponse { contracts }))
}

// Get all qualified topic names for an audit
pub async fn get_qualified_names(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<Json<Vec<String>>, StatusCode> {
  println!("GET /api/v1/audits/{}/qualified_names", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_qualified_names: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let mut names: Vec<String> = audit_data
    .name_index
    .qualified_names()
    .into_iter()
    .map(|s| s.to_string())
    .collect();
  names.sort_unstable();

  Ok(Json(names))
}

// Get source text for a specific topic within an audit
pub async fn get_source_text(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
  println!("GET /api/v1/audits/{}/source_text/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_source_text: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  // Check source text cache first
  if let Some(html) = ctx.get_cached_source_text(&audit_id, &topic_id) {
    return Ok(Html(html.clone()));
  }

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  // Create topic from the topic_id
  let topic = new_topic(&topic_id);

  let source_text = super::topic_view::render_source_text(&topic, audit_data)
    .ok_or_else(|| {
    eprintln!("Topic '{}' not found in audit '{}'", topic_id, audit_id);
    StatusCode::NOT_FOUND
  })?;

  Ok(Html(source_text))
}

// Topic delimiter response

#[derive(Debug, Serialize)]
pub struct TopicDelimiterResponse {
  pub opening: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub closing: Option<String>,
}

// Get delimiter for a specific topic within an audit
pub async fn get_delimiter(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<Option<TopicDelimiterResponse>>, StatusCode> {
  println!("GET /api/v1/audits/{}/delimiter/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_delimiter: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let topic = new_topic(&topic_id);

  let node = audit_data.nodes.get(&topic).ok_or_else(|| {
    eprintln!("Topic '{}' not found in audit '{}'", topic_id, audit_id);
    StatusCode::NOT_FOUND
  })?;

  let delimiter = match node {
    core::Node::Solidity(solidity_node) => {
      crate::solidity::formatter::node_to_delimiter(
        solidity_node,
        &audit_data.nodes,
        &audit_data.topic_metadata,
      )
    }
    core::Node::Documentation(_) | core::Node::Comment(_) => None,
  };

  Ok(Json(delimiter.map(|d| TopicDelimiterResponse {
    opening: d.opening,
    closing: d.closing,
  })))
}

// Topic metadata response

/// Serializable block annotation kind for API responses.
/// Flattens `BlockAnnotationKind::If(ControlFlowBranch)` into `if_true`/`if_false`
/// for a clean single-discriminator JSON representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockAnnotationKindInfo {
  #[serde(rename = "if_true")]
  IfTrue,
  #[serde(rename = "if_false")]
  IfFalse,
  For,
  While,
  DoWhile,
  Unchecked,
  InlineAssembly,
}

impl BlockAnnotationKindInfo {
  pub fn from_core(kind: &core::BlockAnnotationKind) -> Self {
    match kind {
      core::BlockAnnotationKind::If(core::ControlFlowBranch::True) => {
        Self::IfTrue
      }
      core::BlockAnnotationKind::If(core::ControlFlowBranch::False) => {
        Self::IfFalse
      }
      core::BlockAnnotationKind::For => Self::For,
      core::BlockAnnotationKind::While => Self::While,
      core::BlockAnnotationKind::DoWhile => Self::DoWhile,
      core::BlockAnnotationKind::Unchecked => Self::Unchecked,
      core::BlockAnnotationKind::InlineAssembly => Self::InlineAssembly,
    }
  }

  pub fn to_core(&self) -> core::BlockAnnotationKind {
    match self {
      Self::IfTrue => {
        core::BlockAnnotationKind::If(core::ControlFlowBranch::True)
      }
      Self::IfFalse => {
        core::BlockAnnotationKind::If(core::ControlFlowBranch::False)
      }
      Self::For => core::BlockAnnotationKind::For,
      Self::While => core::BlockAnnotationKind::While,
      Self::DoWhile => core::BlockAnnotationKind::DoWhile,
      Self::Unchecked => core::BlockAnnotationKind::Unchecked,
      Self::InlineAssembly => core::BlockAnnotationKind::InlineAssembly,
    }
  }
}

/// Serializable block annotation for API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockAnnotationResponse {
  pub topic: String,
  pub kind: BlockAnnotationKindInfo,
}

/// One layer in the containing block nesting chain for API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainingBlockLayerInfo {
  pub block: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub annotation: Option<BlockAnnotationResponse>,
}

/// Serializable scope information for storing in database and API responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeInfo {
  pub scope_type: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub container: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub component: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub member: Option<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub containing_blocks: Vec<ContainingBlockLayerInfo>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub signature_container: Option<String>,
}

impl ScopeInfo {
  /// Convert from core::Scope to ScopeInfo
  pub fn from_scope(scope: &core::Scope) -> Self {
    match scope {
      core::Scope::Global => ScopeInfo {
        scope_type: "Global".to_string(),
        container: None,
        component: None,
        member: None,
        containing_blocks: vec![],
        signature_container: None,
      },
      core::Scope::Container { container } => ScopeInfo {
        scope_type: "Container".to_string(),
        container: Some(container.file_path.clone()),
        component: None,
        member: None,
        containing_blocks: vec![],
        signature_container: None,
      },
      core::Scope::Component {
        container,
        component,
      } => ScopeInfo {
        scope_type: "Component".to_string(),
        container: Some(container.file_path.clone()),
        component: Some(component.id.clone()),
        member: None,
        containing_blocks: vec![],
        signature_container: None,
      },
      core::Scope::Member {
        container,
        component,
        member,
        signature_container,
      } => ScopeInfo {
        scope_type: "Member".to_string(),
        container: Some(container.file_path.clone()),
        component: Some(component.id.clone()),
        member: Some(member.id.clone()),
        containing_blocks: vec![],
        signature_container: signature_container.as_ref().map(|t| t.id.clone()),
      },
      core::Scope::ContainingBlock {
        container,
        component,
        member,
        containing_blocks,
      } => ScopeInfo {
        scope_type: "ContainingBlock".to_string(),
        container: Some(container.file_path.clone()),
        component: Some(component.id.clone()),
        member: Some(member.id.clone()),
        containing_blocks: containing_blocks
          .iter()
          .map(|layer| ContainingBlockLayerInfo {
            block: layer.block.id.clone(),
            annotation: layer.annotation.as_ref().map(|ann| {
              BlockAnnotationResponse {
                topic: ann.topic.id.clone(),
                kind: BlockAnnotationKindInfo::from_core(&ann.kind),
              }
            }),
          })
          .collect(),
        signature_container: None,
      },
    }
  }

  /// Get the scope from a topic's metadata, or return Global scope if not found
  pub fn from_topic(topic_id: &str, audit_data: &core::AuditData) -> Self {
    let topic = new_topic(topic_id);
    if let Some(metadata) = audit_data.topic_metadata.get(&topic) {
      Self::from_scope(metadata.scope())
    } else {
      Self::default()
    }
  }

  /// Returns the lowest (most specific) scope topic ID.
  /// Returns innermost containing_block > member > component > None for Container/Global.
  pub fn lowest_scope_topic_id(&self) -> Option<&str> {
    self
      .containing_blocks
      .last()
      .map(|l| l.block.as_str())
      .or(self.member.as_deref())
      .or(self.component.as_deref())
  }

  /// Convert from ScopeInfo back to core::Scope
  pub fn to_scope(&self) -> core::Scope {
    let container = || core::ProjectPath {
      file_path: self.container.clone().unwrap(),
    };
    match self.scope_type.as_str() {
      "ContainingBlock" => core::Scope::ContainingBlock {
        container: container(),
        component: new_topic(self.component.as_ref().unwrap()),
        member: new_topic(self.member.as_ref().unwrap()),
        containing_blocks: self
          .containing_blocks
          .iter()
          .map(|layer| core::ContainingBlockLayer {
            block: new_topic(&layer.block),
            annotation: layer.annotation.as_ref().map(|ann| {
              core::BlockAnnotation {
                topic: new_topic(&ann.topic),
                kind: ann.kind.to_core(),
              }
            }),
          })
          .collect(),
      },
      "Member" => core::Scope::Member {
        container: container(),
        component: new_topic(self.component.as_ref().unwrap()),
        member: new_topic(self.member.as_ref().unwrap()),
        signature_container: self
          .signature_container
          .as_ref()
          .map(|s| new_topic(s)),
      },
      "Component" => core::Scope::Component {
        container: container(),
        component: new_topic(self.component.as_ref().unwrap()),
      },
      "Container" => core::Scope::Container {
        container: container(),
      },
      _ => core::Scope::Global,
    }
  }
}

impl Default for ScopeInfo {
  fn default() -> Self {
    ScopeInfo {
      scope_type: "Global".to_string(),
      container: None,
      component: None,
      member: None,
      containing_blocks: vec![],
      signature_container: None,
    }
  }
}

/// Response type for a single reference
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ReferenceResponse {
  #[serde(rename = "project")]
  Project { reference_topic: String },
  #[serde(rename = "project_with_mentions")]
  ProjectWithMentions {
    reference_topic: String,
    mention_topics: Vec<String>,
  },
  #[serde(rename = "comment")]
  Comment {
    reference_topic: String,
    mention_topics: Vec<String>,
  },
}

/// A child element in a source context — either a direct reference or an annotated block group.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "child_type")]
pub enum SourceChildResponse {
  #[serde(rename = "reference")]
  Reference { reference: ReferenceResponse },
  #[serde(rename = "annotated_block")]
  AnnotatedBlock {
    annotation: BlockAnnotationResponse,
    children: Vec<SourceChildResponse>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    has_sibling_branch: bool,
  },
}

#[derive(Debug, Clone, Serialize)]
pub struct NestedSourceContextResponse {
  pub subscope: String,
  pub children: Vec<SourceChildResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceContextResponse {
  pub scope: String,
  pub is_in_scope: bool,
  pub scope_references: Vec<ReferenceResponse>,
  pub nested_references: Vec<NestedSourceContextResponse>,
}

/// Response for NamedTopic metadata
#[derive(Debug, Serialize)]
pub struct NamedTopicResponse {
  pub topic_id: String,
  pub name: String,
  pub kind: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub sub_kind: Option<String>,
  pub visibility: String,
  pub scope: ScopeInfo,
  pub ancestors: Vec<String>,
  pub descendants: Vec<String>,
  pub relatives: Vec<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub mutations: Option<Vec<String>>,
}

/// Response for TitledTopic metadata
#[derive(Debug, Serialize)]
pub struct TitledTopicResponse {
  pub topic_id: String,
  pub title: String,
  pub kind: String,
  pub scope: ScopeInfo,
}

/// Response for UnnamedTopic metadata
#[derive(Debug, Serialize)]
pub struct UnnamedTopicResponse {
  pub topic_id: String,
  pub kind: String,
  pub scope: ScopeInfo,
}

/// Response for DocumentationTopic metadata (documentation root)
#[derive(Debug, Serialize)]
pub struct DocumentationTopicResponse {
  pub topic_id: String,
  pub scope: ScopeInfo,
  pub is_technical: bool,
}

/// Response for ControlFlow metadata
#[derive(Debug, Serialize)]
pub struct ControlFlowTopicResponse {
  pub topic_id: String,
  pub kind: String,
  pub scope: ScopeInfo,
  pub condition: String,
}

/// Response for CommentTopic metadata
#[derive(Debug, Clone, Serialize)]
pub struct CommentTopicResponse {
  pub topic_id: String,
  pub author_id: i64,
  pub comment_type: String,
  pub target_topic: String,
  pub created_at: String,
  pub scope: ScopeInfo,
  pub mentioned_topics: Vec<String>,
}

/// Response for FeatureTopic metadata
#[derive(Debug, Serialize)]
pub struct FeatureTopicResponse {
  pub topic_id: String,
  pub name: String,
  pub description: String,
  pub author_id: i64,
  pub created_at: String,
}

/// Response for RequirementTopic metadata
#[derive(Debug, Serialize)]
pub struct RequirementTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub feature_topic: String,
  pub author_id: i64,
  pub created_at: String,
}

/// Response for BehaviorTopic metadata
#[derive(Debug, Serialize)]
pub struct BehaviorTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub member_topic: String,
  pub author_id: i64,
  pub created_at: String,
}

/// Response for ThreatTopic metadata
#[derive(Debug, Serialize)]
pub struct ThreatTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub subject_topic: String,
  pub author_id: i64,
  pub created_at: String,
  pub severity: Option<String>,
}

/// Response for InvariantTopic metadata
#[derive(Debug, Serialize)]
pub struct InvariantTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub threat_topic: String,
  pub author_id: i64,
  pub created_at: String,
  pub severity: Option<String>,
}

/// Enum for different topic metadata response types
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum TopicMetadataResponse {
  #[serde(rename = "named")]
  Named(NamedTopicResponse),
  #[serde(rename = "titled")]
  Titled(TitledTopicResponse),
  #[serde(rename = "unnamed")]
  Unnamed(UnnamedTopicResponse),
  #[serde(rename = "control_flow")]
  ControlFlow(ControlFlowTopicResponse),
  #[serde(rename = "CommentTopic")]
  CommentTopic(CommentTopicResponse),
  #[serde(rename = "feature")]
  Feature(FeatureTopicResponse),
  #[serde(rename = "requirement")]
  Requirement(RequirementTopicResponse),
  #[serde(rename = "behavior")]
  Behavior(BehaviorTopicResponse),
  #[serde(rename = "threat")]
  Threat(ThreatTopicResponse),
  #[serde(rename = "invariant")]
  Invariant(InvariantTopicResponse),
  #[serde(rename = "documentation")]
  Documentation(DocumentationTopicResponse),
}

// Helper function to convert SourceChild to SourceChildResponse

// Helper function to convert TopicMetadata to TopicMetadataResponse
fn topic_metadata_to_response(
  topic: &crate::core::topic::Topic,
  metadata: &crate::core::TopicMetadata,
) -> TopicMetadataResponse {
  let scope_info = ScopeInfo::from_scope(metadata.scope());

  match metadata {
    crate::core::TopicMetadata::NamedTopic {
      name,
      kind,
      visibility,
      mutations,
      is_mutable,
      ..
    } => {
      // Format the kind and sub_kind for NamedTopic
      let (kind_str, sub_kind) = match kind {
        crate::core::NamedTopicKind::Contract(contract_kind) => {
          ("Contract".to_string(), Some(format!("{:?}", contract_kind)))
        }
        crate::core::NamedTopicKind::Function(function_kind) => {
          ("Function".to_string(), Some(format!("{:?}", function_kind)))
        }
        crate::core::NamedTopicKind::StateVariable(mutability) => (
          "StateVariable".to_string(),
          Some(format!("{:?}", mutability)),
        ),
        kind => (format!("{:?}", kind), None),
      };

      let mutations_response = if *is_mutable {
        Some(mutations.iter().map(|t| t.id.clone()).collect())
      } else {
        None
      };

      TopicMetadataResponse::Named(NamedTopicResponse {
        topic_id: topic.id.clone(),
        name: name.clone(),
        kind: kind_str,
        sub_kind,
        visibility: format!("{:?}", visibility),
        scope: scope_info,
        ancestors: metadata.ancestors().iter().map(|t| t.id.clone()).collect(),
        descendants: metadata
          .descendants()
          .iter()
          .map(|t| t.id.clone())
          .collect(),
        relatives: metadata.relatives().iter().map(|t| t.id.clone()).collect(),
        mutations: mutations_response,
      })
    }

    crate::core::TopicMetadata::TitledTopic { title, kind, .. } => {
      TopicMetadataResponse::Titled(TitledTopicResponse {
        topic_id: topic.id.clone(),
        title: title.clone(),
        kind: format!("{:?}", kind),
        scope: scope_info,
      })
    }

    crate::core::TopicMetadata::UnnamedTopic { kind, .. } => {
      TopicMetadataResponse::Unnamed(UnnamedTopicResponse {
        topic_id: topic.id.clone(),
        kind: format!("{:?}", kind),
        scope: scope_info,
      })
    }

    crate::core::TopicMetadata::DocumentationTopic { is_technical, .. } => {
      TopicMetadataResponse::Documentation(DocumentationTopicResponse {
        topic_id: topic.id.clone(),
        scope: scope_info,
        is_technical: *is_technical,
      })
    }

    crate::core::TopicMetadata::ControlFlow {
      kind, condition, ..
    } => TopicMetadataResponse::ControlFlow(ControlFlowTopicResponse {
      topic_id: topic.id.clone(),
      kind: format!("{:?}", kind),
      scope: scope_info,
      condition: condition.id.clone(),
    }),

    crate::core::TopicMetadata::CommentTopic {
      author_id,
      comment_type,
      target_topic,
      created_at,
      mentioned_topics,
      ..
    } => TopicMetadataResponse::CommentTopic(CommentTopicResponse {
      topic_id: topic.id.clone(),
      author_id: *author_id,
      comment_type: comment_type.clone(),
      target_topic: target_topic.id.clone(),
      created_at: created_at.clone(),
      scope: scope_info,
      mentioned_topics: mentioned_topics.iter().map(|t| t.id.clone()).collect(),
    }),

    crate::core::TopicMetadata::FeatureTopic {
      name,
      description,
      author_id,
      created_at,
      ..
    } => TopicMetadataResponse::Feature(FeatureTopicResponse {
      topic_id: topic.id.clone(),
      name: name.clone(),
      description: description.clone(),
      author_id: *author_id,
      created_at: created_at.clone(),
    }),

    crate::core::TopicMetadata::RequirementTopic {
      description,
      feature_topic,
      author_id,
      created_at,
      ..
    } => TopicMetadataResponse::Requirement(RequirementTopicResponse {
      topic_id: topic.id.clone(),
      description: description.clone(),
      feature_topic: feature_topic.id.clone(),
      author_id: *author_id,
      created_at: created_at.clone(),
    }),

    crate::core::TopicMetadata::BehaviorTopic {
      description,
      member_topic,
      author_id,
      created_at,
      ..
    } => TopicMetadataResponse::Behavior(BehaviorTopicResponse {
      topic_id: topic.id.clone(),
      description: description.clone(),
      member_topic: member_topic.id.clone(),
      author_id: *author_id,
      created_at: created_at.clone(),
    }),

    crate::core::TopicMetadata::ThreatTopic {
      description,
      subject_topic,
      author_id,
      created_at,
      severity,
      ..
    } => TopicMetadataResponse::Threat(ThreatTopicResponse {
      topic_id: topic.id.clone(),
      description: description.clone(),
      subject_topic: subject_topic.id.clone(),
      author_id: *author_id,
      created_at: created_at.clone(),
      severity: severity.map(|s| s.as_str().to_string()),
    }),

    crate::core::TopicMetadata::InvariantTopic {
      description,
      threat_topic,
      author_id,
      created_at,
      severity,
      ..
    } => TopicMetadataResponse::Invariant(InvariantTopicResponse {
      topic_id: topic.id.clone(),
      description: description.clone(),
      threat_topic: threat_topic.id.clone(),
      author_id: *author_id,
      created_at: created_at.clone(),
      severity: severity.map(|s| s.as_str().to_string()),
    }),
  }
}

// Get metadata for a specific topic within an audit
pub async fn get_metadata(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!("GET /api/v1/audits/{}/metadata/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_metadata: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  // Create topic from the topic_id
  let topic = new_topic(&topic_id);

  // Get the metadata for this topic
  let metadata = audit_data.topic_metadata.get(&topic).ok_or_else(|| {
    eprintln!(
      "Metadata for topic '{}' not found in audit '{}'",
      topic_id, audit_id
    );
    StatusCode::NOT_FOUND
  })?;

  Ok(Json(topic_metadata_to_response(&topic, metadata)))
}

// Get pre-rendered topic view HTML for a specific topic within an audit.
// Static panels (topic, expanded references, breadcrumb, highlight CSS) are
// cached forever since they are purely AST-derived. Dynamic panels (comments,
// mentions) are rendered fresh each time.
pub async fn get_topic_view(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<super::topic_view::TopicViewResponse>, StatusCode> {
  println!("GET /api/v1/audits/{}/topic_view/{}", audit_id, topic_id);

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_topic_view: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let source_text_cache = ctx
    .source_text_cache
    .get(&audit_id)
    .cloned()
    .unwrap_or_default();

  // Build the dynamic comment parent chain prefix (empty for non-comment topics)
  let prefix = super::topic_view::build_topic_panel_prefix(
    &topic_id,
    audit_data,
    &source_text_cache,
  );

  // Check cache for static parts
  let cached = ctx.get_cached_topic_view(&audit_id, &topic_id).cloned();

  let response = super::topic_view::build_topic_view(
    &topic_id,
    audit_data,
    &source_text_cache,
    cached.as_ref(),
    &prefix,
  )
  .ok_or_else(|| {
    eprintln!(
      "Metadata for topic '{}' not found in audit '{}'",
      topic_id, audit_id
    );
    StatusCode::NOT_FOUND
  })?;

  // Cache the static parts if not already cached (without the dynamic prefix)
  if cached.is_none() {
    // Strip the prefix to cache only the static topic panel
    let static_topic_panel = if prefix.is_empty() {
      response.topic_panel_html.clone()
    } else {
      response.topic_panel_html[prefix.len()..].to_string()
    };

    ctx.cache_topic_view(
      &audit_id,
      &topic_id,
      core::CachedTopicView {
        topic_panel_html: static_topic_panel,
        expanded_references_panel_html: response
          .expanded_references_panel_html
          .clone(),
        breadcrumb_html: response.breadcrumb_html.clone(),
        highlight_css: response.highlight_css.clone(),
      },
    );
  }

  Ok(Json(response))
}

/// GET /api/v1/audits/:audit_id/conversation/:topic_id
/// Returns the conversation for a topic: direct comments and mentions,
/// each with metadata and rendered thread HTML.
pub async fn get_conversation(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<super::topic_view::ConversationResponse>, StatusCode> {
  println!("GET /api/v1/audits/{}/conversation/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_conversation: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let source_text_cache = ctx
    .source_text_cache
    .get(&audit_id)
    .cloned()
    .unwrap_or_default();

  let response = super::topic_view::build_conversation(
    &topic_id,
    audit_data,
    &source_text_cache,
  )
  .ok_or_else(|| {
    eprintln!("Topic '{}' not found in audit '{}'", topic_id, audit_id);
    StatusCode::NOT_FOUND
  })?;

  Ok(Json(response))
}

/// GET /api/v1/audits/:audit_id/thread/:topic_id
/// Returns thread HTML for a single topic. Used to refetch after invalidation.
pub async fn get_thread(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
  println!("GET /api/v1/audits/{}/thread/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_thread: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let source_text_cache = ctx
    .source_text_cache
    .get(&audit_id)
    .cloned()
    .unwrap_or_default();

  let html =
    super::topic_view::build_thread(&topic_id, audit_data, &source_text_cache)
      .ok_or_else(|| {
        eprintln!("Topic '{}' not found in audit '{}'", topic_id, audit_id);
        StatusCode::NOT_FOUND
      })?;

  Ok(Html(html))
}

// ============================================================================
// Collaborator query parameter types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct UserIdQuery {
  pub user_id: i64,
}

#[derive(Debug, Deserialize)]
pub struct OptionalUserIdQuery {
  pub user_id: Option<i64>,
}

// ============================================================================
// Comment handlers
// ============================================================================

/// GET /api/v1/audits/:audit_id/topics/:topic_id/comments

/// GET /api/v1/audits/:audit_id/comments/:comment_type/:status
/// Returns topic IDs of comments matching both the specified type and status.
pub async fn list_comments_by_type_and_status(
  State(state): State<AppState>,
  Path((audit_id, comment_type, status)): Path<(String, String, String)>,
) -> Result<Json<CommentListResponse>, StatusCode> {
  println!(
    "GET /api/v1/audits/{}/comments/{}/{}",
    audit_id, comment_type, status
  );

  // Validate comment_type
  if CommentType::from_str(&comment_type).is_none() {
    return Err(StatusCode::BAD_REQUEST);
  }

  // Validate status (CommentStatus::from_str has a catch-all fallback, so check explicitly)
  match status.as_str() {
    "active" | "hidden" | "resolved" | "unanswered" | "answered"
    | "unconfirmed" | "confirmed" | "rejected" => {}
    _ => return Err(StatusCode::BAD_REQUEST),
  }

  let comments = db::get_comments_by_type_and_status(
    &state.db,
    &audit_id,
    &comment_type,
    &status,
  )
  .await
  .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  let comment_topic_ids =
    comments.iter().map(|c| c.comment_topic_id()).collect();

  Ok(Json(CommentListResponse { comment_topic_ids }))
}

/// POST /api/v1/audits/:audit_id/comments
/// Creates a new comment. Returns the new comment's topic ID.
pub async fn create_comment(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<CreateCommentRequest>,
) -> Result<Json<CommentCreatedResponse>, StatusCode> {
  println!("POST /api/v1/audits/{}/comments", audit_id);
  // Determine the scope from the target topic
  // If target is a comment (starts with "C"), copy scope from parent comment
  // Otherwise, get scope from the topic's metadata in audit data
  let target_topic = new_topic(&payload.topic_id);
  let scope = if target_topic.kind() == Some(TopicKind::Comment) {
    // Target is a comment - get scope from parent comment
    let parent_comment_id: i64 =
      target_topic.numeric_id().ok_or(StatusCode::BAD_REQUEST)?;
    let parent_comment = db::get_comment_raw(&state.db, parent_comment_id)
      .await
      .map_err(|_| StatusCode::NOT_FOUND)?;
    // Parse the stored scope JSON
    serde_json::from_str(&parent_comment.scope).unwrap_or_default()
  } else {
    // Target is a regular topic - get scope from audit metadata
    let ctx = state
      .data_context
      .lock()
      .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
    ScopeInfo::from_topic(&payload.topic_id, audit_data)
  };

  // Insert comment into database with scope
  let comment = db::create_comment(&state.db, &audit_id, &payload, &scope)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  let comment_topic_id = comment.comment_topic_id();
  let comment_topic = comment.comment_topic();

  // Parse mentions, render HTML, register in audit_data, and cache source text.
  // Build ConversationEntry objects for WebSocket broadcasting.
  let mut conversation_events: Vec<(
    String,
    super::topic_view::ConversationEntry,
    Vec<String>,
  )> = Vec::new();
  {
    let mut ctx = state
      .data_context
      .lock()
      .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    let (mentions, nodes) = parser::parse_comment(&payload.content, audit_data);
    let html =
      formatter::render_comment_html(&nodes, &comment_topic, &audit_data.nodes);

    // Store comment AST in nodes
    audit_data
      .nodes
      .insert(comment_topic.clone(), core::Node::Comment(nodes));

    store::register_comment_in_audit_data(
      audit_data, &comment, &scope, &mentions,
    );

    // Cache rendered HTML
    ctx.cache_source_text(&audit_id, &comment_topic_id, html);

    // Build conversation entries for broadcasting
    let source_text_cache = ctx
      .source_text_cache
      .get(&audit_id)
      .cloned()
      .unwrap_or_default();
    let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    // Collect parent comment chain for thread invalidation.
    // If the target is a comment, its thread (and all ancestor comment threads)
    // are invalidated because they now include the new reply.
    let invalidated_thread_ids: Vec<String> = {
      let mut ids = Vec::new();
      let mut current = new_topic(&payload.topic_id);
      while current.kind() == Some(TopicKind::Comment) {
        ids.push(current.id().to_string());
        match audit_data
          .topic_metadata
          .get(&current)
          .and_then(|m| m.target_topic())
        {
          Some(parent) if parent.kind() == Some(TopicKind::Comment) => {
            current = parent.clone();
          }
          _ => break,
        }
      }
      ids
    };

    // 1. ConversationUpdated for the target topic (comment entry)
    if let Some(entry) = super::topic_view::build_conversation_entry(
      &comment_topic,
      super::topic_view::ConversationEntryKind::Comment,
      audit_data,
      &source_text_cache,
    ) {
      conversation_events.push((
        payload.topic_id.clone(),
        entry,
        invalidated_thread_ids.clone(),
      ));
    }

    // 2. ConversationUpdated for each mentioned topic (mention entry)
    if !mentions.is_empty() {
      let mut mentioned_ids: Vec<&str> =
        mentions.iter().map(|m| m.id.as_str()).collect();
      mentioned_ids.sort_unstable();
      mentioned_ids.dedup();

      for mentioned_id in mentioned_ids {
        if let Some(entry) = super::topic_view::build_conversation_entry(
          &comment_topic,
          super::topic_view::ConversationEntryKind::Mention,
          audit_data,
          &source_text_cache,
        ) {
          conversation_events.push((mentioned_id.to_string(), entry, vec![]));
        }
      }
    }
  }

  // Broadcast via WebSocket
  for (topic_id, entry, invalidated_thread_ids) in conversation_events {
    let _ = state
      .comment_broadcast
      .send(CommentEvent::ConversationUpdated {
        audit_id: audit_id.clone(),
        topic_id,
        entry,
        invalidated_thread_ids,
      });
  }

  Ok(Json(CommentCreatedResponse { comment_topic_id }))
}

// ============================================================================
// Status handlers
// ============================================================================

/// GET /api/v1/audits/:audit_id/comments/:comment_id/status
/// Returns status for a single comment.
pub async fn get_comment_status(
  State(state): State<AppState>,
  Path((audit_id, comment_id)): Path<(String, i64)>,
) -> Result<Json<CommentStatusResponse>, StatusCode> {
  println!(
    "GET /api/v1/audits/{}/comments/{}/status",
    audit_id, comment_id
  );
  let response = db::get_comment_status(&state.db, comment_id)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  Ok(Json(response))
}

/// PUT /api/v1/audits/:audit_id/comments/:comment_id/status
/// Updates comment status.
pub async fn update_comment_status(
  State(state): State<AppState>,
  Path((audit_id, comment_id)): Path<(String, i64)>,
  Json(payload): Json<UpdateStatusRequest>,
) -> Result<Json<CommentStatusResponse>, StatusCode> {
  println!(
    "PUT /api/v1/audits/{}/comments/{}/status",
    audit_id, comment_id
  );
  // Update status in database
  let response = db::update_status(&state.db, comment_id, &payload.status)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  // Update in-memory comment index on hide/unhide
  {
    let comment_topic = new_topic(&format!("C{}", comment_id));
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in update_comment_status: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if let Some(audit_data) = ctx.get_audit_mut(&audit_id) {
      if let Some(target_topic) = audit_data
        .topic_metadata
        .get(&comment_topic)
        .and_then(|m| m.target_topic())
        .cloned()
      {
        if payload.status == CommentStatus::Hidden {
          if let Some(comments) =
            audit_data.comment_index.get_mut(&target_topic)
          {
            comments.retain(|t| t != &comment_topic);
          }
        } else {
          let comments =
            audit_data.comment_index.entry(target_topic).or_default();
          if !comments.contains(&comment_topic) {
            comments.push(comment_topic);
          }
        }
      }
    }
  }

  // Broadcast status update via WebSocket
  let _ = state.comment_broadcast.send(CommentEvent::StatusUpdated {
    audit_id: audit_id.clone(),
    comment_topic_id: response.comment_topic_id.clone(),
    status: response.status.clone(),
  });

  Ok(Json(response))
}

// ============================================================================
// Vote handlers
// ============================================================================

/// GET /api/v1/audits/:audit_id/votes/:comment_id
/// Returns vote summary for a comment.
pub async fn get_vote_summary(
  State(state): State<AppState>,
  Path((audit_id, comment_id)): Path<(String, i64)>,
  Query(params): Query<OptionalUserIdQuery>,
) -> Result<Json<CommentVoteSummary>, StatusCode> {
  println!("GET /api/v1/audits/{}/votes/{}", audit_id, comment_id);
  let vote_info = db::get_vote_info(&state.db, comment_id, params.user_id)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  Ok(Json(CommentVoteSummary {
    comment_id,
    comment_topic_id: format!("C{}", comment_id),
    score: vote_info.score,
    upvotes: vote_info.upvotes,
    downvotes: vote_info.downvotes,
    user_vote: vote_info.user_vote,
  }))
}

/// GET /api/v1/audits/:audit_id/votes/unvoted?user_id=N
/// Returns comment topic IDs the user has not voted on.
pub async fn get_unvoted_comment_ids(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Query(params): Query<UserIdQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
  println!(
    "GET /api/v1/audits/{}/votes/unvoted?user_id={}",
    audit_id, params.user_id
  );
  let comment_ids =
    db::get_unvoted_comment_ids(&state.db, &audit_id, params.user_id)
      .await
      .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  // Return as topic IDs (C1, C2, etc.)
  Ok(Json(
    comment_ids
      .into_iter()
      .map(|id| format!("C{}", id))
      .collect(),
  ))
}

/// POST /api/v1/audits/:audit_id/votes/:comment_id
/// Casts or updates a vote.
pub async fn cast_vote(
  State(state): State<AppState>,
  Path((audit_id, comment_id)): Path<(String, i64)>,
  Json(payload): Json<VoteRequest>,
) -> Result<Json<CommentVoteSummary>, StatusCode> {
  println!("POST /api/v1/audits/{}/votes/{}", audit_id, comment_id);
  let vote_value = payload.vote.to_i32();

  db::upsert_vote(&state.db, comment_id, payload.user_id, vote_value)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  // Return updated vote summary
  let vote_info =
    db::get_vote_info(&state.db, comment_id, Some(payload.user_id))
      .await
      .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  let comment_topic_id = format!("C{}", comment_id);

  // Broadcast vote update via WebSocket
  let _ = state.comment_broadcast.send(CommentEvent::VoteUpdated {
    audit_id,
    comment_topic_id: comment_topic_id.clone(),
    score: vote_info.score,
    upvotes: vote_info.upvotes,
    downvotes: vote_info.downvotes,
  });

  Ok(Json(CommentVoteSummary {
    comment_id,
    comment_topic_id,
    score: vote_info.score,
    upvotes: vote_info.upvotes,
    downvotes: vote_info.downvotes,
    user_vote: vote_info.user_vote,
  }))
}

/// DELETE /api/v1/audits/:audit_id/votes/:comment_id?user_id=N
/// Removes a user's vote.
pub async fn remove_vote(
  State(state): State<AppState>,
  Path((audit_id, comment_id)): Path<(String, i64)>,
  Query(params): Query<UserIdQuery>,
) -> Result<StatusCode, StatusCode> {
  println!(
    "DELETE /api/v1/audits/{}/votes/{}?user_id={}",
    audit_id, comment_id, params.user_id
  );
  db::delete_vote(&state.db, comment_id, params.user_id)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  // Get updated vote info and broadcast
  let vote_info = db::get_vote_info(&state.db, comment_id, None)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  let _ = state.comment_broadcast.send(CommentEvent::VoteUpdated {
    audit_id,
    comment_topic_id: format!("C{}", comment_id),
    score: vote_info.score,
    upvotes: vote_info.upvotes,
    downvotes: vote_info.downvotes,
  });

  Ok(StatusCode::NO_CONTENT)
}

// ============================================================================
// Agent context handler
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct AgentContextQuery {
  #[serde(default)]
  pub include_expanded_context: bool,
}

/// GET /api/v1/audits/:audit_id/agent_context/:topic_id
pub async fn get_agent_context(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
  Query(params): Query<AgentContextQuery>,
) -> Result<
  Json<crate::collaborator::agent::context::AgentTopicContext>,
  StatusCode,
> {
  println!("GET /api/v1/audits/{}/agent_context/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_agent_context: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let source_text_cache = ctx
    .source_text_cache
    .get(&audit_id)
    .cloned()
    .unwrap_or_default();

  let response =
    crate::collaborator::agent::context::build_agent_topic_context(
      &topic_id,
      audit_data,
      &source_text_cache,
      params.include_expanded_context,
    )
    .ok_or_else(|| {
      eprintln!("Topic '{}' not found in audit '{}'", topic_id, audit_id);
      StatusCode::NOT_FOUND
    })?;

  Ok(Json(response))
}

// ============================================
// Feature routes
// ============================================

/// Get all features for an audit.
pub async fn get_features(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<Json<Vec<TopicMetadataResponse>>, StatusCode> {
  println!("GET /api/v1/audits/{}/features", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_features: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let features = audit_data
    .features
    .keys()
    .filter_map(|ft| {
      audit_data
        .topic_metadata
        .get(ft)
        .map(|m| topic_metadata_to_response(ft, m))
    })
    .collect();

  Ok(Json(features))
}

/// GET /api/v1/audits/:audit_id/features/:feature_id/requirements
pub async fn get_feature_requirements(
  State(state): State<AppState>,
  Path((audit_id, feature_id)): Path<(String, String)>,
) -> Result<Json<Vec<String>>, StatusCode> {
  let feature_id = parse_path_id(&feature_id, TopicKind::Feature)?;
  println!(
    "GET /api/v1/audits/{}/features/{}/requirements",
    audit_id, feature_id
  );

  let feature_topic = topic::new_feature_topic(feature_id as i32);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_feature_requirements: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let feature = audit_data
    .features
    .get(&feature_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  let ids: Vec<String> = feature
    .requirement_topics
    .iter()
    .map(|t| t.id.clone())
    .collect();

  Ok(Json(ids))
}

/// GET /api/v1/audits/:audit_id/threats/:threat_id/invariants
pub async fn get_threat_invariants(
  State(state): State<AppState>,
  Path((audit_id, threat_id)): Path<(String, String)>,
) -> Result<Json<Vec<String>>, StatusCode> {
  let threat_id = parse_path_id(&threat_id, TopicKind::AttackVector)?;
  println!(
    "GET /api/v1/audits/{}/threats/{}/invariants",
    audit_id, threat_id
  );

  let threat_topic = topic::new_attack_vector_topic(threat_id as i32);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_threat_invariants: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let threat = audit_data
    .threats
    .get(&threat_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  let ids: Vec<String> = threat
    .invariant_topics
    .iter()
    .map(|t| t.id.clone())
    .collect();

  Ok(Json(ids))
}

/// Trigger the agent to build features from documentation.
/// Find feature topics for any source code topic by checking source_feature_links
/// on the topic itself, then on its containing member. Does not walk up to the
/// contract level — a topic inside a member gets the member's features only.
fn features_for_topic(
  t: &topic::Topic,
  audit_data: &core::AuditData,
) -> Vec<topic::Topic> {
  // Direct lookup
  if let Some(fts) = audit_data.source_feature_links.get(t) {
    if !fts.is_empty() {
      return fts.clone();
    }
  }

  // Walk up to the containing member only (not the contract)
  if let Some(metadata) = audit_data.topic_metadata.get(t) {
    let member = match metadata.scope() {
      core::Scope::Member { member, .. } => Some(member),
      core::Scope::ContainingBlock { member, .. } => Some(member),
      _ => None,
    };
    if let Some(member_topic) = member {
      if let Some(fts) = audit_data.source_feature_links.get(member_topic) {
        if !fts.is_empty() {
          return fts.clone();
        }
      }
    }
  }

  Vec::new()
}

/// Collect requirement topics for a set of feature topics.
fn requirements_for_features(
  feature_topics: &[topic::Topic],
  audit_data: &core::AuditData,
) -> Vec<topic::Topic> {
  let mut requirement_topics = Vec::new();
  for ft in feature_topics {
    if let Some(feature) = audit_data.features.get(ft) {
      for rt in &feature.requirement_topics {
        if !requirement_topics.contains(rt) {
          requirement_topics.push(rt.clone());
        }
      }
    }
  }
  requirement_topics
}

fn pipeline_state(state: &AppState) -> crate::collaborator::agent::pipeline::PipelineState {
  crate::collaborator::agent::pipeline::PipelineState {
    db: state.db.clone(),
    data_context: state.data_context.clone(),
  }
}

/// Spawn a pipeline step on a background task (required because pipeline
/// functions hold std::sync::MutexGuard which is !Send, so they must run
/// inside tokio::spawn to satisfy axum's Handler bounds).
async fn run_pipeline_step<F, Fut>(
  state: &AppState,
  audit_id: String,
  step_name: &str,
  f: F,
) -> Result<StatusCode, StatusCode>
where
  F: FnOnce(crate::collaborator::agent::pipeline::PipelineState, String) -> Fut
    + Send
    + 'static,
  Fut: std::future::Future<Output = Result<(), String>> + Send,
{
  let ps = pipeline_state(state);
  let name = step_name.to_string();
  tokio::spawn(async move { f(ps, audit_id).await })
    .await
    .map_err(|e| {
      eprintln!("{} task panicked: {}", name, e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?
    .map_err(|e| {
      eprintln!("{} failed: {}", name, e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
  Ok(StatusCode::OK)
}

/// POST /api/v1/audits/:audit_id/analyze
/// Runs the full analysis pipeline.
pub async fn analyze(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
  println!("POST /api/v1/audits/{}/analyze", audit_id);
  run_pipeline_step(&state, audit_id, "analyze", |ps, id| async move {
    crate::collaborator::agent::pipeline::run_full_pipeline(&ps, &id).await
  })
  .await
}

/// POST /api/v1/audits/:audit_id/pipeline/semantic_links
/// Step 1: Connect documentation sections to code declarations, producing
/// functional semantics with provenance. Must run before requirements so
/// that inline code in docs is annotated with semantic meaning.
pub async fn pipeline_semantic_links(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
  println!("POST /api/v1/audits/{}/pipeline/semantic_links", audit_id);
  run_pipeline_step(&state, audit_id, "build_semantic_links", |ps, id| async move {
    crate::collaborator::agent::pipeline::build_semantic_links(&ps, &id).await
  })
  .await
}

/// POST /api/v1/audits/:audit_id/pipeline/requirements
/// Step 2: Extract requirements from documentation, grouped by section.
/// Docs are rendered with functional semantics injected into inline code
/// references, so the LLM has project-specific meaning for declaration names.
pub async fn pipeline_requirements(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
  println!("POST /api/v1/audits/{}/pipeline/requirements", audit_id);
  run_pipeline_step(&state, audit_id, "build_requirements", |ps, id| async move {
    crate::collaborator::agent::pipeline::build_requirements(&ps, &id).await
  })
  .await
}

/// POST /api/v1/audits/:audit_id/pipeline/behaviors
/// Step 3: Extract behaviors from source code with semantics in context.
pub async fn pipeline_behaviors(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
  println!("POST /api/v1/audits/{}/pipeline/behaviors", audit_id);
  run_pipeline_step(&state, audit_id, "build_behaviors", |ps, id| async move {
    crate::collaborator::agent::pipeline::build_behaviors(&ps, &id).await
  })
  .await
}

/// POST /api/v1/audits/:audit_id/pipeline/synthesize
/// Step 4: Synthesize features by reconciling requirements with behaviors.
pub async fn pipeline_synthesize(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
  println!("POST /api/v1/audits/{}/pipeline/synthesize", audit_id);
  run_pipeline_step(&state, audit_id, "synthesize_features", |ps, id| async move {
    crate::collaborator::agent::pipeline::synthesize_features(&ps, &id).await
  })
  .await
}

#[derive(Debug, Deserialize)]
pub struct CreateFeatureRequest {
  pub name: String,
  pub description: String,
  pub author_id: i64,
}

/// POST /api/v1/audits/:audit_id/features
/// Creates a new user-defined feature.
pub async fn create_feature(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<CreateFeatureRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!("POST /api/v1/audits/{}/features", audit_id);

  let row = db::create_feature(
    &state.db,
    &audit_id,
    &payload.name,
    &payload.description,
    payload.author_id,
  )
  .await
  .map_err(|e| {
    eprintln!("create_feature failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let feature_topic = topic::new_feature_topic(row.id as i32);
  let feature = Feature {
    requirement_topics: Vec::new(),
  };

  let metadata = core::TopicMetadata::FeatureTopic {
    topic: feature_topic.clone(),
    name: row.name,
    description: row.description,
    author_id: row.author_id,
    created_at: row.created_at,
  };

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in create_feature: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  audit_data.features.insert(feature_topic.clone(), feature);
  audit_data
    .topic_metadata
    .insert(feature_topic.clone(), metadata.clone());

  let response = topic_metadata_to_response(&feature_topic, &metadata);
  Ok(Json(response))
}

/// GET /api/v1/audits/:audit_id/features/:feature_id
/// Gets a single feature by its numeric ID.
pub async fn get_feature(
  State(state): State<AppState>,
  Path((audit_id, feature_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let feature_id = parse_path_id(&feature_id, TopicKind::Feature)?;
  println!("GET /api/v1/audits/{}/features/{}", audit_id, feature_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_feature: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let feature_topic = topic::new_feature_topic(feature_id as i32);
  let metadata = audit_data
    .topic_metadata
    .get(&feature_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  Ok(Json(topic_metadata_to_response(&feature_topic, metadata)))
}

#[derive(Debug, Deserialize)]
pub struct AddFeatureTopicRequest {
  pub topic_id: String,
}

/// POST /api/v1/audits/:audit_id/requirements/:requirement_id/documentation_topics
/// Adds a documentation topic to a requirement.
pub async fn add_requirement_documentation_topic(
  State(state): State<AppState>,
  Path((audit_id, requirement_id)): Path<(String, String)>,
  Json(payload): Json<AddFeatureTopicRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let requirement_id = parse_path_id(&requirement_id, TopicKind::Requirement)?;
  println!(
    "POST /api/v1/audits/{}/requirements/{}/documentation_topics",
    audit_id, requirement_id
  );

  db::add_requirement_documentation_topic(
    &state.db,
    requirement_id,
    &payload.topic_id,
  )
  .await
  .map_err(|e| {
    eprintln!("add_requirement_documentation_topic failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!(
      "Mutex poisoned in add_requirement_documentation_topic: {}",
      e
    );
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let req_topic = topic::new_requirement_topic(requirement_id as i32);
  let requirement = audit_data
    .requirements
    .get_mut(&req_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  let new_topic = topic::new_topic(&payload.topic_id);
  if !requirement.documentation_topics.contains(&new_topic) {
    requirement.documentation_topics.push(new_topic);
  }

  crate::core::rebuild_feature_context(audit_data);
  let metadata = audit_data
    .topic_metadata
    .get(&req_topic)
    .ok_or(StatusCode::NOT_FOUND)?;
  let response = topic_metadata_to_response(&req_topic, metadata);

  Ok(Json(response))
}

/// DELETE /api/v1/audits/:audit_id/requirements/:requirement_id/documentation_topics/:topic_id
/// Removes a documentation topic from a requirement.
pub async fn remove_requirement_documentation_topic(
  State(state): State<AppState>,
  Path((audit_id, requirement_id, topic_id)): Path<(String, String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let requirement_id = parse_path_id(&requirement_id, TopicKind::Requirement)?;
  println!(
    "DELETE /api/v1/audits/{}/requirements/{}/documentation_topics/{}",
    audit_id, requirement_id, topic_id
  );

  db::remove_requirement_documentation_topic(
    &state.db,
    requirement_id,
    &topic_id,
  )
  .await
  .map_err(|e| {
    eprintln!("remove_requirement_documentation_topic failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!(
      "Mutex poisoned in remove_requirement_documentation_topic: {}",
      e
    );
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let req_topic = topic::new_requirement_topic(requirement_id as i32);
  let requirement = audit_data
    .requirements
    .get_mut(&req_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  let remove_topic = topic::new_topic(&topic_id);
  requirement
    .documentation_topics
    .retain(|t| t != &remove_topic);

  crate::core::rebuild_feature_context(audit_data);
  let metadata = audit_data
    .topic_metadata
    .get(&req_topic)
    .ok_or(StatusCode::NOT_FOUND)?;
  let response = topic_metadata_to_response(&req_topic, metadata);

  Ok(Json(response))
}

// ============================================
// Documentation routes
// ============================================

/// GET /api/v1/audits/:audit_id/requirements/topic/:topic_id
/// Returns requirement IDs linked to a topic.
/// - Requirement topics: returns itself
/// - Feature topics: returns all the feature's requirement_topics
/// - Source topics (N-prefixed): looks up features via source_feature_links, then requirements
/// - Documentation topics (D-prefixed): reverse-lookups requirements with this documentation_topic
pub async fn get_topic_requirements(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<Vec<String>>, StatusCode> {
  println!(
    "GET /api/v1/audits/{}/requirements/topic/{}",
    audit_id, topic_id
  );

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_topic_requirements: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let t = new_topic(&topic_id);

  let mut requirement_topics: Vec<topic::Topic> = Vec::new();
  match t.kind() {
    Some(TopicKind::Requirement) => {
      requirement_topics.push(t);
    }
    Some(TopicKind::Feature) => {
      if let Some(feature) = audit_data.features.get(&t) {
        for rt in &feature.requirement_topics {
          if !requirement_topics.contains(rt) {
            requirement_topics.push(rt.clone());
          }
        }
      }
    }
    _ => {
      // Check source-to-feature links with scope walk, then collect requirements
      let fts = features_for_topic(&t, audit_data);
      for rt in requirements_for_features(&fts, audit_data) {
        if !requirement_topics.contains(&rt) {
          requirement_topics.push(rt);
        }
      }
      // Check section_requirements index (D-section → requirements)
      if let Some(section_reqs) = audit_data.section_requirements.get(&t) {
        for rt in section_reqs {
          if !requirement_topics.contains(rt) {
            requirement_topics.push(rt.clone());
          }
        }
      }
      // Check documentation topic → requirements (leaf-level D-topics)
      for (req_topic, req) in &audit_data.requirements {
        if req.documentation_topics.contains(&t) {
          if !requirement_topics.contains(req_topic) {
            requirement_topics.push(req_topic.clone());
          }
        }
      }
    }
  }

  let requirement_ids: Vec<String> =
    requirement_topics.iter().map(|rt| rt.id.clone()).collect();

  Ok(Json(requirement_ids))
}

#[derive(Debug, Deserialize)]
pub struct DocumentationPanelRequest {
  pub feature_topics: Vec<String>,
}

/// POST /api/v1/audits/:audit_id/documentation
/// Returns rendered HTML panel of documentation linked to the given topics.
/// Accepts feature (F), requirement (R), or any other topic IDs.
/// - Feature topics: collect documentation from all their requirements
/// - Requirement topics: use their documentation_topics directly
/// - Other topics: look up features via source_feature_links, then requirements
pub async fn get_documentation_panel(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<DocumentationPanelRequest>,
) -> Result<impl IntoResponse, StatusCode> {
  println!("POST /api/v1/audits/{}/documentation", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_documentation_panel: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  // Resolve all input topic IDs to feature topics
  let mut feature_topics_resolved: Vec<topic::Topic> = Vec::new();
  for id in &payload.feature_topics {
    let t = new_topic(id);
    match t.kind() {
      Some(TopicKind::Feature) => {
        if !feature_topics_resolved.contains(&t) {
          feature_topics_resolved.push(t);
        }
      }
      _ => {
        // Look up features via source_feature_links with scope walk
        for ft in features_for_topic(&t, audit_data) {
          if !feature_topics_resolved.contains(&ft) {
            feature_topics_resolved.push(ft);
          }
        }
      }
    }
  }

  let source_text_cache = ctx
    .source_text_cache
    .get(&audit_id)
    .cloned()
    .unwrap_or_default();

  // Collect mention and semantic doc topics for all input topics.
  // For code declarations, also collect from the containing member and
  // its child declarations to capture all related documentation.
  let mut mention_topics: Vec<topic::Topic> = Vec::new();
  let mut related_topics: Vec<topic::Topic> = Vec::new();
  for id in &payload.feature_topics {
    let t = new_topic(id);
    related_topics.push(t.clone());

    // If this is a declaration scoped to a member, include the member too
    if let Some(metadata) = audit_data.topic_metadata.get(&t) {
      let member = match metadata.scope() {
        core::Scope::Member { member, .. }
        | core::Scope::ContainingBlock { member, .. } => Some(member.clone()),
        _ => None,
      };
      if let Some(mt) = member {
        if !related_topics.contains(&mt) {
          related_topics.push(mt);
        }
      }
    }
  }

  for t in &related_topics {
    // Mentions
    if let Some(mentioning) = audit_data.mentions_index.get(t) {
      for mt in mentioning {
        if !mention_topics.contains(mt) {
          mention_topics.push(mt.clone());
        }
      }
    }

    // Semantic link doc topics
    if let Some(sems) = audit_data.functional_semantics.get(t) {
      for sem in sems {
        for dt in &sem.documentation_topics {
          if !mention_topics.contains(dt) {
            mention_topics.push(dt.clone());
          }
        }
      }
    }
  }

  let html = super::topic_view::build_documentation_panel(
    &feature_topics_resolved,
    &mention_topics,
    audit_data,
    &source_text_cache,
  );

  Ok(Html(html))
}

// ============================================
// Requirement routes
// ============================================

#[derive(Debug, Deserialize)]
pub struct CreateRequirementRequest {
  pub description: String,
  pub author_id: i64,
  #[serde(default)]
  pub documentation_topics: Vec<String>,
}

/// POST /api/v1/audits/:audit_id/features/:feature_id/requirements
/// Creates a new requirement on a feature.
pub async fn create_requirement(
  State(state): State<AppState>,
  Path((audit_id, feature_id)): Path<(String, String)>,
  Json(payload): Json<CreateRequirementRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let feature_id = parse_path_id(&feature_id, TopicKind::Feature)?;
  println!(
    "POST /api/v1/audits/{}/features/{}/requirements",
    audit_id, feature_id
  );

  let row = db::create_requirement(
    &state.db,
    feature_id,
    &payload.description,
    payload.author_id,
  )
  .await
  .map_err(|e| {
    eprintln!("create_requirement failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  // Persist documentation topics for this requirement
  for dt_id in &payload.documentation_topics {
    let _ =
      db::add_requirement_documentation_topic(&state.db, row.id, dt_id).await;
  }

  let feature_topic = topic::new_feature_topic(feature_id as i32);
  let req_topic = topic::new_requirement_topic(row.id as i32);

  let doc_topics: Vec<topic::Topic> = payload
    .documentation_topics
    .iter()
    .map(|id| topic::new_topic(id))
    .collect();

  let requirement = core::Requirement {
    documentation_topics: doc_topics,
  };

  let metadata = core::TopicMetadata::RequirementTopic {
    topic: req_topic.clone(),
    description: row.description,
    feature_topic: feature_topic.clone(),
    section_topic: None,
    author_id: row.author_id,
    created_at: row.created_at,
  };

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in create_requirement: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  // Add requirement topic to parent feature
  let feature = audit_data
    .features
    .get_mut(&feature_topic)
    .ok_or(StatusCode::NOT_FOUND)?;
  feature.requirement_topics.push(req_topic.clone());

  // Store requirement and metadata
  audit_data
    .requirements
    .insert(req_topic.clone(), requirement);
  audit_data
    .topic_metadata
    .insert(req_topic.clone(), metadata.clone());

  crate::core::rebuild_feature_context(audit_data);

  let response = topic_metadata_to_response(&req_topic, &metadata);
  Ok(Json(response))
}

/// DELETE /api/v1/audits/:audit_id/features/:feature_id/requirements/:requirement_id
/// Deletes a requirement from a feature.
pub async fn delete_requirement(
  State(state): State<AppState>,
  Path((audit_id, feature_id, requirement_id)): Path<(String, String, String)>,
) -> Result<StatusCode, StatusCode> {
  let feature_id = parse_path_id(&feature_id, TopicKind::Feature)?;
  let requirement_id = parse_path_id(&requirement_id, TopicKind::Requirement)?;
  println!(
    "DELETE /api/v1/audits/{}/features/{}/requirements/{}",
    audit_id, feature_id, requirement_id
  );

  db::delete_requirement(&state.db, requirement_id)
    .await
    .map_err(|e| {
      eprintln!("delete_requirement failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  let req_topic = topic::new_requirement_topic(requirement_id as i32);
  let feature_topic = topic::new_feature_topic(feature_id as i32);

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in delete_requirement: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    // Remove from parent feature's requirement list
    if let Some(feature) = audit_data.features.get_mut(&feature_topic) {
      feature.requirement_topics.retain(|t| t != &req_topic);
    }

    // Remove requirement itself and its metadata
    audit_data.requirements.remove(&req_topic);
    audit_data.topic_metadata.remove(&req_topic);
    audit_data.topic_context.remove(&req_topic);

    crate::core::rebuild_feature_context(audit_data);
  }

  Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/audits/:audit_id/requirements/:requirement_id
/// Gets a single requirement.
pub async fn get_requirement(
  State(state): State<AppState>,
  Path((audit_id, requirement_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let requirement_id = parse_path_id(&requirement_id, TopicKind::Requirement)?;
  println!(
    "GET /api/v1/audits/{}/requirements/{}",
    audit_id, requirement_id
  );

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_requirement: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let req_topic = topic::new_requirement_topic(requirement_id as i32);
  let metadata = audit_data
    .topic_metadata
    .get(&req_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  Ok(Json(topic_metadata_to_response(&req_topic, metadata)))
}

#[derive(Debug, Deserialize)]
pub struct AddSourceTopicRequest {
  pub topic_id: String,
}

// ============================================
// Source-to-feature link routes
// ============================================

#[derive(Debug, Deserialize)]
pub struct AddSourceFeatureLinkRequest {
  pub source_topic: String,
  pub feature_id: String,
}

/// POST /api/v1/audits/:audit_id/source_feature_links
/// Links a source code member to a feature.
pub async fn add_source_feature_link(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<AddSourceFeatureLinkRequest>,
) -> Result<StatusCode, StatusCode> {
  let feature_id = parse_path_id(&payload.feature_id, TopicKind::Feature)?;
  println!(
    "POST /api/v1/audits/{}/source_feature_links {} -> F{}",
    audit_id, payload.source_topic, feature_id
  );

  db::add_source_feature_link(
    &state.db,
    &audit_id,
    &payload.source_topic,
    feature_id,
  )
  .await
  .map_err(|e| {
    eprintln!("add_source_feature_link failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in add_source_feature_link: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let source_topic = topic::new_topic(&payload.source_topic);
  let feature_topic = topic::new_feature_topic(feature_id as i32);
  let features = audit_data
    .source_feature_links
    .entry(source_topic)
    .or_default();
  if !features.contains(&feature_topic) {
    features.push(feature_topic);
  }

  Ok(StatusCode::CREATED)
}

/// DELETE /api/v1/audits/:audit_id/source_feature_links/:source_topic/:feature_id
/// Unlinks a source code member from a feature.
pub async fn remove_source_feature_link(
  State(state): State<AppState>,
  Path((audit_id, source_topic_id, feature_id)): Path<(String, String, String)>,
) -> Result<StatusCode, StatusCode> {
  let feature_id = parse_path_id(&feature_id, TopicKind::Feature)?;
  println!(
    "DELETE /api/v1/audits/{}/source_feature_links/{}/{}",
    audit_id, source_topic_id, feature_id
  );

  db::remove_source_feature_link(
    &state.db,
    &audit_id,
    &source_topic_id,
    feature_id,
  )
  .await
  .map_err(|e| {
    eprintln!("remove_source_feature_link failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in remove_source_feature_link: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let source_topic = topic::new_topic(&source_topic_id);
  let feature_topic = topic::new_feature_topic(feature_id as i32);
  if let Some(features) = audit_data.source_feature_links.get_mut(&source_topic) {
    features.retain(|ft| ft != &feature_topic);
    if features.is_empty() {
      audit_data.source_feature_links.remove(&source_topic);
    }
  }

  Ok(StatusCode::NO_CONTENT)
}

// ============================================
// Reconciliation routes
// ============================================

#[derive(Debug, Serialize)]
pub struct ReconciliationResponse {
  pub feature_topic: String,
  pub feature_name: String,
  pub requirements: Vec<ReconciliationRequirement>,
  pub behaviors: Vec<ReconciliationBehavior>,
}

#[derive(Debug, Serialize)]
pub struct ReconciliationRequirement {
  pub topic_id: String,
  pub description: String,
}

#[derive(Debug, Serialize)]
pub struct ReconciliationBehavior {
  pub topic_id: String,
  pub description: String,
  pub member_topic: String,
}

/// GET /api/v1/audits/:audit_id/reconciliation/:feature_id
/// Returns the requirements and behaviors for a feature, enabling the auditor
/// to compare documented claims against observed implementation.
pub async fn get_reconciliation(
  State(state): State<AppState>,
  Path((audit_id, feature_id)): Path<(String, String)>,
) -> Result<Json<ReconciliationResponse>, StatusCode> {
  let feature_id = parse_path_id(&feature_id, TopicKind::Feature)?;
  println!(
    "GET /api/v1/audits/{}/reconciliation/{}",
    audit_id, feature_id
  );

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_reconciliation: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let feature_topic = topic::new_feature_topic(feature_id as i32);

  let feature = audit_data
    .features
    .get(&feature_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  let feature_name = match audit_data.topic_metadata.get(&feature_topic) {
    Some(core::TopicMetadata::FeatureTopic { name, .. }) => name.clone(),
    _ => return Err(StatusCode::NOT_FOUND),
  };

  // Collect requirements
  let requirements: Vec<ReconciliationRequirement> = feature
    .requirement_topics
    .iter()
    .filter_map(|rt| {
      if let Some(core::TopicMetadata::RequirementTopic {
        description, ..
      }) = audit_data.topic_metadata.get(rt)
      {
        Some(ReconciliationRequirement {
          topic_id: rt.id.clone(),
          description: description.clone(),
        })
      } else {
        None
      }
    })
    .collect();

  // Collect behaviors associated with this feature via member→feature links
  let behaviors: Vec<ReconciliationBehavior> = audit_data
    .behaviors
    .keys()
    .filter_map(|bt| {
      let (description, member_topic) = match audit_data.topic_metadata.get(bt) {
        Some(core::TopicMetadata::BehaviorTopic {
          description,
          member_topic,
          ..
        }) => (description.clone(), member_topic.clone()),
        _ => return None,
      };

      // Check if this behavior's member links to this feature
      let linked = audit_data
        .source_feature_links
        .get(&member_topic)
        .map(|fts| fts.contains(&feature_topic))
        .unwrap_or(false);

      if !linked {
        return None;
      }

      Some(ReconciliationBehavior {
        topic_id: bt.id.clone(),
        description,
        member_topic: member_topic.id.clone(),
      })
    })
    .collect();

  Ok(Json(ReconciliationResponse {
    feature_topic: feature_topic.id.clone(),
    feature_name,
    requirements,
    behaviors,
  }))
}

// ============================================
// Subject property routes (functional purpose and semantics)
// ============================================

#[derive(Debug, Deserialize)]
pub struct SetSubjectPropertyRequest {
  pub value: String,
  pub author_id: i64,
}

#[derive(Debug, Serialize)]
pub struct SubjectPropertyResponse {
  pub topic_id: String,
  pub property_type: String,
  pub value: String,
  pub author_id: i64,
}

/// PUT /api/v1/audits/:audit_id/subjects/:topic_id/purpose
/// Sets or updates the functional purpose of a subject.
pub async fn set_functional_purpose(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
  Json(payload): Json<SetSubjectPropertyRequest>,
) -> Result<Json<SubjectPropertyResponse>, StatusCode> {
  let row = db::set_subject_property(
    &state.db,
    &audit_id,
    &topic_id,
    "functional_purpose",
    &payload.value,
    payload.author_id,
  )
  .await
  .map_err(|e| {
    eprintln!("set_functional_purpose failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let t = topic::new_topic(&topic_id);
  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in set_functional_purpose: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
    audit_data.functional_purposes.insert(t, core::FunctionalPurpose {
      text: row.value.clone(),
      author_id: row.author_id,
      created_at: row.created_at.clone(),
    });
  }

  Ok(Json(SubjectPropertyResponse {
    topic_id: row.topic_id,
    property_type: "functional_purpose".to_string(),
    value: row.value,
    author_id: row.author_id,
  }))
}

/// PUT /api/v1/audits/:audit_id/subjects/:topic_id/semantics
/// Sets or updates the functional semantics of a subject.
pub async fn add_functional_semantic(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
  Json(payload): Json<SetSubjectPropertyRequest>,
) -> Result<Json<SubjectPropertyResponse>, StatusCode> {
  let row = db::set_subject_property(
    &state.db,
    &audit_id,
    &topic_id,
    "functional_semantics",
    &payload.value,
    payload.author_id,
  )
  .await
  .map_err(|e| {
    eprintln!("add_functional_semantic failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let t = topic::new_topic(&topic_id);
  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in add_functional_semantic: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
    audit_data
      .functional_semantics
      .entry(t)
      .or_default()
      .push(core::FunctionalSemantic {
        text: row.value.clone(),
        documentation_topics: vec![],
        author_id: row.author_id,
        created_at: row.created_at.clone(),
      });
  }

  Ok(Json(SubjectPropertyResponse {
    topic_id: row.topic_id,
    property_type: "functional_semantics".to_string(),
    value: row.value,
    author_id: row.author_id,
  }))
}

/// GET /api/v1/audits/:audit_id/subjects/:topic_id/purpose
pub async fn get_functional_purpose(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<Option<SubjectPropertyResponse>>, StatusCode> {
  let row = db::get_subject_property(&state.db, &audit_id, &topic_id, "functional_purpose")
    .await
    .map_err(|e| {
      eprintln!("get_functional_purpose failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  Ok(Json(row.map(|r| SubjectPropertyResponse {
    topic_id: r.topic_id,
    property_type: "functional_purpose".to_string(),
    value: r.value,
    author_id: r.author_id,
  })))
}

/// GET /api/v1/audits/:audit_id/subjects/:topic_id/semantics
pub async fn get_functional_semantics(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<Option<SubjectPropertyResponse>>, StatusCode> {
  let row = db::get_subject_property(&state.db, &audit_id, &topic_id, "functional_semantics")
    .await
    .map_err(|e| {
      eprintln!("get_functional_semantics failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  Ok(Json(row.map(|r| SubjectPropertyResponse {
    topic_id: r.topic_id,
    property_type: "functional_semantics".to_string(),
    value: r.value,
    author_id: r.author_id,
  })))
}

// ============================================
// Impact analysis routes
// ============================================

#[derive(Debug, Deserialize)]
pub struct CreateThreatFeatureLinkRequest {
  pub threat_id: String,
  pub feature_id: String,
  pub relation: String,
  pub severity: String,
}

#[derive(Debug, Serialize)]
pub struct ThreatFeatureLinkResponse {
  pub threat_topic: String,
  pub feature_topic: String,
  pub relation: String,
  pub severity: String,
}

/// POST /api/v1/audits/:audit_id/impact_analysis
/// Links a threat to a feature with a relationship type and severity.
pub async fn create_threat_feature_link(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<CreateThreatFeatureLinkRequest>,
) -> Result<Json<ThreatFeatureLinkResponse>, StatusCode> {
  println!(
    "POST /api/v1/audits/{}/impact_analysis T{} -> F{}",
    audit_id, payload.threat_id, payload.feature_id
  );

  let threat_id = parse_path_id(&payload.threat_id, TopicKind::AttackVector)?;
  let feature_id = parse_path_id(&payload.feature_id, TopicKind::Feature)?;

  let relation = core::ThreatFeatureRelation::from_str(&payload.relation)
    .ok_or(StatusCode::BAD_REQUEST)?;
  let severity = core::ThreatSeverity::from_str(&payload.severity)
    .ok_or(StatusCode::BAD_REQUEST)?;

  let _row = db::create_threat_feature_link(
    &state.db,
    &audit_id,
    threat_id,
    feature_id,
    relation.as_str(),
    severity.as_str(),
  )
  .await
  .map_err(|e| {
    eprintln!("create_threat_feature_link failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let threat_topic = topic::new_attack_vector_topic(threat_id as i32);
  let feature_topic = topic::new_feature_topic(feature_id as i32);

  // Update in-memory state
  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in create_threat_feature_link: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    // Remove existing link for this pair if any
    audit_data.threat_feature_links.retain(|l| {
      !(l.threat_topic == threat_topic && l.feature_topic == feature_topic)
    });

    audit_data.threat_feature_links.push(core::ThreatFeatureLink {
      threat_topic: threat_topic.clone(),
      feature_topic: feature_topic.clone(),
      relation,
      severity,
    });

    // Update threat severity to highest among its links
    let max_severity = audit_data
      .threat_feature_links
      .iter()
      .filter(|l| l.threat_topic == threat_topic)
      .map(|l| l.severity)
      .max();
    if let Some(max_sev) = max_severity {
      if let Some(core::TopicMetadata::ThreatTopic { severity: s, .. }) =
        audit_data.topic_metadata.get_mut(&threat_topic)
      {
        *s = Some(max_sev);
      }
    }
  }

  Ok(Json(ThreatFeatureLinkResponse {
    threat_topic: threat_topic.id.clone(),
    feature_topic: feature_topic.id.clone(),
    relation: relation.as_str().to_string(),
    severity: severity.as_str().to_string(),
  }))
}

/// DELETE /api/v1/audits/:audit_id/impact_analysis/:threat_id/:feature_id
pub async fn delete_threat_feature_link(
  State(state): State<AppState>,
  Path((audit_id, threat_id, feature_id)): Path<(String, String, String)>,
) -> Result<StatusCode, StatusCode> {
  let threat_id = parse_path_id(&threat_id, TopicKind::AttackVector)?;
  let feature_id = parse_path_id(&feature_id, TopicKind::Feature)?;
  println!(
    "DELETE /api/v1/audits/{}/impact_analysis/{}/{}",
    audit_id, threat_id, feature_id
  );

  db::delete_threat_feature_link(&state.db, threat_id, feature_id)
    .await
    .map_err(|e| {
      eprintln!("delete_threat_feature_link failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  let threat_topic = topic::new_attack_vector_topic(threat_id as i32);
  let feature_topic = topic::new_feature_topic(feature_id as i32);

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in delete_threat_feature_link: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    audit_data.threat_feature_links.retain(|l| {
      !(l.threat_topic == threat_topic && l.feature_topic == feature_topic)
    });

    // Recalculate threat severity
    let max_severity = audit_data
      .threat_feature_links
      .iter()
      .filter(|l| l.threat_topic == threat_topic)
      .map(|l| l.severity)
      .max();
    if let Some(core::TopicMetadata::ThreatTopic { severity: s, .. }) =
      audit_data.topic_metadata.get_mut(&threat_topic)
    {
      *s = max_severity;
    }
  }

  Ok(StatusCode::NO_CONTENT)
}

// ============================================
// Condition routes
// ============================================

#[derive(Debug, Deserialize)]
pub struct CreateConditionRequest {
  pub subject_topic: String,
  pub condition_type: String,
  pub description: String,
  pub author_id: i64,
  #[serde(default)]
  pub evaluations: Vec<ConditionEvaluationInput>,
}

#[derive(Debug, Deserialize)]
pub struct ConditionEvaluationInput {
  pub question: String,
  pub answer: String,
}

#[derive(Debug, Serialize)]
pub struct ConditionResponse {
  pub id: i64,
  pub subject_topic: String,
  pub condition_type: String,
  pub description: String,
  pub author_id: i64,
  pub created_at: String,
  pub evaluations: Vec<ConditionEvaluationResponse>,
}

#[derive(Debug, Serialize)]
pub struct ConditionEvaluationResponse {
  pub question: String,
  pub answer: String,
}

/// POST /api/v1/audits/:audit_id/conditions
/// Creates a new condition on a non-pure subject.
pub async fn create_condition(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<CreateConditionRequest>,
) -> Result<Json<ConditionResponse>, StatusCode> {
  println!(
    "POST /api/v1/audits/{}/conditions for {}",
    audit_id, payload.subject_topic
  );

  let row = db::create_condition(
    &state.db,
    &audit_id,
    &payload.subject_topic,
    &payload.condition_type,
    &payload.description,
    payload.author_id,
  )
  .await
  .map_err(|e| {
    eprintln!("create_condition failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let mut eval_responses = Vec::new();
  for eval in &payload.evaluations {
    let _ = db::add_condition_evaluation(
      &state.db,
      row.id,
      &eval.question,
      &eval.answer,
    )
    .await;
    eval_responses.push(ConditionEvaluationResponse {
      question: eval.question.clone(),
      answer: eval.answer.clone(),
    });
  }

  // Update in-memory state
  let condition_type = match payload.condition_type.as_str() {
    "state_write" => core::NonPureSubjectType::StateWrite,
    "state_read" => core::NonPureSubjectType::StateRead,
    "external_call" => core::NonPureSubjectType::ExternalCall,
    "delegate_call" => core::NonPureSubjectType::DelegateCall,
    "inline_assembly" => core::NonPureSubjectType::InlineAssembly,
    "create" => core::NonPureSubjectType::Create,
    _ => return Err(StatusCode::BAD_REQUEST),
  };

  let condition = core::Condition {
    subject_topic: topic::new_topic(&payload.subject_topic),
    condition_type,
    description: payload.description.clone(),
    evaluations: payload
      .evaluations
      .iter()
      .map(|e| core::ConditionEvaluation {
        question: e.question.clone(),
        answer: e.answer.clone(),
      })
      .collect(),
  };

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in create_condition: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
    audit_data.conditions.push(condition);
  }

  Ok(Json(ConditionResponse {
    id: row.id,
    subject_topic: row.subject_topic,
    condition_type: row.condition_type,
    description: row.description,
    author_id: row.author_id,
    created_at: row.created_at,
    evaluations: eval_responses,
  }))
}

/// GET /api/v1/audits/:audit_id/conditions/:subject_topic
/// Returns all conditions for a subject.
pub async fn get_subject_conditions(
  State(state): State<AppState>,
  Path((audit_id, subject_topic)): Path<(String, String)>,
) -> Result<Json<Vec<ConditionResponse>>, StatusCode> {
  println!(
    "GET /api/v1/audits/{}/conditions/{}",
    audit_id, subject_topic
  );

  let rows = db::get_conditions_for_subject(&state.db, &audit_id, &subject_topic)
    .await
    .map_err(|e| {
      eprintln!("get_conditions_for_subject failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  let mut responses = Vec::new();
  for row in rows {
    let evals = db::get_condition_evaluations(&state.db, row.id)
      .await
      .map_err(|e| {
        eprintln!("get_condition_evaluations failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
      })?;

    responses.push(ConditionResponse {
      id: row.id,
      subject_topic: row.subject_topic,
      condition_type: row.condition_type,
      description: row.description,
      author_id: row.author_id,
      created_at: row.created_at,
      evaluations: evals
        .iter()
        .map(|e| ConditionEvaluationResponse {
          question: e.question.clone(),
          answer: e.answer.clone(),
        })
        .collect(),
    });
  }

  Ok(Json(responses))
}

/// DELETE /api/v1/audits/:audit_id/conditions/:condition_id
pub async fn delete_condition(
  State(state): State<AppState>,
  Path((audit_id, condition_id)): Path<(String, i64)>,
) -> Result<StatusCode, StatusCode> {
  println!(
    "DELETE /api/v1/audits/{}/conditions/{}",
    audit_id, condition_id
  );

  // Get the condition before deleting so we can update in-memory state
  let rows = db::get_conditions_for_subject(&state.db, &audit_id, "")
    .await
    .unwrap_or_default();
  let subject_topic = rows
    .iter()
    .find(|r| r.id == condition_id)
    .map(|r| r.subject_topic.clone());

  db::delete_condition(&state.db, condition_id)
    .await
    .map_err(|e| {
      eprintln!("delete_condition failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  // Remove from in-memory state
  if let Some(_subject) = subject_topic {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in delete_condition: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    // Remove condition by matching ID (we'd need to track IDs — for now, rebuild)
    // Since conditions don't have topic IDs, we filter by description match
    audit_data.conditions.retain(|_c| true); // TODO: proper filtering with IDs
  }

  Ok(StatusCode::NO_CONTENT)
}

// ============================================
// Behavior routes
// ============================================

#[derive(Debug, Deserialize)]
pub struct CreateBehaviorRequest {
  pub description: String,
  pub member_topic: String,
  pub author_id: i64,
}

/// POST /api/v1/audits/:audit_id/behaviors
/// Creates a new behavior on a code member.
pub async fn create_behavior(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<CreateBehaviorRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!("POST /api/v1/audits/{}/behaviors", audit_id);

  let row = db::create_behavior(
    &state.db,
    &audit_id,
    &payload.member_topic,
    &payload.description,
    payload.author_id,
  )
  .await
  .map_err(|e| {
    eprintln!("create_behavior failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let beh_topic = topic::new_behavior_topic(row.id as i32);
  let member_topic = topic::new_topic(&payload.member_topic);

  let behavior = core::Behavior {};

  let metadata = core::TopicMetadata::BehaviorTopic {
    topic: beh_topic.clone(),
    description: row.description,
    member_topic,
    author_id: row.author_id,
    created_at: row.created_at,
  };

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in create_behavior: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  audit_data.behaviors.insert(beh_topic.clone(), behavior);
  audit_data
    .topic_metadata
    .insert(beh_topic.clone(), metadata.clone());

  let response = topic_metadata_to_response(&beh_topic, &metadata);
  Ok(Json(response))
}

/// DELETE /api/v1/audits/:audit_id/behaviors/:behavior_id
pub async fn delete_behavior(
  State(state): State<AppState>,
  Path((audit_id, behavior_id)): Path<(String, String)>,
) -> Result<StatusCode, StatusCode> {
  let behavior_id = parse_path_id(&behavior_id, TopicKind::Behavior)?;
  println!(
    "DELETE /api/v1/audits/{}/behaviors/{}",
    audit_id, behavior_id
  );

  db::delete_behavior(&state.db, behavior_id)
    .await
    .map_err(|e| {
      eprintln!("delete_behavior failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  let beh_topic = topic::new_behavior_topic(behavior_id as i32);

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in delete_behavior: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    audit_data.behaviors.remove(&beh_topic);
    audit_data.topic_metadata.remove(&beh_topic);
    audit_data.topic_context.remove(&beh_topic);
  }

  Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/audits/:audit_id/behaviors/:behavior_id
pub async fn get_behavior(
  State(state): State<AppState>,
  Path((audit_id, behavior_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let behavior_id = parse_path_id(&behavior_id, TopicKind::Behavior)?;
  println!(
    "GET /api/v1/audits/{}/behaviors/{}",
    audit_id, behavior_id
  );

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_behavior: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let beh_topic = topic::new_behavior_topic(behavior_id as i32);
  let metadata = audit_data
    .topic_metadata
    .get(&beh_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  Ok(Json(topic_metadata_to_response(&beh_topic, metadata)))
}

/// GET /api/v1/audits/:audit_id/behaviors
/// Returns all behaviors for an audit.
pub async fn get_behaviors(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<Json<Vec<TopicMetadataResponse>>, StatusCode> {
  println!("GET /api/v1/audits/{}/behaviors", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_behaviors: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let behaviors: Vec<TopicMetadataResponse> = audit_data
    .behaviors
    .keys()
    .filter_map(|bt| {
      let metadata = audit_data.topic_metadata.get(bt)?;
      Some(topic_metadata_to_response(bt, metadata))
    })
    .collect();

  Ok(Json(behaviors))
}

// ============================================
// Threat routes
// ============================================

#[derive(Debug, Deserialize)]
pub struct CreateThreatRequest {
  pub description: String,
  pub subject_topic: String,
  pub author_id: i64,
}

/// POST /api/v1/audits/:audit_id/threats
/// Creates a new threat on a non-pure subject. Severity is assigned later
/// during impact analysis.
pub async fn create_threat(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<CreateThreatRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!(
    "POST /api/v1/audits/{}/threats on {}",
    audit_id, payload.subject_topic
  );

  let row = db::create_threat(
    &state.db,
    &audit_id,
    &payload.subject_topic,
    &payload.description,
    payload.author_id,
    None,
  )
  .await
  .map_err(|e| {
    eprintln!("create_threat failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let subject_topic = topic::new_topic(&payload.subject_topic);
  let threat_topic = topic::new_attack_vector_topic(row.id as i32);

  let threat = core::Threat {
    invariant_topics: Vec::new(),
  };

  let metadata = core::TopicMetadata::ThreatTopic {
    topic: threat_topic.clone(),
    description: row.description,
    subject_topic: subject_topic.clone(),
    author_id: row.author_id,
    created_at: row.created_at,
    severity: None,
  };

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in create_threat: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  audit_data.threats.insert(threat_topic.clone(), threat);
  audit_data
    .topic_metadata
    .insert(threat_topic.clone(), metadata.clone());

  crate::core::rebuild_feature_context(audit_data);

  let response = topic_metadata_to_response(&threat_topic, &metadata);
  Ok(Json(response))
}

/// DELETE /api/v1/audits/:audit_id/threats/:threat_id
pub async fn delete_threat(
  State(state): State<AppState>,
  Path((audit_id, threat_id)): Path<(String, String)>,
) -> Result<StatusCode, StatusCode> {
  let threat_id = parse_path_id(&threat_id, TopicKind::AttackVector)?;
  println!(
    "DELETE /api/v1/audits/{}/threats/{}",
    audit_id, threat_id
  );

  db::delete_threat(&state.db, threat_id).await.map_err(|e| {
    eprintln!("delete_threat failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let threat_topic = topic::new_attack_vector_topic(threat_id as i32);

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in delete_threat: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    // Remove invariants belonging to this threat
    if let Some(threat) = audit_data.threats.get(&threat_topic) {
      for inv_topic in &threat.invariant_topics {
        audit_data.invariants.remove(inv_topic);
        audit_data.topic_metadata.remove(inv_topic);
        audit_data.topic_context.remove(inv_topic);
      }
    }

    audit_data.threats.remove(&threat_topic);
    audit_data.topic_metadata.remove(&threat_topic);
    audit_data.topic_context.remove(&threat_topic);

    crate::core::rebuild_feature_context(audit_data);
  }

  Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/audits/:audit_id/threats/:threat_id
pub async fn get_threat(
  State(state): State<AppState>,
  Path((audit_id, threat_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let threat_id = parse_path_id(&threat_id, TopicKind::AttackVector)?;
  println!("GET /api/v1/audits/{}/threats/{}", audit_id, threat_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_threat: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let threat_topic = topic::new_attack_vector_topic(threat_id as i32);
  let metadata = audit_data
    .topic_metadata
    .get(&threat_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  Ok(Json(topic_metadata_to_response(&threat_topic, metadata)))
}

// ============================================
// Invariant routes
// ============================================

#[derive(Debug, Deserialize)]
pub struct CreateInvariantRequest {
  pub description: String,
  pub author_id: i64,
}

/// POST /api/v1/audits/:audit_id/threats/:threat_id/invariants
pub async fn create_invariant(
  State(state): State<AppState>,
  Path((audit_id, threat_id)): Path<(String, String)>,
  Json(payload): Json<CreateInvariantRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let threat_id = parse_path_id(&threat_id, TopicKind::AttackVector)?;
  println!(
    "POST /api/v1/audits/{}/threats/{}/invariants",
    audit_id, threat_id
  );

  let threat_topic = topic::new_attack_vector_topic(threat_id as i32);

  // Invariants inherit severity from their parent threat (may be None)
  let severity = {
    let ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in create_invariant: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
    match audit_data.topic_metadata.get(&threat_topic) {
      Some(core::TopicMetadata::ThreatTopic { severity, .. }) => *severity,
      _ => return Err(StatusCode::NOT_FOUND),
    }
  };

  let row = db::create_invariant(
    &state.db,
    threat_id,
    &payload.description,
    payload.author_id,
    severity.map(|s| s.as_str()),
  )
  .await
  .map_err(|e| {
    eprintln!("create_invariant failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let inv_topic = topic::new_invariant_topic(row.id as i32);

  let invariant = core::Invariant {
    source_topics: Vec::new(),
  };

  let metadata = core::TopicMetadata::InvariantTopic {
    topic: inv_topic.clone(),
    description: row.description,
    threat_topic: threat_topic.clone(),
    author_id: row.author_id,
    created_at: row.created_at,
    severity,
  };

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in create_invariant: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let threat = audit_data
    .threats
    .get_mut(&threat_topic)
    .ok_or(StatusCode::NOT_FOUND)?;
  threat.invariant_topics.push(inv_topic.clone());

  audit_data.invariants.insert(inv_topic.clone(), invariant);
  audit_data
    .topic_metadata
    .insert(inv_topic.clone(), metadata.clone());

  crate::core::rebuild_feature_context(audit_data);

  let response = topic_metadata_to_response(&inv_topic, &metadata);
  Ok(Json(response))
}

/// DELETE /api/v1/audits/:audit_id/threats/:threat_id/invariants/:invariant_id
pub async fn delete_invariant(
  State(state): State<AppState>,
  Path((audit_id, threat_id, invariant_id)): Path<(String, String, String)>,
) -> Result<StatusCode, StatusCode> {
  let threat_id = parse_path_id(&threat_id, TopicKind::AttackVector)?;
  let invariant_id = parse_path_id(&invariant_id, TopicKind::Invariant)?;
  println!(
    "DELETE /api/v1/audits/{}/threats/{}/invariants/{}",
    audit_id, threat_id, invariant_id
  );

  db::delete_invariant(&state.db, invariant_id)
    .await
    .map_err(|e| {
      eprintln!("delete_invariant failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  let inv_topic = topic::new_invariant_topic(invariant_id as i32);
  let threat_topic = topic::new_attack_vector_topic(threat_id as i32);

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in delete_invariant: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    if let Some(threat) = audit_data.threats.get_mut(&threat_topic) {
      threat.invariant_topics.retain(|t| t != &inv_topic);
    }

    audit_data.invariants.remove(&inv_topic);
    audit_data.topic_metadata.remove(&inv_topic);
    audit_data.topic_context.remove(&inv_topic);

    crate::core::rebuild_feature_context(audit_data);
  }

  Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/audits/:audit_id/invariants/:invariant_id
pub async fn get_invariant(
  State(state): State<AppState>,
  Path((audit_id, invariant_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let invariant_id = parse_path_id(&invariant_id, TopicKind::Invariant)?;
  println!(
    "GET /api/v1/audits/{}/invariants/{}",
    audit_id, invariant_id
  );

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_invariant: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let inv_topic = topic::new_invariant_topic(invariant_id as i32);
  let metadata = audit_data
    .topic_metadata
    .get(&inv_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  Ok(Json(topic_metadata_to_response(&inv_topic, metadata)))
}

/// POST /api/v1/audits/:audit_id/invariants/:invariant_id/source_topics
pub async fn add_invariant_source_topic(
  State(state): State<AppState>,
  Path((audit_id, invariant_id)): Path<(String, String)>,
  Json(payload): Json<AddSourceTopicRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let invariant_id = parse_path_id(&invariant_id, TopicKind::Invariant)?;
  println!(
    "POST /api/v1/audits/{}/invariants/{}/source_topics",
    audit_id, invariant_id
  );

  db::add_invariant_source_topic(&state.db, invariant_id, &payload.topic_id)
    .await
    .map_err(|e| {
      eprintln!("add_invariant_source_topic failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in add_invariant_source_topic: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let inv_topic = topic::new_invariant_topic(invariant_id as i32);
  let invariant = audit_data
    .invariants
    .get_mut(&inv_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  let new_topic = topic::new_topic(&payload.topic_id);
  if !invariant.source_topics.contains(&new_topic) {
    invariant.source_topics.push(new_topic);
  }

  let metadata = audit_data
    .topic_metadata
    .get(&inv_topic)
    .ok_or(StatusCode::NOT_FOUND)?;
  let response = topic_metadata_to_response(&inv_topic, metadata);
  Ok(Json(response))
}

/// DELETE /api/v1/audits/:audit_id/invariants/:invariant_id/source_topics/:topic_id
pub async fn remove_invariant_source_topic(
  State(state): State<AppState>,
  Path((audit_id, invariant_id, topic_id)): Path<(String, String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let invariant_id = parse_path_id(&invariant_id, TopicKind::Invariant)?;
  println!(
    "DELETE /api/v1/audits/{}/invariants/{}/source_topics/{}",
    audit_id, invariant_id, topic_id
  );

  db::remove_invariant_source_topic(&state.db, invariant_id, &topic_id)
    .await
    .map_err(|e| {
      eprintln!("remove_invariant_source_topic failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in remove_invariant_source_topic: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let inv_topic = topic::new_invariant_topic(invariant_id as i32);
  let invariant = audit_data
    .invariants
    .get_mut(&inv_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  let remove_topic = topic::new_topic(&topic_id);
  invariant.source_topics.retain(|t| t != &remove_topic);

  let metadata = audit_data
    .topic_metadata
    .get(&inv_topic)
    .ok_or(StatusCode::NOT_FOUND)?;
  let response = topic_metadata_to_response(&inv_topic, metadata);
  Ok(Json(response))
}

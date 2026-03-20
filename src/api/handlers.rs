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

/// Response for ThreatTopic metadata
#[derive(Debug, Serialize)]
pub struct ThreatTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub feature_topic: String,
  pub author_id: i64,
  pub created_at: String,
  pub severity: String,
}

/// Response for InvariantTopic metadata
#[derive(Debug, Serialize)]
pub struct InvariantTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub threat_topic: String,
  pub author_id: i64,
  pub created_at: String,
  pub severity: String,
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

    crate::core::TopicMetadata::ThreatTopic {
      description,
      feature_topic,
      author_id,
      created_at,
      severity,
      ..
    } => TopicMetadataResponse::Threat(ThreatTopicResponse {
      topic_id: topic.id.clone(),
      description: description.clone(),
      feature_topic: feature_topic.id.clone(),
      author_id: *author_id,
      created_at: created_at.clone(),
      severity: severity.as_str().to_string(),
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
      severity: severity.as_str().to_string(),
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
  Path((audit_id, feature_id)): Path<(String, i64)>,
) -> Result<Json<Vec<String>>, StatusCode> {
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

/// GET /api/v1/audits/:audit_id/features/:feature_id/threats
pub async fn get_feature_threats(
  State(state): State<AppState>,
  Path((audit_id, feature_id)): Path<(String, i64)>,
) -> Result<Json<Vec<String>>, StatusCode> {
  println!(
    "GET /api/v1/audits/{}/features/{}/threats",
    audit_id, feature_id
  );

  let feature_topic = topic::new_feature_topic(feature_id as i32);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_feature_threats: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let feature = audit_data
    .features
    .get(&feature_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  let ids: Vec<String> =
    feature.threat_topics.iter().map(|t| t.id.clone()).collect();

  Ok(Json(ids))
}

/// GET /api/v1/audits/:audit_id/threats/:threat_id/invariants
pub async fn get_threat_invariants(
  State(state): State<AppState>,
  Path((audit_id, threat_id)): Path<(String, i64)>,
) -> Result<Json<Vec<String>>, StatusCode> {
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
pub async fn build_features(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
  println!("POST /api/v1/audits/{}/features/build", audit_id);

  // Render per-file documentation JSON while holding the lock, then release it
  let documentation_files = {
    let ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in build_features: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
    crate::collaborator::agent::task::render_documentation_files(audit_data)
  };

  // Run the iterative LLM task (lock is released)
  let parsed =
    crate::collaborator::agent::task::build_features_from_documentation(
      &documentation_files,
    )
    .await
    .map_err(|e| {
      eprintln!("build_features failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  // Persist to database: clear old features, insert new ones
  db::delete_all_features_for_audit(&state.db, &audit_id)
    .await
    .map_err(|e| {
      eprintln!("delete_all_features_for_audit failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  for (feat_topic, feature) in &parsed.features {
    // Get name/description from topic_metadata
    let (name, description) = match parsed.topic_metadata.get(feat_topic) {
      Some(core::TopicMetadata::FeatureTopic {
        name, description, ..
      }) => (name.as_str(), description.as_str()),
      _ => continue,
    };
    let row = db::create_feature(
      &state.db,
      &audit_id,
      name,
      description,
      AUTHOR_AGENT, // agent-created
    )
    .await
    .map_err(|e| {
      eprintln!("create_feature failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    // Persist requirements for this feature
    for req_topic in &feature.requirement_topics {
      // Get description from topic_metadata
      let req_desc = match parsed.topic_metadata.get(req_topic) {
        Some(core::TopicMetadata::RequirementTopic { description, .. }) => {
          description.as_str()
        }
        _ => continue,
      };
      let req_row = db::create_requirement(&state.db, row.id, req_desc, 0)
        .await
        .map_err(|e| {
          eprintln!("create_requirement failed: {}", e);
          StatusCode::INTERNAL_SERVER_ERROR
        })?;
      if let Some(req) = parsed.requirements.get(req_topic) {
        for dt in &req.documentation_topics {
          let _ = db::add_requirement_documentation_topic(
            &state.db,
            req_row.id,
            dt.id(),
          )
          .await;
        }
        for st in &req.source_topics {
          let _ =
            db::add_requirement_source_topic(&state.db, req_row.id, st.id())
              .await;
        }
      }
    }
  }

  // Store in audit data and build response
  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in build_features (store): {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  // Remove old feature/requirement/threat/invariant TopicMetadata entries
  audit_data.topic_metadata.retain(|_, m| {
    !matches!(
      m,
      core::TopicMetadata::FeatureTopic { .. }
        | core::TopicMetadata::RequirementTopic { .. }
        | core::TopicMetadata::ThreatTopic { .. }
        | core::TopicMetadata::InvariantTopic { .. }
    )
  });

  // Insert TopicMetadata from parsed result
  audit_data.topic_metadata.extend(parsed.topic_metadata);

  audit_data.features = parsed.features;
  audit_data.requirements = parsed.requirements;
  audit_data.threats.clear();
  audit_data.invariants.clear();
  crate::core::rebuild_feature_context(audit_data);

  Ok(StatusCode::OK)
}

/// Trigger the agent to build threats and invariants from existing features.
pub async fn build_threats(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
  println!("POST /api/v1/audits/{}/threats/build", audit_id);

  // Render per-feature JSON while holding the lock, then release it
  let (feature_entries, security_notes): (
    Vec<(topic::Topic, String)>,
    Option<String>,
  ) = {
    let ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in build_threats: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    let entries = audit_data
      .features
      .iter()
      .filter_map(|(ft, feature)| {
        let json =
          crate::collaborator::agent::context::render_feature_to_json(
            ft, feature, audit_data,
          )?;
        Some((ft.clone(), json))
      })
      .collect();

    (entries, audit_data.security_notes.clone())
  };

  if feature_entries.is_empty() {
    return Err(StatusCode::NOT_FOUND);
  }

  // Fire one LLM call per feature concurrently
  let security_notes = std::sync::Arc::new(security_notes);
  let mut handles = Vec::new();
  for (i, (ft, json)) in feature_entries.into_iter().enumerate() {
    let threat_start = (i * 1000) as i32;
    let invariant_start = (i * 1000) as i32;
    let notes = security_notes.clone();
    handles.push(tokio::spawn(async move {
      let result = crate::collaborator::agent::task::build_threats_for_feature(
        &ft,
        &json,
        threat_start,
        invariant_start,
        notes.as_deref(),
      )
      .await;
      (ft, result)
    }));
  }

  // Collect results, log errors but continue with successes
  let mut all_parsed = Vec::new();
  for handle in handles {
    match handle.await {
      Ok((_, Ok(parsed))) => all_parsed.push(parsed),
      Ok((ft, Err(e))) => {
        eprintln!("build_threats failed for {}: {}", ft.id(), e);
      }
      Err(e) => {
        eprintln!("build_threats task panicked: {}", e);
      }
    }
  }

  // Delete existing threats/invariants from the database
  db::delete_all_threats_for_audit(&state.db, &audit_id)
    .await
    .map_err(|e| {
      eprintln!("delete_all_threats_for_audit failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  // Persist new threats and invariants; build in-memory state from DB row IDs
  let mut new_threats = std::collections::BTreeMap::new();
  let mut new_invariants = std::collections::BTreeMap::new();
  let mut new_topic_metadata = std::collections::BTreeMap::new();
  let mut new_feature_threats: std::collections::BTreeMap<
    topic::Topic,
    Vec<topic::Topic>,
  > = std::collections::BTreeMap::new();

  for parsed in &all_parsed {
    for (feat_topic, threat_topics) in &parsed.feature_threat_topics {
      let feature_id = feat_topic.numeric_id().ok_or_else(|| {
        eprintln!("Invalid feature topic: {}", feat_topic.id());
        StatusCode::INTERNAL_SERVER_ERROR
      })?;

      for tt in threat_topics {
        let (description, severity) = match parsed.topic_metadata.get(tt) {
          Some(core::TopicMetadata::ThreatTopic {
            description,
            severity,
            ..
          }) => (description.clone(), *severity),
          _ => continue,
        };

        let threat_row = db::create_threat(
          &state.db,
          feature_id,
          &description,
          AUTHOR_AGENT,
          severity.as_str(),
        )
        .await
        .map_err(|e| {
          eprintln!("create_threat failed: {}", e);
          StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let real_threat_topic =
          topic::new_attack_vector_topic(threat_row.id as i32);

        let mut real_invariant_topics = Vec::new();
        if let Some(threat) = parsed.threats.get(tt) {
          for inv_topic in &threat.invariant_topics {
            let inv_desc = match parsed.topic_metadata.get(inv_topic) {
              Some(core::TopicMetadata::InvariantTopic {
                description, ..
              }) => description.clone(),
              _ => continue,
            };

            let inv_row = db::create_invariant(
              &state.db,
              threat_row.id,
              &inv_desc,
              AUTHOR_AGENT,
              severity.as_str(),
            )
            .await
            .map_err(|e| {
              eprintln!("create_invariant failed: {}", e);
              StatusCode::INTERNAL_SERVER_ERROR
            })?;

            let real_inv_topic = topic::new_invariant_topic(inv_row.id as i32);

            new_topic_metadata.insert(
              real_inv_topic.clone(),
              core::TopicMetadata::InvariantTopic {
                topic: real_inv_topic.clone(),
                description: inv_desc,
                threat_topic: real_threat_topic.clone(),
                author_id: AUTHOR_AGENT,
                created_at: inv_row.created_at,
                severity,
              },
            );
            new_invariants.insert(
              real_inv_topic.clone(),
              core::Invariant {
                source_topics: Vec::new(),
              },
            );
            real_invariant_topics.push(real_inv_topic);
          }
        }

        new_topic_metadata.insert(
          real_threat_topic.clone(),
          core::TopicMetadata::ThreatTopic {
            topic: real_threat_topic.clone(),
            description,
            feature_topic: feat_topic.clone(),
            author_id: AUTHOR_AGENT,
            created_at: threat_row.created_at,
            severity,
          },
        );
        new_threats.insert(
          real_threat_topic.clone(),
          core::Threat {
            invariant_topics: real_invariant_topics,
          },
        );
        new_feature_threats
          .entry(feat_topic.clone())
          .or_default()
          .push(real_threat_topic);
      }
    }
  }

  // Update in-memory audit data and extract review input (if security notes exist)
  let review_input = {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in build_threats (store): {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    // Clear old threat/invariant metadata
    audit_data.topic_metadata.retain(|_, m| {
      !matches!(
        m,
        core::TopicMetadata::ThreatTopic { .. }
          | core::TopicMetadata::InvariantTopic { .. }
      )
    });

    audit_data.topic_metadata.extend(new_topic_metadata);
    audit_data.threats = new_threats;
    audit_data.invariants = new_invariants;

    // Update feature threat_topics
    for feature in audit_data.features.values_mut() {
      feature.threat_topics.clear();
    }
    for (feat_topic, threat_topics) in new_feature_threats {
      if let Some(feature) = audit_data.features.get_mut(&feat_topic) {
        feature.threat_topics = threat_topics;
      }
    }

    crate::core::rebuild_feature_context(audit_data);

    // --- Security coverage review pass ---
    // Extract what we need while holding the lock, then release it.
    if let Some(ref notes) = *security_notes {
      let features_json =
        crate::collaborator::agent::context::render_all_features_with_threats(
          audit_data,
        );

      let review_threat_start = audit_data
        .threats
        .keys()
        .filter_map(|t| t.numeric_id())
        .max()
        .unwrap_or(0) as i32;
      let review_inv_start = audit_data
        .invariants
        .keys()
        .filter_map(|t| t.numeric_id())
        .max()
        .unwrap_or(0) as i32;

      Some((
        notes.clone(),
        features_json,
        review_threat_start,
        review_inv_start,
      ))
    } else {
      None
    }
  }; // ctx dropped here

  if let Some((notes, features_json, review_threat_start, review_inv_start)) =
    review_input
  {
    let review_results =
      crate::collaborator::agent::task::review_security_coverage(
        &notes,
        &features_json,
        review_threat_start,
        review_inv_start,
      )
      .await
      .map_err(|e| {
        eprintln!("review_security_coverage failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
      })?;

    // Persist additional threats/invariants from the review
    if !review_results.is_empty() {
      let mut review_threats = std::collections::BTreeMap::new();
      let mut review_invariants = std::collections::BTreeMap::new();
      let mut review_metadata = std::collections::BTreeMap::new();
      let mut review_feature_threats: std::collections::BTreeMap<
        topic::Topic,
        Vec<topic::Topic>,
      > = std::collections::BTreeMap::new();

      for parsed in &review_results {
        for (feat_topic, threat_topics) in &parsed.feature_threat_topics {
          let feature_id = feat_topic.numeric_id().ok_or_else(|| {
            eprintln!("Invalid feature topic in review: {}", feat_topic.id());
            StatusCode::INTERNAL_SERVER_ERROR
          })?;

          for tt in threat_topics {
            let (description, severity) = match parsed.topic_metadata.get(tt) {
              Some(core::TopicMetadata::ThreatTopic {
                description,
                severity,
                ..
              }) => (description.clone(), *severity),
              _ => continue,
            };

            let threat_row = db::create_threat(
              &state.db,
              feature_id,
              &description,
              AUTHOR_AGENT,
              severity.as_str(),
            )
            .await
            .map_err(|e| {
              eprintln!("create_threat (review) failed: {}", e);
              StatusCode::INTERNAL_SERVER_ERROR
            })?;

            let real_threat_topic =
              topic::new_attack_vector_topic(threat_row.id as i32);

            let mut real_invariant_topics = Vec::new();
            if let Some(threat) = parsed.threats.get(tt) {
              for inv_topic in &threat.invariant_topics {
                let inv_desc = match parsed.topic_metadata.get(inv_topic) {
                  Some(core::TopicMetadata::InvariantTopic {
                    description,
                    ..
                  }) => description.clone(),
                  _ => continue,
                };

                let inv_row = db::create_invariant(
                  &state.db,
                  threat_row.id,
                  &inv_desc,
                  AUTHOR_AGENT,
                  severity.as_str(),
                )
                .await
                .map_err(|e| {
                  eprintln!("create_invariant (review) failed: {}", e);
                  StatusCode::INTERNAL_SERVER_ERROR
                })?;

                let real_inv_topic =
                  topic::new_invariant_topic(inv_row.id as i32);

                review_metadata.insert(
                  real_inv_topic.clone(),
                  core::TopicMetadata::InvariantTopic {
                    topic: real_inv_topic.clone(),
                    description: inv_desc,
                    threat_topic: real_threat_topic.clone(),
                    author_id: AUTHOR_AGENT,
                    created_at: inv_row.created_at,
                    severity,
                  },
                );
                review_invariants.insert(
                  real_inv_topic.clone(),
                  core::Invariant {
                    source_topics: Vec::new(),
                  },
                );
                real_invariant_topics.push(real_inv_topic);
              }
            }

            review_metadata.insert(
              real_threat_topic.clone(),
              core::TopicMetadata::ThreatTopic {
                topic: real_threat_topic.clone(),
                description,
                feature_topic: feat_topic.clone(),
                author_id: AUTHOR_AGENT,
                created_at: threat_row.created_at,
                severity,
              },
            );
            review_threats.insert(
              real_threat_topic.clone(),
              core::Threat {
                invariant_topics: real_invariant_topics,
              },
            );
            review_feature_threats
              .entry(feat_topic.clone())
              .or_default()
              .push(real_threat_topic);
          }
        }
      }

      // Merge review results into in-memory state
      let mut ctx = state.data_context.lock().map_err(|e| {
        eprintln!("Mutex poisoned in build_threats (review store): {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
      })?;
      let audit_data =
        ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

      audit_data.topic_metadata.extend(review_metadata);
      audit_data.threats.extend(review_threats);
      audit_data.invariants.extend(review_invariants);

      for (feat_topic, threat_topics) in review_feature_threats {
        if let Some(feature) = audit_data.features.get_mut(&feat_topic) {
          feature.threat_topics.extend(threat_topics);
        }
      }

      crate::core::rebuild_feature_context(audit_data);
    }
  }

  Ok(StatusCode::OK)
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
    threat_topics: Vec::new(),
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
  Path((audit_id, feature_id)): Path<(String, i32)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!("GET /api/v1/audits/{}/features/{}", audit_id, feature_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_feature: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let feature_topic = topic::new_feature_topic(feature_id);
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
  Path((audit_id, requirement_id)): Path<(String, i64)>,
  Json(payload): Json<AddFeatureTopicRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
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
  Path((audit_id, requirement_id, topic_id)): Path<(String, i64, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
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
/// - Source topics (N-prefixed): reverse-lookups requirements with this source_topic
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
      for (req_topic, req) in &audit_data.requirements {
        if req.source_topics.contains(&t)
          || req.documentation_topics.contains(&t)
        {
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
/// - Other topics: reverse-lookup requirements with this source_topic
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

  // Resolve all input topic IDs to requirement topics
  let mut requirement_topics: Vec<topic::Topic> = Vec::new();
  for id in &payload.feature_topics {
    let t = new_topic(id);
    match t.kind() {
      Some(TopicKind::Feature) => {
        // Collect all requirements for this feature
        if let Some(feature) = audit_data.features.get(&t) {
          for rt in &feature.requirement_topics {
            if !requirement_topics.contains(rt) {
              requirement_topics.push(rt.clone());
            }
          }
        }
      }
      Some(TopicKind::Requirement) => {
        if !requirement_topics.contains(&t) {
          requirement_topics.push(t);
        }
      }
      _ => {
        // Reverse lookup: find requirements that have this topic as a source_topic
        for (req_topic, req) in &audit_data.requirements {
          if req.source_topics.contains(&t) {
            if !requirement_topics.contains(req_topic) {
              requirement_topics.push(req_topic.clone());
            }
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

  let html = super::topic_view::build_documentation_panel(
    &requirement_topics,
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
  Path((audit_id, feature_id)): Path<(String, i64)>,
  Json(payload): Json<CreateRequirementRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
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
    source_topics: Vec::new(),
  };

  let metadata = core::TopicMetadata::RequirementTopic {
    topic: req_topic.clone(),
    description: row.description,
    feature_topic: feature_topic.clone(),
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
  Path((audit_id, feature_id, requirement_id)): Path<(String, i64, i64)>,
) -> Result<StatusCode, StatusCode> {
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
  Path((audit_id, requirement_id)): Path<(String, i32)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!(
    "GET /api/v1/audits/{}/requirements/{}",
    audit_id, requirement_id
  );

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_requirement: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let req_topic = topic::new_requirement_topic(requirement_id);
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

/// POST /api/v1/audits/:audit_id/requirements/:requirement_id/source_topics
/// Links a source topic to a requirement.
pub async fn add_requirement_source_topic(
  State(state): State<AppState>,
  Path((audit_id, requirement_id)): Path<(String, i64)>,
  Json(payload): Json<AddSourceTopicRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!(
    "POST /api/v1/audits/{}/requirements/{}/source_topics",
    audit_id, requirement_id
  );

  db::add_requirement_source_topic(
    &state.db,
    requirement_id,
    &payload.topic_id,
  )
  .await
  .map_err(|e| {
    eprintln!("add_requirement_source_topic failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in add_requirement_source_topic: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let req_topic = topic::new_requirement_topic(requirement_id as i32);
  let requirement = audit_data
    .requirements
    .get_mut(&req_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  let new_topic = topic::new_topic(&payload.topic_id);
  if !requirement.source_topics.contains(&new_topic) {
    requirement.source_topics.push(new_topic);
  }

  let metadata = audit_data
    .topic_metadata
    .get(&req_topic)
    .ok_or(StatusCode::NOT_FOUND)?;
  let response = topic_metadata_to_response(&req_topic, metadata);
  Ok(Json(response))
}

/// DELETE /api/v1/audits/:audit_id/requirements/:requirement_id/source_topics/:topic_id
/// Unlinks a source topic from a requirement.
pub async fn remove_requirement_source_topic(
  State(state): State<AppState>,
  Path((audit_id, requirement_id, topic_id)): Path<(String, i64, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!(
    "DELETE /api/v1/audits/{}/requirements/{}/source_topics/{}",
    audit_id, requirement_id, topic_id
  );

  db::remove_requirement_source_topic(&state.db, requirement_id, &topic_id)
    .await
    .map_err(|e| {
      eprintln!("remove_requirement_source_topic failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in remove_requirement_source_topic: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let req_topic = topic::new_requirement_topic(requirement_id as i32);
  let requirement = audit_data
    .requirements
    .get_mut(&req_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  let remove_topic = topic::new_topic(&topic_id);
  requirement.source_topics.retain(|t| t != &remove_topic);

  let metadata = audit_data
    .topic_metadata
    .get(&req_topic)
    .ok_or(StatusCode::NOT_FOUND)?;
  let response = topic_metadata_to_response(&req_topic, metadata);
  Ok(Json(response))
}

// ============================================
// Threat routes
// ============================================

#[derive(Debug, Deserialize)]
pub struct CreateThreatRequest {
  pub description: String,
  pub author_id: i64,
  pub severity: String,
}

/// POST /api/v1/audits/:audit_id/features/:feature_id/threats
/// Creates a new threat on a feature.
pub async fn create_threat(
  State(state): State<AppState>,
  Path((audit_id, feature_id)): Path<(String, i64)>,
  Json(payload): Json<CreateThreatRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!(
    "POST /api/v1/audits/{}/features/{}/threats",
    audit_id, feature_id
  );

  let severity = core::ThreatSeverity::from_str(&payload.severity)
    .ok_or(StatusCode::BAD_REQUEST)?;

  let row = db::create_threat(
    &state.db,
    feature_id,
    &payload.description,
    payload.author_id,
    severity.as_str(),
  )
  .await
  .map_err(|e| {
    eprintln!("create_threat failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let feature_topic = topic::new_feature_topic(feature_id as i32);
  let threat_topic = topic::new_attack_vector_topic(row.id as i32);

  let threat = core::Threat {
    invariant_topics: Vec::new(),
  };

  let metadata = core::TopicMetadata::ThreatTopic {
    topic: threat_topic.clone(),
    description: row.description,
    feature_topic: feature_topic.clone(),
    author_id: row.author_id,
    created_at: row.created_at,
    severity,
  };

  let mut ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in create_threat: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let feature = audit_data
    .features
    .get_mut(&feature_topic)
    .ok_or(StatusCode::NOT_FOUND)?;
  feature.threat_topics.push(threat_topic.clone());

  audit_data.threats.insert(threat_topic.clone(), threat);
  audit_data
    .topic_metadata
    .insert(threat_topic.clone(), metadata.clone());

  crate::core::rebuild_feature_context(audit_data);

  let response = topic_metadata_to_response(&threat_topic, &metadata);
  Ok(Json(response))
}

/// DELETE /api/v1/audits/:audit_id/features/:feature_id/threats/:threat_id
pub async fn delete_threat(
  State(state): State<AppState>,
  Path((audit_id, feature_id, threat_id)): Path<(String, i64, i64)>,
) -> Result<StatusCode, StatusCode> {
  println!(
    "DELETE /api/v1/audits/{}/features/{}/threats/{}",
    audit_id, feature_id, threat_id
  );

  db::delete_threat(&state.db, threat_id).await.map_err(|e| {
    eprintln!("delete_threat failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let threat_topic = topic::new_attack_vector_topic(threat_id as i32);
  let feature_topic = topic::new_feature_topic(feature_id as i32);

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      eprintln!("Mutex poisoned in delete_threat: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    if let Some(feature) = audit_data.features.get_mut(&feature_topic) {
      feature.threat_topics.retain(|t| t != &threat_topic);
    }

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
  Path((audit_id, threat_id)): Path<(String, i32)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!("GET /api/v1/audits/{}/threats/{}", audit_id, threat_id);

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_threat: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let threat_topic = topic::new_attack_vector_topic(threat_id);
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
  Path((audit_id, threat_id)): Path<(String, i64)>,
  Json(payload): Json<CreateInvariantRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!(
    "POST /api/v1/audits/{}/threats/{}/invariants",
    audit_id, threat_id
  );

  let threat_topic = topic::new_attack_vector_topic(threat_id as i32);

  // Invariants always inherit severity from their parent threat
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
    severity.as_str(),
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
  Path((audit_id, threat_id, invariant_id)): Path<(String, i64, i64)>,
) -> Result<StatusCode, StatusCode> {
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
  Path((audit_id, invariant_id)): Path<(String, i32)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  println!(
    "GET /api/v1/audits/{}/invariants/{}",
    audit_id, invariant_id
  );

  let ctx = state.data_context.lock().map_err(|e| {
    eprintln!("Mutex poisoned in get_invariant: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let inv_topic = topic::new_invariant_topic(invariant_id);
  let metadata = audit_data
    .topic_metadata
    .get(&inv_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  Ok(Json(topic_metadata_to_response(&inv_topic, metadata)))
}

/// POST /api/v1/audits/:audit_id/invariants/:invariant_id/source_topics
pub async fn add_invariant_source_topic(
  State(state): State<AppState>,
  Path((audit_id, invariant_id)): Path<(String, i64)>,
  Json(payload): Json<AddSourceTopicRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
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
  Path((audit_id, invariant_id, topic_id)): Path<(String, i64, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
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

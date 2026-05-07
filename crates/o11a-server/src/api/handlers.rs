use axum::{
  Json,
  extract::{Path, Query, State},
  http::StatusCode,
  response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use o11a_core::analysis_artifact::{self, ArtifactError};
use o11a_core::collaborator::{db, models::*};
use o11a_core::domain::{
  self,
  topic::{self, new_topic},
};
use o11a_core::feature_lookup::features_for_topic;
use o11a_core::report::{self, AuditReport};
use o11a_core::state::AppState;

use crate::api::error::artifact_error_response;

// Health check handler
pub async fn health_check() -> StatusCode {
  tracing::debug!("GET /health");
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
  tracing::debug!("GET /api/v1/audits/{}/data-context", audit_id);
  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_data_context: {}", e);
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
  tracing::debug!("GET /api/v1/chats");
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
  tracing::debug!("POST /api/v1/chats");
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
  tracing::debug!("GET /api/v1/audits/{}/boundaries", audit_id);
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
  tracing::debug!("GET /api/v1/audits/{}/in_scope_files", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_in_scope_files: {}", e);
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
  tracing::debug!("GET /api/v1/audits");
  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in list_audits: {}", e);
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

// Create a new audit by loading its pre-built artifact and report.
pub async fn create_audit(
  State(state): State<AppState>,
  Json(payload): Json<CreateAuditRequest>,
) -> Result<Json<CreateAuditResponse>, StatusCode> {
  tracing::debug!("POST /api/v1/audits");
  let project_root = std::path::Path::new(&payload.project_root);
  let artifact_path = project_root.join("o11a").join("audit.analysis.bin");
  let report_path = project_root.join("o11a").join("audit.json");

  let artifact =
    analysis_artifact::read_artifact(&artifact_path).map_err(|e| {
      tracing::error!(
        "create_audit: failed to read {}: {}",
        artifact_path.display(),
        e
      );
      artifact_error_response(e).0
    })?;

  if artifact.audit_id != payload.audit_id {
    let err = ArtifactError::AuditIdMismatch {
      expected: payload.audit_id.clone(),
      found: artifact.audit_id.clone(),
    };
    tracing::warn!("create_audit: {}", err);
    return Err(artifact_error_response(err).0);
  }

  let report_body = std::fs::read_to_string(&report_path).map_err(|e| {
    tracing::error!(
      "create_audit: failed to read {}: {}",
      report_path.display(),
      e
    );
    StatusCode::BAD_REQUEST
  })?;
  let report: AuditReport =
    serde_json::from_str(&report_body).map_err(|e| {
      tracing::error!("create_audit: failed to parse audit.json: {}", e);
      StatusCode::BAD_REQUEST
    })?;

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in create_audit: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    ctx.create_audit(
      payload.audit_id.clone(),
      artifact.payload.audit_name.clone(),
      artifact.payload.in_scope_files.clone(),
      artifact.payload.security_notes.clone(),
    );
    let audit_data = ctx.get_audit_mut(&payload.audit_id).ok_or_else(|| {
      tracing::warn!(
        "create_audit: audit '{}' missing after create_audit",
        payload.audit_id
      );
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    analysis_artifact::apply_snapshot(audit_data, artifact.payload);

    if let Err(e) = report::apply_report(&payload.audit_id, audit_data, &report)
    {
      tracing::error!("create_audit: failed to apply audit report: {}", e);
      return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    domain::rebuild_feature_context(audit_data);
  }

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
  tracing::debug!("DELETE /api/v1/audits/{}", audit_id);
  let mut ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in delete_audit: {}", e);
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
  tracing::debug!("GET /api/v1/audits/{}/documents", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_documents: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let mut documents = Vec::new();

  // Iterate through all topic metadata and filter for documentation roots
  for (topic, metadata) in &audit_data.topic_metadata {
    if matches!(
      metadata,
      o11a_core::domain::TopicMetadata::DocumentationTopic { .. }
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
  tracing::debug!("GET /api/v1/audits/{}/contracts", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_contracts: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let mut contracts = Vec::new();

  // Iterate through all topic metadata and filter for contracts in scope files
  for (topic, metadata) in &audit_data.topic_metadata {
    let is_contract = match metadata {
      o11a_core::domain::TopicMetadata::NamedTopic { kind, .. } => {
        matches!(kind, o11a_core::domain::NamedTopicKind::Contract(_))
      }
      o11a_core::domain::TopicMetadata::UnnamedTopic { .. }
      | o11a_core::domain::TopicMetadata::ControlFlow { .. }
      | o11a_core::domain::TopicMetadata::TitledTopic { .. }
      | o11a_core::domain::TopicMetadata::CommentTopic { .. }
      | o11a_core::domain::TopicMetadata::FeatureTopic { .. }
      | o11a_core::domain::TopicMetadata::RequirementTopic { .. }
      | o11a_core::domain::TopicMetadata::BehaviorTopic { .. }
      | o11a_core::domain::TopicMetadata::FunctionalSemanticTopic { .. }
      | o11a_core::domain::TopicMetadata::FunctionalPurposeTopic { .. }
      | o11a_core::domain::TopicMetadata::PlacementRationaleTopic { .. }
      | o11a_core::domain::TopicMetadata::ThreatTopic { .. }
      | o11a_core::domain::TopicMetadata::InvariantTopic { .. }
      | o11a_core::domain::TopicMetadata::DocumentationTopic { .. } => false,
    };

    if is_contract {
      // Check if the contract is in an in-scope file
      let is_in_scope = match metadata.scope() {
        o11a_core::domain::Scope::Container { container } => {
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
  tracing::debug!("GET /api/v1/audits/{}/qualified_names", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_qualified_names: {}", e);
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

// Get the structural delimiter data for a topic. Returns `null` when the
// topic does not map to a Solidity control-flow node with delimiters.
// Frontend consumers render the opening/closing presentation from this data.
pub async fn get_delimiter(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<
  Json<Option<o11a_core::solidity::delimiter::DelimiterInfo>>,
  StatusCode,
> {
  tracing::debug!("GET /api/v1/audits/{}/delimiter/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_delimiter: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let topic = new_topic(&topic_id);

  let node = audit_data.nodes.get(&topic).ok_or_else(|| {
    tracing::warn!("Topic '{}' not found in audit '{}'", topic_id, audit_id);
    StatusCode::NOT_FOUND
  })?;

  let info = match node {
    domain::Node::Solidity(solidity_node) => {
      o11a_core::solidity::delimiter::delimiter_info_for_node(solidity_node)
    }
    domain::Node::Documentation(_)
    | domain::Node::Comment(_)
    | domain::Node::Rust(_) => None,
  };

  Ok(Json(info))
}

// Topic metadata response

pub use o11a_core::collaborator::scope_info::ScopeInfo;

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
  #[serde(rename = "author_id")]
  pub author: Author,
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
  #[serde(rename = "author_id")]
  pub author: Author,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub created_at: Option<String>,
}

/// Response for RequirementTopic metadata
#[derive(Debug, Serialize)]
pub struct RequirementTopicResponse {
  pub topic_id: String,
  pub description: String,
  #[serde(rename = "author_id")]
  pub author: Author,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub created_at: Option<String>,
}

/// Response for BehaviorTopic metadata
#[derive(Debug, Serialize)]
pub struct BehaviorTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub member_topic: String,
  #[serde(rename = "author_id")]
  pub author: Author,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub created_at: Option<String>,
}

/// Response for FunctionalSemanticTopic metadata
#[derive(Debug, Serialize)]
pub struct SemanticTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub declaration_topic: String,
  pub documentation_topics: Vec<String>,
  #[serde(rename = "author_id")]
  pub author: Author,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub created_at: Option<String>,
}

/// Response for FunctionalPurposeTopic metadata
#[derive(Debug, Serialize)]
pub struct FunctionalPurposeTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub subject_topic: String,
  #[serde(rename = "author_id")]
  pub author: Author,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub created_at: Option<String>,
}

/// Response for PlacementRationaleTopic metadata
#[derive(Debug, Serialize)]
pub struct PlacementRationaleTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub subject_topic: String,
  #[serde(rename = "author_id")]
  pub author: Author,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub created_at: Option<String>,
}

/// Response for ThreatTopic metadata
#[derive(Debug, Serialize)]
pub struct ThreatTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub subject_topic: String,
  #[serde(rename = "author_id")]
  pub author: Author,
  pub created_at: String,
  pub severity: Option<String>,
}

/// Response for InvariantTopic metadata
#[derive(Debug, Serialize)]
pub struct InvariantTopicResponse {
  pub topic_id: String,
  pub description: String,
  pub threat_topic: String,
  #[serde(rename = "author_id")]
  pub author: Author,
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
  #[serde(rename = "semantic")]
  Semantic(SemanticTopicResponse),
  #[serde(rename = "purpose")]
  Purpose(FunctionalPurposeTopicResponse),
  #[serde(rename = "placement")]
  Placement(PlacementRationaleTopicResponse),
  #[serde(rename = "threat")]
  Threat(ThreatTopicResponse),
  #[serde(rename = "invariant")]
  Invariant(InvariantTopicResponse),
  #[serde(rename = "documentation")]
  Documentation(DocumentationTopicResponse),
}

// Helper function to convert TopicMetadata to TopicMetadataResponse
fn topic_metadata_to_response(
  topic: &o11a_core::domain::topic::Topic,
  metadata: &o11a_core::domain::TopicMetadata,
) -> TopicMetadataResponse {
  let scope_info = ScopeInfo::from_scope(metadata.scope());

  match metadata {
    o11a_core::domain::TopicMetadata::NamedTopic {
      name,
      kind,
      visibility,
      mutations,
      is_mutable,
      ..
    } => {
      // Format the kind and sub_kind for NamedTopic
      let (kind_str, sub_kind) = match kind {
        o11a_core::domain::NamedTopicKind::Contract(contract_kind) => {
          ("Contract".to_string(), Some(format!("{:?}", contract_kind)))
        }
        o11a_core::domain::NamedTopicKind::Function(function_kind) => {
          ("Function".to_string(), Some(format!("{:?}", function_kind)))
        }
        o11a_core::domain::NamedTopicKind::StateVariable(mutability) => (
          "StateVariable".to_string(),
          Some(format!("{:?}", mutability)),
        ),
        kind => (format!("{:?}", kind), None),
      };

      let mutations_response = if *is_mutable {
        Some(mutations.iter().map(|t| t.id()).collect())
      } else {
        None
      };

      TopicMetadataResponse::Named(NamedTopicResponse {
        topic_id: topic.id(),
        name: name.clone(),
        kind: kind_str,
        sub_kind,
        visibility: format!("{:?}", visibility),
        scope: scope_info,
        ancestors: metadata.ancestors().iter().map(|t| t.id()).collect(),
        descendants: metadata.descendants().iter().map(|t| t.id()).collect(),
        relatives: metadata.relatives().iter().map(|t| t.id()).collect(),
        mutations: mutations_response,
      })
    }

    o11a_core::domain::TopicMetadata::TitledTopic { title, kind, .. } => {
      TopicMetadataResponse::Titled(TitledTopicResponse {
        topic_id: topic.id(),
        title: title.clone(),
        kind: format!("{:?}", kind),
        scope: scope_info,
      })
    }

    o11a_core::domain::TopicMetadata::UnnamedTopic { kind, .. } => {
      TopicMetadataResponse::Unnamed(UnnamedTopicResponse {
        topic_id: topic.id(),
        kind: format!("{:?}", kind),
        scope: scope_info,
      })
    }

    o11a_core::domain::TopicMetadata::DocumentationTopic {
      is_technical,
      ..
    } => TopicMetadataResponse::Documentation(DocumentationTopicResponse {
      topic_id: topic.id(),
      scope: scope_info,
      is_technical: *is_technical,
    }),

    o11a_core::domain::TopicMetadata::ControlFlow {
      kind, condition, ..
    } => TopicMetadataResponse::ControlFlow(ControlFlowTopicResponse {
      topic_id: topic.id(),
      kind: format!("{:?}", kind),
      scope: scope_info,
      condition: condition.id(),
    }),

    o11a_core::domain::TopicMetadata::CommentTopic {
      author: author_id,
      comment_type,
      target_topic,
      created_at,
      mentioned_topics,
      ..
    } => TopicMetadataResponse::CommentTopic(CommentTopicResponse {
      topic_id: topic.id(),
      author: *author_id,
      comment_type: comment_type.as_str().to_string(),
      target_topic: target_topic.id(),
      created_at: created_at.clone(),
      scope: scope_info,
      mentioned_topics: mentioned_topics.iter().map(|t| t.id()).collect(),
    }),

    o11a_core::domain::TopicMetadata::FeatureTopic {
      name,
      description,
      author: author_id,
      created_at,
      ..
    } => TopicMetadataResponse::Feature(FeatureTopicResponse {
      topic_id: topic.id(),
      name: name.clone(),
      description: description.clone(),
      author: *author_id,
      created_at: created_at.clone(),
    }),

    o11a_core::domain::TopicMetadata::RequirementTopic {
      description,
      author: author_id,
      created_at,
      ..
    } => TopicMetadataResponse::Requirement(RequirementTopicResponse {
      topic_id: topic.id(),
      description: description.clone(),
      author: *author_id,
      created_at: created_at.clone(),
    }),

    o11a_core::domain::TopicMetadata::BehaviorTopic {
      description,
      member_topic,
      author: author_id,
      created_at,
      ..
    } => TopicMetadataResponse::Behavior(BehaviorTopicResponse {
      topic_id: topic.id(),
      description: description.clone(),
      member_topic: member_topic.id(),
      author: *author_id,
      created_at: created_at.clone(),
    }),

    o11a_core::domain::TopicMetadata::FunctionalSemanticTopic {
      description,
      declaration_topic,
      documentation_topics,
      author: author_id,
      created_at,
      ..
    } => TopicMetadataResponse::Semantic(SemanticTopicResponse {
      topic_id: topic.id(),
      description: description.clone(),
      declaration_topic: declaration_topic.id(),
      documentation_topics: documentation_topics
        .iter()
        .map(|t| t.id())
        .collect(),
      author: *author_id,
      created_at: created_at.clone(),
    }),

    o11a_core::domain::TopicMetadata::FunctionalPurposeTopic {
      description,
      subject_topic,
      author: author_id,
      created_at,
      ..
    } => TopicMetadataResponse::Purpose(FunctionalPurposeTopicResponse {
      topic_id: topic.id(),
      description: description.clone(),
      subject_topic: subject_topic.id(),
      author: *author_id,
      created_at: created_at.clone(),
    }),

    o11a_core::domain::TopicMetadata::PlacementRationaleTopic {
      description,
      subject_topic,
      author: author_id,
      created_at,
      ..
    } => TopicMetadataResponse::Placement(PlacementRationaleTopicResponse {
      topic_id: topic.id(),
      description: description.clone(),
      subject_topic: subject_topic.id(),
      author: *author_id,
      created_at: created_at.clone(),
    }),

    o11a_core::domain::TopicMetadata::ThreatTopic {
      description,
      subject_topic,
      author: author_id,
      created_at,
      severity,
      ..
    } => TopicMetadataResponse::Threat(ThreatTopicResponse {
      topic_id: topic.id(),
      description: description.clone(),
      subject_topic: subject_topic.id(),
      author: *author_id,
      created_at: created_at.clone(),
      severity: severity.map(|s| s.as_str().to_string()),
    }),

    o11a_core::domain::TopicMetadata::InvariantTopic {
      description,
      threat_topic,
      author: author_id,
      created_at,
      severity,
      ..
    } => TopicMetadataResponse::Invariant(InvariantTopicResponse {
      topic_id: topic.id(),
      description: description.clone(),
      threat_topic: threat_topic.id(),
      author: *author_id,
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
  tracing::debug!("GET /api/v1/audits/{}/metadata/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_metadata: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  // Create topic from the topic_id
  let topic = new_topic(&topic_id);

  // Get the metadata for this topic
  let metadata = audit_data.topic_metadata.get(&topic).ok_or_else(|| {
    tracing::warn!(
      "Metadata for topic '{}' not found in audit '{}'",
      topic_id,
      audit_id
    );
    StatusCode::NOT_FOUND
  })?;

  Ok(Json(topic_metadata_to_response(&topic, metadata)))
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

/// GET /api/v1/audits/:audit_id/comments/:comment_type/:status
/// Returns topic IDs of comments matching both the specified type and status.
pub async fn list_comments_by_type_and_status(
  State(state): State<AppState>,
  Path((audit_id, comment_type, status)): Path<(String, String, String)>,
) -> Result<Json<CommentListResponse>, StatusCode> {
  tracing::debug!(
    "GET /api/v1/audits/{}/comments/{}/{}",
    audit_id,
    comment_type,
    status
  );

  // Validate comment_type
  if CommentType::parse_str(&comment_type).is_none() {
    return Err(StatusCode::BAD_REQUEST);
  }

  // Validate status (CommentStatus::parse_str has a catch-all fallback, so check explicitly)
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
) -> Result<impl IntoResponse, (StatusCode, String)> {
  tracing::debug!(
    "POST /api/v1/audits/{}/comments body: {:?}",
    audit_id,
    payload
  );
  // Determine the scope from the target topic
  // If target is a comment (starts with "C"), copy scope from parent comment
  // Otherwise, get scope from the topic's metadata in audit data
  let target_topic = new_topic(&payload.topic_id);
  tracing::debug!("create_comment: resolved target_topic={:?}", target_topic,);
  let scope = if let topic::Topic::Comment(parent_comment_id) = target_topic {
    // Target is a comment - get scope from parent comment
    let parent_comment_id = parent_comment_id as i64;
    let parent_comment = db::get_comment_raw(&state.db, parent_comment_id)
      .await
      .map_err(|e| {
        let msg =
          format!("Parent comment {} not found: {}", parent_comment_id, e);
        tracing::error!("ERROR create_comment: {}", msg);
        (StatusCode::NOT_FOUND, msg)
      })?;
    // Parse the stored scope JSON
    serde_json::from_str(&parent_comment.scope).unwrap_or_default()
  } else {
    // Target is a regular topic - get scope from audit metadata
    tracing::debug!(
      "create_comment: acquiring first lock for scope resolution..."
    );
    let ctx = state.data_context.lock().map_err(|e| {
      let msg = format!("Failed to lock data context: {}", e);
      tracing::error!("ERROR create_comment: {}", msg);
      (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;
    tracing::debug!(
      "create_comment: first lock acquired, looking up audit '{}'...",
      audit_id
    );
    let audit_data = ctx.get_audit(&audit_id).ok_or_else(|| {
      let msg = format!("Audit '{}' not found in data context", audit_id);
      tracing::error!("ERROR create_comment: {}", msg);
      (StatusCode::NOT_FOUND, msg)
    })?;
    let scope = ScopeInfo::from_topic(&payload.topic_id, audit_data);
    tracing::debug!("create_comment: resolved scope={:?}", scope.scope_type);
    scope
  };

  // Insert comment into database with scope
  tracing::debug!("create_comment: inserting into DB...");
  let comment = db::create_comment(&state.db, &audit_id, &payload, &scope)
    .await
    .map_err(|e| {
      let msg = format!("Failed to create comment in DB: {}", e);
      tracing::error!("ERROR create_comment: {}", msg);
      (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;

  let comment_topic_id = comment.comment_topic_id();
  let comment_topic = comment.comment_topic();
  tracing::debug!(
    "create_comment: inserted as {}, ingesting...",
    comment_topic_id
  );

  // Ingest the parsed AST into in-memory state and collect the set of topic
  // IDs that need to be told about this comment (the direct target plus any
  // mentioned topics). Rendered representations are produced on demand by
  // the frontend, so no HTML construction happens here.
  let (affected_topic_ids, invalidated_thread_ids) = {
    let mut ctx = state.data_context.lock().map_err(|e| {
      let msg = format!("Failed to lock data context for ingest: {}", e);
      tracing::error!("ERROR create_comment: {}", msg);
      (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;

    let mentions = db::ingest_comment(&mut ctx, &comment, &scope);

    let audit_data = ctx.get_audit(&audit_id).ok_or_else(|| {
      let msg = format!("Audit '{}' not found after ingest", audit_id);
      tracing::error!("ERROR create_comment: {}", msg);
      (StatusCode::NOT_FOUND, msg)
    })?;

    // Collect parent comment chain for thread invalidation.
    // If the target is a comment, its thread (and all ancestor comment threads)
    // must be refetched by the client because they now include the new reply.
    let invalidated_thread_ids: Vec<String> = {
      let mut ids = Vec::new();
      let mut current = new_topic(&payload.topic_id);
      while matches!(current, topic::Topic::Comment(_)) {
        ids.push(current.id());
        match audit_data
          .topic_metadata
          .get(&current)
          .and_then(|m| m.target_topic())
        {
          Some(parent @ topic::Topic::Comment(_)) => {
            current = *parent;
          }
          _ => break,
        }
      }
      ids
    };

    let mut affected: Vec<String> = Vec::new();
    affected.push(payload.topic_id.clone());
    for m in &mentions {
      let mid = m.id();
      if !affected.contains(&mid) {
        affected.push(mid);
      }
    }

    (affected, invalidated_thread_ids)
  };

  // Broadcast the event for each affected topic. The first broadcast carries
  // the thread-invalidation list; subsequent mentions don't invalidate threads.
  let _ = comment_topic;
  for (i, topic_id) in affected_topic_ids.into_iter().enumerate() {
    let thread_ids = if i == 0 {
      invalidated_thread_ids.clone()
    } else {
      Vec::new()
    };
    let _ = state.event_broadcast.send(AuditEvent::TopicUpdated {
      audit_id: audit_id.clone(),
      topic_id,
      comment_topic_id: comment_topic_id.clone(),
      invalidated_thread_ids: thread_ids,
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
  Path((audit_id, comment_id)): Path<(String, String)>,
) -> Result<Json<CommentStatusResponse>, StatusCode> {
  let comment_topic = topic::parse_comment_topic(&comment_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  let comment_id_num = comment_topic.numeric_id() as i64;
  tracing::debug!(
    "GET /api/v1/audits/{}/comments/{}/status",
    audit_id,
    comment_id
  );
  let response = db::get_comment_status(&state.db, comment_id_num)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  Ok(Json(response))
}

/// PUT /api/v1/audits/:audit_id/comments/:comment_id/status
/// Updates comment status.
pub async fn update_comment_status(
  State(state): State<AppState>,
  Path((audit_id, comment_id)): Path<(String, String)>,
  Json(payload): Json<UpdateStatusRequest>,
) -> Result<Json<CommentStatusResponse>, StatusCode> {
  let comment_topic = topic::parse_comment_topic(&comment_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  let comment_id_num = comment_topic.numeric_id() as i64;
  tracing::debug!(
    "PUT /api/v1/audits/{}/comments/{}/status",
    audit_id,
    comment_id
  );
  // Update status in database
  let response = db::update_status(&state.db, comment_id_num, &payload.status)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  // Update in-memory comment index on hide/unhide
  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in update_comment_status: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if let Some(audit_data) = ctx.get_audit_mut(&audit_id)
      && let Some(target_topic) = audit_data
        .topic_metadata
        .get(&comment_topic)
        .and_then(|m| m.target_topic())
        .cloned()
    {
      if payload.status == CommentStatus::Hidden {
        if let Some(comments) = audit_data.comment_index.get_mut(&target_topic)
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

  // Broadcast status update via WebSocket
  let _ = state.event_broadcast.send(AuditEvent::StatusUpdated {
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
  Path((audit_id, comment_id)): Path<(String, String)>,
  Query(params): Query<OptionalUserIdQuery>,
) -> Result<Json<CommentVoteSummary>, StatusCode> {
  let comment_topic = topic::parse_comment_topic(&comment_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  let comment_id_num = comment_topic.numeric_id() as i64;
  tracing::debug!("GET /api/v1/audits/{}/votes/{}", audit_id, comment_id);
  let vote_info = db::get_vote_info(&state.db, comment_id_num, params.user_id)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  Ok(Json(CommentVoteSummary {
    comment_id: comment_id_num,
    comment_topic_id: comment_topic.id(),
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
  tracing::debug!(
    "GET /api/v1/audits/{}/votes/unvoted?user_id={}",
    audit_id,
    params.user_id
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
  Path((audit_id, comment_id)): Path<(String, String)>,
  Json(payload): Json<VoteRequest>,
) -> Result<Json<CommentVoteSummary>, StatusCode> {
  let comment_topic = topic::parse_comment_topic(&comment_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  let comment_id_num = comment_topic.numeric_id() as i64;
  tracing::debug!("POST /api/v1/audits/{}/votes/{}", audit_id, comment_id);
  let vote_value = payload.vote.to_i32();

  db::upsert_vote(&state.db, comment_id_num, payload.user_id, vote_value)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  // Return updated vote summary
  let vote_info =
    db::get_vote_info(&state.db, comment_id_num, Some(payload.user_id))
      .await
      .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  let comment_topic_id = comment_topic.id();

  // Broadcast vote update via WebSocket
  let _ = state.event_broadcast.send(AuditEvent::VoteUpdated {
    audit_id,
    comment_topic_id: comment_topic_id.clone(),
    score: vote_info.score,
    upvotes: vote_info.upvotes,
    downvotes: vote_info.downvotes,
  });

  Ok(Json(CommentVoteSummary {
    comment_id: comment_id_num,
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
  Path((audit_id, comment_id)): Path<(String, String)>,
  Query(params): Query<UserIdQuery>,
) -> Result<StatusCode, StatusCode> {
  let comment_topic = topic::parse_comment_topic(&comment_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  let comment_id_num = comment_topic.numeric_id() as i64;
  tracing::debug!(
    "DELETE /api/v1/audits/{}/votes/{}?user_id={}",
    audit_id,
    comment_id,
    params.user_id
  );
  db::delete_vote(&state.db, comment_id_num, params.user_id)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  // Get updated vote info and broadcast
  let vote_info = db::get_vote_info(&state.db, comment_id_num, None)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

  let _ = state.event_broadcast.send(AuditEvent::VoteUpdated {
    audit_id,
    comment_topic_id: comment_topic.id(),
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
  Json<o11a_core::collaborator::agent::context::AgentTopicContext>,
  StatusCode,
> {
  tracing::debug!("GET /api/v1/audits/{}/agent_context/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_agent_context: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let response =
    o11a_core::collaborator::agent::context::build_agent_topic_context(
      &topic_id,
      audit_data,
      params.include_expanded_context,
    )
    .ok_or_else(|| {
      tracing::warn!("Topic '{}' not found in audit '{}'", topic_id, audit_id);
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
  tracing::debug!("GET /api/v1/audits/{}/features", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_features: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let features = audit_data
    .topic_metadata
    .iter()
    .filter_map(|(t, m)| {
      if matches!(m, o11a_core::domain::TopicMetadata::FeatureTopic { .. }) {
        Some(topic_metadata_to_response(t, m))
      } else {
        None
      }
    })
    .collect();

  Ok(Json(features))
}

/// GET /api/v1/audits/:audit_id/features/:topic_id/requirements
pub async fn get_feature_requirements(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<Vec<String>>, StatusCode> {
  let feature_topic = topic::parse_feature_topic(&topic_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  tracing::debug!(
    "GET /api/v1/audits/{}/features/{}/requirements",
    audit_id,
    topic_id
  );

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_feature_requirements: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  if !matches!(
    audit_data.topic_metadata.get(&feature_topic),
    Some(o11a_core::domain::TopicMetadata::FeatureTopic { .. })
  ) {
    return Err(StatusCode::NOT_FOUND);
  }

  let ids: Vec<String> = audit_data
    .feature_requirement_links
    .get(&feature_topic)
    .map(|rts| rts.iter().map(|t| t.id()).collect())
    .unwrap_or_default();

  Ok(Json(ids))
}

/// GET /api/v1/audits/:audit_id/threats/:threat_id/invariants
pub async fn get_threat_invariants(
  State(state): State<AppState>,
  Path((audit_id, threat_id)): Path<(String, String)>,
) -> Result<Json<Vec<String>>, StatusCode> {
  let threat_topic =
    topic::parse_attack_vector_topic(&threat_id).map_err(|e| {
      tracing::warn!("Invalid topic ID in path: {}", e);
      StatusCode::BAD_REQUEST
    })?;
  tracing::debug!(
    "GET /api/v1/audits/{}/threats/{}/invariants",
    audit_id,
    threat_id
  );

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_threat_invariants: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let threat = audit_data
    .threats
    .get(&threat_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  let ids: Vec<String> =
    threat.invariant_topics.iter().map(|t| t.id()).collect();

  Ok(Json(ids))
}

/// Collect requirement topics for a set of feature topics.
fn requirements_for_features(
  feature_topics: &[topic::Topic],
  audit_data: &domain::AuditData,
) -> Vec<topic::Topic> {
  let mut requirement_topics = Vec::new();
  for ft in feature_topics {
    if let Some(req_topics) = audit_data.feature_requirement_links.get(ft) {
      for rt in req_topics {
        if !requirement_topics.contains(rt) {
          requirement_topics.push(*rt);
        }
      }
    }
  }
  requirement_topics
}

/// GET /api/v1/audits/:audit_id/features/:topic_id
/// Gets a single feature by its topic ID.
pub async fn get_feature(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let feature_topic = topic::parse_feature_topic(&topic_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  tracing::debug!("GET /api/v1/audits/{}/features/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_feature: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let metadata = audit_data
    .topic_metadata
    .get(&feature_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  Ok(Json(topic_metadata_to_response(&feature_topic, metadata)))
}

// ============================================
// Documentation routes
// ============================================

/// GET /api/v1/audits/:audit_id/requirements/:topic_id
/// Gets a single requirement.
pub async fn get_requirement(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let req_topic = topic::parse_requirement_topic(&topic_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  tracing::debug!("GET /api/v1/audits/{}/requirements/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_requirement: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let metadata = audit_data
    .topic_metadata
    .get(&req_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  Ok(Json(topic_metadata_to_response(&req_topic, metadata)))
}

#[derive(Debug, Deserialize)]
pub struct GetRequirementsQuery {
  pub for_topic: Option<String>,
}

/// GET /api/v1/audits/:audit_id/requirements
/// Returns all requirements for an audit (pipeline-produced and user-created).
///
/// When `?for_topic=T` is supplied, returns only requirements related to that
/// topic:
/// - Requirement topic: returns itself
/// - Feature topic: returns the feature's requirement_topics
/// - Source topic (N-prefixed): walks features via feature_behavior_links,
///   then collects their requirements
/// - Documentation topic (D-prefixed): returns requirements that reference
///   this documentation topic (via section_requirements or
///   documentation_topics)
pub async fn get_requirements(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Query(params): Query<GetRequirementsQuery>,
) -> Result<Json<Vec<TopicMetadataResponse>>, StatusCode> {
  tracing::debug!(
    "GET /api/v1/audits/{}/requirements{}",
    audit_id,
    params
      .for_topic
      .as_deref()
      .map(|t| format!("?for_topic={}", t))
      .unwrap_or_default()
  );

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_requirements: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  if let Some(for_topic) = params.for_topic.as_deref() {
    let t = new_topic(for_topic);
    let mut requirement_topics: Vec<topic::Topic> = Vec::new();
    match t {
      topic::Topic::Requirement(_) => {
        requirement_topics.push(t);
      }
      topic::Topic::Feature(_) => {
        if let Some(req_topics) = audit_data.feature_requirement_links.get(&t) {
          for rt in req_topics {
            if !requirement_topics.contains(rt) {
              requirement_topics.push(*rt);
            }
          }
        }
      }
      _ => {
        let fts = features_for_topic(&t, audit_data);
        for rt in requirements_for_features(&fts, audit_data) {
          if !requirement_topics.contains(&rt) {
            requirement_topics.push(rt);
          }
        }
        if let Some(section_reqs) = audit_data.section_requirements.get(&t) {
          for rt in section_reqs {
            if !requirement_topics.contains(rt) {
              requirement_topics.push(*rt);
            }
          }
        }
        for (req_topic, req) in &audit_data.requirements {
          if req.documentation_topics.contains(&t)
            && !requirement_topics.contains(req_topic)
          {
            requirement_topics.push(*req_topic);
          }
        }
      }
    }

    let responses = requirement_topics
      .iter()
      .filter_map(|rt| {
        let metadata = audit_data.topic_metadata.get(rt)?;
        if matches!(metadata, domain::TopicMetadata::RequirementTopic { .. }) {
          Some(topic_metadata_to_response(rt, metadata))
        } else {
          None
        }
      })
      .collect();
    return Ok(Json(responses));
  }

  let requirements = audit_data
    .topic_metadata
    .iter()
    .filter_map(|(t, m)| {
      if matches!(m, domain::TopicMetadata::RequirementTopic { .. }) {
        Some(topic_metadata_to_response(t, m))
      } else {
        None
      }
    })
    .collect();

  Ok(Json(requirements))
}

#[derive(Debug, Deserialize)]
pub struct AddSourceTopicRequest {
  pub topic_id: String,
}

// ============================================
// Topic property routes (functional semantics)
// ============================================

#[derive(Debug, Serialize)]
pub struct SubjectPropertyResponse {
  pub topic_id: String,
  pub property_type: String,
  pub value: String,
  #[serde(rename = "author_id")]
  pub author: Author,
}
/// GET /api/v1/audits/:audit_id/topics/:topic_id/semantics
/// Returns all functional semantics for a topic from the in-memory state.
pub async fn get_functional_semantics(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<Vec<SubjectPropertyResponse>>, StatusCode> {
  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_functional_semantics: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let t = topic::new_topic(&topic_id);
  let entries = audit_data
    .declaration_semantics
    .get(&t)
    .map(|sem_topics| {
      sem_topics
        .iter()
        .filter_map(|sem_topic| {
          let metadata = audit_data.topic_metadata.get(sem_topic)?;
          if let domain::TopicMetadata::FunctionalSemanticTopic {
            description,
            author,
            ..
          } = metadata
          {
            Some(SubjectPropertyResponse {
              topic_id: topic_id.clone(),
              property_type: "functional_semantics".to_string(),
              value: description.clone(),
              author: *author,
            })
          } else {
            None
          }
        })
        .collect()
    })
    .unwrap_or_default();

  Ok(Json(entries))
}

/// GET /api/v1/audits/:audit_id/functional_semantics
/// Returns all functional semantics for an audit (pipeline + user).
pub async fn get_all_functional_semantics(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
) -> Result<Json<Vec<TopicMetadataResponse>>, StatusCode> {
  tracing::debug!("GET /api/v1/audits/{}/functional_semantics", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_all_functional_semantics: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let entries = audit_data
    .topic_metadata
    .iter()
    .filter_map(|(t, m)| {
      if matches!(m, domain::TopicMetadata::FunctionalSemanticTopic { .. }) {
        Some(topic_metadata_to_response(t, m))
      } else {
        None
      }
    })
    .collect();

  Ok(Json(entries))
}

/// GET /api/v1/audits/:audit_id/functional_semantics/:topic_id
/// Gets a single functional semantic by its topic ID.
pub async fn get_functional_semantic(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let sem_topic =
    topic::parse_functional_property_topic(&topic_id).map_err(|e| {
      tracing::warn!("Invalid topic ID in path: {}", e);
      StatusCode::BAD_REQUEST
    })?;
  tracing::debug!(
    "GET /api/v1/audits/{}/functional_semantics/{}",
    audit_id,
    topic_id
  );

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_functional_semantic: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
  let metadata = audit_data
    .topic_metadata
    .get(&sem_topic)
    .ok_or(StatusCode::NOT_FOUND)?;

  Ok(Json(topic_metadata_to_response(&sem_topic, metadata)))
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
  tracing::debug!(
    "POST /api/v1/audits/{}/impact_analysis T{} -> F{}",
    audit_id,
    payload.threat_id,
    payload.feature_id
  );

  let threat_topic = topic::parse_attack_vector_topic(&payload.threat_id)
    .map_err(|e| {
      tracing::warn!("Invalid topic ID in payload: {}", e);
      StatusCode::BAD_REQUEST
    })?;
  let feature_topic =
    topic::parse_feature_topic(&payload.feature_id).map_err(|e| {
      tracing::warn!("Invalid topic ID in payload: {}", e);
      StatusCode::BAD_REQUEST
    })?;
  let threat_id = threat_topic.numeric_id() as i64;
  let feature_id = feature_topic.numeric_id() as i64;

  let relation = domain::ThreatFeatureRelation::parse_str(&payload.relation)
    .ok_or(StatusCode::BAD_REQUEST)?;
  let severity = domain::ThreatSeverity::parse_str(&payload.severity)
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
    tracing::error!("create_threat_feature_link failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  // Update in-memory state
  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in create_threat_feature_link: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    // Remove existing link for this pair if any
    audit_data.threat_feature_links.retain(|l| {
      !(l.threat_topic == threat_topic && l.feature_topic == feature_topic)
    });

    audit_data
      .threat_feature_links
      .push(domain::ThreatFeatureLink {
        threat_topic: threat_topic,
        feature_topic: feature_topic,
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
    if let Some(max_sev) = max_severity
      && let Some(domain::TopicMetadata::ThreatTopic { severity: s, .. }) =
        audit_data.topic_metadata.get_mut(&threat_topic)
    {
      *s = Some(max_sev);
    }
  }

  Ok(Json(ThreatFeatureLinkResponse {
    threat_topic: threat_topic.id(),
    feature_topic: feature_topic.id(),
    relation: relation.as_str().to_string(),
    severity: severity.as_str().to_string(),
  }))
}

/// DELETE /api/v1/audits/:audit_id/impact_analysis/:threat_id/:feature_id
pub async fn delete_threat_feature_link(
  State(state): State<AppState>,
  Path((audit_id, threat_id, feature_id)): Path<(String, String, String)>,
) -> Result<StatusCode, StatusCode> {
  let threat_topic =
    topic::parse_attack_vector_topic(&threat_id).map_err(|e| {
      tracing::warn!("Invalid topic ID in path: {}", e);
      StatusCode::BAD_REQUEST
    })?;
  let feature_topic = topic::parse_feature_topic(&feature_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  let threat_id = threat_topic.numeric_id() as i64;
  let feature_id = feature_topic.numeric_id() as i64;
  tracing::debug!(
    "DELETE /api/v1/audits/{}/impact_analysis/{}/{}",
    audit_id,
    threat_id,
    feature_id
  );

  db::delete_threat_feature_link(&state.db, threat_id, feature_id)
    .await
    .map_err(|e| {
      tracing::error!("delete_threat_feature_link failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in delete_threat_feature_link: {}", e);
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
    if let Some(domain::TopicMetadata::ThreatTopic { severity: s, .. }) =
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
  #[serde(rename = "author_id")]
  pub author: Author,
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
  #[serde(rename = "author_id")]
  pub author: Author,
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
  tracing::debug!(
    "POST /api/v1/audits/{}/conditions for {}",
    audit_id,
    payload.subject_topic
  );

  let row = db::create_condition(
    &state.db,
    &audit_id,
    &payload.subject_topic,
    &payload.condition_type,
    &payload.description,
    payload.author,
  )
  .await
  .map_err(|e| {
    tracing::error!("create_condition failed: {}", e);
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
    "state_write" => domain::NonPureSubjectType::StateWrite,
    "state_read" => domain::NonPureSubjectType::StateRead,
    "external_call" => domain::NonPureSubjectType::ExternalCall,
    "delegate_call" => domain::NonPureSubjectType::DelegateCall,
    "inline_assembly" => domain::NonPureSubjectType::InlineAssembly,
    "create" => domain::NonPureSubjectType::Create,
    _ => return Err(StatusCode::BAD_REQUEST),
  };

  let condition = domain::Condition {
    subject_topic: topic::new_topic(&payload.subject_topic),
    condition_type,
    description: payload.description.clone(),
    evaluations: payload
      .evaluations
      .iter()
      .map(|e| domain::ConditionEvaluation {
        question: e.question.clone(),
        answer: e.answer.clone(),
      })
      .collect(),
  };

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in create_condition: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
    audit_data.conditions.push(condition);
  }

  Ok(Json(ConditionResponse {
    id: row.id,
    subject_topic: row.subject_topic,
    condition_type: row.condition_type,
    description: row.description,
    author: row.author,
    created_at: row.created_at,
    evaluations: eval_responses,
  }))
}

/// GET /api/v1/audits/:audit_id/conditions/:condition_id
/// Returns all conditions for a subject. The path slot carries the subject's
/// topic ID for this handler; numeric condition IDs are used by the DELETE
/// variant on the same route.
pub async fn get_subject_conditions(
  State(state): State<AppState>,
  Path((audit_id, subject_topic)): Path<(String, String)>,
) -> Result<Json<Vec<ConditionResponse>>, StatusCode> {
  tracing::debug!(
    "GET /api/v1/audits/{}/conditions/{}",
    audit_id,
    subject_topic
  );

  let rows =
    db::get_conditions_for_subject(&state.db, &audit_id, &subject_topic)
      .await
      .map_err(|e| {
        tracing::error!("get_conditions_for_subject failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
      })?;

  let mut responses = Vec::new();
  for row in rows {
    let evals = db::get_condition_evaluations(&state.db, row.id)
      .await
      .map_err(|e| {
        tracing::error!("get_condition_evaluations failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
      })?;

    responses.push(ConditionResponse {
      id: row.id,
      subject_topic: row.subject_topic,
      condition_type: row.condition_type,
      description: row.description,
      author: row.author,
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
  tracing::debug!(
    "DELETE /api/v1/audits/{}/conditions/{}",
    audit_id,
    condition_id
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
      tracing::error!("delete_condition failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  // Remove from in-memory state
  if let Some(_subject) = subject_topic {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in delete_condition: {}", e);
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

/// GET /api/v1/audits/:audit_id/behaviors/:topic_id
pub async fn get_behavior(
  State(state): State<AppState>,
  Path((audit_id, topic_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let beh_topic = topic::parse_behavior_topic(&topic_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  tracing::debug!("GET /api/v1/audits/{}/behaviors/{}", audit_id, topic_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_behavior: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
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
  tracing::debug!("GET /api/v1/audits/{}/behaviors", audit_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_behaviors: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let behaviors: Vec<TopicMetadataResponse> = audit_data
    .topic_metadata
    .iter()
    .filter_map(|(bt, m)| {
      if matches!(m, domain::TopicMetadata::BehaviorTopic { .. }) {
        Some(topic_metadata_to_response(bt, m))
      } else {
        None
      }
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
  #[serde(rename = "author_id")]
  pub author: Author,
}

/// POST /api/v1/audits/:audit_id/threats
/// Creates a new threat on a non-pure subject. Severity is assigned later
/// during impact analysis.
pub async fn create_threat(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<CreateThreatRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  tracing::debug!(
    "POST /api/v1/audits/{}/threats on {}",
    audit_id,
    payload.subject_topic
  );

  let row = db::create_threat(
    &state.db,
    &audit_id,
    &payload.subject_topic,
    &payload.description,
    payload.author,
    None,
  )
  .await
  .map_err(|e| {
    tracing::error!("create_threat failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let subject_topic = topic::new_topic(&payload.subject_topic);
  let threat_topic = topic::new_attack_vector_topic(row.id as i32);

  let threat = domain::Threat {
    invariant_topics: Vec::new(),
  };

  let metadata = domain::TopicMetadata::ThreatTopic {
    topic: threat_topic,
    description: row.description,
    subject_topic: subject_topic,
    author: row.author,
    created_at: row.created_at,
    severity: None,
  };

  let mut ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in create_threat: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  audit_data.threats.insert(threat_topic, threat);
  audit_data
    .topic_metadata
    .insert(threat_topic, metadata.clone());

  o11a_core::domain::rebuild_feature_context(audit_data);

  let response = topic_metadata_to_response(&threat_topic, &metadata);
  Ok(Json(response))
}

/// DELETE /api/v1/audits/:audit_id/threats/:threat_id
pub async fn delete_threat(
  State(state): State<AppState>,
  Path((audit_id, threat_id)): Path<(String, String)>,
) -> Result<StatusCode, StatusCode> {
  let threat_topic =
    topic::parse_attack_vector_topic(&threat_id).map_err(|e| {
      tracing::warn!("Invalid topic ID in path: {}", e);
      StatusCode::BAD_REQUEST
    })?;
  let threat_id = threat_topic.numeric_id() as i64;
  tracing::debug!("DELETE /api/v1/audits/{}/threats/{}", audit_id, threat_id);

  db::delete_threat(&state.db, threat_id).await.map_err(|e| {
    tracing::error!("delete_threat failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in delete_threat: {}", e);
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

    o11a_core::domain::rebuild_feature_context(audit_data);
  }

  Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/audits/:audit_id/threats/:threat_id
pub async fn get_threat(
  State(state): State<AppState>,
  Path((audit_id, threat_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let threat_topic =
    topic::parse_attack_vector_topic(&threat_id).map_err(|e| {
      tracing::warn!("Invalid topic ID in path: {}", e);
      StatusCode::BAD_REQUEST
    })?;
  tracing::debug!("GET /api/v1/audits/{}/threats/{}", audit_id, threat_id);

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_threat: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
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
  #[serde(rename = "author_id")]
  pub author: Author,
}

/// POST /api/v1/audits/:audit_id/threats/:threat_id/invariants
pub async fn create_invariant(
  State(state): State<AppState>,
  Path((audit_id, threat_id)): Path<(String, String)>,
  Json(payload): Json<CreateInvariantRequest>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let threat_topic =
    topic::parse_attack_vector_topic(&threat_id).map_err(|e| {
      tracing::warn!("Invalid topic ID in path: {}", e);
      StatusCode::BAD_REQUEST
    })?;
  let threat_id = threat_topic.numeric_id() as i64;
  tracing::debug!(
    "POST /api/v1/audits/{}/threats/{}/invariants",
    audit_id,
    threat_id
  );

  // Invariants inherit severity from their parent threat (may be None)
  let severity = {
    let ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in create_invariant: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
    match audit_data.topic_metadata.get(&threat_topic) {
      Some(domain::TopicMetadata::ThreatTopic { severity, .. }) => *severity,
      _ => return Err(StatusCode::NOT_FOUND),
    }
  };

  let row = db::create_invariant(
    &state.db,
    threat_id,
    &payload.description,
    payload.author,
    severity.map(|s| s.as_str()),
  )
  .await
  .map_err(|e| {
    tracing::error!("create_invariant failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let inv_topic = topic::new_invariant_topic(row.id as i32);

  let invariant = domain::Invariant {
    source_topics: Vec::new(),
  };

  let metadata = domain::TopicMetadata::InvariantTopic {
    topic: inv_topic,
    description: row.description,
    threat_topic: threat_topic,
    author: row.author,
    created_at: row.created_at,
    severity,
  };

  let mut ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in create_invariant: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

  let threat = audit_data
    .threats
    .get_mut(&threat_topic)
    .ok_or(StatusCode::NOT_FOUND)?;
  threat.invariant_topics.push(inv_topic);

  audit_data.invariants.insert(inv_topic, invariant);
  audit_data
    .topic_metadata
    .insert(inv_topic, metadata.clone());

  o11a_core::domain::rebuild_feature_context(audit_data);

  let response = topic_metadata_to_response(&inv_topic, &metadata);
  Ok(Json(response))
}

/// DELETE /api/v1/audits/:audit_id/threats/:threat_id/invariants/:invariant_id
pub async fn delete_invariant(
  State(state): State<AppState>,
  Path((audit_id, threat_id, invariant_id)): Path<(String, String, String)>,
) -> Result<StatusCode, StatusCode> {
  let threat_topic =
    topic::parse_attack_vector_topic(&threat_id).map_err(|e| {
      tracing::warn!("Invalid topic ID in path: {}", e);
      StatusCode::BAD_REQUEST
    })?;
  let inv_topic = topic::parse_invariant_topic(&invariant_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  let threat_id = threat_topic.numeric_id() as i64;
  let invariant_id = inv_topic.numeric_id() as i64;
  tracing::debug!(
    "DELETE /api/v1/audits/{}/threats/{}/invariants/{}",
    audit_id,
    threat_id,
    invariant_id
  );

  db::delete_invariant(&state.db, invariant_id)
    .await
    .map_err(|e| {
      tracing::error!("delete_invariant failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in delete_invariant: {}", e);
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

    o11a_core::domain::rebuild_feature_context(audit_data);
  }

  Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/audits/:audit_id/invariants/:invariant_id
pub async fn get_invariant(
  State(state): State<AppState>,
  Path((audit_id, invariant_id)): Path<(String, String)>,
) -> Result<Json<TopicMetadataResponse>, StatusCode> {
  let inv_topic = topic::parse_invariant_topic(&invariant_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  tracing::debug!(
    "GET /api/v1/audits/{}/invariants/{}",
    audit_id,
    invariant_id
  );

  let ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in get_invariant: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let audit_data = ctx.get_audit(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
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
  let inv_topic = topic::parse_invariant_topic(&invariant_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  let invariant_id = inv_topic.numeric_id() as i64;
  tracing::debug!(
    "POST /api/v1/audits/{}/invariants/{}/source_topics",
    audit_id,
    invariant_id
  );

  db::add_invariant_source_topic(&state.db, invariant_id, &payload.topic_id)
    .await
    .map_err(|e| {
      tracing::error!("add_invariant_source_topic failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  let mut ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in add_invariant_source_topic: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
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
  let inv_topic = topic::parse_invariant_topic(&invariant_id).map_err(|e| {
    tracing::warn!("Invalid topic ID in path: {}", e);
    StatusCode::BAD_REQUEST
  })?;
  let invariant_id = inv_topic.numeric_id() as i64;
  tracing::debug!(
    "DELETE /api/v1/audits/{}/invariants/{}/source_topics/{}",
    audit_id,
    invariant_id,
    topic_id
  );

  db::remove_invariant_source_topic(&state.db, invariant_id, &topic_id)
    .await
    .map_err(|e| {
      tracing::error!("remove_invariant_source_topic failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;

  let mut ctx = state.data_context.lock().map_err(|e| {
    tracing::error!("Mutex poisoned in remove_invariant_source_topic: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;
  let audit_data = ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;
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

// ============================================
// User entity creation
// ============================================

#[derive(Debug, Serialize)]
pub struct CreatedResponse {
  pub topic_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateUserFeatureRequest {
  pub name: String,
  pub description: String,
  #[serde(rename = "author_id")]
  pub author: Author,
  #[serde(default)]
  pub requirement_topics: Vec<String>,
  #[serde(default)]
  pub behavior_topics: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateUserRequirementRequest {
  pub description: String,
  #[serde(rename = "author_id")]
  pub author: Author,
  #[serde(default)]
  pub section_topic: Option<String>,
  #[serde(default)]
  pub documentation_topics: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateUserBehaviorRequest {
  pub description: String,
  pub member_topic: String,
  #[serde(rename = "author_id")]
  pub author: Author,
}

#[derive(Debug, Deserialize)]
pub struct CreateUserFunctionalSemanticRequest {
  pub description: String,
  pub declaration_topic: String,
  #[serde(rename = "author_id")]
  pub author: Author,
  #[serde(default)]
  pub documentation_topics: Vec<String>,
}

/// POST /api/v1/audits/:audit_id/features
pub async fn create_user_feature(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<CreateUserFeatureRequest>,
) -> Result<Json<CreatedResponse>, StatusCode> {
  tracing::debug!("POST /api/v1/audits/{}/features", audit_id);

  let id = o11a_core::ids::allocate_feature_id();
  let created_at = o11a_core::ids::now_iso8601();

  db::user_entities::create_user_feature(
    &state.db,
    id,
    &audit_id,
    &payload.name,
    &payload.description,
    payload.author,
    &created_at,
  )
  .await
  .map_err(|e| {
    tracing::error!("create_user_feature failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  for rt in &payload.requirement_topics {
    db::user_entities::add_user_feature_requirement_link(&state.db, id, rt)
      .await
      .map_err(|e| {
        tracing::error!("add_user_feature_requirement_link failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
      })?;
  }
  for bt in &payload.behavior_topics {
    db::user_entities::add_user_feature_behavior_link(&state.db, id, bt)
      .await
      .map_err(|e| {
        tracing::error!("add_user_feature_behavior_link failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
      })?;
  }

  let feature_topic = topic::new_feature_topic(id);
  let requirement_topics: Vec<topic::Topic> = payload
    .requirement_topics
    .iter()
    .map(|s| topic::new_topic(s))
    .collect();
  let behavior_topics: Vec<topic::Topic> = payload
    .behavior_topics
    .iter()
    .map(|s| topic::new_topic(s))
    .collect();

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in create_user_feature: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    audit_data.topic_metadata.insert(
      feature_topic,
      domain::TopicMetadata::FeatureTopic {
        topic: feature_topic,
        name: payload.name.clone(),
        description: payload.description.clone(),
        author: payload.author,
        created_at: Some(created_at),
      },
    );

    if !requirement_topics.is_empty() {
      audit_data
        .feature_requirement_links
        .entry(feature_topic)
        .or_default()
        .extend(requirement_topics);
    }
    if !behavior_topics.is_empty() {
      audit_data
        .feature_behavior_links
        .entry(feature_topic)
        .or_default()
        .extend(behavior_topics);
    }

    domain::rebuild_feature_context(audit_data);
  }

  Ok(Json(CreatedResponse {
    topic_id: feature_topic.id(),
  }))
}

/// POST /api/v1/audits/:audit_id/requirements
pub async fn create_user_requirement(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<CreateUserRequirementRequest>,
) -> Result<Json<CreatedResponse>, StatusCode> {
  tracing::debug!("POST /api/v1/audits/{}/requirements", audit_id);

  let id = o11a_core::ids::allocate_requirement_id();
  let created_at = o11a_core::ids::now_iso8601();

  db::user_entities::create_user_requirement(
    &state.db,
    id,
    &audit_id,
    &payload.description,
    payload.section_topic.as_deref(),
    payload.author,
    &created_at,
  )
  .await
  .map_err(|e| {
    tracing::error!("create_user_requirement failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  for dt in &payload.documentation_topics {
    db::user_entities::add_user_requirement_documentation_topic(
      &state.db, id, dt,
    )
    .await
    .map_err(|e| {
      tracing::error!("add_user_requirement_documentation_topic failed: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
  }

  let req_topic = topic::new_requirement_topic(id);
  let section_topic =
    topic::new_topic(payload.section_topic.as_deref().unwrap_or(""));
  let documentation_topics: Vec<topic::Topic> = payload
    .documentation_topics
    .iter()
    .map(|s| topic::new_topic(s))
    .collect();

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in create_user_requirement: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    audit_data.requirements.insert(
      req_topic,
      domain::Requirement {
        documentation_topics,
      },
    );

    audit_data.topic_metadata.insert(
      req_topic,
      domain::TopicMetadata::RequirementTopic {
        topic: req_topic,
        description: payload.description.clone(),
        section_topic,
        author: payload.author,
        created_at: Some(created_at),
      },
    );

    domain::rebuild_feature_context(audit_data);
  }

  Ok(Json(CreatedResponse {
    topic_id: req_topic.id(),
  }))
}

/// POST /api/v1/audits/:audit_id/behaviors
pub async fn create_user_behavior(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<CreateUserBehaviorRequest>,
) -> Result<Json<CreatedResponse>, StatusCode> {
  tracing::debug!("POST /api/v1/audits/{}/behaviors", audit_id);

  let id = o11a_core::ids::allocate_behavior_id();
  let created_at = o11a_core::ids::now_iso8601();

  db::user_entities::create_user_behavior(
    &state.db,
    id,
    &audit_id,
    &payload.description,
    &payload.member_topic,
    payload.author,
    &created_at,
  )
  .await
  .map_err(|e| {
    tracing::error!("create_user_behavior failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  let beh_topic = topic::new_behavior_topic(id);
  let member_topic = topic::new_topic(&payload.member_topic);

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!("Mutex poisoned in create_user_behavior: {}", e);
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    audit_data.topic_metadata.insert(
      beh_topic,
      domain::TopicMetadata::BehaviorTopic {
        topic: beh_topic,
        description: payload.description.clone(),
        member_topic,
        author: payload.author,
        created_at: Some(created_at),
      },
    );

    domain::rebuild_feature_context(audit_data);
  }

  Ok(Json(CreatedResponse {
    topic_id: beh_topic.id(),
  }))
}

/// POST /api/v1/audits/:audit_id/functional_semantics
pub async fn create_user_functional_semantic(
  State(state): State<AppState>,
  Path(audit_id): Path<String>,
  Json(payload): Json<CreateUserFunctionalSemanticRequest>,
) -> Result<Json<CreatedResponse>, StatusCode> {
  tracing::debug!("POST /api/v1/audits/{}/functional_semantics", audit_id);

  let id = o11a_core::ids::allocate_functional_property_id();
  let created_at = o11a_core::ids::now_iso8601();

  db::user_entities::create_user_functional_semantic(
    &state.db,
    id,
    &audit_id,
    &payload.description,
    &payload.declaration_topic,
    payload.author,
    &created_at,
  )
  .await
  .map_err(|e| {
    tracing::error!("create_user_functional_semantic failed: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
  })?;

  for dt in &payload.documentation_topics {
    db::user_entities::add_user_functional_semantic_documentation_topic(
      &state.db, id, dt,
    )
    .await
    .map_err(|e| {
      tracing::error!(
        "add_user_functional_semantic_documentation_topic failed: {}",
        e
      );
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
  }

  let sem_topic = topic::new_functional_property_topic(id);
  let declaration_topic = topic::new_topic(&payload.declaration_topic);
  let documentation_topics: Vec<topic::Topic> = payload
    .documentation_topics
    .iter()
    .map(|s| topic::new_topic(s))
    .collect();

  {
    let mut ctx = state.data_context.lock().map_err(|e| {
      tracing::error!(
        "Mutex poisoned in create_user_functional_semantic: {}",
        e
      );
      StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let audit_data =
      ctx.get_audit_mut(&audit_id).ok_or(StatusCode::NOT_FOUND)?;

    audit_data.topic_metadata.insert(
      sem_topic,
      domain::TopicMetadata::FunctionalSemanticTopic {
        topic: sem_topic,
        description: payload.description.clone(),
        declaration_topic,
        documentation_topics,
        author: payload.author,
        created_at: Some(created_at),
        // User-authored semantics carry no workflow provenance.
        match_source: None,
      },
    );

    domain::rebuild_feature_context(audit_data);
  }

  Ok(Json(CreatedResponse {
    topic_id: sem_topic.id(),
  }))
}

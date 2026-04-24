//! HTTP error mapping for typed domain errors.
//!
//! Handlers translate library-level errors (`PipelineError`, `ArtifactError`,
//! etc.) to HTTP responses via the helpers in this module. Matching on
//! typed variants lets a handler return 404 on unknown audit or 400 on
//! an id mismatch without grepping an error message.

use axum::http::StatusCode;

use o11a_core::analysis_artifact::ArtifactError;
use o11a_core::collaborator::agent::pipeline::PipelineError;
use o11a_core::collaborator::agent::task::TaskError;

/// Map a `PipelineError` to an HTTP status code and a human-readable body.
/// Kept available for handlers that trigger pipeline steps; no route wires
/// this up yet, so the function is allowed to be unused.
#[allow(dead_code)]
pub fn into_http_response(err: PipelineError) -> (StatusCode, String) {
  let status = match &err {
    PipelineError::AuditNotFound { .. } => StatusCode::NOT_FOUND,
    PipelineError::Database(_) | PipelineError::LockPoisoned(_) => {
      StatusCode::INTERNAL_SERVER_ERROR
    }
    PipelineError::AgentTask(inner) => task_status(inner),
    PipelineError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
  };
  (status, err.to_string())
}

/// Map an `ArtifactError` to an HTTP status code and a human-readable body.
pub fn artifact_error_response(err: ArtifactError) -> (StatusCode, String) {
  let status = match &err {
    ArtifactError::AuditIdMismatch { .. } => StatusCode::BAD_REQUEST,
    ArtifactError::VersionMismatch { .. } => StatusCode::CONFLICT,
    ArtifactError::Decode(_) | ArtifactError::Encode(_) => {
      StatusCode::BAD_REQUEST
    }
    ArtifactError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
  };
  (status, err.to_string())
}

#[allow(dead_code)]
fn task_status(err: &TaskError) -> StatusCode {
  match err {
    TaskError::MissingEnv(_) => StatusCode::INTERNAL_SERVER_ERROR,
    TaskError::Io(_) | TaskError::HttpError(_) => {
      StatusCode::INTERNAL_SERVER_ERROR
    }
    TaskError::JsonParse { .. } | TaskError::MissingField(_) => {
      StatusCode::BAD_GATEWAY
    }
    TaskError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
  }
}

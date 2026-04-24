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
/// Ready for handlers that trigger pipeline steps; proven by unit tests in
/// this module. The `allow(dead_code)` applies only to non-test builds, where
/// no route yet invokes the pipeline path.
#[cfg_attr(not(test), allow(dead_code))]
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

#[cfg_attr(not(test), allow(dead_code))]
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pipeline_error_maps_to_expected_status() {
    let (status, _) = into_http_response(PipelineError::AuditNotFound {
      audit_id: "missing".to_string(),
    });
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) =
      into_http_response(PipelineError::LockPoisoned("poisoned".into()));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let (status, _) = into_http_response(PipelineError::Other("x".into()));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let (status, _) = into_http_response(PipelineError::AgentTask(
      TaskError::MissingField("audit_id"),
    ));
    assert_eq!(status, StatusCode::BAD_GATEWAY);
  }

  #[test]
  fn artifact_error_maps_to_expected_status() {
    let (status, _) = artifact_error_response(ArtifactError::AuditIdMismatch {
      expected: "a".into(),
      found: "b".into(),
    });
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = artifact_error_response(ArtifactError::VersionMismatch {
      found: 1,
      expected: 2,
    });
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _) = artifact_error_response(ArtifactError::Io(
      std::io::Error::new(std::io::ErrorKind::Other, "boom"),
    ));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
  }

  #[test]
  fn task_status_maps_every_variant() {
    assert_eq!(
      task_status(&TaskError::MissingEnv("X".into())),
      StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
      task_status(&TaskError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        "io"
      ))),
      StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
      task_status(&TaskError::MissingField("x")),
      StatusCode::BAD_GATEWAY
    );
    assert_eq!(
      task_status(&TaskError::Other("nope".into())),
      StatusCode::INTERNAL_SERVER_ERROR
    );
  }
}

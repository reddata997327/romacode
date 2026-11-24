//! Error types: HTTP exceptions (`HttpError`), ORM errors
//! (`DoesNotExist`, `MultipleObjectsReturned`) and the general framework
//! error, mirroring `django.http.Http404`, `django.core.exceptions`, etc.

use thiserror::Error;

/// An error raised inside a view — the equivalent of raising
/// `Http404`, `PermissionDenied` or `SuspiciousOperation` in Django.
#[derive(Debug, Error, Clone)]
pub enum HttpError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("method not allowed: {0}")]
    MethodNotAllowed(String),
    #[error("csrf verification failed: {0}")]
    CsrfFailure(String),
    #[error("server error: {0}")]
    ServerError(String),
}

impl HttpError {
    pub fn status(&self) -> u16 {
        match self {
            HttpError::NotFound(_) => 404,
            HttpError::PermissionDenied(_) => 403,
            HttpError::BadRequest(_) => 400,
            HttpError::MethodNotAllowed(_) => 405,
            HttpError::CsrfFailure(_) => 403,
            HttpError::ServerError(_) => 500,
        }
    }
}

impl From<std::io::Error> for OrmError {
    fn from(e: std::io::Error) -> Self {
        OrmError::Backend(format!("io error: {e}"))
    }
}

impl From<serde_json::Error> for OrmError {
    fn from(e: serde_json::Error) -> Self {
        OrmError::Backend(format!("json error: {e}"))
    }
}

impl From<crate::orm::OrmError> for HttpError {
    fn from(e: crate::orm::OrmError) -> Self {
        match e {
            crate::orm::OrmError::DoesNotExist(_) => HttpError::NotFound(e.to_string()),
            crate::orm::OrmError::MultipleObjectsReturned(_) => HttpError::BadRequest(e.to_string()),
            crate::orm::OrmError::PermissionDenied(_) => HttpError::PermissionDenied(e.to_string()),
            other => HttpError::ServerError(other.to_string()),
        }
    }
}

impl From<std::io::Error> for HttpError {
    fn from(e: std::io::Error) -> Self {
        HttpError::ServerError(format!("io error: {e}"))
    }
}

/// ORM-level errors, mirroring `django.db` exception hierarchy.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum OrmError {
    #[error("query returned no results for {0}")]
    DoesNotExist(String),
    #[error("query returned multiple results for {0}")]
    MultipleObjectsReturned(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("invalid value for field `{0}`: {1}")]
    InvalidValue(String, String),
    #[error("unknown field `{0}`")]
    UnknownField(String),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("backend error: {0}")]
    Backend(String),
}

/// Anything the framework can produce at runtime.
#[derive(Debug, Error)]
pub enum RustangoError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(transparent)]
    Orm(#[from] crate::orm::OrmError),
    #[error("template error: {0}")]
    Template(String),
    #[error("form error: {0}")]
    Form(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = RustangoError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        assert_eq!(HttpError::NotFound("x".into()).status(), 404);
        assert_eq!(HttpError::CsrfFailure("bad token".into()).status(), 403);
        assert_eq!(HttpError::MethodNotAllowed("POST only".into()).status(), 405);
    }

    #[test]
    fn orm_to_http_conversion() {
        let e: HttpError = OrmError::DoesNotExist("polls".into()).into();
        assert_eq!(e.status(), 404);
        let e: HttpError = OrmError::Backend("disk full".into()).into();
        assert_eq!(e.status(), 500);
    }
}

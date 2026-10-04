use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The structured error shown to API callers and the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoxError {
    pub error: String,
    pub message: String,
    pub recoverable: bool,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Backend(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    /// An error with its own HTTP status and machine-readable code (for example `EXISTS`, 409).
    #[error("{message}")]
    Coded {
        status: u16,
        code: String,
        message: String,
    },
    #[error("{0}")]
    Db(#[from] rusqlite::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub fn validation(msg: impl Into<String>) -> Self {
        Error::Validation(msg.into())
    }

    pub fn coded(status: u16, code: &str, message: impl Into<String>) -> Self {
        Error::Coded {
            status,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Error::NotFound(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Error::Conflict(msg.into())
    }

    pub fn backend(msg: impl Into<String>) -> Self {
        Error::Backend(msg.into())
    }

    pub fn to_box_error(&self) -> BoxError {
        let (code, recoverable) = match self {
            Error::Validation(_) => ("VALIDATION_ERROR", false),
            Error::Backend(_) => ("BACKEND_ERROR", true),
            Error::NotFound(_) => ("NOT_FOUND", false),
            Error::Conflict(_) => ("CONFLICT", true),
            Error::Coded { code, .. } => {
                return BoxError {
                    error: code.clone(),
                    message: self.to_string(),
                    recoverable: true,
                };
            }
            Error::Db(_) => ("DATABASE_ERROR", false),
            Error::Io(_) => ("IO_ERROR", false),
            Error::Json(_) => ("JSON_PARSE_ERROR", false),
        };
        BoxError {
            error: code.into(),
            message: self.to_string(),
            recoverable,
        }
    }
}

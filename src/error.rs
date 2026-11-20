use serde::Serialize;
use std::path::PathBuf;
use thiserror::Error;

/// Canonical exit codes for deterministic human and agent branching.
pub const EXIT_OK: i32 = 0;
pub const EXIT_VIOLATION: i32 = 1;
pub const EXIT_ERROR: i32 = 2;

#[derive(Debug, Error)]
pub enum SqlKeelError {
    #[error("Failed to read '{path}': {source}")]
    IoRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to write '{path}': {source}")]
    IoWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to parse schema file '{path}': {detail}")]
    SchemaParse { path: String, detail: String },

    #[error("No SQL files found in the provided paths")]
    NoSqlFiles,

    #[error("Unknown rule code '{code}'")]
    UnknownRule { code: String },

    #[error("{0}")]
    General(String),
}

impl SqlKeelError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::IoRead { .. } => "SK_IO_READ",
            Self::IoWrite { .. } => "SK_IO_WRITE",
            Self::SchemaParse { .. } => "SK_SCHEMA_PARSE",
            Self::NoSqlFiles => "SK_NO_SQL_FILES",
            Self::UnknownRule { .. } => "SK_UNKNOWN_RULE",
            Self::General(_) => "SK_ERROR",
        }
    }

    pub fn fix_hint(&self) -> Option<String> {
        match self {
            Self::IoRead { path, .. } => Some(format!(
                "Verify that '{}' exists and is readable.",
                path.display()
            )),
            Self::SchemaParse { path, .. } => Some(format!(
                "Verify '{}' is valid ODCS v3 YAML, datakeel TOML, or SQL DDL.",
                path
            )),
            Self::NoSqlFiles => Some(
                "Pass one or more `.sql` files, a directory containing `.sql` files, or `-` for stdin."
                    .to_string(),
            ),
            Self::UnknownRule { .. } => Some(
                "Run `sqlkeel rules` to list all valid `SK_*` rule codes.".to_string(),
            ),
            _ => None,
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self, Self::IoRead { .. } | Self::IoWrite { .. })
    }

    pub fn to_envelope(&self) -> ErrorEnvelope {
        ErrorEnvelope {
            schema_version: "1.0",
            ok: false,
            error: ErrorBody {
                code: self.code(),
                message: self.to_string(),
                fix_hint: self.fix_hint(),
                retryable: self.retryable(),
            },
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    pub schema_version: &'static str,
    pub ok: bool,
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_hint: Option<String>,
    pub retryable: bool,
}

pub type Result<T> = std::result::Result<T, SqlKeelError>;

//! The one error model of the operation API (note DESIGN/03): every failure has a code, on the socket, in MCP and
//! from D-Bus. Code that knows the kind of a failure raises [`ApiError`]; any other error is `failed`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    /// No such operation, session, dossier or other named thing.
    NotFound,
    /// The input does not fit the operation.
    InvalidInput,
    /// The state does not allow it now: a dossier held by another session, a file that exists, uncommitted changes.
    Conflict,
    /// The caller may not do this: polkit said no, the user did not approve, or it is outside the caller's session.
    Denied,
    /// The operator or the machine daemon does not answer.
    Unavailable,
    /// The client and the operator are different builds.
    VersionMismatch,
    /// The operation ran and failed.
    Failed,
    /// The operation crashed.
    Internal,
}

impl Code {
    /// The code as it is on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Code::NotFound => "not_found",
            Code::InvalidInput => "invalid_input",
            Code::Conflict => "conflict",
            Code::Denied => "denied",
            Code::Unavailable => "unavailable",
            Code::VersionMismatch => "version_mismatch",
            Code::Failed => "failed",
            Code::Internal => "internal",
        }
    }

    /// The HTTP status of the code on the operator's socket.
    #[must_use]
    pub fn status(self) -> u16 {
        match self {
            Code::NotFound => 404,
            Code::InvalidInput => 400,
            Code::Conflict => 409,
            Code::Denied => 403,
            Code::Unavailable => 503,
            Code::VersionMismatch => 412,
            Code::Failed => 422,
            Code::Internal => 500,
        }
    }
}

/// A failure with its code; the body of every error answer is `{"error": ApiError}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApiError {
    pub code: Code,
    pub message: String,
    /// The operation that failed; empty where none applies.
    #[serde(default)]
    pub op: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// An error with `code`, for `bail!`/`?`.
pub fn coded(code: Code, message: impl Into<String>) -> anyhow::Error {
    ApiError {
        code,
        message: message.into(),
        op: String::new(),
    }
    .into()
}

/// The code of `error`: the first [`ApiError`] in its chain, else `failed`.
#[must_use]
pub fn code_of(error: &anyhow::Error) -> Code {
    error
        .chain()
        .find_map(|e| e.downcast_ref::<ApiError>())
        .map_or(Code::Failed, |e| e.code)
}

//! Typed errors that carry the process exit code.
//!
//! The exit-code taxonomy is part of the public contract (see SPEC §4). Keeping it on the
//! error type means the binary's `main` can map a failure to the right code without string
//! matching, instead of collapsing everything to `1`.

use std::fmt;

/// Stable process exit codes. See SPEC §4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    /// Success.
    Success = 0,
    /// Generic runtime error.
    Runtime = 1,
    /// Usage error (bad flags, no server resolved, unknown tool name locally).
    Usage = 2,
    /// Connection or protocol error (couldn't connect, handshake failed, provider exited,
    /// or the provider returned a JSON-RPC error response).
    Connection = 3,
    /// Job failed — `tools.result` with `success: false`.
    Job = 4,
    /// Timeout — host-synthesized `TIMEOUT`.
    Timeout = 5,
    /// Validation failed — parameters didn't satisfy `parameter_schema`.
    Validation = 6,
    /// Interrupted (Ctrl-C / cancellation).
    Interrupted = 130,
}

impl ExitCode {
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// The library's error type. Every variant maps to exactly one [`ExitCode`].
#[derive(Debug, thiserror::Error)]
pub enum StpError {
    #[error("usage: {0}")]
    Usage(String),

    #[error("connection error: {0}")]
    Connection(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    /// A JSON-RPC error response from the provider (numeric `-326xx` code + message).
    #[error("provider returned JSON-RPC error {code}: {message}")]
    JsonRpc { code: i64, message: String },

    /// A terminal `tools.result` with `success: false` (STP job error code + message).
    #[error("job failed [{code}]: {message}")]
    Job { code: String, message: String },

    #[error("timed out after {0:?}")]
    Timeout(std::time::Duration),

    #[error("validation failed:\n{0}")]
    Validation(String),

    #[error("interrupted")]
    Interrupted,

    #[error("{0}")]
    Runtime(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl StpError {
    /// The exit code this error maps to.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            StpError::Usage(_) => ExitCode::Usage,
            StpError::Connection(_) | StpError::Protocol(_) | StpError::JsonRpc { .. } => {
                ExitCode::Connection
            }
            StpError::Job { .. } => ExitCode::Job,
            StpError::Timeout(_) => ExitCode::Timeout,
            StpError::Validation(_) => ExitCode::Validation,
            StpError::Interrupted => ExitCode::Interrupted,
            StpError::Runtime(_) | StpError::Io(_) | StpError::Json(_) => ExitCode::Runtime,
        }
    }

    pub fn runtime(msg: impl fmt::Display) -> Self {
        StpError::Runtime(msg.to_string())
    }
    pub fn connection(msg: impl fmt::Display) -> Self {
        StpError::Connection(msg.to_string())
    }
    pub fn protocol(msg: impl fmt::Display) -> Self {
        StpError::Protocol(msg.to_string())
    }
    pub fn usage(msg: impl fmt::Display) -> Self {
        StpError::Usage(msg.to_string())
    }
}

pub type Result<T> = std::result::Result<T, StpError>;

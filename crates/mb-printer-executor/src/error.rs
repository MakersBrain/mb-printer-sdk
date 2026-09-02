// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::Progress;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TransportErrorKind {
    #[error("connection")]
    Connection,
    #[error("disconnected")]
    Disconnected,
    #[error("unsupported")]
    Unsupported,
    #[error("invalid configuration")]
    InvalidConfiguration,
    #[error("permission denied")]
    PermissionDenied,
    #[error("timeout")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    #[error("I/O")]
    Io,
}

/// A sanitized transport failure. Messages must not include credentials,
/// printer payloads, or unfiltered platform debug output.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{kind}: {message}")]
pub struct TransportError {
    pub kind: TransportErrorKind,
    pub message: String,
}

impl TransportError {
    pub fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExecuteError {
    #[error("atomic command at action {action} is {length} bytes but transport limit is {limit}")]
    AtomicTooLarge {
        action: usize,
        length: usize,
        limit: usize,
    },
    #[error("invalid execution plan at action {action}: {message}")]
    InvalidPlan {
        action: usize,
        message: &'static str,
    },
    #[error("execution key was already attempted: {0}")]
    Replay(String),
    #[error("replay store failed: {0}")]
    ReplayStore(String),
    #[error("execution cancelled")]
    Cancelled { progress: Progress },
    #[error("write outcome is unknown")]
    WriteOutcomeUnknown {
        progress: Progress,
        #[source]
        source: Option<TransportError>,
    },
    #[error("transport failed")]
    Transport {
        progress: Progress,
        #[source]
        source: TransportError,
    },
    #[error("response timed out")]
    Timeout { progress: Progress },
    #[error("invalid response: {message}")]
    Response { progress: Progress, message: String },
}

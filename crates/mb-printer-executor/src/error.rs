// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::Progress;
use thiserror::Error;

/// Stable category for a transport-boundary failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TransportErrorKind {
    /// A connection could not be established.
    #[error("connection")]
    Connection,
    /// An established connection ended or is no longer usable.
    #[error("disconnected")]
    Disconnected,
    /// The requested operation is unsupported by the transport.
    #[error("unsupported")]
    Unsupported,
    /// Transport limits or options are invalid.
    #[error("invalid configuration")]
    InvalidConfiguration,
    /// The platform denied access to the device.
    #[error("permission denied")]
    PermissionDenied,
    /// A transport operation exceeded its deadline.
    #[error("timeout")]
    Timeout,
    /// The host cancelled the transport operation.
    #[error("cancelled")]
    Cancelled,
    /// A platform I/O operation failed.
    #[error("I/O")]
    Io,
}

/// A sanitized transport failure. Messages must not include credentials,
/// printer payloads, or unfiltered platform debug output.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{kind}: {message}")]
pub struct TransportError {
    /// Machine-readable failure category.
    pub kind: TransportErrorKind,
    /// Redacted, user-safe diagnostic message.
    pub message: String,
}

impl TransportError {
    /// Creates a redacted transport error with a stable category.
    pub fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Failure returned while preflighting or executing a printer action plan.
#[derive(Debug, Error)]
pub enum ExecuteError {
    /// An atomic command exceeds the transport command limit.
    #[error("atomic command at action {action} is {length} bytes but transport limit is {limit}")]
    AtomicTooLarge {
        /// Zero-based action index.
        action: usize,
        /// Command size in bytes.
        length: usize,
        /// Transport command limit in bytes.
        limit: usize,
    },
    /// The plan violates a structural executor invariant.
    #[error("invalid execution plan at action {action}: {message}")]
    InvalidPlan {
        /// Zero-based action index associated with the error.
        action: usize,
        /// Static, non-sensitive validation message.
        message: &'static str,
    },
    /// The execution key was previously claimed.
    #[error("execution key was already attempted: {0}")]
    Replay(String),
    /// The durable or in-memory replay store failed.
    #[error("replay store failed")]
    ReplayStore {
        /// Backend-specific error retained for diagnostics.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Cancellation occurred before an ambiguous write boundary.
    #[error("execution cancelled")]
    Cancelled {
        /// Progress completed before cancellation.
        progress: Progress,
    },
    /// Cancellation or failure occurred while a write may have reached hardware.
    #[error("write outcome is unknown")]
    WriteOutcomeUnknown {
        /// Progress at the ambiguous write boundary.
        progress: Progress,
        /// Optional underlying transport failure.
        #[source]
        source: Option<TransportError>,
    },
    /// A transport operation failed without an ambiguous write outcome.
    #[error("transport failed")]
    Transport {
        /// Progress completed before the transport failure.
        progress: Progress,
        /// Underlying transport failure.
        #[source]
        source: TransportError,
    },
    /// A required printer response was not received in time.
    #[error("response timed out")]
    Timeout {
        /// Progress completed before the timeout.
        progress: Progress,
    },
    /// A received response failed protocol validation.
    #[error("invalid response: {message}")]
    Response {
        /// Progress completed before validation failed.
        progress: Progress,
        /// Redacted validation diagnostic.
        message: String,
    },
}

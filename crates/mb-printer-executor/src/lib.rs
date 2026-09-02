// SPDX-License-Identifier: AGPL-3.0-or-later
#![deny(unsafe_op_in_unsafe_fn)]

mod error;
mod executor;
mod replay;
mod transport;

pub use error::{ExecuteError, TransportError, TransportErrorKind};
pub use executor::{
    ExecutionOptions, MAX_PLAN_ACTIONS, MAX_RETAINED_RESPONSE_BYTES, Progress, ReferenceTiming,
    execute, execute_with_options,
};
pub use replay::{MemoryReplayStore, ReplayGuard, ReplayStore, execute_once_with_store};
pub use transport::{
    Cancellation, NeverCancelled, NotificationSupport, Transport, TransportFuture, WaitOutcome,
    WriteKind,
};

#[cfg(not(target_arch = "wasm32"))]
pub use transport::CancellationToken;

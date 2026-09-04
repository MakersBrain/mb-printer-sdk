// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeSet;
use std::convert::Infallible;
use std::error::Error;

use mb_printer_core::protocol::Plan;

use crate::{ExecuteError, Progress, Transport, execute};

/// Atomic execution-key store used to prevent plan replay.
pub trait ReplayStore {
    /// Backend-specific failure returned while claiming an execution key.
    type Error: Error + Send + Sync + 'static;

    /// Atomically claims a key. False means that it was already claimed.
    fn claim(&mut self, key: &str) -> Result<bool, Self::Error>;
}

/// Process-local replay store backed by an ordered set.
#[derive(Debug, Default)]
pub struct MemoryReplayStore {
    attempted: BTreeSet<String>,
}

impl ReplayStore for MemoryReplayStore {
    type Error = Infallible;

    fn claim(&mut self, key: &str) -> Result<bool, Self::Error> {
        Ok(self.attempted.insert(key.to_owned()))
    }
}

/// Process-local helper that claims keys before executing a plan.
#[derive(Debug, Default)]
pub struct ReplayGuard {
    attempted: BTreeSet<String>,
}

impl ReplayGuard {
    /// Claims `key` and executes `plan` exactly once for this guard.
    ///
    /// # Errors
    ///
    /// Returns [`ExecuteError::Replay`] when the key was already claimed, or
    /// forwards an execution failure. Failed executions remain claimed.
    pub async fn execute_once<T: Transport + ?Sized>(
        &mut self,
        key: &str,
        plan: &Plan,
        transport: &mut T,
    ) -> Result<Progress, ExecuteError> {
        if !self.attempted.insert(key.to_owned()) {
            return Err(ExecuteError::Replay(key.to_owned()));
        }
        execute(plan, transport).await
    }

    /// Returns whether this guard has claimed `key`.
    pub fn was_attempted(&self, key: &str) -> bool {
        self.attempted.contains(key)
    }
}

/// Claims an execution key in `store` before executing the plan.
///
/// # Errors
///
/// Returns a replay-store, duplicate-key, or execution error. Claims remain
/// consumed after failure so ambiguous writes cannot be retried.
pub async fn execute_once_with_store<T: Transport + ?Sized, S: ReplayStore + ?Sized>(
    store: &mut S,
    key: &str,
    plan: &Plan,
    transport: &mut T,
) -> Result<Progress, ExecuteError> {
    match store
        .claim(key)
        .map_err(|source| ExecuteError::ReplayStore {
            source: Box::new(source),
        })? {
        true => execute(plan, transport).await,
        false => Err(ExecuteError::Replay(key.to_owned())),
    }
}

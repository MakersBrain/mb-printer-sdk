// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeSet;

use mb_printer_core::protocol::Plan;

use crate::{ExecuteError, Progress, Transport, execute};

pub trait ReplayStore {
    /// Atomically claims a key. False means that it was already claimed.
    fn claim(&mut self, key: &str) -> Result<bool, String>;
}

#[derive(Debug, Default)]
pub struct MemoryReplayStore {
    attempted: BTreeSet<String>,
}

impl ReplayStore for MemoryReplayStore {
    fn claim(&mut self, key: &str) -> Result<bool, String> {
        Ok(self.attempted.insert(key.to_owned()))
    }
}

#[derive(Debug, Default)]
pub struct ReplayGuard {
    attempted: BTreeSet<String>,
}

impl ReplayGuard {
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

    pub fn was_attempted(&self, key: &str) -> bool {
        self.attempted.contains(key)
    }
}

pub async fn execute_once_with_store<T: Transport + ?Sized, S: ReplayStore + ?Sized>(
    store: &mut S,
    key: &str,
    plan: &Plan,
    transport: &mut T,
) -> Result<Progress, ExecuteError> {
    match store.claim(key).map_err(ExecuteError::ReplayStore)? {
        true => execute(plan, transport).await,
        false => Err(ExecuteError::Replay(key.to_owned())),
    }
}

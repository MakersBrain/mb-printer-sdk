// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded adapter for hardware APIs that cannot be made natively async.

use std::{sync::Arc, time::Duration};
use thiserror::Error;
use tokio::sync::Semaphore;

#[derive(Debug, Clone)]
pub struct BoundedHardwareWorkers {
    permits: Arc<Semaphore>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareOperationLimits {
    pub queue_timeout: Duration,
    /// Passed into the backend so the device/OS call itself is also bounded.
    pub device_timeout: Duration,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WorkerError {
    #[error("hardware worker queue timed out")]
    QueueTimeout,
    #[error("hardware operation timed out")]
    OperationTimeout,
    #[error("hardware worker terminated unexpectedly")]
    WorkerFailed,
}

impl BoundedHardwareWorkers {
    pub fn new(maximum_concurrent_operations: usize) -> Result<Self, WorkerError> {
        if maximum_concurrent_operations == 0 {
            return Err(WorkerError::WorkerFailed);
        }
        Ok(Self {
            permits: Arc::new(Semaphore::new(maximum_concurrent_operations)),
        })
    }

    pub async fn run<T, F>(
        &self,
        limits: HardwareOperationLimits,
        operation: F,
    ) -> Result<T, WorkerError>
    where
        T: Send + 'static,
        F: FnOnce(Duration) -> T + Send + 'static,
    {
        if limits.queue_timeout.is_zero() || limits.device_timeout.is_zero() {
            return Err(WorkerError::WorkerFailed);
        }
        let permit =
            tokio::time::timeout(limits.queue_timeout, self.permits.clone().acquire_owned())
                .await
                .map_err(|_| WorkerError::QueueTimeout)?
                .map_err(|_| WorkerError::WorkerFailed)?;
        let worker = tokio::task::spawn_blocking(move || {
            // Capacity remains occupied until the system call truly exits,
            // even when its awaiting future times out or is cancelled.
            let _permit = permit;
            operation(limits.device_timeout)
        });
        tokio::time::timeout(limits.device_timeout, worker)
            .await
            .map_err(|_| WorkerError::OperationTimeout)?
            .map_err(|_| WorkerError::WorkerFailed)
    }
}

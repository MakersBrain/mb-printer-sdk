// SPDX-License-Identifier: AGPL-3.0-or-later
#![forbid(unsafe_code)]
use mb_printer_core::protocol::{Action, Plan, ResponseValidation};
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    Response(Vec<u8>),
    Timeout,
    Unavailable,
}

pub trait Transport {
    fn payload_limit(&self) -> usize;
    fn subscribe_notifications(&mut self) -> Result<(), String>;
    fn write(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn delay_monotonic(&mut self, milliseconds: u64);
    fn wait_response(&mut self, timeout_ms: u64) -> Result<WaitOutcome, String>;
}
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Progress {
    pub last_completed_action: Option<usize>,
    pub bytes_written: u64,
    pub potentially_accepted_write: bool,
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
    #[error("transport failed after {progress:?}: {message}")]
    Transport { progress: Progress, message: String },
    #[error("response timed out after {progress:?}")]
    Timeout { progress: Progress },
    #[error("invalid response after {progress:?}: {message}")]
    Response { progress: Progress, message: String },
}
pub fn execute<T: Transport>(plan: &Plan, t: &mut T) -> Result<Progress, ExecuteError> {
    let limit = t.payload_limit();
    if limit == 0 {
        return Err(ExecuteError::InvalidPlan {
            action: 0,
            message: "zero transport payload limit",
        });
    }
    for (i, a) in plan.actions.iter().enumerate() {
        if let Action::CommandWrite {
            bytes,
            atomic: true,
            ..
        } = a
            && bytes.len() > limit
        {
            return Err(ExecuteError::AtomicTooLarge {
                action: i,
                length: bytes.len(),
                limit,
            });
        }
        if let Action::RasterWrite {
            logical_chunk: 0, ..
        } = a
        {
            return Err(ExecuteError::InvalidPlan {
                action: i,
                message: "zero logical raster chunk",
            });
        }
    }
    let mut p = Progress::default();
    for (i, a) in plan.actions.iter().enumerate() {
        let result: Result<(), ExecuteError> = match a {
            Action::JobBoundary { .. } => Ok(()),
            Action::SubscribeNotifications => t
                .subscribe_notifications()
                .map_err(|message| fail(&p, message)),
            Action::Delay { milliseconds } => {
                t.delay_monotonic(*milliseconds);
                Ok(())
            }
            Action::CommandWrite { bytes, .. } => write(t, bytes, &mut p),
            Action::RasterWrite {
                bytes,
                logical_chunk,
                delay_after_each_physical_write_ms,
            } => {
                let chunk = (*logical_chunk).min(limit);
                let mut result = Ok(());
                for piece in bytes.chunks(chunk) {
                    if let Err(e) = write(t, piece, &mut p) {
                        result = Err(e);
                        break;
                    }
                    t.delay_monotonic(*delay_after_each_physical_write_ms)
                }
                result
            }
            Action::WaitForResponse {
                timeout_ms,
                fallback_delay_ms,
                validation,
            } => match t.wait_response(*timeout_ms) {
                Ok(WaitOutcome::Response(bytes)) => validate(*validation, &bytes, &p),
                Ok(WaitOutcome::Unavailable) if *fallback_delay_ms > 0 => {
                    t.delay_monotonic(*fallback_delay_ms);
                    Ok(())
                }
                Ok(WaitOutcome::Unavailable | WaitOutcome::Timeout) => Err(ExecuteError::Timeout {
                    progress: p.clone(),
                }),
                Err(message) => Err(fail(&p, message)),
            },
        };
        result?;
        p.last_completed_action = Some(i)
    }
    Ok(p)
}

/// Process-local single-attempt guard. Keys must be durable job IDs supplied by the caller.
#[derive(Debug, Default)]
pub struct ReplayGuard {
    attempted: BTreeSet<String>,
}
impl ReplayGuard {
    pub fn execute_once<T: Transport>(
        &mut self,
        key: &str,
        plan: &Plan,
        transport: &mut T,
    ) -> Result<Progress, ExecuteError> {
        if !self.attempted.insert(key.to_owned()) {
            return Err(ExecuteError::Replay(key.to_owned()));
        }
        execute(plan, transport)
    }
    pub fn was_attempted(&self, key: &str) -> bool {
        self.attempted.contains(key)
    }
}
fn write<T: Transport>(t: &mut T, b: &[u8], p: &mut Progress) -> Result<(), ExecuteError> {
    t.write(b).map_err(|message| fail(p, message))?;
    p.bytes_written += b.len() as u64;
    p.potentially_accepted_write = true;
    Ok(())
}
fn fail(p: &Progress, message: String) -> ExecuteError {
    ExecuteError::Transport {
        progress: p.clone(),
        message,
    }
}
fn validate(v: ResponseValidation, b: &[u8], p: &Progress) -> Result<(), ExecuteError> {
    match v {
        ResponseValidation::AnyNotification if !b.is_empty() => Ok(()),
        ResponseValidation::BrotherStatus32
            if b.len() >= 32 && b.starts_with(&[0x80, 0x20, 0x42]) =>
        {
            Ok(())
        }
        _ => Err(ExecuteError::Response {
            progress: p.clone(),
            message: "response did not match declared validator".into(),
        }),
    }
}

// SPDX-License-Identifier: AGPL-3.0-or-later
#![deny(unsafe_op_in_unsafe_fn)]
pub mod transports;
use mb_printer_core::protocol::{Action, Plan, ResponseValidation};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    Response(Vec<u8>),
    Timeout,
    Unavailable,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReferenceTiming {
    #[default]
    Preserve,
    /// Safely add time to every reference pacing/fallback delay.
    IncreaseBy(u64),
    /// Explicit diagnostic-only reduction. This must never be persisted as a default.
    UnsafeDiagnosticReduceBy(u64),
}
impl ReferenceTiming {
    fn apply(self, reference_ms: u64) -> u64 {
        match self {
            Self::Preserve => reference_ms,
            Self::IncreaseBy(extra) => reference_ms.saturating_add(extra),
            Self::UnsafeDiagnosticReduceBy(reduction) => reference_ms.saturating_sub(reduction),
        }
    }
}

pub trait Transport {
    /// Maximum physical raster write. This is normally the negotiated MTU or
    /// USB endpoint packet size.
    fn payload_limit(&self) -> usize;
    /// Maximum indivisible protocol command. Stream transports normally use
    /// the payload limit; USB may submit a larger bulk transfer which the host
    /// controller divides into endpoint packets.
    fn command_limit(&self) -> usize {
        self.payload_limit()
    }
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
    /// Every reply the printer returned, in action order. Status plans read the last one.
    pub responses: Vec<Vec<u8>>,
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
    #[error("transport failed after {progress:?}: {message}")]
    Transport { progress: Progress, message: String },
    #[error("response timed out after {progress:?}")]
    Timeout { progress: Progress },
    #[error("invalid response after {progress:?}: {message}")]
    Response { progress: Progress, message: String },
}
pub fn execute<T: Transport>(plan: &Plan, t: &mut T) -> Result<Progress, ExecuteError> {
    execute_with_timing(plan, t, ReferenceTiming::Preserve)
}
pub fn execute_with_timing<T: Transport>(
    plan: &Plan,
    t: &mut T,
    timing: ReferenceTiming,
) -> Result<Progress, ExecuteError> {
    let limit = t.payload_limit();
    let command_limit = t.command_limit();
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
            && bytes.len() > command_limit
        {
            return Err(ExecuteError::AtomicTooLarge {
                action: i,
                length: bytes.len(),
                limit: command_limit,
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
                t.delay_monotonic(timing.apply(*milliseconds));
                Ok(())
            }
            Action::CommandWrite { bytes, .. } => write(t, bytes, &mut p),
            Action::RasterWrite {
                bytes,
                logical_chunk,
                delay_after_each_physical_write_ms,
            } => {
                let mut result = Ok(());
                'logical: for logical in bytes.chunks(*logical_chunk) {
                    for piece in logical.chunks(limit) {
                        if let Err(e) = write(t, piece, &mut p) {
                            result = Err(e);
                            break 'logical;
                        }
                        t.delay_monotonic(timing.apply(*delay_after_each_physical_write_ms))
                    }
                }
                result
            }
            Action::WaitForResponse {
                timeout_ms,
                fallback_delay_ms,
                validation,
            } => match collect_response(t, *timeout_ms, *validation) {
                Ok(WaitOutcome::Response(bytes)) => {
                    let outcome = validate(*validation, &bytes, &p);
                    if outcome.is_ok() {
                        p.responses.push(bytes)
                    }
                    outcome
                }
                Ok(WaitOutcome::Unavailable) if *fallback_delay_ms > 0 => {
                    t.delay_monotonic(timing.apply(*fallback_delay_ms));
                    Ok(())
                }
                // Brother's reference drivers use the status request as a
                // best-effort preflight. Raw TCP print servers commonly do
                // not return it, but still accept the raster job. Validate a
                // reply when present and otherwise continue, matching the
                // JavaScript/Python implementations.
                Ok(WaitOutcome::Unavailable | WaitOutcome::Timeout)
                    if matches!(validation, ResponseValidation::BrotherStatus32) =>
                {
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

/// Bulk endpoints may split a reply across reads, so keep reading until the
/// declared validator has enough bytes or the printer stops answering.
fn collect_response<T: Transport>(
    t: &mut T,
    timeout_ms: u64,
    validation: ResponseValidation,
) -> Result<WaitOutcome, String> {
    let expected = match validation {
        ResponseValidation::BrotherStatus32 => 32,
        ResponseValidation::AnyNotification => 1,
    };
    let mut bytes: Vec<u8> = Vec::new();
    for _ in 0..16 {
        match t.wait_response(timeout_ms)? {
            WaitOutcome::Response(chunk) => {
                bytes.extend(chunk);
                if bytes.len() >= expected {
                    return Ok(WaitOutcome::Response(bytes));
                }
            }
            outcome => {
                return Ok(if bytes.is_empty() {
                    outcome
                } else {
                    WaitOutcome::Response(bytes)
                });
            }
        }
    }
    Ok(WaitOutcome::Response(bytes))
}

/// Process-local single-attempt guard. Keys must be durable job IDs supplied by the caller.
#[derive(Debug, Default)]
pub struct ReplayGuard {
    attempted: BTreeSet<String>,
}

pub trait ReplayStore {
    /// Atomically claims a key. Returns false when another process claimed it first.
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
/// Cross-process replay store using atomic `create_new` marker files.
#[derive(Debug, Clone)]
pub struct FileReplayStore {
    directory: PathBuf,
}
impl FileReplayStore {
    pub fn new(directory: impl AsRef<Path>) -> Result<Self, String> {
        std::fs::create_dir_all(directory.as_ref()).map_err(|error| error.to_string())?;
        Ok(Self {
            directory: directory.as_ref().to_owned(),
        })
    }
}
impl ReplayStore for FileReplayStore {
    fn claim(&mut self, key: &str) -> Result<bool, String> {
        let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.directory.join(digest))
        {
            Ok(mut file) => {
                file.write_all(key.as_bytes())
                    .and_then(|_| file.sync_all())
                    .map_err(|error| error.to_string())?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error.to_string()),
        }
    }
}
pub fn execute_once_with_store<T: Transport, S: ReplayStore>(
    store: &mut S,
    key: &str,
    plan: &Plan,
    transport: &mut T,
) -> Result<Progress, ExecuteError> {
    match store.claim(key).map_err(ExecuteError::ReplayStore)? {
        true => execute(plan, transport),
        false => Err(ExecuteError::Replay(key.to_owned())),
    }
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
    // Once the transport call begins, the printer may have accepted any prefix even if it errors.
    p.potentially_accepted_write = true;
    t.write(b).map_err(|message| fail(p, message))?;
    p.bytes_written += b.len() as u64;
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
            if b.len() == 32 && b.starts_with(&[0x80, 0x20, 0x42]) =>
        {
            Ok(())
        }
        _ => Err(ExecuteError::Response {
            progress: p.clone(),
            message: "response did not match declared validator".into(),
        }),
    }
}

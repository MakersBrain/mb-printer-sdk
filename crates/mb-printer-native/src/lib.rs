// SPDX-License-Identifier: AGPL-3.0-or-later
#![deny(unsafe_op_in_unsafe_fn)]
pub mod brother_settings;
pub mod transports;
#[cfg(any(feature = "ble", feature = "ipp", feature = "snmp"))]
pub mod workers;
use mb_printer_core::{
    capabilities::Protocol,
    protocol::{Action, Plan, ResponseValidation},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
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
    #[error("transport failed: {message}")]
    Transport { progress: Progress, message: String },
    #[error("response timed out")]
    Timeout { progress: Progress },
    #[error("invalid response: {message}")]
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
    let payload_limit = t.payload_limit();
    let command_limit = t.command_limit();
    let span = execution_span(
        plan.protocol,
        plan.actions.len(),
        payload_limit,
        command_limit,
        timing,
    );
    let transport_span = transport_lifecycle_span(&span, payload_limit, command_limit);
    let started = std::time::Instant::now();
    let result = {
        let _entered = span.enter();
        let _transport_entered = transport_span.enter();
        execute_with_timing_inner(plan, t, timing, payload_limit, command_limit)
    };
    let progress = match &result {
        Ok(progress) => Some(progress),
        Err(error) => error_progress(error),
    };
    if let Some(progress) = progress {
        if let Some(action) = progress.last_completed_action {
            span.record("last_completed_action", action);
        }
        span.record("bytes_written", progress.bytes_written);
        span.record("response_count", progress.responses.len());
    }
    span.record(
        "duration_ms",
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
    match &result {
        Ok(_) => {
            span.record("outcome", "completed");
            transport_span.record("outcome", "completed");
            tracing::debug!(parent: &transport_span, "native transport lifecycle completed");
            tracing::info!(parent: &span, "native plan execution completed");
        }
        Err(error) => {
            span.record("outcome", "failed");
            span.record("error_code", execute_error_code(error));
            transport_span.record("outcome", "failed");
            tracing::debug!(parent: &transport_span, "native transport lifecycle failed");
            tracing::warn!(parent: &span, "native plan execution failed");
        }
    }
    result
}

fn execute_with_timing_inner<T: Transport>(
    plan: &Plan,
    t: &mut T,
    timing: ReferenceTiming,
    limit: usize,
    command_limit: usize,
) -> Result<Progress, ExecuteError> {
    if limit == 0 {
        return Err(ExecuteError::InvalidPlan {
            action: 0,
            message: "zero transport payload limit",
        });
    }
    for (i, a) in plan.actions.iter().enumerate() {
        if let Action::WaitForResponse { validation, .. } = a
            && matches!(
                validation,
                ResponseValidation::BrotherObjbrnet
                    | ResponseValidation::BrotherWifiScan
                    | ResponseValidation::BrotherSystemReport
            )
        {
            return Err(ExecuteError::InvalidPlan {
                action: i,
                message: "variable-length validator requires response collection",
            });
        }
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
        if let Action::CollectResponse {
            timeout_ms,
            idle_timeout_ms,
            maximum_bytes,
            ..
        } = a
            && (*timeout_ms == 0 || *idle_timeout_ms == 0 || *maximum_bytes == 0)
        {
            return Err(ExecuteError::InvalidPlan {
                action: i,
                message: "response collection bounds must be positive",
            });
        }
    }
    let mut p = Progress::default();
    for (i, a) in plan.actions.iter().enumerate() {
        let result: Result<(), ExecuteError> = match a {
            Action::JobBoundary { .. } => Ok(()),
            Action::SubscribeNotifications => {
                let operation = transport_operation_span("subscribe", i);
                let result = {
                    let _entered = operation.enter();
                    t.subscribe_notifications()
                        .map_err(|message| fail(&p, message))
                };
                operation.record(
                    "outcome",
                    if result.is_ok() {
                        "completed"
                    } else {
                        "failed"
                    },
                );
                tracing::debug!(parent: &operation, "native transport operation finished");
                result
            }
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
            } => match collect_response_observed(t, *timeout_ms, *validation, i) {
                Ok(WaitOutcome::Response(bytes)) => {
                    let outcome = validate(*validation, &bytes, &p);
                    if outcome.is_ok() {
                        p.responses.push(bytes)
                    }
                    outcome
                }
                Ok(WaitOutcome::Unavailable | WaitOutcome::Timeout) if *fallback_delay_ms > 0 => {
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
            Action::CollectResponse {
                timeout_ms,
                idle_timeout_ms,
                maximum_bytes,
                validation,
            } => match collect_multipart_response_observed(
                t,
                *timeout_ms,
                *idle_timeout_ms,
                *maximum_bytes,
                i,
            ) {
                Ok(WaitOutcome::Response(bytes)) => {
                    let outcome = validate(*validation, &bytes, &p);
                    if outcome.is_ok() {
                        p.responses.push(bytes)
                    }
                    outcome
                }
                Ok(WaitOutcome::Timeout | WaitOutcome::Unavailable) => Err(ExecuteError::Timeout {
                    progress: p.clone(),
                }),
                Err(MultipartError::Transport(message)) => Err(fail(&p, message)),
                Err(MultipartError::Response(message)) => Err(ExecuteError::Response {
                    progress: p.clone(),
                    message: message.into(),
                }),
            },
        };
        result?;
        p.last_completed_action = Some(i)
    }
    Ok(p)
}

fn execution_span(
    protocol: Protocol,
    action_count: usize,
    payload_limit: usize,
    command_limit: usize,
    timing: ReferenceTiming,
) -> tracing::Span {
    tracing::info_span!(
        "native.plan.execute",
        protocol = protocol_name(protocol),
        action_count,
        payload_limit,
        command_limit,
        timing = timing_name(timing),
        outcome = tracing::field::Empty,
        error_code = tracing::field::Empty,
        last_completed_action = tracing::field::Empty,
        bytes_written = tracing::field::Empty,
        response_count = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
    )
}

fn transport_operation_span(operation: &'static str, action_index: usize) -> tracing::Span {
    tracing::debug_span!(
        "native.transport.operation",
        operation,
        action_index,
        outcome = tracing::field::Empty,
    )
}

fn transport_lifecycle_span(
    parent: &tracing::Span,
    payload_limit: usize,
    command_limit: usize,
) -> tracing::Span {
    tracing::info_span!(
        parent: parent,
        "native.transport.lifecycle",
        payload_limit,
        command_limit,
        outcome = tracing::field::Empty,
    )
}

const fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::MSeries => "m-series",
        Protocol::M02 => "m02",
        Protocol::M04 => "m04",
        Protocol::M110 => "m110",
        Protocol::DSeries => "d-series",
        Protocol::P12 => "p12",
        Protocol::Tspl => "tspl",
        Protocol::Brother => "brother",
    }
}

const fn timing_name(timing: ReferenceTiming) -> &'static str {
    match timing {
        ReferenceTiming::Preserve => "preserve",
        ReferenceTiming::IncreaseBy(_) => "increased",
        ReferenceTiming::UnsafeDiagnosticReduceBy(_) => "diagnostic-reduced",
    }
}

const fn execute_error_code(error: &ExecuteError) -> &'static str {
    match error {
        ExecuteError::AtomicTooLarge { .. } => "atomic-too-large",
        ExecuteError::InvalidPlan { .. } => "invalid-plan",
        ExecuteError::Replay(_) => "replay",
        ExecuteError::ReplayStore(_) => "replay-store",
        ExecuteError::Transport { .. } => "transport",
        ExecuteError::Timeout { .. } => "timeout",
        ExecuteError::Response { .. } => "response",
    }
}

fn error_progress(error: &ExecuteError) -> Option<&Progress> {
    match error {
        ExecuteError::Transport { progress, .. }
        | ExecuteError::Timeout { progress }
        | ExecuteError::Response { progress, .. } => Some(progress),
        ExecuteError::AtomicTooLarge { .. }
        | ExecuteError::InvalidPlan { .. }
        | ExecuteError::Replay(_)
        | ExecuteError::ReplayStore(_) => None,
    }
}

fn collect_response_observed<T: Transport>(
    t: &mut T,
    timeout_ms: u64,
    validation: ResponseValidation,
    action_index: usize,
) -> Result<WaitOutcome, String> {
    let operation = transport_operation_span("wait-response", action_index);
    let outcome = {
        let _entered = operation.enter();
        collect_response(t, timeout_ms, validation)
    };
    operation.record(
        "outcome",
        match &outcome {
            Ok(WaitOutcome::Response(_)) => "response",
            Ok(WaitOutcome::Timeout) => "timeout",
            Ok(WaitOutcome::Unavailable) => "unavailable",
            Err(_) => "failed",
        },
    );
    tracing::debug!(parent: &operation, "native transport operation finished");
    outcome
}

enum MultipartError {
    Transport(String),
    Response(&'static str),
}

const MAX_MULTIPART_READS: usize = 4096;

fn collect_multipart_response_observed<T: Transport>(
    t: &mut T,
    timeout_ms: u64,
    idle_timeout_ms: u64,
    maximum_bytes: usize,
    action_index: usize,
) -> Result<WaitOutcome, MultipartError> {
    let operation = transport_operation_span("collect-response", action_index);
    let outcome = {
        let _entered = operation.enter();
        collect_multipart_response(t, timeout_ms, idle_timeout_ms, maximum_bytes)
    };
    operation.record(
        "outcome",
        match &outcome {
            Ok(WaitOutcome::Response(_)) => "response",
            Ok(WaitOutcome::Timeout) => "timeout",
            Ok(WaitOutcome::Unavailable) => "unavailable",
            Err(_) => "failed",
        },
    );
    tracing::debug!(parent: &operation, "native transport operation finished");
    outcome
}

/// Collect a variable-length response until the transport goes quiet, while
/// bounding total time, retained bytes, and the number of backend calls.
fn collect_multipart_response<T: Transport>(
    t: &mut T,
    timeout_ms: u64,
    idle_timeout_ms: u64,
    maximum_bytes: usize,
) -> Result<WaitOutcome, MultipartError> {
    let started = Instant::now();
    let total = std::time::Duration::from_millis(timeout_ms);
    let mut bytes = Vec::new();
    let mut became_idle = false;

    for read in 0..MAX_MULTIPART_READS {
        let remaining = total.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        let remaining_ms = u64::try_from(remaining.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let wait_ms = if read == 0 {
            remaining_ms
        } else {
            idle_timeout_ms.min(remaining_ms)
        };
        match t
            .wait_response(wait_ms)
            .map_err(MultipartError::Transport)?
        {
            WaitOutcome::Response(chunk) if chunk.is_empty() => {
                became_idle = true;
                break;
            }
            WaitOutcome::Response(chunk) => {
                let new_length = bytes
                    .len()
                    .checked_add(chunk.len())
                    .ok_or(MultipartError::Response("response size overflow"))?;
                if new_length > maximum_bytes {
                    return Err(MultipartError::Response(
                        "response exceeded declared maximum",
                    ));
                }
                bytes.extend_from_slice(&chunk);
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

    if bytes.is_empty() {
        Ok(WaitOutcome::Timeout)
    } else if !became_idle && started.elapsed() < total {
        // Reaching the read ceiling while the transport is still producing
        // packets is an invalid, non-idle response rather than success.
        Err(MultipartError::Response(
            "response did not become idle before the read limit",
        ))
    } else {
        Ok(WaitOutcome::Response(bytes))
    }
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
        ResponseValidation::PhomemoNotification => 3,
        ResponseValidation::BrotherObjbrnet
        | ResponseValidation::BrotherWifiScan
        | ResponseValidation::BrotherSystemReport => usize::MAX,
    };
    let mut bytes: Vec<u8> = Vec::new();
    for _ in 0..16 {
        match t.wait_response(timeout_ms)? {
            WaitOutcome::Response(chunk) => {
                bytes.extend(chunk);
                if matches!(validation, ResponseValidation::PhomemoNotification) {
                    if let Some(start) = bytes.iter().position(|byte| *byte == 0x1a)
                        && bytes.len() - start >= expected
                    {
                        return Ok(WaitOutcome::Response(bytes.split_off(start)));
                    }
                    continue;
                }
                if bytes.len() >= expected {
                    return Ok(WaitOutcome::Response(bytes));
                }
            }
            outcome => {
                return Ok(
                    if bytes.is_empty()
                        || matches!(validation, ResponseValidation::PhomemoNotification)
                    {
                        outcome
                    } else {
                        WaitOutcome::Response(bytes)
                    },
                );
            }
        }
    }
    Ok(
        if matches!(validation, ResponseValidation::PhomemoNotification) {
            WaitOutcome::Timeout
        } else {
            WaitOutcome::Response(bytes)
        },
    )
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
        ResponseValidation::PhomemoNotification if b.len() >= 3 && b[0] == 0x1a => Ok(()),
        ResponseValidation::BrotherStatus32
            if b.len() == 32 && b.starts_with(&[0x80, 0x20, 0x42]) =>
        {
            Ok(())
        }
        ResponseValidation::BrotherObjbrnet if contains(b, b"OBJBRNET") => Ok(()),
        ResponseValidation::BrotherWifiScan
            if contains(b, b"AVAILABLEWLAN") || contains(b, b"VAP,") =>
        {
            Ok(())
        }
        ResponseValidation::BrotherSystemReport if contains(b, b"<<PRINTER CONFIGURATION>>") => {
            Ok(())
        }
        _ => Err(ExecuteError::Response {
            progress: p.clone(),
            message: "response did not match declared validator".into(),
        }),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod observability_tests {
    use super::*;
    use tracing::{
        Event, Metadata, Subscriber,
        span::{Attributes, Id, Record},
    };

    #[derive(Debug)]
    struct EnabledSubscriber;

    impl Subscriber for EnabledSubscriber {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, _: &Event<'_>) {}

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    fn field_names(span: &tracing::Span) -> Vec<&'static str> {
        span.metadata()
            .expect("test subscriber enables spans")
            .fields()
            .iter()
            .map(|field| field.name())
            .collect()
    }

    #[test]
    fn trace_fields_are_strict_safe_allowlists() {
        tracing::subscriber::with_default(EnabledSubscriber, || {
            assert_eq!(
                field_names(&execution_span(
                    Protocol::Brother,
                    4,
                    512,
                    512,
                    ReferenceTiming::Preserve,
                )),
                [
                    "protocol",
                    "action_count",
                    "payload_limit",
                    "command_limit",
                    "timing",
                    "outcome",
                    "error_code",
                    "last_completed_action",
                    "bytes_written",
                    "response_count",
                    "duration_ms",
                ]
            );
            assert_eq!(
                field_names(&transport_lifecycle_span(
                    &tracing::Span::current(),
                    512,
                    512,
                )),
                ["payload_limit", "command_limit", "outcome"]
            );
            assert_eq!(
                field_names(&transport_operation_span("wait-response", 2)),
                ["operation", "action_index", "outcome"]
            );
        });
    }

    #[test]
    fn execute_error_display_never_contains_progress_or_response_frames() {
        let progress = Progress {
            last_completed_action: Some(7),
            bytes_written: 12_345,
            potentially_accepted_write: true,
            responses: vec![vec![222, 173, 190, 239]],
        };
        let errors = [
            ExecuteError::Transport {
                progress: progress.clone(),
                message: "socket closed".into(),
            },
            ExecuteError::Timeout {
                progress: progress.clone(),
            },
            ExecuteError::Response {
                progress,
                message: "validator mismatch".into(),
            },
        ];

        for error in errors {
            let display = error.to_string();
            for forbidden in ["Progress", "responses", "222", "12345", "12_345"] {
                assert!(
                    !display.contains(forbidden),
                    "error Display leaked {forbidden}: {display}"
                );
            }
        }
    }
}

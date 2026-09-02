// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use futures_util::future::{Either, select};
use mb_printer_core::{
    capabilities::Protocol,
    protocol::{Action, Plan, ResponseValidation},
};
use web_time::Instant;

use crate::{
    Cancellation, ExecuteError, NeverCancelled, NotificationSupport, Transport, TransportError,
    TransportErrorKind, TransportFuture, WaitOutcome, WriteKind,
};

pub const MAX_PLAN_ACTIONS: usize = 16_384;
pub const MAX_RETAINED_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_FIXED_RESPONSE_READS: usize = 16;
const MAX_MULTIPART_READS: usize = 4096;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReferenceTiming {
    #[default]
    Preserve,
    IncreaseBy(u64),
    UnsafeDiagnosticReduceBy(u64),
}

impl ReferenceTiming {
    fn apply(self, reference_ms: u64) -> Duration {
        Duration::from_millis(match self {
            Self::Preserve => reference_ms,
            Self::IncreaseBy(extra) => reference_ms.saturating_add(extra),
            Self::UnsafeDiagnosticReduceBy(reduction) => reference_ms.saturating_sub(reduction),
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Progress {
    pub last_completed_action: Option<usize>,
    pub bytes_written: u64,
    pub potentially_accepted_write: bool,
    pub responses: Vec<Vec<u8>>,
}

#[derive(Clone, Copy)]
pub struct ExecutionOptions<'a> {
    pub timing: ReferenceTiming,
    pub cancellation: &'a dyn Cancellation,
}

pub async fn execute<T>(plan: &Plan, transport: &mut T) -> Result<Progress, ExecuteError>
where
    T: Transport + ?Sized,
{
    execute_with_options(
        plan,
        transport,
        ExecutionOptions {
            timing: ReferenceTiming::Preserve,
            cancellation: &NeverCancelled,
        },
        |_| {},
    )
    .await
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn execute_with_options<T, F>(
    plan: &Plan,
    transport: &mut T,
    options: ExecutionOptions<'_>,
    mut progress: F,
) -> Result<Progress, ExecuteError>
where
    T: Transport + ?Sized,
    F: FnMut(&Progress) + Send,
{
    execute_inner(plan, transport, options, &mut progress).await
}

#[cfg(target_arch = "wasm32")]
pub async fn execute_with_options<T, F>(
    plan: &Plan,
    transport: &mut T,
    options: ExecutionOptions<'_>,
    mut progress: F,
) -> Result<Progress, ExecuteError>
where
    T: Transport + ?Sized,
    F: FnMut(&Progress),
{
    execute_inner(plan, transport, options, &mut progress).await
}

#[cfg(not(target_arch = "wasm32"))]
type ProgressCallback<'a> = dyn FnMut(&Progress) + Send + 'a;
#[cfg(target_arch = "wasm32")]
type ProgressCallback<'a> = dyn FnMut(&Progress) + 'a;

async fn execute_inner<T: Transport + ?Sized>(
    plan: &Plan,
    transport: &mut T,
    options: ExecutionOptions<'_>,
    progress_callback: &mut ProgressCallback<'_>,
) -> Result<Progress, ExecuteError> {
    let payload_limit = transport.payload_limit();
    let command_limit = transport.command_limit();
    preflight(plan, payload_limit, command_limit)?;

    let span = execution_span(
        plan.protocol,
        plan.actions.len(),
        payload_limit,
        command_limit,
        options.timing,
    );
    let transport_span = transport_lifecycle_span(&span, payload_limit, command_limit);
    let started = Instant::now();
    let result = execute_actions(
        plan,
        transport,
        options,
        payload_limit,
        progress_callback,
        &transport_span,
    )
    .await;
    record_execution_result(&span, started, &result);
    transport_span.record(
        "outcome",
        if result.is_ok() {
            "completed"
        } else {
            "failed"
        },
    );
    result
}

fn preflight(plan: &Plan, payload_limit: usize, command_limit: usize) -> Result<(), ExecuteError> {
    if payload_limit == 0 {
        return Err(invalid(0, "zero transport payload limit"));
    }
    if command_limit == 0 {
        return Err(invalid(0, "zero transport command limit"));
    }
    if plan.actions.len() > MAX_PLAN_ACTIONS {
        return Err(invalid(0, "plan action count exceeds executor limit"));
    }

    let mut retained = 0usize;
    for (index, action) in plan.actions.iter().enumerate() {
        match action {
            Action::CommandWrite {
                bytes,
                atomic: true,
                ..
            } if bytes.len() > command_limit => {
                return Err(ExecuteError::AtomicTooLarge {
                    action: index,
                    length: bytes.len(),
                    limit: command_limit,
                });
            }
            Action::RasterWrite {
                logical_chunk: 0, ..
            } => return Err(invalid(index, "zero logical raster chunk")),
            Action::WaitForResponse { timeout_ms: 0, .. } => {
                return Err(invalid(index, "response timeout must be positive"));
            }
            Action::WaitForResponse { validation, .. } if is_variable_length(*validation) => {
                return Err(invalid(
                    index,
                    "variable-length validator requires response collection",
                ));
            }
            Action::CollectResponse {
                timeout_ms,
                idle_timeout_ms,
                maximum_bytes,
                validation: _,
            } => {
                if *timeout_ms == 0 || *idle_timeout_ms == 0 || *maximum_bytes == 0 {
                    return Err(invalid(
                        index,
                        "response collection bounds must be positive",
                    ));
                }
                retained = retained
                    .checked_add(*maximum_bytes)
                    .ok_or_else(|| invalid(index, "retained response byte limit overflow"))?;
                if retained > MAX_RETAINED_RESPONSE_BYTES {
                    return Err(invalid(
                        index,
                        "retained response bytes exceed executor limit",
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

const fn invalid(action: usize, message: &'static str) -> ExecuteError {
    ExecuteError::InvalidPlan { action, message }
}

const fn is_variable_length(validation: ResponseValidation) -> bool {
    matches!(
        validation,
        ResponseValidation::BrotherObjbrnet
            | ResponseValidation::BrotherWifiScan
            | ResponseValidation::BrotherSystemReport
    )
}

async fn execute_actions<T: Transport + ?Sized>(
    plan: &Plan,
    transport: &mut T,
    options: ExecutionOptions<'_>,
    payload_limit: usize,
    progress_callback: &mut ProgressCallback<'_>,
    transport_span: &tracing::Span,
) -> Result<Progress, ExecuteError> {
    let mut progress = Progress::default();

    for (index, action) in plan.actions.iter().enumerate() {
        check_cancelled(options.cancellation, &progress)?;
        match action {
            Action::JobBoundary { .. } => {}
            Action::SubscribeNotifications => {
                let operation = transport_operation_span(transport_span, "subscribe", index);
                let result = race(transport.subscribe_notifications(), options.cancellation).await;
                record_operation(
                    &operation,
                    match &result {
                        Race::Cancelled => "cancelled",
                        Race::Completed(Ok(_)) => "completed",
                        Race::Completed(Err(_)) => "failed",
                    },
                );
                match result {
                    Race::Cancelled => return Err(cancelled(&progress)),
                    Race::Completed(Ok(
                        NotificationSupport::Subscribed | NotificationSupport::Unavailable,
                    )) => {}
                    Race::Completed(Err(error)) => {
                        return Err(non_write_error(&progress, error));
                    }
                }
            }
            Action::Delay { milliseconds } => {
                if matches!(
                    race(
                        transport.delay(options.timing.apply(*milliseconds)),
                        options.cancellation,
                    )
                    .await,
                    Race::Cancelled
                ) {
                    return Err(cancelled(&progress));
                }
            }
            Action::CommandWrite { bytes, .. } => {
                write_piece(
                    transport,
                    bytes,
                    WriteKind::Command,
                    options.cancellation,
                    &mut progress,
                )
                .await?;
            }
            Action::RasterWrite {
                bytes,
                logical_chunk,
                delay_after_each_physical_write_ms,
            } => {
                for logical in bytes.chunks(*logical_chunk) {
                    for piece in logical.chunks(payload_limit) {
                        check_cancelled(options.cancellation, &progress)?;
                        write_piece(
                            transport,
                            piece,
                            WriteKind::Raster,
                            options.cancellation,
                            &mut progress,
                        )
                        .await?;
                        if matches!(
                            race(
                                transport.delay(
                                    options.timing.apply(*delay_after_each_physical_write_ms,)
                                ),
                                options.cancellation,
                            )
                            .await,
                            Race::Cancelled
                        ) {
                            return Err(cancelled(&progress));
                        }
                    }
                }
            }
            Action::WaitForResponse {
                timeout_ms,
                fallback_delay_ms,
                validation,
            } => {
                let operation = transport_operation_span(transport_span, "wait-response", index);
                let result = collect_response(
                    transport,
                    Duration::from_millis(*timeout_ms),
                    *validation,
                    options.cancellation,
                    retained_bytes(&progress),
                )
                .await;
                record_operation(&operation, effect_outcome(&result));
                let outcome = result.map_err(|error| response_effect_error(&progress, error))?;
                match outcome {
                    WaitOutcome::Response(bytes) => {
                        validate(*validation, &bytes, &progress)?;
                        progress.responses.push(bytes);
                    }
                    WaitOutcome::Unavailable | WaitOutcome::Timeout if *fallback_delay_ms > 0 => {
                        if matches!(
                            race(
                                transport.delay(options.timing.apply(*fallback_delay_ms)),
                                options.cancellation,
                            )
                            .await,
                            Race::Cancelled
                        ) {
                            return Err(cancelled(&progress));
                        }
                    }
                    WaitOutcome::Unavailable | WaitOutcome::Timeout
                        if matches!(validation, ResponseValidation::BrotherStatus32) => {}
                    WaitOutcome::Unavailable | WaitOutcome::Timeout => {
                        return Err(ExecuteError::Timeout {
                            progress: progress.clone(),
                        });
                    }
                }
            }
            Action::CollectResponse {
                timeout_ms,
                idle_timeout_ms,
                maximum_bytes,
                validation,
            } => {
                let operation = transport_operation_span(transport_span, "collect-response", index);
                let result = collect_multipart_response(
                    transport,
                    Duration::from_millis(*timeout_ms),
                    Duration::from_millis(*idle_timeout_ms),
                    *maximum_bytes,
                    options.cancellation,
                )
                .await;
                record_operation(&operation, effect_outcome(&result));
                let outcome = result.map_err(|error| response_effect_error(&progress, error))?;
                match outcome {
                    WaitOutcome::Response(bytes) => {
                        validate(*validation, &bytes, &progress)?;
                        progress.responses.push(bytes);
                    }
                    WaitOutcome::Timeout | WaitOutcome::Unavailable => {
                        return Err(ExecuteError::Timeout {
                            progress: progress.clone(),
                        });
                    }
                }
            }
        }
        progress.last_completed_action = Some(index);
        progress_callback(&progress);
    }
    Ok(progress)
}

fn retained_bytes(progress: &Progress) -> usize {
    progress.responses.iter().map(Vec::len).sum()
}

fn check_cancelled(
    cancellation: &dyn Cancellation,
    progress: &Progress,
) -> Result<(), ExecuteError> {
    if cancellation.is_cancelled() {
        Err(cancelled(progress))
    } else {
        Ok(())
    }
}

async fn write_piece<T: Transport + ?Sized>(
    transport: &mut T,
    bytes: &[u8],
    kind: WriteKind,
    cancellation: &dyn Cancellation,
    progress: &mut Progress,
) -> Result<(), ExecuteError> {
    check_cancelled(cancellation, progress)?;
    progress.potentially_accepted_write = true;
    match race(transport.write(bytes, kind), cancellation).await {
        Race::Cancelled => Err(ExecuteError::WriteOutcomeUnknown {
            progress: progress.clone(),
            source: None,
        }),
        Race::Completed(Err(source)) => Err(ExecuteError::WriteOutcomeUnknown {
            progress: progress.clone(),
            source: Some(source),
        }),
        Race::Completed(Ok(())) => {
            progress.bytes_written = progress
                .bytes_written
                .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            Ok(())
        }
    }
}

enum Race<T> {
    Completed(T),
    Cancelled,
}

async fn race<'a, T>(
    operation: TransportFuture<'a, T>,
    cancellation: &'a dyn Cancellation,
) -> Race<T> {
    match select(operation, cancellation.cancelled()).await {
        Either::Left((value, _)) => Race::Completed(value),
        Either::Right(((), _)) => Race::Cancelled,
    }
}

enum EffectError {
    Cancelled,
    Transport(TransportError),
    Response(&'static str),
}

fn effect_outcome(result: &Result<WaitOutcome, EffectError>) -> &'static str {
    match result {
        Ok(WaitOutcome::Response(_)) => "response",
        Ok(WaitOutcome::Timeout) => "timeout",
        Ok(WaitOutcome::Unavailable) => "unavailable",
        Err(EffectError::Cancelled) => "cancelled",
        Err(EffectError::Transport(_) | EffectError::Response(_)) => "failed",
    }
}

async fn wait<T: Transport + ?Sized>(
    transport: &mut T,
    timeout: Duration,
    cancellation: &dyn Cancellation,
) -> Result<WaitOutcome, EffectError> {
    match race(transport.wait_response(timeout), cancellation).await {
        Race::Cancelled => Err(EffectError::Cancelled),
        Race::Completed(Err(error)) if error.kind == TransportErrorKind::Cancelled => {
            Err(EffectError::Cancelled)
        }
        Race::Completed(Err(error)) => Err(EffectError::Transport(error)),
        Race::Completed(Ok(outcome)) => Ok(outcome),
    }
}

async fn collect_response<T: Transport + ?Sized>(
    transport: &mut T,
    timeout: Duration,
    validation: ResponseValidation,
    cancellation: &dyn Cancellation,
    already_retained: usize,
) -> Result<WaitOutcome, EffectError> {
    let expected = match validation {
        ResponseValidation::BrotherStatus32 => 32,
        ResponseValidation::AnyNotification => 1,
        ResponseValidation::PhomemoNotification => 3,
        _ => usize::MAX,
    };
    let started = Instant::now();
    let mut bytes = Vec::new();
    for read in 0..MAX_FIXED_RESPONSE_READS {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Ok(
                if bytes.is_empty() || matches!(validation, ResponseValidation::PhomemoNotification)
                {
                    WaitOutcome::Timeout
                } else {
                    WaitOutcome::Response(bytes)
                },
            );
        }
        let wait_timeout = if read == 0 { timeout } else { remaining };
        match wait(transport, wait_timeout, cancellation).await? {
            WaitOutcome::Response(chunk) => {
                let retained = already_retained
                    .checked_add(bytes.len())
                    .and_then(|value| value.checked_add(chunk.len()))
                    .ok_or(EffectError::Response("response size overflow"))?;
                if retained > MAX_RETAINED_RESPONSE_BYTES {
                    return Err(EffectError::Response(
                        "retained response bytes exceed executor limit",
                    ));
                }
                bytes.extend(chunk);
                if matches!(validation, ResponseValidation::PhomemoNotification) {
                    if let Some(start) = bytes.iter().position(|byte| *byte == 0x1a)
                        && bytes.len() - start >= expected
                    {
                        return Ok(WaitOutcome::Response(bytes.split_off(start)));
                    }
                } else if bytes.len() >= expected {
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

async fn collect_multipart_response<T: Transport + ?Sized>(
    transport: &mut T,
    timeout: Duration,
    idle_timeout: Duration,
    maximum_bytes: usize,
    cancellation: &dyn Cancellation,
) -> Result<WaitOutcome, EffectError> {
    let started = Instant::now();
    let mut bytes = Vec::new();
    let mut became_idle = false;

    for read in 0..MAX_MULTIPART_READS {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        let wait_timeout = if read == 0 {
            timeout
        } else {
            idle_timeout.min(remaining)
        };
        match wait(transport, wait_timeout, cancellation).await? {
            WaitOutcome::Response(chunk) if chunk.is_empty() => {
                became_idle = true;
                break;
            }
            WaitOutcome::Response(chunk) => {
                let new_length = bytes
                    .len()
                    .checked_add(chunk.len())
                    .ok_or(EffectError::Response("response size overflow"))?;
                if new_length > maximum_bytes {
                    return Err(EffectError::Response("response exceeded declared maximum"));
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
    } else if !became_idle && started.elapsed() < timeout {
        Err(EffectError::Response(
            "response did not become idle before the read limit",
        ))
    } else {
        Ok(WaitOutcome::Response(bytes))
    }
}

fn validate(
    validation: ResponseValidation,
    bytes: &[u8],
    progress: &Progress,
) -> Result<(), ExecuteError> {
    let valid = match validation {
        ResponseValidation::AnyNotification => !bytes.is_empty(),
        ResponseValidation::PhomemoNotification => bytes.len() >= 3 && bytes[0] == 0x1a,
        ResponseValidation::BrotherStatus32 => {
            bytes.len() == 32 && bytes.starts_with(&[0x80, 0x20, 0x42])
        }
        ResponseValidation::BrotherObjbrnet => contains(bytes, b"OBJBRNET"),
        ResponseValidation::BrotherWifiScan => {
            contains(bytes, b"AVAILABLEWLAN") || contains(bytes, b"VAP,")
        }
        ResponseValidation::BrotherSystemReport => contains(bytes, b"<<PRINTER CONFIGURATION>>"),
    };
    if valid {
        Ok(())
    } else {
        Err(ExecuteError::Response {
            progress: progress.clone(),
            message: "response did not match declared validator".into(),
        })
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn cancelled(progress: &Progress) -> ExecuteError {
    ExecuteError::Cancelled {
        progress: progress.clone(),
    }
}

fn non_write_error(progress: &Progress, source: TransportError) -> ExecuteError {
    if source.kind == TransportErrorKind::Cancelled {
        cancelled(progress)
    } else {
        ExecuteError::Transport {
            progress: progress.clone(),
            source,
        }
    }
}

fn response_effect_error(progress: &Progress, error: EffectError) -> ExecuteError {
    match error {
        EffectError::Cancelled => cancelled(progress),
        EffectError::Transport(source) => non_write_error(progress, source),
        EffectError::Response(message) => ExecuteError::Response {
            progress: progress.clone(),
            message: message.into(),
        },
    }
}

fn execution_span(
    protocol: Protocol,
    action_count: usize,
    payload_limit: usize,
    command_limit: usize,
    timing: ReferenceTiming,
) -> tracing::Span {
    tracing::info_span!(
        "printer.plan.execute",
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

fn transport_lifecycle_span(
    parent: &tracing::Span,
    payload_limit: usize,
    command_limit: usize,
) -> tracing::Span {
    tracing::info_span!(
        parent: parent,
        "printer.transport.lifecycle",
        payload_limit,
        command_limit,
        outcome = tracing::field::Empty,
    )
}

fn transport_operation_span(
    parent: &tracing::Span,
    operation: &'static str,
    action_index: usize,
) -> tracing::Span {
    tracing::debug_span!(
        parent: parent,
        "printer.transport.operation",
        operation,
        action_index,
        outcome = tracing::field::Empty,
    )
}

fn record_operation(span: &tracing::Span, outcome: &'static str) {
    span.record("outcome", outcome);
}

fn record_execution_result(
    span: &tracing::Span,
    started: Instant,
    result: &Result<Progress, ExecuteError>,
) {
    let progress = match result {
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
    match result {
        Ok(_) => {
            span.record("outcome", "completed");
        }
        Err(error) => {
            span.record("outcome", "failed");
            span.record("error_code", execute_error_code(error));
        }
    }
}

fn error_progress(error: &ExecuteError) -> Option<&Progress> {
    match error {
        ExecuteError::Cancelled { progress }
        | ExecuteError::WriteOutcomeUnknown { progress, .. }
        | ExecuteError::Transport { progress, .. }
        | ExecuteError::Timeout { progress }
        | ExecuteError::Response { progress, .. } => Some(progress),
        _ => None,
    }
}

const fn execute_error_code(error: &ExecuteError) -> &'static str {
    match error {
        ExecuteError::AtomicTooLarge { .. } => "atomic-too-large",
        ExecuteError::InvalidPlan { .. } => "invalid-plan",
        ExecuteError::Replay(_) => "replay",
        ExecuteError::ReplayStore(_) => "replay-store",
        ExecuteError::Cancelled { .. } => "cancelled",
        ExecuteError::WriteOutcomeUnknown { .. } => "write-outcome-unknown",
        ExecuteError::Transport { .. } => "transport",
        ExecuteError::Timeout { .. } => "timeout",
        ExecuteError::Response { .. } => "response",
    }
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

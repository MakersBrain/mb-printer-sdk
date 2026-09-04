// SPDX-License-Identifier: AGPL-3.0-or-later
//! Policy-enforcing execution for authenticated printer-agent requests.
//!
//! The agent validates short-lived wire requests, resolves only locally published
//! printers, applies output and write policy, and delegates native IPP I/O to
//! [`mb_printer_native`]. Callers retain ownership of the Tokio runtime.
#![forbid(unsafe_code)]

mod administration_execution;
mod inspect_execution;
mod probe_execution;

use mb_printer_agent_proto::{
    ContractError,
    v1::{
        AgentCapabilities, ApplyChange, EvidenceOrigin, OperationKind,
        OutputMode as WireOutputMode, PlanChange, ProtocolRequest, ProtocolRequestAccepted,
        ProtocolRequestRejected, ProtocolResult, PublishedPrinter as WirePublishedPrinter,
        RejectionReason, ResultOutcome, protocol_request,
    },
    validate_initial_release_request, validate_request,
};
use mb_printer_core::discovery::{
    ObservationOrigin, OutputAuthorization, OutputMode, ProtocolFamily, TransportKind,
    normalize_ipp, prepare_snapshot_output, redact_identifier,
};
use mb_printer_core::{
    administration::{
        ChangeBinding, PlanChangeRequest, confirmed_ipp_plan_from_wire, ipp_value_hash,
    },
    ipp::{self, Value},
    probe::{
        ProbeExecutionReport, ProbeLimits, ProbeRegistry, ProbeRequest as PreparedProbeRequest,
        build_read_only_probe_report, prepare_registered_probe,
    },
};
use mb_printer_native::Cancellation;
pub use mb_printer_native::CancellationToken;
use mb_printer_native::transports::ipp::{
    ApplyChangeOutcome, InspectLimits, IppClient, IppClientError, IppEndpoint, IppScheme,
    PlanChangeError as NativePlanChangeError,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, PoisonError, RwLock},
    time::Duration,
};
use std::{future::Future, pin::Pin};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPolicy {
    pub agent_id: String,
    pub contract_version: u32,
    pub maximum_timeout_ms: u64,
    pub maximum_response_bytes: u64,
    pub allow_cloud_raw_redacted: bool,
    pub allow_cloud_raw_sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedPrinter {
    pub printer_id: String,
    pub endpoint_generation: u64,
    pub endpoint: IppEndpoint,
}

#[derive(Debug)]
pub enum InitialExecution {
    Rejected(ProtocolRequestRejected),
    Accepted {
        accepted: ProtocolRequestAccepted,
        result: ProtocolResult,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedWritePolicy {
    /// Persistent IPP attributes separately approved by local policy.
    pub allowed_settings: std::collections::BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudChangeReceipt {
    pub printer_id: String,
    pub endpoint_generation: u64,
    pub setting: String,
    pub expected_old_value_hash: [u8; 32],
    pub expected_requested_value_hash: [u8; 32],
    pub principal: String,
    pub protocol: ProtocolFamily,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone)]
pub struct PublishedProbeTarget {
    pub printer_id: String,
    pub endpoint_generation: u64,
    pub endpoint_identity: String,
    pub transport: TransportKind,
    pub protocol: ProtocolFamily,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub firmware: Option<String>,
    pub printer_definition: Option<mb_printer_core::capabilities::PrinterDefinition>,
}

pub type ProbeRunnerFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProbeRunOutput, String>> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRunOutput {
    pub response: Vec<u8>,
    pub duration_ms: u64,
}

pub trait RegisteredProbeRunner: Send + Sync {
    fn run(&self, request: PreparedProbeRequest, limits: ProbeLimits) -> ProbeRunnerFuture<'_>;
}

#[derive(Debug, Error)]
pub enum AgentBuildError {
    #[error("agent policy identifiers and limits must be non-empty and positive")]
    InvalidPolicy,
    #[error("failed to construct IPP client: {0}")]
    Client(#[from] IppClientError),
}

#[derive(Debug)]
pub struct AgentExecutor {
    policy: AgentPolicy,
    client: IppClient,
    printers: RwLock<BTreeMap<String, PublishedPrinter>>,
    guarded_write_requests: Mutex<BTreeSet<String>>,
}

impl AgentExecutor {
    pub fn new(policy: AgentPolicy) -> Result<Self, AgentBuildError> {
        if policy.agent_id.is_empty()
            || policy.contract_version == 0
            || policy.maximum_timeout_ms == 0
            || policy.maximum_response_bytes < 8
        {
            return Err(AgentBuildError::InvalidPolicy);
        }
        Ok(Self {
            policy,
            client: IppClient::new()?,
            printers: RwLock::new(BTreeMap::new()),
            guarded_write_requests: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn with_client(policy: AgentPolicy, client: IppClient) -> Result<Self, AgentBuildError> {
        if policy.agent_id.is_empty()
            || policy.contract_version == 0
            || policy.maximum_timeout_ms == 0
            || policy.maximum_response_bytes < 8
        {
            return Err(AgentBuildError::InvalidPolicy);
        }
        Ok(Self {
            policy,
            client,
            printers: RwLock::new(BTreeMap::new()),
            guarded_write_requests: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn publish(&self, printer: PublishedPrinter) -> Result<(), &'static str> {
        if printer.printer_id.is_empty() || printer.endpoint_generation == 0 {
            return Err("published printer identity and generation must be valid");
        }
        self.printers
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(printer.printer_id.clone(), printer);
        Ok(())
    }

    pub fn capabilities(&self) -> AgentCapabilities {
        AgentCapabilities {
            agent_id: self.policy.agent_id.clone(),
            contract_versions: vec![self.policy.contract_version],
            operations: vec![OperationKind::IppInspect as i32],
            transports: vec!["ipp".into(), "ipps".into()],
            maximum_response_bytes: self.policy.maximum_response_bytes,
            maximum_timeout_ms: self.policy.maximum_timeout_ms,
            supports_redaction: true,
            printers: self
                .printers
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .values()
                .map(|printer| WirePublishedPrinter {
                    printer_id: printer.printer_id.clone(),
                    endpoint_generation: printer.endpoint_generation,
                    operations: vec![OperationKind::IppInspect as i32],
                })
                .collect(),
        }
    }

    /// Capability advertisement for the separately enabled guarded-write
    /// phase. Empty local policy never advertises write operations.
    pub fn guarded_ipp_capabilities(&self, policy: &GuardedWritePolicy) -> AgentCapabilities {
        let mut capabilities = self.capabilities();
        if policy.allowed_settings.is_empty() {
            return capabilities;
        }
        capabilities.operations.extend([
            OperationKind::PlanChange as i32,
            OperationKind::ApplyChange as i32,
        ]);
        for printer in &mut capabilities.printers {
            printer.operations.extend([
                OperationKind::PlanChange as i32,
                OperationKind::ApplyChange as i32,
            ]);
        }
        capabilities
    }

    pub fn registered_probe_capabilities(&self, registry: &ProbeRegistry) -> AgentCapabilities {
        let mut capabilities = self.capabilities();
        if registry.iter().next().is_none() {
            return capabilities;
        }
        capabilities.operations.push(OperationKind::RunProbe as i32);
        for printer in &mut capabilities.printers {
            printer.operations.push(OperationKind::RunProbe as i32);
        }
        capabilities
    }
}

fn probe_result(
    request_id: &str,
    report: &ProbeExecutionReport,
    maximum_response_bytes: usize,
) -> ProtocolResult {
    bounded_json_result(request_id, report, maximum_response_bytes)
}

fn bounded_json_result<T: serde::Serialize>(
    request_id: &str,
    value: &T,
    maximum_response_bytes: usize,
) -> ProtocolResult {
    match serde_json::to_vec(value) {
        Ok(bytes) if bytes.len() <= maximum_response_bytes => ProtocolResult {
            request_id: request_id.into(),
            outcome: ResultOutcome::Succeeded as i32,
            bounded_response: bytes,
            evidence: Vec::new(),
            output_mode: WireOutputMode::NormalizedRedacted as i32,
            safe_error: String::new(),
            persistence_allowed: false,
            logging_allowed: false,
        },
        Ok(_) => terminal(
            request_id,
            ResultOutcome::ResponseTooLarge,
            "response exceeds configured limit",
        ),
        Err(_) => terminal(
            request_id,
            ResultOutcome::Failed,
            "response serialization failed",
        ),
    }
}

fn success_empty(request_id: &str) -> ProtocolResult {
    bounded_json_result(request_id, &serde_json::json!({ "verified": true }), 1024)
}

fn safe_client_error(error: &IppClientError) -> (ResultOutcome, &'static str) {
    match error {
        IppClientError::Timeout => (ResultOutcome::TimedOut, "IPP request timed out"),
        IppClientError::ResponseTooLarge { .. } => (
            ResultOutcome::ResponseTooLarge,
            "IPP response exceeded the configured limit",
        ),
        IppClientError::Decode(_) => (ResultOutcome::Failed, "IPP response was malformed"),
        IppClientError::RequestIdMismatch { .. } => {
            (ResultOutcome::Failed, "IPP response correlation failed")
        }
        _ => (ResultOutcome::Failed, "IPP transport failed"),
    }
}

fn rejection(request_id: &str, error: ContractError) -> ProtocolRequestRejected {
    let reason = match error {
        ContractError::Expired => RejectionReason::Expired,
        ContractError::InvalidLimits => RejectionReason::LimitExceeded,
        ContractError::UnsupportedOperation | ContractError::InitialReleaseIppInspectOnly => {
            RejectionReason::UnsupportedOperation
        }
        ContractError::MissingIdentity | ContractError::MissingOperation => RejectionReason::Policy,
    };
    reject(request_id.into(), reason, &error.to_string())
}

fn reject(request_id: String, reason: RejectionReason, message: &str) -> ProtocolRequestRejected {
    ProtocolRequestRejected {
        request_id,
        reason: reason as i32,
        safe_message: message.into(),
    }
}

fn terminal(request_id: &str, outcome: ResultOutcome, message: &str) -> ProtocolResult {
    ProtocolResult {
        request_id: request_id.into(),
        outcome: outcome as i32,
        bounded_response: Vec::new(),
        evidence: Vec::new(),
        output_mode: WireOutputMode::NormalizedRedacted as i32,
        safe_error: message.into(),
        persistence_allowed: false,
        logging_allowed: false,
    }
}

fn output_authorized(mode: i32, policy: &AgentPolicy) -> bool {
    match WireOutputMode::try_from(mode).ok() {
        Some(WireOutputMode::NormalizedRedacted) => true,
        Some(WireOutputMode::RawRedacted) => policy.allow_cloud_raw_redacted,
        Some(WireOutputMode::RawSensitive) => policy.allow_cloud_raw_sensitive,
        None => false,
    }
}

fn output_policy(mode: i32, policy: &AgentPolicy) -> Option<(OutputMode, OutputAuthorization)> {
    Some(match WireOutputMode::try_from(mode).ok()? {
        WireOutputMode::NormalizedRedacted => (
            OutputMode::NormalizedRedacted,
            OutputAuthorization::default(),
        ),
        WireOutputMode::RawRedacted => (
            OutputMode::LocalRawRedacted,
            OutputAuthorization {
                raw_local: policy.allow_cloud_raw_redacted,
                ..OutputAuthorization::default()
            },
        ),
        WireOutputMode::RawSensitive => (
            OutputMode::CloudRawAuthorized,
            OutputAuthorization {
                sensitive_values: policy.allow_cloud_raw_sensitive,
                cloud_raw: policy.allow_cloud_raw_sensitive,
                ..OutputAuthorization::default()
            },
        ),
    })
}

fn request_number(request_id: &str) -> u32 {
    request_id.bytes().fold(2_166_136_261u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    })
}

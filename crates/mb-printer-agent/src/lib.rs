// SPDX-License-Identifier: AGPL-3.0-or-later
#![forbid(unsafe_code)]

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
use mb_printer_native::transports::ipp::{
    ApplyChangeOutcome, InspectLimits, IppClient, IppClientError, IppEndpoint, IppScheme,
    PlanChangeError as NativePlanChangeError,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use std::{future::Future, pin::Pin};
use thiserror::Error;
use tokio::sync::Notify;

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

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: std::sync::atomic::AtomicBool,
    notify: Notify,
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<CancellationState>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0
            .cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        self.0.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(std::sync::atomic::Ordering::Acquire)
    }

    async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.0.notify.notified().await;
    }
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
            .expect("published-printer registry lock poisoned")
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
                .expect("published-printer registry lock poisoned")
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

    pub async fn execute_initial(
        &self,
        request: ProtocolRequest,
        now_unix_ms: u64,
        cancellation: CancellationToken,
    ) -> InitialExecution {
        if let Err(error) = validate_request(
            &request,
            now_unix_ms,
            self.policy.maximum_timeout_ms,
            self.policy.maximum_response_bytes,
            &[OperationKind::IppInspect],
        )
        .and_then(|()| validate_initial_release_request(&request))
        {
            return InitialExecution::Rejected(rejection(&request.request_id, error));
        }
        if request.contract_version != self.policy.contract_version {
            return InitialExecution::Rejected(ProtocolRequestRejected {
                request_id: request.request_id,
                reason: RejectionReason::UnsupportedVersion as i32,
                safe_message: "unsupported contract version".into(),
            });
        }
        let printer = self
            .printers
            .read()
            .expect("published-printer registry lock poisoned")
            .get(&request.printer_id)
            .cloned();
        let Some(printer) = printer else {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::UnknownPrinter,
                "unknown published printer",
            ));
        };
        if printer.endpoint_generation != request.endpoint_generation {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::StaleEndpoint,
                "stale endpoint generation",
            ));
        }
        let protocol_request::Operation::IppInspect(operation) =
            request.operation.clone().expect("validated operation")
        else {
            unreachable!("initial-release validation permits only IppInspect")
        };
        if !output_authorized(operation.output_mode, &self.policy) {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::Unauthorized,
                "requested output mode is not authorized",
            ));
        }
        let accepted = ProtocolRequestAccepted {
            request_id: request.request_id.clone(),
            accepted_at_unix_ms: now_unix_ms,
        };
        if cancellation.is_cancelled() {
            return InitialExecution::Accepted {
                accepted,
                result: terminal(&request.request_id, ResultOutcome::Cancelled, "cancelled"),
            };
        }
        let limits = request.limits.as_ref().expect("validated limits");
        let inspect_limits = InspectLimits {
            timeout: Duration::from_millis(limits.timeout_ms),
            maximum_response_bytes: limits.maximum_response_bytes as usize,
            codec: mb_printer_core::ipp::Limits {
                max_message_bytes: limits.maximum_response_bytes as usize,
                ..mb_printer_core::ipp::Limits::default()
            },
        };
        let printer_uri = match printer.endpoint.printer_uri() {
            Ok(uri) => uri,
            Err(_) => {
                return InitialExecution::Accepted {
                    accepted,
                    result: terminal(
                        &request.request_id,
                        ResultOutcome::Failed,
                        "published printer endpoint is invalid",
                    ),
                };
            }
        };
        let requested_attributes = if operation.requested_attributes.is_empty() {
            vec!["all".to_owned()]
        } else {
            operation.requested_attributes
        };
        let ipp_request = mb_printer_core::ipp::get_printer_attributes_request(
            &printer_uri,
            requested_attributes.iter().map(String::as_str),
            operation.document_format.as_deref(),
            request_number(&request.request_id),
        );
        let response = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return InitialExecution::Accepted {
                    accepted,
                    result: terminal(&request.request_id, ResultOutcome::Cancelled, "cancelled"),
                };
            }
            response = self.client.inspect(&printer.endpoint, &ipp_request, inspect_limits) => response,
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let (outcome, safe_message) = safe_client_error(&error);
                return InitialExecution::Accepted {
                    accepted,
                    result: terminal(&request.request_id, outcome, safe_message),
                };
            }
        };
        let origin = ObservationOrigin {
            agent_id: Some(self.policy.agent_id.clone()),
            printer_id: printer.printer_id,
            endpoint: printer_uri,
            endpoint_generation: printer.endpoint_generation,
            transport: match printer.endpoint.scheme {
                IppScheme::Ipp => TransportKind::Ipp,
                IppScheme::Ipps => TransportKind::Ipps,
            },
            protocol: ProtocolFamily::Ipp,
            request_id: request.request_id.clone(),
            probe_id: None,
            observed_at: now_unix_ms.to_string(),
            qualification: None,
        };
        let snapshot = normalize_ipp(&response, &origin, operation.document_format.as_deref());
        let (mode, authorization) = output_policy(operation.output_mode, &self.policy);
        let prepared = match prepare_snapshot_output(snapshot, mode, authorization) {
            Ok(prepared) => prepared,
            Err(_) => {
                return InitialExecution::Accepted {
                    accepted,
                    result: terminal(
                        &request.request_id,
                        ResultOutcome::Failed,
                        "output policy rejected the response",
                    ),
                };
            }
        };
        let bounded_response = match serde_json::to_vec(&prepared.snapshot) {
            Ok(response) if response.len() <= limits.maximum_response_bytes as usize => response,
            Ok(_) => {
                return InitialExecution::Accepted {
                    accepted,
                    result: terminal(
                        &request.request_id,
                        ResultOutcome::ResponseTooLarge,
                        "normalized response exceeds configured limit",
                    ),
                };
            }
            Err(_) => {
                return InitialExecution::Accepted {
                    accepted,
                    result: terminal(
                        &request.request_id,
                        ResultOutcome::Failed,
                        "response serialization failed",
                    ),
                };
            }
        };
        let result_endpoint = prepared
            .snapshot
            .observations
            .first()
            .map(|observation| observation.evidence.origin.endpoint.clone())
            .unwrap_or_else(|| "[REDACTED]".into());
        InitialExecution::Accepted {
            accepted,
            result: ProtocolResult {
                request_id: request.request_id.clone(),
                outcome: ResultOutcome::Succeeded as i32,
                bounded_response,
                evidence: vec![EvidenceOrigin {
                    agent_id: self.policy.agent_id.clone(),
                    printer_id: origin.printer_id,
                    endpoint: result_endpoint,
                    endpoint_generation: origin.endpoint_generation,
                    transport: match origin.transport {
                        TransportKind::Ipp => "ipp",
                        TransportKind::Ipps => "ipps",
                        _ => unreachable!(),
                    }
                    .into(),
                    protocol: "ipp".into(),
                    request_id: request.request_id,
                    probe_id: None,
                    qualification_id: None,
                }],
                output_mode: operation.output_mode,
                safe_error: String::new(),
                // Raw cloud material is ephemeral regardless of whether its
                // sensitive values were redacted before transmission.
                persistence_allowed: operation.output_mode
                    == WireOutputMode::NormalizedRedacted as i32
                    && prepared.retention.may_persist,
                logging_allowed: operation.output_mode == WireOutputMode::NormalizedRedacted as i32
                    && prepared.retention.may_log,
            },
        }
    }

    /// Later-phase guarded cloud administration. This API is deliberately
    /// separate from `execute_initial`, whose advertised contract remains
    /// read-only IppInspect.
    pub async fn execute_guarded_ipp_change(
        &self,
        request: ProtocolRequest,
        now_unix_ms: u64,
        policy: &GuardedWritePolicy,
    ) -> InitialExecution {
        self.execute_guarded_ipp_change_with_cancellation(
            request,
            now_unix_ms,
            policy,
            CancellationToken::default(),
        )
        .await
    }

    pub async fn execute_registered_probe(
        &self,
        request: ProtocolRequest,
        now_unix_ms: u64,
        registry: &ProbeRegistry,
        target: &PublishedProbeTarget,
        runner: &dyn RegisteredProbeRunner,
        cancellation: CancellationToken,
    ) -> InitialExecution {
        if let Err(error) = validate_request(
            &request,
            now_unix_ms,
            self.policy.maximum_timeout_ms,
            self.policy.maximum_response_bytes,
            &[OperationKind::RunProbe],
        ) {
            return InitialExecution::Rejected(rejection(&request.request_id, error));
        }
        if request.contract_version != self.policy.contract_version {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::UnsupportedVersion,
                "unsupported contract version",
            ));
        }
        let published = self
            .printers
            .read()
            .expect("published-printer registry lock poisoned")
            .get(&request.printer_id)
            .cloned();
        let Some(published) = published else {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::UnknownPrinter,
                "unknown published printer",
            ));
        };
        if published.endpoint_generation != request.endpoint_generation
            || target.printer_id != request.printer_id
            || target.endpoint_generation != request.endpoint_generation
        {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::StaleEndpoint,
                "stale endpoint generation",
            ));
        }
        let protocol_request::Operation::RunProbe(operation) =
            request.operation.as_ref().expect("validated operation")
        else {
            unreachable!("RunProbe validation permits only RunProbe")
        };
        let id = mb_printer_core::probe::ProbeId(operation.probe_id.clone());
        let Some(definition) = registry.get(&id) else {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::UnsupportedOperation,
                "probe is not registered",
            ));
        };
        if !definition.applies_to(
            target.protocol,
            target.transport,
            target.manufacturer.as_deref(),
            target.model.as_deref(),
            target.firmware.as_deref(),
        ) {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::Policy,
                "probe is not qualified for this target",
            ));
        }
        let prepared =
            match prepare_registered_probe(registry, &id, target.printer_definition.as_ref()) {
                Ok(prepared) => prepared,
                Err(_) => {
                    return InitialExecution::Rejected(reject(
                        request.request_id,
                        RejectionReason::Policy,
                        "registered probe could not be prepared",
                    ));
                }
            };
        let limits = request.limits.as_ref().expect("validated limits");
        if definition.limits.timeout_ms > limits.timeout_ms
            || definition.limits.maximum_response_bytes > limits.maximum_response_bytes as usize
        {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::LimitExceeded,
                "request limits are below the registered probe limits",
            ));
        }
        let accepted = ProtocolRequestAccepted {
            request_id: request.request_id.clone(),
            accepted_at_unix_ms: now_unix_ms,
        };
        if cancellation.is_cancelled() {
            return InitialExecution::Accepted {
                accepted,
                result: terminal(&request.request_id, ResultOutcome::Cancelled, "cancelled"),
            };
        }
        let output = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return InitialExecution::Accepted {
                    accepted,
                    result: terminal(&request.request_id, ResultOutcome::Cancelled, "cancelled"),
                };
            }
            output = runner.run(prepared, definition.limits) => output,
        };
        let output = match output {
            Ok(output) if output.response.len() <= definition.limits.maximum_response_bytes => {
                output
            }
            Ok(_) => {
                return InitialExecution::Accepted {
                    accepted,
                    result: terminal(
                        &request.request_id,
                        ResultOutcome::ResponseTooLarge,
                        "probe response exceeded its registered limit",
                    ),
                };
            }
            Err(_) => {
                return InitialExecution::Accepted {
                    accepted,
                    result: terminal(
                        &request.request_id,
                        ResultOutcome::Failed,
                        "registered probe execution failed",
                    ),
                };
            }
        };
        let mut report = match build_read_only_probe_report(
            registry,
            &id,
            &output.response,
            ObservationOrigin {
                agent_id: Some(self.policy.agent_id.clone()),
                printer_id: request.printer_id.clone(),
                endpoint: target.endpoint_identity.clone(),
                endpoint_generation: target.endpoint_generation,
                transport: target.transport,
                protocol: target.protocol,
                request_id: request.request_id.clone(),
                probe_id: Some(id.0.clone()),
                observed_at: now_unix_ms.to_string(),
                qualification: target.firmware.as_ref().map(|firmware| {
                    mb_printer_core::discovery::QualificationMetadata {
                        qualification_id: definition.qualification.qualification_id.clone(),
                        firmware: Some(firmware.clone()),
                        response_hash: None,
                    }
                }),
            },
            output.duration_ms,
        ) {
            Ok(report) => report,
            Err(_) => {
                return InitialExecution::Accepted {
                    accepted,
                    result: terminal(
                        &request.request_id,
                        ResultOutcome::Failed,
                        "registered probe response was malformed",
                    ),
                };
            }
        };
        let redacted_endpoint = redact_identifier(&report.endpoint);
        report.endpoint = redacted_endpoint.clone();
        report.origin.endpoint = redacted_endpoint;
        InitialExecution::Accepted {
            accepted,
            result: probe_result(
                &request.request_id,
                &report,
                limits.maximum_response_bytes as usize,
            ),
        }
    }

    pub async fn execute_guarded_ipp_change_with_cancellation(
        &self,
        request: ProtocolRequest,
        now_unix_ms: u64,
        policy: &GuardedWritePolicy,
        cancellation: CancellationToken,
    ) -> InitialExecution {
        if let Err(error) = validate_request(
            &request,
            now_unix_ms,
            self.policy.maximum_timeout_ms,
            self.policy.maximum_response_bytes,
            &[OperationKind::PlanChange, OperationKind::ApplyChange],
        ) {
            return InitialExecution::Rejected(rejection(&request.request_id, error));
        }
        if request.contract_version != self.policy.contract_version {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::UnsupportedVersion,
                "unsupported contract version",
            ));
        }
        let printer = self
            .printers
            .read()
            .expect("published-printer registry lock poisoned")
            .get(&request.printer_id)
            .cloned();
        let Some(printer) = printer else {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::UnknownPrinter,
                "unknown published printer",
            ));
        };
        if printer.endpoint_generation != request.endpoint_generation {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::StaleEndpoint,
                "stale endpoint generation",
            ));
        }
        let operation = request.operation.clone().expect("validated operation");
        let setting = match &operation {
            protocol_request::Operation::PlanChange(operation) => &operation.setting_id,
            protocol_request::Operation::ApplyChange(operation) => &operation.setting_id,
            _ => unreachable!("guarded validation permits only change operations"),
        };
        if !policy.allowed_settings.contains(setting) {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::Unauthorized,
                "setting is not authorized by local policy",
            ));
        }
        if matches!(&operation, protocol_request::Operation::ApplyChange(_))
            && !self
                .guarded_write_requests
                .lock()
                .expect("guarded-write request registry lock poisoned")
                .insert(request.request_id.clone())
        {
            return InitialExecution::Rejected(reject(
                request.request_id,
                RejectionReason::Policy,
                "guarded write request ID was already consumed",
            ));
        }
        let accepted = ProtocolRequestAccepted {
            request_id: request.request_id.clone(),
            accepted_at_unix_ms: now_unix_ms,
        };
        if cancellation.is_cancelled() {
            return InitialExecution::Accepted {
                accepted,
                result: terminal(&request.request_id, ResultOutcome::Cancelled, "cancelled"),
            };
        }
        let limits = request.limits.as_ref().expect("validated limits");
        let inspect_limits = InspectLimits {
            timeout: Duration::from_millis(limits.timeout_ms),
            maximum_response_bytes: limits.maximum_response_bytes as usize,
            codec: ipp::Limits {
                max_message_bytes: limits.maximum_response_bytes as usize,
                ..ipp::Limits::default()
            },
        };
        let result = match operation {
            protocol_request::Operation::PlanChange(operation) => {
                self.plan_cloud_change(&request, &printer, operation, inspect_limits)
                    .await
            }
            protocol_request::Operation::ApplyChange(operation) => {
                self.apply_cloud_change(&request, &printer, operation, now_unix_ms, inspect_limits)
                    .await
            }
            _ => unreachable!(),
        };
        InitialExecution::Accepted { accepted, result }
    }

    async fn plan_cloud_change(
        &self,
        request: &ProtocolRequest,
        printer: &PublishedPrinter,
        operation: PlanChange,
        limits: InspectLimits,
    ) -> ProtocolResult {
        if operation.protocol != "ipp" {
            return terminal(
                &request.request_id,
                ResultOutcome::Failed,
                "protocol must be IPP",
            );
        }
        let requested_value: Value = match serde_json::from_slice(&operation.requested_value) {
            Ok(value) => value,
            Err(_) => {
                return terminal(
                    &request.request_id,
                    ResultOutcome::Failed,
                    "requested IPP value is malformed",
                );
            }
        };
        let plan = match self
            .client
            .plan_change(
                &printer.endpoint,
                PlanChangeRequest {
                    printer_id: &request.printer_id,
                    endpoint_generation: request.endpoint_generation,
                    setting: &operation.setting_id,
                    requested_value,
                    principal: &request.authenticated_principal,
                    protocol: ProtocolFamily::Ipp,
                    expires_at_unix_ms: request.expires_at_unix_ms,
                },
                limits,
            )
            .await
        {
            Ok(plan) => plan,
            Err(NativePlanChangeError::Read(error)) => {
                let (outcome, safe) = safe_client_error(&error);
                return terminal(&request.request_id, outcome, safe);
            }
            Err(NativePlanChangeError::Endpoint(_)) => {
                return terminal(
                    &request.request_id,
                    ResultOutcome::Failed,
                    "published printer endpoint is invalid",
                );
            }
            Err(
                NativePlanChangeError::SupportedValues(_) | NativePlanChangeError::InvalidPlan(_),
            ) => {
                return terminal(
                    &request.request_id,
                    ResultOutcome::Rejected,
                    "printer does not confirm this change",
                );
            }
        };
        let receipt = CloudChangeReceipt {
            printer_id: plan.printer_id,
            endpoint_generation: plan.endpoint_generation,
            setting: plan.setting,
            expected_old_value_hash: plan.expected_old_value_hash,
            expected_requested_value_hash: ipp_value_hash(&plan.requested_protocol_value),
            principal: plan.principal,
            protocol: plan.protocol,
            expires_at_unix_ms: plan.expires_at_unix_ms,
        };
        bounded_json_result(&request.request_id, &receipt, limits.maximum_response_bytes)
    }

    async fn apply_cloud_change(
        &self,
        request: &ProtocolRequest,
        printer: &PublishedPrinter,
        operation: ApplyChange,
        now_unix_ms: u64,
        limits: InspectLimits,
    ) -> ProtocolResult {
        if operation.protocol != "ipp"
            || operation.expected_old_value_hash.len() != 32
            || operation.expected_requested_value_hash.len() != 32
            || operation.plan_expires_at_unix_ms > request.expires_at_unix_ms
        {
            return terminal(
                &request.request_id,
                ResultOutcome::Rejected,
                "confirmed change fields are invalid",
            );
        }
        let requested_value: Value = match serde_json::from_slice(&operation.requested_value) {
            Ok(value) => value,
            Err(_) => {
                return terminal(
                    &request.request_id,
                    ResultOutcome::Rejected,
                    "requested IPP value is malformed",
                );
            }
        };
        if operation.expected_requested_value_hash.as_slice() != ipp_value_hash(&requested_value) {
            return terminal(
                &request.request_id,
                ResultOutcome::Rejected,
                "requested value no longer matches the confirmation",
            );
        }
        let mut expected_old_value_hash = [0; 32];
        expected_old_value_hash.copy_from_slice(&operation.expected_old_value_hash);
        let plan = match confirmed_ipp_plan_from_wire(
            request.printer_id.clone(),
            request.endpoint_generation,
            operation.setting_id,
            expected_old_value_hash,
            requested_value,
            request.authenticated_principal.clone(),
            operation.plan_expires_at_unix_ms,
        ) {
            Ok(plan) => plan,
            Err(_) => {
                return terminal(
                    &request.request_id,
                    ResultOutcome::Rejected,
                    "confirmed change value is invalid",
                );
            }
        };
        let outcome = self
            .client
            .apply_confirmed_change(
                &printer.endpoint,
                &plan,
                ChangeBinding {
                    printer_id: &request.printer_id,
                    endpoint_generation: request.endpoint_generation,
                    principal: &request.authenticated_principal,
                    protocol: ProtocolFamily::Ipp,
                    now_unix_ms,
                },
                limits,
            )
            .await;
        match outcome {
            Ok(ApplyChangeOutcome::Verified { .. }) => success_empty(&request.request_id),
            Ok(ApplyChangeOutcome::Rejected { .. }) => terminal(
                &request.request_id,
                ResultOutcome::Rejected,
                "printer rejected the change",
            ),
            Ok(ApplyChangeOutcome::ReadBackMismatch { .. }) => terminal(
                &request.request_id,
                ResultOutcome::Failed,
                "post-write verification did not match",
            ),
            Ok(ApplyChangeOutcome::Ambiguous { .. }) => terminal(
                &request.request_id,
                ResultOutcome::AmbiguousWrite,
                "write outcome is ambiguous and will not be retried",
            ),
            Err(_) => terminal(
                &request.request_id,
                ResultOutcome::Rejected,
                "confirmed change became stale or could not be read",
            ),
        }
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

fn output_policy(mode: i32, policy: &AgentPolicy) -> (OutputMode, OutputAuthorization) {
    match WireOutputMode::try_from(mode).expect("authorized output mode") {
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
    }
}

fn request_number(request_id: &str) -> u32 {
    request_id.bytes().fold(2_166_136_261u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    })
}

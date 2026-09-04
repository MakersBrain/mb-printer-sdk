// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

impl AgentExecutor {
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
            .unwrap_or_else(PoisonError::into_inner)
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
        let Some(protocol_request::Operation::RunProbe(operation)) = request.operation.as_ref()
        else {
            return InitialExecution::Rejected(rejection(
                &request.request_id,
                ContractError::MissingOperation,
            ));
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
        let Some(limits) = request.limits.as_ref() else {
            return InitialExecution::Rejected(rejection(
                &request.request_id,
                ContractError::InvalidLimits,
            ));
        };
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
}

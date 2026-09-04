// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

impl AgentExecutor {
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
            .unwrap_or_else(PoisonError::into_inner)
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
        let Some(protocol_request::Operation::IppInspect(operation)) = request.operation.clone()
        else {
            return InitialExecution::Rejected(rejection(
                &request.request_id,
                ContractError::MissingOperation,
            ));
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
        let Some(limits) = request.limits.as_ref() else {
            return InitialExecution::Rejected(rejection(
                &request.request_id,
                ContractError::InvalidLimits,
            ));
        };
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
        let Some((mode, authorization)) = output_policy(operation.output_mode, &self.policy) else {
            return InitialExecution::Accepted {
                accepted,
                result: terminal(
                    &request.request_id,
                    ResultOutcome::Failed,
                    "output policy rejected the response",
                ),
            };
        };
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
                    transport: match printer.endpoint.scheme {
                        IppScheme::Ipp => "ipp",
                        IppScheme::Ipps => "ipps",
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
}

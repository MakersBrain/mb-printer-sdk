// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

impl AgentExecutor {
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
        let Some(operation) = request.operation.clone() else {
            return InitialExecution::Rejected(rejection(
                &request.request_id,
                ContractError::MissingOperation,
            ));
        };
        let setting = match &operation {
            protocol_request::Operation::PlanChange(operation) => &operation.setting_id,
            protocol_request::Operation::ApplyChange(operation) => &operation.setting_id,
            _ => {
                return InitialExecution::Rejected(rejection(
                    &request.request_id,
                    ContractError::UnsupportedOperation,
                ));
            }
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
                .unwrap_or_else(PoisonError::into_inner)
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
        let Some(limits) = request.limits.as_ref() else {
            return InitialExecution::Rejected(rejection(
                &request.request_id,
                ContractError::InvalidLimits,
            ));
        };
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
            _ => {
                return InitialExecution::Rejected(rejection(
                    &request.request_id,
                    ContractError::UnsupportedOperation,
                ));
            }
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
                NativePlanChangeError::SupportedValues(_)
                | NativePlanChangeError::InvalidPlan(_)
                | NativePlanChangeError::SupportedValuesRequest(_),
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

// SPDX-License-Identifier: AGPL-3.0-or-later
#![forbid(unsafe_code)]

pub mod v1 {
    tonic::include_proto!("makersbrain.printer.agent.v1");
}

use thiserror::Error;
use v1::{OperationKind, ProtocolRequest, protocol_request};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ContractError {
    #[error("request identity and principal fields must be non-empty")]
    MissingIdentity,
    #[error("request has expired")]
    Expired,
    #[error("request limits are missing, zero, or exceed agent capabilities")]
    InvalidLimits,
    #[error("operation is missing")]
    MissingOperation,
    #[error("operation is not enabled by this agent session")]
    UnsupportedOperation,
    #[error("initial cloud release accepts only IppInspect")]
    InitialReleaseIppInspectOnly,
}

pub fn validate_request(
    request: &ProtocolRequest,
    now_unix_ms: u64,
    maximum_timeout_ms: u64,
    maximum_response_bytes: u64,
    enabled_operations: &[OperationKind],
) -> Result<(), ContractError> {
    if request.request_id.is_empty()
        || request.authenticated_principal.is_empty()
        || request.printer_id.is_empty()
        || request.contract_version == 0
    {
        return Err(ContractError::MissingIdentity);
    }
    if request.expires_at_unix_ms <= now_unix_ms {
        return Err(ContractError::Expired);
    }
    let limits = request
        .limits
        .as_ref()
        .ok_or(ContractError::InvalidLimits)?;
    if limits.timeout_ms == 0
        || limits.maximum_response_bytes == 0
        || limits.timeout_ms > maximum_timeout_ms
        || limits.maximum_response_bytes > maximum_response_bytes
    {
        return Err(ContractError::InvalidLimits);
    }
    let operation = request
        .operation
        .as_ref()
        .ok_or(ContractError::MissingOperation)?;
    let kind = match operation {
        protocol_request::Operation::IppInspect(_) => OperationKind::IppInspect,
        protocol_request::Operation::ReadSetting(_) => OperationKind::ReadSetting,
        protocol_request::Operation::RunProbe(_) => OperationKind::RunProbe,
        protocol_request::Operation::PlanChange(_) => OperationKind::PlanChange,
        protocol_request::Operation::ApplyChange(_) => OperationKind::ApplyChange,
    };
    if !enabled_operations.contains(&kind) {
        return Err(ContractError::UnsupportedOperation);
    }
    Ok(())
}

pub fn validate_initial_release_request(request: &ProtocolRequest) -> Result<(), ContractError> {
    if !matches!(
        request.operation,
        Some(protocol_request::Operation::IppInspect(_))
    ) {
        return Err(ContractError::InitialReleaseIppInspectOnly);
    }
    Ok(())
}

// SPDX-License-Identifier: AGPL-3.0-or-later
//! Caller-runtime async SNMP read boundary. No hidden runtime is created.

use mb_printer_core::snmp::{
    self, DecodeLimits, ObjectId, ObjectRegistry, Response, SnmpError, VarBind,
};
use std::{net::SocketAddr, time::Duration};
use thiserror::Error;
use tokio::net::UdpSocket;

#[derive(Clone)]
pub struct Community(Vec<u8>);

impl Community {
    pub fn new(value: impl Into<Vec<u8>>) -> Result<Self, ClientError> {
        let value = value.into();
        if value.is_empty() || value.len() > 255 {
            return Err(ClientError::InvalidLimits);
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Community {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Community([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientLimits {
    pub timeout: Duration,
    pub retries: u8,
    pub maximum_response_bytes: usize,
    pub maximum_walk_steps: usize,
    pub decode: DecodeLimits,
}

impl Default for ClientLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            retries: 1,
            maximum_response_bytes: 64 * 1024,
            maximum_walk_steps: 128,
            decode: DecodeLimits::default(),
        }
    }
}

impl ClientLimits {
    fn validate(self) -> Result<Self, ClientError> {
        if self.timeout.is_zero()
            || self.retries > 5
            || self.maximum_response_bytes < 16
            || self.maximum_response_bytes > self.decode.maximum_message_bytes
            || self.maximum_walk_steps == 0
            || self.maximum_walk_steps > 4_096
        {
            return Err(ClientError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("invalid SNMP limits or credentials")]
    InvalidLimits,
    #[error("SNMP object is not registered")]
    UnregisteredObject,
    #[error("SNMP transport failed")]
    Transport(#[source] std::io::Error),
    #[error("SNMP request timed out")]
    Timeout,
    #[error("SNMP response exceeded its byte limit")]
    ResponseTooLarge,
    #[error("SNMP protocol response was invalid: {0}")]
    Protocol(#[from] SnmpError),
    #[error("SNMP agent returned error status {status} at index {index}")]
    Agent { status: i32, index: i32 },
    #[error("SNMP walk exhausted its step limit")]
    WalkLimit,
}

#[derive(Debug, Clone, Default)]
pub struct SnmpClient;

impl SnmpClient {
    pub async fn get(
        &self,
        endpoint: SocketAddr,
        registry: &ObjectRegistry,
        community: &Community,
        oid: &ObjectId,
        request_id: i32,
        limits: ClientLimits,
    ) -> Result<Vec<VarBind>, ClientError> {
        if registry.get(oid).is_none() {
            return Err(ClientError::UnregisteredObject);
        }
        let request = snmp::encode_get(registry, community.expose(), request_id, oid)?;
        let response = exchange(endpoint, &request, request_id, limits).await?;
        check_agent_error(&response)?;
        if response
            .varbinds
            .iter()
            .any(|binding| registry.get(&binding.oid).is_none())
        {
            return Err(ClientError::UnregisteredObject);
        }
        Ok(response.varbinds)
    }

    pub async fn walk(
        &self,
        endpoint: SocketAddr,
        registry: &ObjectRegistry,
        community: &Community,
        root: &ObjectId,
        first_request_id: i32,
        limits: ClientLimits,
    ) -> Result<Vec<VarBind>, ClientError> {
        let limits = limits.validate()?;
        if !registry.permits_root(root) {
            return Err(ClientError::UnregisteredObject);
        }
        let mut current = root.clone();
        let mut results = Vec::new();
        for step in 0..limits.maximum_walk_steps {
            let request_id = first_request_id.wrapping_add(step as i32);
            let request =
                snmp::encode_get_next(registry, community.expose(), request_id, &current)?;
            let response = exchange(endpoint, &request, request_id, limits).await?;
            check_agent_error(&response)?;
            let Some(binding) = response.varbinds.into_iter().next() else {
                return Ok(results);
            };
            if !binding.oid.is_within(root)
                || matches!(binding.value, snmp::ObjectValue::EndOfMibView)
            {
                return Ok(results);
            }
            if binding.oid <= current {
                return Err(ClientError::Protocol(SnmpError::Malformed));
            }
            current = binding.oid.clone();
            if registry.get(&binding.oid).is_some() {
                results.push(binding);
            }
        }
        Err(ClientError::WalkLimit)
    }
}

async fn exchange(
    endpoint: SocketAddr,
    request: &[u8],
    request_id: i32,
    limits: ClientLimits,
) -> Result<Response, ClientError> {
    let limits = limits.validate()?;
    let bind_address = if endpoint.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind_address)
        .await
        .map_err(ClientError::Transport)?;
    socket
        .connect(endpoint)
        .await
        .map_err(ClientError::Transport)?;
    let mut response = vec![0; limits.maximum_response_bytes];
    for attempt in 0..=limits.retries {
        socket.send(request).await.map_err(ClientError::Transport)?;
        match tokio::time::timeout(limits.timeout, socket.recv(&mut response)).await {
            Ok(Ok(length)) => {
                if length == response.len() {
                    return Err(ClientError::ResponseTooLarge);
                }
                return snmp::decode_response(&response[..length], request_id, limits.decode)
                    .map_err(ClientError::Protocol);
            }
            Ok(Err(error)) => return Err(ClientError::Transport(error)),
            Err(_) if attempt < limits.retries => continue,
            Err(_) => return Err(ClientError::Timeout),
        }
    }
    unreachable!("bounded retry loop always returns")
}

fn check_agent_error(response: &Response) -> Result<(), ClientError> {
    if response.error_status == 0 {
        Ok(())
    } else {
        Err(ClientError::Agent {
            status: response.error_status,
            index: response.error_index,
        })
    }
}

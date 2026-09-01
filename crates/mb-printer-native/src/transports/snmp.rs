// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed, allowlisted SNMP manager adapter backed by `async-snmp`.

use async_snmp::{
    Auth, Client, CommunityResponsePolicy, ErrorKind, Oid, ResponseShapePolicy, Retry,
    UdpTransport, Value,
};
use mb_printer_core::snmp::{
    DecodeLimits, ObjectDefinition, ObjectId, ObjectRegistry, ObjectValue, ResponseEvidence,
    SetValue, SnmpError, VarBind, validate_set_value,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, net::SocketAddr, time::Duration};
use thiserror::Error;

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

#[derive(Clone)]
pub enum Credentials {
    V2c(Community),
    #[cfg(feature = "snmp-v3")]
    V3(V3Credentials),
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::V2c(_) => formatter.write_str("Credentials::V2c([REDACTED])"),
            #[cfg(feature = "snmp-v3")]
            Self::V3(_) => formatter.write_str("Credentials::V3([REDACTED])"),
        }
    }
}

#[cfg(feature = "snmp-v3")]
#[derive(Clone)]
pub struct V3Credentials {
    pub username: String,
    pub authentication: V3Authentication,
    pub privacy: V3Privacy,
}

#[cfg(feature = "snmp-v3")]
#[derive(Clone)]
pub enum V3Authentication {
    Sha256(String),
    Sha512(String),
}

#[cfg(feature = "snmp-v3")]
#[derive(Clone)]
pub enum V3Privacy {
    Aes128(String),
    Aes256Blumenthal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientLimits {
    pub timeout: Duration,
    pub retries: u8,
    pub maximum_response_bytes: usize,
    pub maximum_walk_steps: usize,
    pub maximum_oids_per_request: usize,
    pub decode: DecodeLimits,
}

impl Default for ClientLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            retries: 1,
            // The UDP payload ceiling is smaller than 64 KiB once the UDP
            // header is accounted for.
            maximum_response_bytes: 65_507,
            maximum_walk_steps: 128,
            maximum_oids_per_request: 16,
            decode: DecodeLimits::default(),
        }
    }
}

impl ClientLimits {
    fn validate(self) -> Result<Self, ClientError> {
        if self.timeout.is_zero()
            || self.timeout > Duration::from_secs(300)
            || self.retries > 5
            || self.maximum_response_bytes < 484
            || self.maximum_response_bytes > 65_507
            || self.maximum_response_bytes > self.decode.maximum_message_bytes
            || self.maximum_walk_steps == 0
            || self.maximum_walk_steps > 4_096
            || self.maximum_oids_per_request == 0
            || self.maximum_oids_per_request > self.decode.maximum_varbinds
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
    #[error("SNMP operation timed out")]
    Timeout,
    #[error("SNMP response exceeded a configured limit")]
    ResponseTooLarge,
    #[error("SNMP protocol response was invalid: {0}")]
    Protocol(#[from] SnmpError),
    #[error("SNMP agent rejected the operation")]
    Agent,
    #[error("SNMP walk exhausted its configured limit")]
    WalkLimit,
    #[error("SNMP transport failed")]
    Transport,
    #[error("SNMP write outcome is ambiguous and was not retried")]
    AmbiguousWrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResult {
    pub varbinds: Vec<VarBind>,
    pub evidence: ResponseEvidence,
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
        _request_id: i32,
        limits: ClientLimits,
    ) -> Result<Vec<VarBind>, ClientError> {
        self.get_many(
            endpoint,
            registry,
            &Credentials::V2c(community.clone()),
            std::slice::from_ref(oid),
            limits,
        )
        .await
        .map(|result| result.varbinds)
    }

    pub async fn get_many(
        &self,
        endpoint: SocketAddr,
        registry: &ObjectRegistry,
        credentials: &Credentials,
        oids: &[ObjectId],
        limits: ClientLimits,
    ) -> Result<ReadResult, ClientError> {
        let limits = limits.validate()?;
        if oids.is_empty()
            || oids.len() > limits.maximum_oids_per_request
            || oids.iter().any(|oid| registry.get(oid).is_none())
        {
            return Err(ClientError::UnregisteredObject);
        }
        let requested = oids.iter().cloned().collect::<BTreeSet<_>>();
        if requested.len() != oids.len() {
            return Err(ClientError::Protocol(SnmpError::Malformed));
        }
        let wire_oids = oids
            .iter()
            .map(to_wire_oid)
            .collect::<Result<Vec<_>, _>>()?;
        let client = build_client(endpoint, credentials, limits, false).await?;
        let response = tokio::time::timeout(limits.timeout, client.get_many(&wire_oids))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(map_read_error)?;
        if !response.anomalies.is_empty() {
            return Err(ClientError::Protocol(SnmpError::Malformed));
        }
        let varbinds = response
            .varbinds
            .into_iter()
            .map(from_wire_varbind)
            .collect::<Result<Vec<_>, _>>()?;
        if varbinds.len() != oids.len()
            || varbinds.iter().zip(oids).any(|(binding, expected)| {
                &binding.oid != expected
                    || !requested.contains(&binding.oid)
                    || registry.get(&binding.oid).is_none()
            })
        {
            return Err(ClientError::Protocol(SnmpError::Malformed));
        }
        enforce_decoded_limits(&varbinds, limits)?;
        let evidence = structured_evidence(&varbinds, &response.metadata.decode_anomalies);
        Ok(ReadResult { varbinds, evidence })
    }

    pub async fn walk(
        &self,
        endpoint: SocketAddr,
        registry: &ObjectRegistry,
        community: &Community,
        root: &ObjectId,
        _first_request_id: i32,
        limits: ClientLimits,
    ) -> Result<Vec<VarBind>, ClientError> {
        let limits = limits.validate()?;
        if !registry.permits_root(root) {
            return Err(ClientError::UnregisteredObject);
        }
        let client = build_client(
            endpoint,
            &Credentials::V2c(community.clone()),
            limits,
            false,
        )
        .await?;
        let mut current = to_wire_oid(root)?;
        let mut output = Vec::new();
        let deadline = tokio::time::Instant::now() + limits.timeout;
        for _ in 0..limits.maximum_walk_steps {
            let response = tokio::time::timeout_at(deadline, client.get_next(&current))
                .await
                .map_err(|_| ClientError::Timeout)?
                .map_err(map_read_error)?;
            let binding = response
                .into_single()
                .map_err(|_| ClientError::Protocol(SnmpError::Malformed))?;
            let converted = from_wire_varbind(binding)?;
            if !converted.oid.is_within(root)
                || matches!(converted.value, ObjectValue::EndOfMibView)
            {
                return Ok(output);
            }
            if converted.oid.0.as_slice() <= current.arcs() {
                return Err(ClientError::Protocol(SnmpError::Malformed));
            }
            current = to_wire_oid(&converted.oid)?;
            if registry.get(&converted.oid).is_some() {
                output.push(converted);
                enforce_decoded_limits(&output, limits)?;
            }
        }
        Err(ClientError::WalkLimit)
    }

    /// Dormant protocol primitive for the crate's future confirmed-change
    /// state machine. It is deliberately not part of the public SDK surface.
    #[allow(dead_code, reason = "reserved for the confirmed-change state machine")]
    pub(crate) async fn set_once(
        &self,
        endpoint: SocketAddr,
        definition: &ObjectDefinition,
        credentials: &Credentials,
        value: &SetValue,
        limits: ClientLimits,
    ) -> Result<ReadResult, ClientError> {
        validate_set_value(definition, value)?;
        let limits = limits.validate()?;
        let client = build_client(endpoint, credentials, limits, true).await?;
        let oid = to_wire_oid(&definition.oid)?;
        let response =
            tokio::time::timeout(limits.timeout, client.set(&oid, to_wire_set_value(value)?))
                .await
                .map_err(|_| ClientError::AmbiguousWrite)?
                .map_err(map_write_error)?;
        if !response.anomalies.is_empty() {
            return Err(ClientError::Protocol(SnmpError::Malformed));
        }
        let varbinds = response
            .varbinds
            .into_iter()
            .map(from_wire_varbind)
            .collect::<Result<Vec<_>, _>>()?;
        enforce_decoded_limits(&varbinds, limits)?;
        let evidence = structured_evidence(&varbinds, &response.metadata.decode_anomalies);
        Ok(ReadResult { varbinds, evidence })
    }
}

async fn build_client(
    endpoint: SocketAddr,
    credentials: &Credentials,
    limits: ClientLimits,
    write: bool,
) -> Result<async_snmp::UdpClient, ClientError> {
    let retry = if write {
        Retry::none()
    } else {
        Retry::fixed(u32::from(limits.retries), Duration::ZERO)
            .map_err(|_| ClientError::InvalidLimits)?
    };
    let bind_address = if endpoint.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let transport = UdpTransport::builder()
        .bind(bind_address)
        .max_message_size(limits.maximum_response_bytes)
        .build()
        .await
        .map_err(map_read_error)?;
    Client::builder(endpoint, credentials_auth(credentials)?)
        .strict_source(true)
        .community_response_policy(CommunityResponsePolicy::Exact)
        .request_timeout(limits.timeout)
        .retry(retry)
        .max_oids_per_request(limits.maximum_oids_per_request)
        .response_shape_policy(ResponseShapePolicy::Strict)
        .build_with(&transport)
        .await
        .map_err(|_| ClientError::Transport)
}

fn credentials_auth(credentials: &Credentials) -> Result<Auth, ClientError> {
    match credentials {
        Credentials::V2c(community) => Ok(Auth::v2c(community.expose().to_vec())),
        #[cfg(feature = "snmp-v3")]
        Credentials::V3(credentials) => {
            use async_snmp::{AuthProtocol, PrivProtocol, UsmConfig};
            let (auth_protocol, auth_password) = match &credentials.authentication {
                V3Authentication::Sha256(password) => (AuthProtocol::Sha256, password),
                V3Authentication::Sha512(password) => (AuthProtocol::Sha512, password),
            };
            let (privacy_protocol, privacy_password) = match &credentials.privacy {
                V3Privacy::Aes128(password) => (PrivProtocol::Aes128, password),
                V3Privacy::Aes256Blumenthal(password) => (PrivProtocol::Aes256Blumenthal, password),
            };
            let usm = UsmConfig::new(credentials.username.clone())
                .auth_priv(
                    auth_protocol,
                    auth_password.as_bytes(),
                    privacy_protocol,
                    privacy_password.as_bytes(),
                )
                .map_err(|_| ClientError::InvalidLimits)?;
            Ok(Auth::Usm(usm))
        }
    }
}

fn to_wire_oid(oid: &ObjectId) -> Result<Oid, ClientError> {
    let oid = Oid::from_slice(&oid.0);
    oid.validate()
        .map_err(|_| ClientError::Protocol(SnmpError::InvalidOid))?;
    Ok(oid)
}

fn from_wire_varbind(binding: async_snmp::VarBind) -> Result<VarBind, ClientError> {
    let oid = ObjectId(binding.oid.arcs().to_vec());
    let value = match binding.value {
        Value::Integer(value) => ObjectValue::Integer(i64::from(value)),
        Value::OctetString(value) => ObjectValue::Bytes(value.to_vec()),
        Value::Null => ObjectValue::Null,
        Value::ObjectIdentifier(value) => ObjectValue::ObjectId(ObjectId(value.arcs().to_vec())),
        Value::IpAddress(value) => ObjectValue::IpAddress(value),
        Value::Counter32(value) => ObjectValue::Counter32(value),
        Value::Gauge32(value) => ObjectValue::Gauge32(value),
        Value::UInteger32(value) => ObjectValue::Unsigned32(value),
        Value::TimeTicks(value) => ObjectValue::TimeTicks(value),
        Value::Opaque(value) => ObjectValue::Opaque(value.to_vec()),
        Value::Nsap(value) => ObjectValue::Nsap(value.to_vec()),
        Value::Counter64(value) => ObjectValue::Counter64(value),
        Value::NoSuchObject => ObjectValue::NoSuchObject,
        Value::NoSuchInstance => ObjectValue::NoSuchInstance,
        Value::EndOfMibView => ObjectValue::EndOfMibView,
        Value::Unknown { tag, data } => ObjectValue::Unknown {
            tag,
            bytes: data.to_vec(),
        },
        _ => return Err(ClientError::Protocol(SnmpError::Malformed)),
    };
    Ok(VarBind { oid, value })
}

#[allow(dead_code, reason = "used only by the dormant private SET primitive")]
fn to_wire_set_value(value: &SetValue) -> Result<Value, ClientError> {
    Ok(match value {
        SetValue::Integer(value) => Value::Integer(
            i32::try_from(*value).map_err(|_| ClientError::Protocol(SnmpError::InvalidValue))?,
        ),
        SetValue::Octets(value) => Value::from(value.as_slice()),
        SetValue::Text(value) => Value::from(value.clone()),
        SetValue::IpAddress(value) => Value::IpAddress(*value),
        SetValue::ObjectId(value) => Value::ObjectIdentifier(to_wire_oid(value)?),
    })
}

fn structured_evidence(
    varbinds: &[VarBind],
    anomalies: &[async_snmp::DecodeAnomaly],
) -> ResponseEvidence {
    let mut digest = Sha256::new();
    digest.update(b"mb-printer-async-snmp-evidence-v1\0");
    digest.update(serde_json::to_vec(varbinds).expect("SNMP varbinds are serializable"));
    for anomaly in anomalies {
        digest.update(format!("{anomaly:?}").as_bytes());
    }
    ResponseEvidence {
        credential_elided_hash: digest.finalize().into(),
        original_length: 0,
        sanitized_bytes: None,
    }
}

fn enforce_decoded_limits(varbinds: &[VarBind], limits: ClientLimits) -> Result<(), ClientError> {
    if varbinds.len() > limits.decode.maximum_varbinds {
        return Err(ClientError::ResponseTooLarge);
    }
    let mut estimated_message_bytes = 64usize;
    for binding in varbinds {
        let value_bytes = match &binding.value {
            ObjectValue::Bytes(value)
            | ObjectValue::Opaque(value)
            | ObjectValue::Nsap(value)
            | ObjectValue::Unknown { bytes: value, .. } => value.len(),
            ObjectValue::ObjectId(value) => value.0.len().saturating_mul(5),
            _ => 8,
        };
        if value_bytes > limits.decode.maximum_value_bytes {
            return Err(ClientError::ResponseTooLarge);
        }
        estimated_message_bytes = estimated_message_bytes
            .saturating_add(binding.oid.0.len().saturating_mul(5))
            .saturating_add(value_bytes)
            .saturating_add(16);
    }
    if estimated_message_bytes > limits.maximum_response_bytes {
        Err(ClientError::ResponseTooLarge)
    } else {
        Ok(())
    }
}

fn map_read_error(error: impl AsRef<async_snmp::Error>) -> ClientError {
    match error.as_ref().kind() {
        ErrorKind::Timeout | ErrorKind::ConstructionTimeout => ClientError::Timeout,
        ErrorKind::OutboundMessageTooLarge => ClientError::ResponseTooLarge,
        ErrorKind::Snmp | ErrorKind::Auth | ErrorKind::Report => ClientError::Agent,
        ErrorKind::Decode
        | ErrorKind::MalformedResponse
        | ErrorKind::ResponseShape
        | ErrorKind::InvalidMessage
        | ErrorKind::InvalidOid => ClientError::Protocol(SnmpError::Malformed),
        _ => ClientError::Transport,
    }
}

#[allow(dead_code, reason = "used only by the dormant private SET primitive")]
fn map_write_error(error: impl AsRef<async_snmp::Error>) -> ClientError {
    match error.as_ref().kind() {
        // These are definitive agent responses, not an unknown write outcome.
        ErrorKind::Snmp | ErrorKind::Auth | ErrorKind::Report => ClientError::Agent,
        // These local failures occur while encoding, before a datagram can be sent.
        ErrorKind::OutboundMessageTooLarge
        | ErrorKind::Config
        | ErrorKind::InvalidMessage
        | ErrorKind::InvalidOid => ClientError::InvalidLimits,
        // Once `set` starts, all transport and response failures are ambiguous:
        // the single datagram may have reached and changed the device.
        _ => ClientError::AmbiguousWrite,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mb_printer_core::snmp::{
        DeviceQualification, ObjectAccess, ObjectKey, ObjectSyntax, Sensitivity,
    };

    #[tokio::test]
    async fn dormant_set_rejects_read_only_definition_before_network_io() {
        let definition = ObjectDefinition {
            key: ObjectKey::new("printer.system-location").unwrap(),
            oid: ObjectId::parse("1.3.6.1.2.1.1.6.0").unwrap(),
            syntax: ObjectSyntax::Utf8 {
                trim_trailing_nul: true,
            },
            sensitivity: Sensitivity::Identifier,
            access: ObjectAccess::ReadOnly,
            qualification: DeviceQualification {
                manufacturer: "Brother".into(),
                models: vec!["fixture".into()],
                firmware: None,
                qualification_id: "fixture".into(),
            },
        };
        let error = SnmpClient
            .set_once(
                "127.0.0.1:9".parse().unwrap(),
                &definition,
                &Credentials::V2c(Community::new("private").unwrap()),
                &SetValue::Text("office".into()),
                ClientLimits::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ClientError::Protocol(SnmpError::UnregisteredObject)
        ));
    }

    #[test]
    fn decoded_values_are_checked_against_sdk_limits() {
        let binding = VarBind {
            oid: ObjectId::parse("1.3.6.1.2.1.1.1.0").unwrap(),
            value: ObjectValue::Bytes(vec![0; 500]),
        };
        let limits = ClientLimits {
            maximum_response_bytes: 484,
            ..ClientLimits::default()
        };
        assert!(matches!(
            enforce_decoded_limits(&[binding], limits),
            Err(ClientError::ResponseTooLarge)
        ));
    }
}

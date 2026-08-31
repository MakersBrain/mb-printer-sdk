// SPDX-License-Identifier: AGPL-3.0-or-later
//! Async native HTTP/IPPS boundary for the portable IPP codec.
//!
//! All futures run on the caller's executor. This module never creates or
//! enters a Tokio runtime.

use futures_util::StreamExt as _;
use mb_printer_core::{
    administration::{
        ChangeBinding, ChangePlanError, ConfirmedChangePlan, PlanChangeRequest,
        SupportedValuesError, ipp_change_is_applied, parse_get_printer_supported_values,
        plan_ipp_change_with_supported_values, set_printer_attributes_request,
        validate_confirmed_ipp_change_with_supported_values,
    },
    discovery::{DeviceSnapshot, ObservationOrigin, normalize_ipp},
    ipp::{self as codec, Limits, Message, ValueData},
};
use std::{collections::BTreeMap, future::Future, pin::Pin, time::Duration};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IppScheme {
    #[default]
    Ipp,
    Ipps,
}

impl IppScheme {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ipp => "ipp",
            Self::Ipps => "ipps",
        }
    }

    const fn http_scheme(self) -> &'static str {
        match self {
            Self::Ipp => "http",
            Self::Ipps => "https",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IppEndpoint {
    pub scheme: IppScheme,
    pub host: String,
    pub port: u16,
    pub resource: String,
}

impl IppEndpoint {
    pub fn ipp(host: impl Into<String>, port: u16, resource: impl Into<String>) -> Self {
        Self {
            scheme: IppScheme::Ipp,
            host: host.into(),
            port,
            resource: resource.into(),
        }
    }

    pub fn ipps(host: impl Into<String>, port: u16, resource: impl Into<String>) -> Self {
        Self {
            scheme: IppScheme::Ipps,
            host: host.into(),
            port,
            resource: resource.into(),
        }
    }

    pub fn printer_uri(&self) -> Result<String, IppClientError> {
        self.validate()?;
        let host = bracket_ipv6(&self.host);
        Ok(format!(
            "{}://{}:{}{}",
            self.scheme.as_str(),
            host,
            self.port,
            self.resource
        ))
    }

    fn http_url(&self) -> Result<reqwest::Url, IppClientError> {
        self.validate()?;
        let host = bracket_ipv6(&self.host);
        reqwest::Url::parse(&format!(
            "{}://{}:{}{}",
            self.scheme.http_scheme(),
            host,
            self.port,
            self.resource
        ))
        .map_err(|_| IppClientError::InvalidEndpoint)
    }

    fn validate(&self) -> Result<(), IppClientError> {
        if self.host.is_empty()
            || self.host.contains(['\r', '\n', '/', '@'])
            || self.port == 0
            || !self.resource.starts_with('/')
            || self.resource.contains(['\r', '\n'])
        {
            return Err(IppClientError::InvalidEndpoint);
        }
        Ok(())
    }
}

fn bracket_ipv6(host: &str) -> String {
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InspectLimits {
    pub timeout: Duration,
    pub maximum_response_bytes: usize,
    pub codec: Limits,
}

impl Default for InspectLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            maximum_response_bytes: Limits::default().max_message_bytes,
            codec: Limits::default(),
        }
    }
}

impl InspectLimits {
    fn validate(self) -> Result<Self, IppClientError> {
        if self.timeout.is_zero()
            || self.maximum_response_bytes < 8
            || self.maximum_response_bytes > self.codec.max_message_bytes
        {
            return Err(IppClientError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Debug, Error)]
pub enum IppClientError {
    #[error("invalid IPP endpoint")]
    InvalidEndpoint,
    #[error("invalid IPP inspection limits")]
    InvalidLimits,
    #[error("failed to encode IPP request: {0}")]
    Encode(#[from] codec::EncodeError),
    #[error("IPP HTTP transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("IPP HTTP request timed out")]
    Timeout,
    #[error("IPP HTTP response status was {0}")]
    HttpStatus(reqwest::StatusCode),
    #[error("IPP response is larger than the configured {limit}-byte limit")]
    ResponseTooLarge { limit: usize },
    #[error("failed to decode IPP response: {0}")]
    Decode(#[from] codec::DecodeError),
    #[error("IPP response request ID {actual} did not match request ID {expected}")]
    RequestIdMismatch { expected: u32, actual: u32 },
}

pub type IppOverUsbFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>>;

/// Caller-owned USB transaction boundary. Implementations perform one
/// standards-defined IPP-over-USB request/response exchange and must not
/// retry writes or create an executor.
pub trait AsyncIppOverUsbBackend: Send + Sync {
    fn transact(&self, request: Vec<u8>, maximum_response_bytes: usize) -> IppOverUsbFuture<'_>;
}

#[derive(Debug, Error)]
pub enum IppOverUsbError {
    #[error("invalid IPP-over-USB inspection limits")]
    InvalidLimits,
    #[error("failed to encode IPP-over-USB request: {0}")]
    Encode(#[from] codec::EncodeError),
    #[error("IPP-over-USB transaction timed out")]
    Timeout,
    #[error("IPP-over-USB transport failed: {0}")]
    Transport(String),
    #[error("IPP-over-USB response is larger than the configured {limit}-byte limit")]
    ResponseTooLarge { limit: usize },
    #[error("failed to decode IPP-over-USB response: {0}")]
    Decode(#[from] codec::DecodeError),
    #[error("IPP-over-USB response request ID {actual} did not match request ID {expected}")]
    RequestIdMismatch { expected: u32, actual: u32 },
}

#[derive(Debug, Clone)]
pub struct IppOverUsbClient<B> {
    backend: B,
}

impl<B: AsyncIppOverUsbBackend> IppOverUsbClient<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub async fn inspect(
        &self,
        request: &Message,
        limits: InspectLimits,
    ) -> Result<Message, IppOverUsbError> {
        let limits = limits
            .validate()
            .map_err(|_| IppOverUsbError::InvalidLimits)?;
        let expected_request_id = request.request_id;
        let encoded = request.encode(limits.codec)?;
        let bytes = tokio::time::timeout(
            limits.timeout,
            self.backend
                .transact(encoded, limits.maximum_response_bytes),
        )
        .await
        .map_err(|_| IppOverUsbError::Timeout)?
        .map_err(IppOverUsbError::Transport)?;
        if bytes.len() > limits.maximum_response_bytes {
            return Err(IppOverUsbError::ResponseTooLarge {
                limit: limits.maximum_response_bytes,
            });
        }
        let response = codec::decode(&bytes, limits.codec)?;
        if response.request_id != expected_request_id {
            return Err(IppOverUsbError::RequestIdMismatch {
                expected: expected_request_id,
                actual: response.request_id,
            });
        }
        Ok(response)
    }
}

#[derive(Debug)]
pub enum ApplyChangeOutcome {
    Verified {
        write_response: Message,
        observation: Message,
    },
    Rejected {
        write_response: Message,
    },
    ReadBackMismatch {
        write_response: Message,
        observation: Message,
    },
    /// The write may have reached the printer. This outcome must never be
    /// automatically retried.
    Ambiguous {
        stage: &'static str,
        error: IppClientError,
    },
}

#[derive(Debug, Error)]
pub enum ApplyChangeError {
    #[error("pre-write IPP read failed: {0}")]
    PreWriteRead(#[source] IppClientError),
    #[error("confirmed change validation failed: {0}")]
    InvalidPlan(#[from] ChangePlanError),
    #[error("Get-Printer-Supported-Values response failed validation: {0}")]
    SupportedValues(#[from] SupportedValuesError),
    #[error("invalid IPP endpoint: {0}")]
    Endpoint(#[source] IppClientError),
}

#[derive(Debug, Error)]
pub enum PlanChangeError {
    #[error("invalid IPP endpoint: {0}")]
    Endpoint(#[source] IppClientError),
    #[error("IPP capability read failed: {0}")]
    Read(#[source] IppClientError),
    #[error("Get-Printer-Supported-Values response failed validation: {0}")]
    SupportedValues(#[from] SupportedValuesError),
    #[error("change planning failed: {0}")]
    InvalidPlan(#[from] ChangePlanError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryOptions {
    pub maximum_formats: usize,
    pub maximum_focused_queries: usize,
    pub inspect: InspectLimits,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self {
            maximum_formats: 16,
            maximum_focused_queries: 8,
            inspect: InspectLimits::default(),
        }
    }
}

#[derive(Debug)]
pub struct IppDiscoveryResult {
    pub base_response: Message,
    pub focused_responses: Vec<Message>,
    pub format_responses: BTreeMap<String, Message>,
    pub snapshot: DeviceSnapshot,
    pub focused_queries_truncated: bool,
    pub formats_truncated: bool,
}

#[derive(Clone)]
pub struct IppClient {
    http: reqwest::Client,
}

impl std::fmt::Debug for IppClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("IppClient").finish_non_exhaustive()
    }
}

impl IppClient {
    pub fn new() -> Result<Self, IppClientError> {
        Ok(Self {
            http: reqwest::Client::builder().build()?,
        })
    }

    /// Construct a client from a caller-configured reqwest client, for example
    /// one containing explicit additional trust roots.
    pub fn with_http_client(http: reqwest::Client) -> Self {
        Self { http }
    }

    pub async fn inspect(
        &self,
        endpoint: &IppEndpoint,
        request: &Message,
        limits: InspectLimits,
    ) -> Result<Message, IppClientError> {
        let limits = limits.validate()?;
        let url = endpoint.http_url()?;
        let expected_request_id = request.request_id;
        let body = request.encode(limits.codec)?;
        let request = self
            .http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/ipp")
            .header(reqwest::header::ACCEPT, "application/ipp")
            .body(body);
        let response = tokio::time::timeout(limits.timeout, request.send())
            .await
            .map_err(|_| IppClientError::Timeout)??;
        if !response.status().is_success() {
            return Err(IppClientError::HttpStatus(response.status()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > limits.maximum_response_bytes as u64)
        {
            return Err(IppClientError::ResponseTooLarge {
                limit: limits.maximum_response_bytes,
            });
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = tokio::time::timeout(limits.timeout, stream.next())
            .await
            .map_err(|_| IppClientError::Timeout)?
        {
            let chunk = chunk?;
            let new_length = bytes.len().saturating_add(chunk.len());
            if new_length > limits.maximum_response_bytes {
                return Err(IppClientError::ResponseTooLarge {
                    limit: limits.maximum_response_bytes,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        let response = codec::decode(&bytes, limits.codec)?;
        if response.request_id != expected_request_id {
            return Err(IppClientError::RequestIdMismatch {
                expected: expected_request_id,
                actual: response.request_id,
            });
        }
        Ok(response)
    }

    /// Read and validate all constraints needed to create a confirmed change.
    /// Settable `xxx-supported` attributes always use RFC 3380's
    /// Get-Printer-Supported-Values operation and fail closed without it.
    pub async fn plan_change(
        &self,
        endpoint: &IppEndpoint,
        request: PlanChangeRequest<'_>,
        limits: InspectLimits,
    ) -> Result<ConfirmedChangePlan, PlanChangeError> {
        let printer_uri = endpoint.printer_uri().map_err(PlanChangeError::Endpoint)?;
        let mut requested = vec![
            request.setting.to_owned(),
            "operations-supported".into(),
            "printer-settable-attributes-supported".into(),
        ];
        if let Some(base) = request.setting.strip_suffix("-default") {
            requested.push(format!("{base}-supported"));
        }
        let read = codec::get_printer_attributes_request(
            &printer_uri,
            requested.iter().map(String::as_str),
            None,
            1,
        );
        let observation = self
            .inspect(endpoint, &read, limits)
            .await
            .map_err(PlanChangeError::Read)?;
        let supported = if request.setting.ends_with("-supported") {
            let supported_request =
                codec::get_printer_supported_values_request(&printer_uri, [request.setting], 2)
                    .expect("a suffix-checked xxx-supported attribute is valid");
            let response = self
                .inspect(endpoint, &supported_request, limits)
                .await
                .map_err(PlanChangeError::Read)?;
            Some(parse_get_printer_supported_values(&response)?)
        } else {
            None
        };
        plan_ipp_change_with_supported_values(&observation, supported.as_ref(), request)
            .map_err(PlanChangeError::InvalidPlan)
    }

    /// Apply one already-confirmed IPP change. The method re-reads immediately
    /// before transmission, sends the write exactly once, and verifies by
    /// reading again. Post-transmission transport failures are ambiguous.
    pub async fn apply_confirmed_change(
        &self,
        endpoint: &IppEndpoint,
        plan: &ConfirmedChangePlan,
        binding: ChangeBinding<'_>,
        limits: InspectLimits,
    ) -> Result<ApplyChangeOutcome, ApplyChangeError> {
        let printer_uri = endpoint.printer_uri().map_err(ApplyChangeError::Endpoint)?;
        let mut requested = vec![
            plan.setting.clone(),
            "operations-supported".into(),
            "printer-settable-attributes-supported".into(),
        ];
        if let Some(base) = plan.setting.strip_suffix("-default") {
            requested.push(format!("{base}-supported"));
        }
        let read_request = codec::get_printer_attributes_request(
            &printer_uri,
            requested.iter().map(String::as_str),
            None,
            1,
        );
        let supported = if plan.setting.ends_with("-supported") {
            let request = codec::get_printer_supported_values_request(
                &printer_uri,
                [plan.setting.as_str()],
                3,
            )
            .expect("a suffix-checked xxx-supported attribute is valid");
            let response = self
                .inspect(endpoint, &request, limits)
                .await
                .map_err(ApplyChangeError::PreWriteRead)?;
            Some(parse_get_printer_supported_values(&response)?)
        } else {
            None
        };
        let immediately_current = self
            .inspect(endpoint, &read_request, limits)
            .await
            .map_err(ApplyChangeError::PreWriteRead)?;
        validate_confirmed_ipp_change_with_supported_values(
            plan,
            &immediately_current,
            supported.as_ref(),
            binding,
        )?;

        let write_request = set_printer_attributes_request(
            &printer_uri,
            &plan.setting,
            plan.requested_protocol_value.clone(),
            2,
        );
        let write_response = match self.inspect(endpoint, &write_request, limits).await {
            Ok(response) => response,
            Err(error) => {
                return Ok(ApplyChangeOutcome::Ambiguous {
                    stage: "write",
                    error,
                });
            }
        };
        if write_response.code >= 0x0100 {
            return Ok(ApplyChangeOutcome::Rejected { write_response });
        }
        let observation = match self.inspect(endpoint, &read_request, limits).await {
            Ok(response) => response,
            Err(error) => {
                return Ok(ApplyChangeOutcome::Ambiguous {
                    stage: "verification-read",
                    error,
                });
            }
        };
        if ipp_change_is_applied(plan, &observation) {
            Ok(ApplyChangeOutcome::Verified {
                write_response,
                observation,
            })
        } else {
            Ok(ApplyChangeOutcome::ReadBackMismatch {
                write_response,
                observation,
            })
        }
    }

    pub async fn discover(
        &self,
        endpoint: &IppEndpoint,
        origin: &ObservationOrigin,
        options: DiscoveryOptions,
    ) -> Result<IppDiscoveryResult, IppClientError> {
        if options.maximum_formats == 0 || options.maximum_focused_queries == 0 {
            return Err(IppClientError::InvalidLimits);
        }
        let printer_uri = endpoint.printer_uri()?;
        let base_request = codec::get_printer_attributes_request(
            &printer_uri,
            ["all"],
            None,
            request_number(&origin.request_id, 1),
        );
        let base_response = self
            .inspect(endpoint, &base_request, options.inspect)
            .await?;
        let mut snapshot = normalize_ipp(&base_response, origin, None);
        let focused_queries = focused_query_sets(&base_response);
        let focused_queries_truncated = focused_queries.len() > options.maximum_focused_queries;
        let mut focused_responses = Vec::new();
        for (index, attributes) in focused_queries
            .into_iter()
            .take(options.maximum_focused_queries)
            .enumerate()
        {
            let request = codec::get_printer_attributes_request(
                &printer_uri,
                attributes,
                None,
                request_number(&origin.request_id, index as u32 + 2),
            );
            let response = self.inspect(endpoint, &request, options.inspect).await?;
            let mut focused_origin = origin.clone();
            focused_origin.request_id = format!("{}:focused:{}", origin.request_id, index + 1);
            merge_supplemental_snapshot(
                &mut snapshot,
                normalize_ipp(&response, &focused_origin, None),
            );
            focused_responses.push(response);
        }
        let formats = if has_format_varying_attributes(&base_response) {
            text_attribute_values(&base_response, b"document-format-supported")
        } else {
            Vec::new()
        };
        let formats_truncated = formats.len() > options.maximum_formats;
        let mut format_responses = BTreeMap::new();
        for (index, format) in formats
            .into_iter()
            .take(options.maximum_formats)
            .enumerate()
        {
            let request = codec::get_printer_attributes_request(
                &printer_uri,
                ["all"],
                Some(&format),
                request_number(
                    &origin.request_id,
                    index as u32 + options.maximum_focused_queries as u32 + 2,
                ),
            );
            let response = self.inspect(endpoint, &request, options.inspect).await?;
            let mut format_origin = origin.clone();
            format_origin.request_id = format!("{}:format:{format}", origin.request_id);
            let normalized = normalize_ipp(&response, &format_origin, Some(&format));
            snapshot
                .job_capabilities
                .extend(normalized.job_capabilities);
            snapshot.observations.extend(normalized.observations);
            format_responses.insert(format, response);
        }
        Ok(IppDiscoveryResult {
            base_response,
            focused_responses,
            format_responses,
            snapshot,
            focused_queries_truncated,
            formats_truncated,
        })
    }
}

fn focused_query_sets(message: &Message) -> Vec<&'static [&'static str]> {
    const STATE: &[&str] = &["printer-state", "printer-state-reasons", "printer-alert"];
    const SUPPLIES: &[&str] = &[
        "marker-names",
        "marker-types",
        "marker-colors",
        "marker-levels",
        "marker-high-levels",
        "marker-low-levels",
    ];
    const MEDIA_READY: &[&str] = &["media-ready", "media-col-ready"];
    const SECURITY: &[&str] = &[
        "uri-authentication-supported",
        "uri-security-supported",
        "ipp-features-supported",
    ];

    let advertised = text_attribute_values(message, b"requested-attributes-supported")
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    [STATE, SUPPLIES, MEDIA_READY, SECURITY]
        .into_iter()
        .filter(|group| {
            let indicated = group.iter().any(|name| {
                advertised.contains(*name) || find_attribute(message, name.as_bytes()).is_some()
            });
            indicated
                && group
                    .iter()
                    .any(|name| find_attribute(message, name.as_bytes()).is_none())
        })
        .collect()
}

fn merge_supplemental_snapshot(target: &mut DeviceSnapshot, supplemental: DeviceSnapshot) {
    if target.identity.uuid.is_none() {
        target.identity.uuid = supplemental.identity.uuid;
    }
    if target.identity.serial_number.is_none() {
        target.identity.serial_number = supplemental.identity.serial_number;
    }
    if target.identity.device_id.is_none() {
        target.identity.device_id = supplemental.identity.device_id;
    }
    if target.identity.manufacturer.is_none() {
        target.identity.manufacturer = supplemental.identity.manufacturer;
    }
    if target.identity.model.is_none() {
        target.identity.model = supplemental.identity.model;
    }
    if target.state.state.is_none() {
        target.state.state = supplemental.state.state;
    }
    for reason in supplemental.state.reasons {
        if !target.state.reasons.contains(&reason) {
            target.state.reasons.push(reason);
        }
    }
    merge_unique_by(&mut target.supplies, supplemental.supplies, |value| {
        value.id.clone()
    });
    merge_unique_by(
        &mut target.job_capabilities,
        supplemental.job_capabilities,
        |value| (value.id.clone(), value.format_scope.clone()),
    );
    merge_unique_by(
        &mut target.device_settings,
        supplemental.device_settings,
        |value| value.id.clone(),
    );
    merge_unique_by(
        &mut target.mutation_support,
        supplemental.mutation_support,
        |value| value.setting.clone(),
    );
    merge_unique_by(&mut target.operations, supplemental.operations, |value| {
        value.operation.clone()
    });
    target.observations.extend(supplemental.observations);
}

fn merge_unique_by<T, K: PartialEq>(
    target: &mut Vec<T>,
    supplemental: Vec<T>,
    key: impl Fn(&T) -> K,
) {
    for value in supplemental {
        let value_key = key(&value);
        if !target.iter().any(|existing| key(existing) == value_key) {
            target.push(value);
        }
    }
}

fn find_attribute<'a>(message: &'a Message, name: &[u8]) -> Option<&'a codec::Attribute> {
    message
        .groups
        .iter()
        .flat_map(|group| &group.attributes)
        .find(|attribute| attribute.name == name)
}

fn has_format_varying_attributes(message: &Message) -> bool {
    text_attribute_values(message, b"document-format-varying-attributes")
        .into_iter()
        .any(|value| !value.is_empty() && value != "none")
}

fn text_attribute_values(message: &Message, name: &[u8]) -> Vec<String> {
    message
        .groups
        .iter()
        .flat_map(|group| group.attributes.iter())
        .filter(|attribute| attribute.name == name)
        .flat_map(|attribute| attribute.values.iter())
        .filter_map(|value| {
            let ValueData::Bytes(value) = &value.data else {
                return None;
            };
            std::str::from_utf8(value).ok().map(str::to_owned)
        })
        .collect()
}

fn request_number(seed: &str, offset: u32) -> u32 {
    let mut hash = 2_166_136_261u32;
    for byte in seed.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    hash.wrapping_add(offset)
}

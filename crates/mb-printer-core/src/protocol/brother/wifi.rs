// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    capabilities::Protocol,
    protocol::{Action, Plan, ResponseValidation, SOURCE_COMMIT},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PJL_HEADER: &[u8] = b"\x1b%-12345X@PJL\r\n";
pub const PJL_FOOTER: &[u8] = b"\x1b%-12345X";
pub const REBOOT_COMMAND: &[u8] = &[
    0x1b, 0x69, 0x58, 0x2a, 0x31, 0x03, 0, 0x01, 0x2e, 0, 0, 0, 0x2c, 0,
];
const PASSWORD_KEY: [u8; 16] = [
    0x0d, 0xae, 0xe4, 0xa1, 0x8b, 0x7f, 0x26, 0x5e, 0x72, 0x5b, 0x17, 0x7a, 0x71, 0xcd, 0xec, 0x4d,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WirelessEncryption {
    None,
    Wep,
    Tkip,
    Aes,
    Ckip,
    Cmic,
    CkipCmic,
    TkipAes,
}

impl WirelessEncryption {
    pub const fn code(self) -> u8 {
        match self {
            Self::None => 1,
            Self::Wep => 2,
            Self::Tkip => 3,
            Self::Aes => 4,
            Self::Ckip => 5,
            Self::Cmic => 6,
            Self::CkipCmic => 7,
            Self::TkipAes => 8,
        }
    }
}

impl TryFrom<&str> for WirelessEncryption {
    type Error = WirelessError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "none" => Ok(Self::None),
            "wep" => Ok(Self::Wep),
            "tkip" => Ok(Self::Tkip),
            "aes" => Ok(Self::Aes),
            "ckip" => Ok(Self::Ckip),
            "cmic" => Ok(Self::Cmic),
            "ckip-cmic" => Ok(Self::CkipCmic),
            "tkip-aes" => Ok(Self::TkipAes),
            _ => Err(WirelessError::UnknownEncryption),
        }
    }
}

impl TryFrom<u8> for WirelessEncryption {
    type Error = WirelessError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::None),
            2 => Ok(Self::Wep),
            3 => Ok(Self::Tkip),
            4 => Ok(Self::Aes),
            5 => Ok(Self::Ckip),
            6 => Ok(Self::Cmic),
            7 => Ok(Self::CkipCmic),
            8 => Ok(Self::TkipAes),
            _ => Err(WirelessError::UnknownEncryption),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WirelessAuthentication {
    Open,
    SharedKey,
    WpaPsk,
    Leap,
    EapFast,
    Peap,
    EapTtls,
    EapTls,
    WpaOnly,
    Wpa2Only,
}

impl WirelessAuthentication {
    pub const fn code(self) -> u8 {
        match self {
            Self::Open => 1,
            Self::SharedKey => 2,
            Self::WpaPsk => 3,
            Self::Leap => 7,
            Self::EapFast => 13,
            Self::Peap => 15,
            Self::EapTtls => 16,
            Self::EapTls => 17,
            Self::WpaOnly => 18,
            Self::Wpa2Only => 19,
        }
    }

    const fn uses_psk(self) -> bool {
        matches!(self, Self::WpaPsk | Self::WpaOnly | Self::Wpa2Only)
    }
}

impl TryFrom<&str> for WirelessAuthentication {
    type Error = WirelessError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "open" => Ok(Self::Open),
            "shared-key" => Ok(Self::SharedKey),
            "wpa-psk" => Ok(Self::WpaPsk),
            "leap" => Ok(Self::Leap),
            "eap-fast" => Ok(Self::EapFast),
            "peap" => Ok(Self::Peap),
            "eap-ttls" => Ok(Self::EapTtls),
            "eap-tls" => Ok(Self::EapTls),
            "wpa-only" => Ok(Self::WpaOnly),
            "wpa2-only" => Ok(Self::Wpa2Only),
            _ => Err(WirelessError::UnknownAuthentication),
        }
    }
}

impl TryFrom<u8> for WirelessAuthentication {
    type Error = WirelessError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Open),
            2 => Ok(Self::SharedKey),
            3 => Ok(Self::WpaPsk),
            7 => Ok(Self::Leap),
            13 => Ok(Self::EapFast),
            15 => Ok(Self::Peap),
            16 => Ok(Self::EapTtls),
            17 => Ok(Self::EapTls),
            18 => Ok(Self::WpaOnly),
            19 => Ok(Self::Wpa2Only),
            _ => Err(WirelessError::UnknownAuthentication),
        }
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WirelessError {
    #[error("invalid OBJBRNET OID")]
    InvalidOid,
    #[error("SSID must not be empty")]
    EmptySsid,
    #[error("authentication needs a password")]
    MissingPassword,
    #[error("open authentication requires no encryption")]
    InvalidOpenSecurity,
    #[error("unknown encryption")]
    UnknownEncryption,
    #[error("unknown authentication")]
    UnknownAuthentication,
}

#[derive(Clone)]
pub struct WirelessSettings {
    pub ssid: String,
    pub password: String,
    pub encryption: WirelessEncryption,
    pub authentication: WirelessAuthentication,
    pub infrastructure: bool,
    pub wireless_direct: bool,
    pub reboot: bool,
}

impl std::fmt::Debug for WirelessSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WirelessSettings")
            .field("ssid", &self.ssid)
            .field("password", &"[REDACTED]")
            .field("encryption", &self.encryption)
            .field("authentication", &self.authentication)
            .field("infrastructure", &self.infrastructure)
            .field("wireless_direct", &self.wireless_direct)
            .field("reboot", &self.reboot)
            .finish()
    }
}

impl WirelessSettings {
    pub fn command(&self) -> Result<Vec<u8>, WirelessError> {
        if self.ssid.is_empty() {
            return Err(WirelessError::EmptySsid);
        }
        if self.authentication != WirelessAuthentication::Open && self.password.is_empty() {
            return Err(WirelessError::MissingPassword);
        }
        if self.authentication == WirelessAuthentication::Open
            && self.encryption != WirelessEncryption::None
        {
            return Err(WirelessError::InvalidOpenSecurity);
        }

        let mut parameters = vec![
            ("458867", b"0".to_vec()),
            ("458878", b"1".to_vec()),
            ("458877", encode_ssid(&self.ssid)),
        ];
        if self.authentication.uses_psk() {
            parameters.push(("99458890", xor_password(self.password.as_bytes())))
        } else if self.encryption == WirelessEncryption::Wep {
            parameters.push(("99458889.1", xor_password(self.password.as_bytes())))
        }
        parameters.extend([
            ("458880", self.encryption.code().to_string().into_bytes()),
            (
                "458881",
                self.authentication.code().to_string().into_bytes(),
            ),
            (
                "459138.2",
                u8::from(self.infrastructure).to_string().into_bytes(),
            ),
            (
                "459138.3",
                u8::from(self.wireless_direct).to_string().into_bytes(),
            ),
            ("458865", b"1".to_vec()),
        ]);
        let mut output = PJL_HEADER.to_vec();
        for (oid, value) in parameters {
            output.extend(b"@PJL DEFAULT OBJBRNET=\"");
            output.extend(oid.as_bytes());
            output.push(b':');
            output.extend(value);
            output.extend(b"\"\r\n")
        }
        output.extend(PJL_FOOTER);
        if self.reboot {
            output.extend(REBOOT_COMMAND)
        }
        Ok(output)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WirelessField {
    Connected,
    Ipv4,
    Ssid,
    Encryption,
    Authentication,
    Infrastructure,
    WirelessDirect,
}

impl WirelessField {
    pub const ALL: [Self; 7] = [
        Self::Connected,
        Self::Ipv4,
        Self::Ssid,
        Self::Encryption,
        Self::Authentication,
        Self::Infrastructure,
        Self::WirelessDirect,
    ];

    pub const fn oid(self) -> &'static str {
        match self {
            Self::Connected => "458867",
            Self::Ipv4 => "458967.2",
            Self::Ssid => "458877",
            Self::Encryption => "458880",
            Self::Authentication => "458881",
            Self::Infrastructure => "459138.2",
            Self::WirelessDirect => "459138.3",
        }
    }

    pub fn command(self) -> Vec<u8> {
        inquire_command(self.oid()).expect("allowlisted OBJBRNET OID")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessPoint {
    pub ssid: String,
    pub channel: u8,
    pub power: i16,
    pub enterprise: bool,
    pub encrypted: bool,
}

pub fn xor_password(value: &[u8]) -> Vec<u8> {
    value
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ PASSWORD_KEY[index % PASSWORD_KEY.len()])
        .collect()
}

pub fn encode_ssid(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .iter()
        .flat_map(|byte| format!("-{byte:x}").into_bytes())
        .collect()
}

pub fn inquire_command(oid: &str) -> Result<Vec<u8>, WirelessError> {
    if !valid_oid(oid) {
        return Err(WirelessError::InvalidOid);
    }
    let mut output = PJL_HEADER.to_vec();
    output.extend(b"@PJL DEFAULT OBJBRNET=\"");
    output.extend(oid.as_bytes());
    output.extend(b"\"\r\n@PJL INQUIRE OBJBRNET\r\n");
    output.extend(PJL_FOOTER);
    Ok(output)
}

fn valid_oid(oid: &str) -> bool {
    let mut parts = oid.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    !first.is_empty()
        && first.bytes().all(|byte| byte.is_ascii_digit())
        && parts
            .next()
            .is_none_or(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && parts.next().is_none()
}

pub fn wifi_scan_start_command() -> Vec<u8> {
    pjl(b"DEFAULT OBJBRNET=\"458845:31-3a\"")
}

pub fn wifi_scan_result_command() -> Vec<u8> {
    pjl(b"INFO AVAILABLEWLAN")
}

fn pjl(command: &[u8]) -> Vec<u8> {
    let mut output = PJL_HEADER.to_vec();
    output.extend(b"@PJL ");
    output.extend(command);
    output.extend(b"\r\n");
    output.extend(PJL_FOOTER);
    output
}

pub fn wifi_status_command() -> Vec<u8> {
    WirelessField::Connected.command()
}

pub fn ip_address_command() -> Vec<u8> {
    WirelessField::Ipv4.command()
}

pub fn parse_oid_value(data: &[u8], oid: &str) -> Option<String> {
    if !valid_oid(oid) {
        return None;
    }
    let text = String::from_utf8_lossy(data);
    text.split(['\r', '\n', '\u{c}']).find_map(|line| {
        let line = line.trim().trim_start_matches('"');
        let (name, value) = line.split_once(':')?;
        if name.trim() != oid {
            return None;
        }
        let value = value.trim().trim_matches('"');
        Some(if oid == WirelessField::Ssid.oid() {
            decode_ssid(value)
        } else {
            value.into()
        })
    })
}

pub fn parse_wifi_status(data: &[u8]) -> Option<bool> {
    match parse_oid_value(data, WirelessField::Connected.oid())?.trim() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

pub fn parse_ip_address(data: &[u8]) -> Option<String> {
    let value = parse_oid_value(data, WirelessField::Ipv4.oid())?;
    let octets = value
        .trim_matches('-')
        .split('-')
        .map(|part| u8::from_str_radix(part, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    (octets.len() == 4).then(|| {
        octets
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(".")
    })
}

pub fn parse_encryption(data: &[u8]) -> Option<WirelessEncryption> {
    let code = parse_oid_value(data, WirelessField::Encryption.oid())
        .and_then(|value| value.trim().parse::<u8>().ok())?;
    WirelessEncryption::try_from(code).ok()
}

pub fn parse_authentication(data: &[u8]) -> Option<WirelessAuthentication> {
    let code = parse_oid_value(data, WirelessField::Authentication.oid())
        .and_then(|value| value.trim().parse::<u8>().ok())?;
    WirelessAuthentication::try_from(code).ok()
}

pub fn parse_boolean_field(data: &[u8], field: WirelessField) -> Option<bool> {
    match field {
        WirelessField::Connected
        | WirelessField::Infrastructure
        | WirelessField::WirelessDirect => {}
        _ => return None,
    }
    match parse_oid_value(data, field.oid())?.trim() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

pub fn parse_access_points(data: &[u8]) -> Vec<AccessPoint> {
    String::from_utf8_lossy(data)
        .replace('\0', "")
        .lines()
        .filter_map(|line| {
            let fields = csv_fields(line);
            if fields.len() < 8 || fields[0].trim() != "VAP" {
                return None;
            }
            Some(AccessPoint {
                ssid: decode_ssid(fields[1].trim()),
                channel: fields[4].trim().parse().ok()?,
                power: fields[5].trim().parse().ok()?,
                enterprise: fields[6].trim() == "3",
                encrypted: fields[7].trim() == "2",
            })
        })
        .collect()
}

fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '"' if quoted && characters.peek() == Some(&'"') => {
                field.push('"');
                characters.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            _ => field.push(character),
        }
    }
    fields.push(field);
    fields
}

fn decode_ssid(value: &str) -> String {
    let parts = value.trim_matches('-').split('-').collect::<Vec<_>>();
    if parts.len() < 2
        || parts.iter().any(|part| {
            part.is_empty() || part.len() > 2 || !part.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return value.into();
    }
    let Some(bytes) = parts
        .iter()
        .map(|part| u8::from_str_radix(part, 16).ok())
        .collect::<Option<Vec<_>>>()
    else {
        return value.into();
    };
    String::from_utf8(bytes).unwrap_or_else(|_| value.into())
}

pub fn wireless_status_plan() -> Plan {
    let mut actions = Vec::with_capacity(WirelessField::ALL.len() * 2);
    for field in WirelessField::ALL {
        actions.extend(wireless_field_plan(field).actions);
    }
    Plan {
        protocol: Protocol::Brother,
        source_commit: SOURCE_COMMIT.into(),
        actions,
    }
}

/// Builds one bounded, read-only query for a known OBJBRNET field.
///
/// Keeping this constructor keyed by [`WirelessField`] prevents callers from
/// accidentally turning arbitrary, unqualified OIDs into device probes.
pub fn wireless_field_plan(field: WirelessField) -> Plan {
    Plan {
        protocol: Protocol::Brother,
        source_commit: SOURCE_COMMIT.into(),
        actions: vec![
            command("Brother OBJBRNET inquire", field.command()),
            Action::CollectResponse {
                timeout_ms: 2000,
                idle_timeout_ms: 200,
                maximum_bytes: 4 * 1024,
                validation: ResponseValidation::BrotherObjbrnet,
            },
        ],
    }
}

pub fn wireless_scan_plan() -> Plan {
    Plan {
        protocol: Protocol::Brother,
        source_commit: SOURCE_COMMIT.into(),
        actions: vec![
            command("Brother WLAN scan start", wifi_scan_start_command()),
            Action::Delay { milliseconds: 5000 },
            command("Brother WLAN scan results", wifi_scan_result_command()),
            Action::CollectResponse {
                timeout_ms: 8000,
                idle_timeout_ms: 300,
                maximum_bytes: 16 * 1024,
                validation: ResponseValidation::BrotherWifiScan,
            },
        ],
    }
}

fn command(name: &str, bytes: Vec<u8>) -> Action {
    Action::CommandWrite {
        name: name.into(),
        bytes,
        atomic: true,
    }
}

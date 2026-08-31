// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "dns-sd")]

use mb_printer_native::transports::{
    dns_sd::{self, DiscoveryLimits, DnsSdBackend, ResolvedService},
    wifi::{self, IppEndpoint, IppProbeError, IppScheme},
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Default)]
struct FakeBackend {
    ipp: Vec<ResolvedService>,
    ipps: Vec<ResolvedService>,
    calls: Vec<(String, DiscoveryLimits)>,
}

impl DnsSdBackend for FakeBackend {
    fn browse(
        &mut self,
        service_type: &str,
        limits: DiscoveryLimits,
    ) -> Result<Vec<ResolvedService>, String> {
        self.calls.push((service_type.into(), limits));
        Ok(match service_type {
            dns_sd::IPP_SERVICE_TYPE => std::mem::take(&mut self.ipp),
            dns_sd::IPPS_SERVICE_TYPE => std::mem::take(&mut self.ipps),
            _ => Vec::new(),
        })
    }
}

fn service(service_type: &str, fullname: &str, host: &str, port: u16) -> ResolvedService {
    ResolvedService {
        service_type: service_type.into(),
        fullname: fullname.into(),
        host: host.into(),
        port,
        addresses: vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))],
        txt: vec![
            ("rp".into(), b"ipp/print".to_vec()),
            ("Ty".into(), b"Brother QL-1110NWB".to_vec()),
            ("UUID".into(), b"test-uuid".to_vec()),
            ("unknown".into(), vec![0xff, 0]),
        ],
    }
}

#[test]
fn discovery_preserves_scheme_resource_addresses_and_bounded_txt() {
    let mut ipp = service(
        dns_sd::IPP_SERVICE_TYPE,
        "Labels._ipp._tcp.local.",
        "labels.local.",
        631,
    );
    ipp.addresses.push(IpAddr::V6(Ipv6Addr::LOCALHOST));
    let ipps = service(
        dns_sd::IPPS_SERVICE_TYPE,
        "Secure._ipps._tcp.local.",
        "secure.local.",
        443,
    );
    let mut backend = FakeBackend {
        ipp: vec![ipp],
        ipps: vec![ipps],
        ..Default::default()
    };
    let discovered = dns_sd::discover_with_backend(&mut backend, DiscoveryLimits::default())
        .expect("fake discovery");
    assert_eq!(discovered.len(), 2);
    assert_eq!(discovered[0].endpoint.scheme, IppScheme::Ipp);
    assert_eq!(discovered[0].endpoint.resource, "/ipp/print");
    assert_eq!(discovered[0].addresses.len(), 2);
    assert_eq!(discovered[0].txt_utf8("TY"), Some("Brother QL-1110NWB"));
    assert_eq!(discovered[0].txt["unknown"], [0xff, 0]);
    assert_eq!(discovered[1].endpoint.scheme, IppScheme::Ipps);
    assert_eq!(
        backend
            .calls
            .iter()
            .map(|call| call.0.as_str())
            .collect::<Vec<_>>(),
        [dns_sd::IPP_SERVICE_TYPE, dns_sd::IPPS_SERVICE_TYPE]
    );
    assert!(backend.calls.iter().all(|call| call.1.timeout_ms > 0));
    assert!(backend.calls[0].1.timeout_ms <= 1500);
}

#[test]
fn discovery_is_sorted_deduplicated_and_capped() {
    let first = service(
        dns_sd::IPP_SERVICE_TYPE,
        "z._ipp._tcp.local.",
        "z.local.",
        631,
    );
    let duplicate = service(
        dns_sd::IPP_SERVICE_TYPE,
        "Z._ipp._tcp.local.",
        "replacement.local.",
        631,
    );
    let second = service(
        dns_sd::IPP_SERVICE_TYPE,
        "a._ipp._tcp.local.",
        "a.local.",
        631,
    );
    let mut backend = FakeBackend {
        ipp: vec![first, duplicate, second],
        ..Default::default()
    };
    let discovered = dns_sd::discover_with_backend(
        &mut backend,
        DiscoveryLimits {
            maximum_services: 2,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(discovered.len(), 2);
    assert_eq!(discovered[0].fullname, "a._ipp._tcp.local.");
    assert_eq!(discovered[1].fullname, "Z._ipp._tcp.local.");
    assert_eq!(discovered[1].endpoint.host, "replacement.local.");
    assert_eq!(backend.calls.len(), 1, "cap stops the second browse");
}

#[test]
fn malformed_or_over_limit_services_are_ignored() {
    let mut no_address = service(
        dns_sd::IPP_SERVICE_TYPE,
        "no-address._ipp._tcp.local.",
        "missing.local.",
        631,
    );
    no_address.addresses.clear();
    let mut oversized_txt = service(
        dns_sd::IPP_SERVICE_TYPE,
        "large._ipp._tcp.local.",
        "large.local.",
        631,
    );
    oversized_txt.txt = vec![("x".into(), vec![0; 64])];
    let wrong_type = service(
        "_http._tcp.local.",
        "wrong._http._tcp.local.",
        "wrong.local.",
        80,
    );
    let mut backend = FakeBackend {
        ipp: vec![no_address, oversized_txt, wrong_type],
        ..Default::default()
    };
    let discovered = dns_sd::discover_with_backend(
        &mut backend,
        DiscoveryLimits {
            maximum_txt_bytes: 16,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(discovered.is_empty());

    assert!(matches!(
        dns_sd::discover_with_backend(
            &mut FakeBackend::default(),
            DiscoveryLimits {
                timeout_ms: 0,
                ..Default::default()
            }
        ),
        Err(dns_sd::DiscoveryError::InvalidLimits)
    ));
}

#[test]
fn ipps_probe_is_explicitly_unavailable_and_never_downgraded() {
    let endpoint = IppEndpoint {
        scheme: IppScheme::Ipps,
        host: "127.0.0.1".into(),
        port: 9,
        resource: "/ipp/print".into(),
    };
    assert_eq!(
        wifi::query_ipp_status(&endpoint, 100).unwrap_err(),
        IppProbeError::SecureTransportUnavailable
    );
    let probes = wifi::probe_ipp_endpoints(std::slice::from_ref(&endpoint), 100);
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].0, endpoint);
    assert_eq!(probes[0].1, Err(IppProbeError::SecureTransportUnavailable));
}

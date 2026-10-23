//! `X-Forwarded-For`, `X-Forwarded-Proto`, and `X-Forwarded-Host` from a
//! trusted proxy. Oracle: never panics; the answer is 200 or a 400 refusal;
//! the reported client is the peer or an address the input names.

#![no_main]

use std::net::IpAddr;

use libfuzzer_sys::fuzz_target;
use serve_net::testing::TestRequest;
use serve_net::{ProxySource, TrustedProxies};

fuzz_target!(|data: &[u8]| {
    // Up to three fields, separated by 0xFF bytes in the input.
    // CR and LF would start new header lines, which this target does not test.
    let clean: Vec<u8> = data.iter().map(|&b| if b == b'\r' || b == b'\n' { b' ' } else { b }).collect();
    let mut parts = clean.splitn(3, |&b| b == 0xFF);
    let xff = parts.next().unwrap_or_default();
    let proto = parts.next().unwrap_or_default();
    let host = parts.next().unwrap_or_default();
    let mut request = b"GET / HTTP/1.1\r\nHost: a\r\nX-Forwarded-For: ".to_vec();
    request.extend_from_slice(xff);
    request.extend_from_slice(b"\r\nX-Forwarded-Proto: ");
    request.extend_from_slice(proto);
    request.extend_from_slice(b"\r\nX-Forwarded-Host: ");
    request.extend_from_slice(host);
    request.extend_from_slice(b"\r\n\r\n");
    let site = serve_net::handler(|req, reply| {
        let client = req.client().addr().map(|a| a.to_string()).unwrap_or_default();
        reply.text(200, &client);
    });
    let proxy = ProxySource::forwarded_for(TrustedProxies::loopback()).trust_scheme(true).trust_host(true);
    let reply = TestRequest::new(&request).behind(proxy).send(&site);
    assert!(reply.status == 200 || reply.status == 400 || reply.status == 431, "status {}", reply.status);
    if reply.status == 200 && !reply.body.is_empty() {
        let client: IpAddr = reply.text().parse().expect("the service writes a parsed address");
        let named = String::from_utf8_lossy(xff)
            .split(',')
            .filter_map(|e| e.trim().trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().ok())
            .map(|ip| match ip {
                IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
                v4 => v4,
            })
            .any(|ip| ip == client);
        assert!(client == IpAddr::from([127, 0, 0, 1]) || named, "client {client} not in the input");
    }
});

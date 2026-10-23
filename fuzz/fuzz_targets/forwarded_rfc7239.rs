//! RFC 7239 `Forwarded` from a trusted proxy. Oracle: never panics; the
//! answer is 200 or a refusal; an accepted host is echoed only when valid.

#![no_main]

use libfuzzer_sys::fuzz_target;
use serve_net::testing::TestRequest;
use serve_net::{ForwardedSource, ProxySource, TrustedProxies, UnknownClient};

fuzz_target!(|data: &[u8]| {
    let unknown = if data.first().is_some_and(|b| b & 1 == 1) { UnknownClient::Refuse } else { UnknownClient::Allow };
    // CR and LF would start new header lines, which this target does not test.
    let value: Vec<u8> = data.get(1..).unwrap_or_default().iter().map(|&b| if b == b'\r' || b == b'\n' { b' ' } else { b }).collect();
    let mut request = b"GET / HTTP/1.1\r\nHost: a\r\nForwarded: ".to_vec();
    request.extend_from_slice(&value);
    request.extend_from_slice(b"\r\n\r\n");
    let site = serve_net::handler(|req, reply| {
        let authority = req.head().authority().unwrap_or("").to_owned();
        reply.text(200, &authority);
    });
    let proxy = ProxySource::headers(ForwardedSource::Forwarded, TrustedProxies::loopback())
        .trust_scheme(true)
        .trust_host(true)
        .unknown(unknown);
    let reply = TestRequest::new(&request).behind(proxy).send(&site);
    assert!(matches!(reply.status, 200 | 400 | 431), "status {}", reply.status);
    if reply.status == 200 {
        // The authority never carries bytes a host may not contain.
        assert!(!reply.body.iter().any(|&b| b == b' ' || b == b'/' || b == b'"' || b < 0x21));
    }
});

//! The scheme and authority from forwarded headers (T-17), in both header
//! kinds, from a trusted and from an untrusted peer. Oracle: never panics;
//! the answer is 200 or a refusal; from an untrusted peer the scheme and the
//! authority are the listener's and the `Host` field's whatever the headers
//! say; from a trusted one the authority is the `Host` field or a valid
//! authority taken from the input, never anything else.

#![no_main]

use std::net::SocketAddr;

use std::sync::Mutex;

use libfuzzer_sys::fuzz_target;
use serve_net::testing::TestRequest;
use serve_net::{
    Envelope, ForwardedSource, HeadAction, HttpHead, HttpReply, HttpRequest, HttpService, Outcome, ProxySource, Scheme,
    TrustedProxies,
};

const HOST: &str = "origin.example";

/// Answers with the scheme and authority `call` sees, and keeps what `head` saw.
#[derive(Default)]
struct Seen {
    head: Mutex<Option<String>>,
}

fn line(client: &serve_net::ClientInfo, authority: Option<&str>) -> String {
    let scheme = if matches!(client.scheme(), Scheme::Https) { "https" } else { "http" };
    format!("{scheme}
{}", authority.unwrap_or(""))
}

impl HttpService for Seen {
    fn head(&self, head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
        *self.head.lock().unwrap() = Some(line(head.client(), head.authority()));
        HeadAction::Proceed
    }
    fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        reply.text(200, &line(req.client(), req.head().authority()));
        Outcome::Done
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else { return };
    // CR and LF would start new header lines, which this target does not test.
    let clean: Vec<u8> = rest.iter().map(|&b| if b == b'\r' || b == b'\n' { b' ' } else { b }).collect();
    let mut parts = clean.splitn(2, |&b| b == 0xFF);
    let proto = parts.next().unwrap_or_default();
    let host = parts.next().unwrap_or_default();
    let rfc7239 = mode & 1 == 1;
    let trusted_peer = mode & 2 == 2;

    let mut request = format!("GET / HTTP/1.1\r\nHost: {HOST}\r\n").into_bytes();
    if rfc7239 {
        request.extend_from_slice(b"Forwarded: for=198.51.100.7;proto=");
        request.extend_from_slice(proto);
        request.extend_from_slice(b";host=");
        request.extend_from_slice(host);
    } else {
        request.extend_from_slice(b"X-Forwarded-For: 198.51.100.7\r\nX-Forwarded-Proto: ");
        request.extend_from_slice(proto);
        request.extend_from_slice(b"\r\nX-Forwarded-Host: ");
        request.extend_from_slice(host);
    }
    request.extend_from_slice(b"\r\n\r\n");

    let site = Seen::default();
    let source = if rfc7239 { ForwardedSource::Forwarded } else { ForwardedSource::XForwardedFor };
    let proxy = ProxySource::headers(source, TrustedProxies::loopback()).trust_scheme(true).trust_host(true);
    let peer: SocketAddr = if trusted_peer { ([127, 0, 0, 1], 40000).into() } else { ([203, 0, 113, 5], 40000).into() };
    let reply = TestRequest::new(&request).client(peer).behind(proxy).send(&site);
    assert!(
        reply.status == 200 || (400..=499).contains(&reply.status),
        "status {}",
        reply.status
    );
    if reply.status != 200 {
        return;
    }
    let text = reply.text();
    let at_head = site.head.lock().unwrap().clone().expect("head ran before call");
    assert_eq!(at_head, text, "head and call saw different requests");
    let (scheme, authority) = text.split_once('\n').expect("the service writes two lines");
    assert!(scheme == "http" || scheme == "https");
    if !trusted_peer {
        assert_eq!(scheme, "http", "an untrusted peer set the scheme");
        assert_eq!(authority, HOST, "an untrusted peer set the authority");
        return;
    }
    if authority != HOST {
        assert!(!authority.is_empty() && authority.len() <= 255, "authority {authority:?}");
        // RFC 3986 authority bytes: unreserved, percent-encoded, sub-delims,
        // the port separator, and the brackets of an IPv6 literal.
        assert!(
            authority.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~%!$&'()*+,;=:[]".contains(&b)),
            "authority with a byte no authority has: {authority:?}"
        );
        let from_input = String::from_utf8_lossy(host);
        assert!(from_input.contains(authority), "authority {authority:?} not in the input");
    }
});

//! Reading a reply back through `Reply::parts`: status, fields, body, and the
//! flags a cache needs, as the service set them.

#![cfg(feature = "http1")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serve_net::testing::respond;

/// What a handler read back from its own reply.
#[derive(Debug, Default, Clone, PartialEq)]
struct Seen {
    status: u16,
    fields: Vec<(String, Vec<u8>)>,
    first_x: Option<Vec<u8>>,
    all_x: Vec<Vec<u8>>,
    body: Vec<u8>,
    streamed: bool,
    closes: bool,
    retry: Option<Duration>,
}

fn seen(reply: &serve_net::HttpReply) -> Seen {
    let p = reply.parts();
    Seen {
        status: p.status(),
        fields: p
            .headers()
            .map(|(n, v)| (n.to_string(), v.to_vec()))
            .collect(),
        first_x: p.header("X-Tag").map(<[u8]>::to_vec),
        all_x: p.header_all("x-tag").map(<[u8]>::to_vec).collect(),
        body: p.body().to_vec(),
        streamed: p.is_streamed(),
        closes: p.closes(),
        retry: p.retry_after(),
    }
}

const GET: &[u8] = b"GET / HTTP/1.1\r\nHost: a\r\n\r\n";

#[test]
fn test_reply_reads_back_status_fields_and_body() {
    let got = Arc::new(Mutex::new(Seen::default()));
    let out = Arc::clone(&got);
    let site = serve_net::handler(move |_req, reply| {
        reply
            .text(201, "hello")
            .header("x-tag", "one")
            .header("X-Tag", "two")
            .retry_after(Duration::from_secs(3))
            .close();
        *out.lock().unwrap() = seen(reply);
    });
    let wire = respond(&site, GET);
    let got = got.lock().unwrap().clone();
    assert_eq!(got.status, 201);
    assert_eq!(
        got.fields,
        vec![
            (
                "content-type".to_string(),
                b"text/plain; charset=utf-8".to_vec()
            ),
            ("x-tag".to_string(), b"one".to_vec()),
            ("X-Tag".to_string(), b"two".to_vec()),
        ]
    );
    assert_eq!(got.first_x.as_deref(), Some(&b"one"[..]));
    assert_eq!(got.all_x, vec![b"one".to_vec(), b"two".to_vec()]);
    assert_eq!(got.body, b"hello");
    assert!(!got.streamed);
    assert!(got.closes);
    assert_eq!(got.retry, Some(Duration::from_secs(3)));
    // What was read back is what went out.
    assert_eq!(wire.status, 201);
    assert_eq!(wire.body, b"hello");
    assert_eq!(wire.header("x-tag"), Some(&b"one"[..]));
}

#[test]
fn test_untouched_reply_reads_as_200_empty() {
    let got = Arc::new(Mutex::new(Seen::default()));
    let out = Arc::clone(&got);
    let site = serve_net::handler(move |_req, reply| {
        *out.lock().unwrap() = seen(reply);
    });
    let wire = respond(&site, GET);
    let got = got.lock().unwrap().clone();
    assert_eq!(got.status, 200);
    assert!(got.fields.is_empty());
    assert!(got.first_x.is_none());
    assert!(got.body.is_empty());
    assert!(!got.closes);
    assert_eq!(got.retry, None);
    assert_eq!(wire.status, 200);
}

#[test]
fn test_static_body_reads_back_without_a_copy() {
    static PAGE: &[u8] = b"<p>cached</p>";
    let same = Arc::new(Mutex::new(false));
    let out = Arc::clone(&same);
    let site = serve_net::handler(move |_req, reply| {
        reply.status(200).body_static(PAGE);
        // The very bytes the service gave, not a copy.
        *out.lock().unwrap() = core::ptr::eq(reply.parts().body(), PAGE);
    });
    let wire = respond(&site, GET);
    assert!(*same.lock().unwrap());
    assert_eq!(wire.body, PAGE);
}

#[test]
fn test_streamed_reply_reads_as_streamed() {
    let got = Arc::new(Mutex::new(Seen::default()));
    let out = Arc::clone(&got);
    let site = serve_net::handler(move |_req, reply| {
        reply.stream().send(b"part");
        *out.lock().unwrap() = seen(reply);
    });
    let _ = respond(&site, GET);
    let got = got.lock().unwrap().clone();
    assert!(got.streamed);
    assert_eq!(got.body, b"part");
}

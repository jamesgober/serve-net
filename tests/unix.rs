//! Real Unix sockets: the listener, the peer credential check, and forwarded
//! headers from a proxy on the other end of the socket.

#![cfg(all(unix, feature = "http1"))]

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serve_net::{Ingress, ListenerSpec, ProxySource, Server, TrustedProxies, UnixPeerTrust};

fn socket_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("serve-net-{tag}-{}.sock", std::process::id()))
}

fn start(tag: &str, ingress: Ingress) -> (serve_net::Running, PathBuf) {
    let path = socket_path(tag);
    let server = Server::builder()
        .listener(ListenerSpec::unix("api", &path).ingress(ingress))
        .serve(serve_net::handler(|req, reply| {
            let c = req.client();
            let text = format!(
                "{:?} {} uid={:?}",
                c.ingress(),
                c.addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "-".into()),
                c.peer_credentials().and_then(|p| p.uid)
            );
            reply.text(200, &text);
        }))
        .unwrap();
    (server, path)
}

/// One request; `None` when the server closed without answering.
fn ask(path: &PathBuf, request: &[u8]) -> Option<String> {
    let mut s = UnixStream::connect(path).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(request).ok()?;
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got);
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if got.len() >= end + 4 + len {
                        break;
                    }
                }
            }
        }
    }
    if got.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&got).into_owned())
    }
}

fn own_uid(path: &PathBuf) -> u32 {
    std::fs::metadata(path).unwrap().uid()
}

#[test]
fn test_unix_same_user_is_served_with_credentials() {
    let (server, path) = start("same", Ingress::Unix(UnixPeerTrust::SameUser, None));
    let uid = own_uid(&path);
    let a = ask(&path, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").expect("served");
    assert!(a.starts_with("HTTP/1.1 200"), "{a}");
    assert!(a.ends_with(&format!("Unix - uid=Some({uid})")), "{a}");
    let listeners = server.listeners();
    assert_eq!(listeners[0].ingress, serve_net::IngressKind::Unix);
    let _ = server.drain(Duration::from_secs(2));
    assert!(!path.exists(), "the socket file was left behind");
}

#[test]
fn test_unix_peer_outside_the_allowlist_is_closed_unread() {
    let path = socket_path("probe");
    let probe = Server::builder()
        .listener(ListenerSpec::unix("probe", &path))
        .serve(serve_net::handler(|_r, reply| {
            reply.text(200, "x");
        }))
        .unwrap();
    let other = own_uid(&path).wrapping_add(12_345);
    let _ = probe.drain(Duration::from_secs(1));

    let (server, path) = start(
        "deny",
        Ingress::Unix(UnixPeerTrust::Users(vec![other]), None),
    );
    assert!(ask(&path, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").is_none());
    assert_eq!(server.stats().requests, 0);
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_unix_proxy_on_the_socket_names_the_client() {
    let inner = ProxySource::forwarded_for(TrustedProxies::none());
    let (server, path) = start("xff", Ingress::Unix(UnixPeerTrust::SameUser, Some(inner)));
    let a = ask(
        &path,
        b"GET / HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 203.0.113.9\r\n\r\n",
    )
    .expect("served");
    assert!(a.contains(" 203.0.113.9 "), "{a}");
    // The proxy on the socket must say who the client is.
    let b = ask(&path, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").expect("answered");
    assert!(b.starts_with("HTTP/1.1 400"), "{b}");
    let _ = server.drain(Duration::from_secs(2));
}

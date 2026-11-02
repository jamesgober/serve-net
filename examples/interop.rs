//! The server behind real front ends, for the interop CI job: forwarded
//! headers on 127.0.0.1:9001 (nginx, Apache) and the PROXY protocol on
//! 127.0.0.1:9002 (HAProxy). Each reply says what the server learned, and
//! `.github/interop/run.sh` checks it. Press Enter to stop.

use std::time::Duration;

use serve_net::{Ingress, ListenerSpec, ProxySource, ProxyVersions, Server, TrustedProxies};

fn main() -> Result<(), serve_net::ServeError> {
    let forwarded = Ingress::Proxy(ProxySource::forwarded_for(TrustedProxies::loopback()));
    let proxy = Ingress::Proxy(ProxySource::proxy_protocol(
        ProxyVersions::Both,
        TrustedProxies::loopback(),
    ));
    let server = Server::builder()
        .listener(ListenerSpec::tcp("forwarded", "127.0.0.1:9001").ingress(forwarded))
        .listener(ListenerSpec::tcp("proxy", "127.0.0.1:9002").ingress(proxy))
        .serve(serve_net::handler(|req, reply| {
            let c = req.client();
            let addr = c
                .addr()
                .map_or_else(|| "unknown".to_owned(), |a| a.to_string());
            reply.text(200, &format!("{:?} {addr}\n", c.ingress()));
        }))?;
    println!("listening");
    let drain = server.drain_handle();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        drain.drain(Duration::from_secs(5));
    });
    let report = server.wait();
    println!(
        "stopped: {} graceful, {} killed",
        report.graceful, report.killed
    );
    Ok(())
}

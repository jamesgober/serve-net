//! Behind Apache or nginx on the same host: the real client comes from
//! `X-Forwarded-For`, believed only when the connection comes from 127.0.0.1
//! or ::1.
//!
//! ```text
//! cargo run --example behind_proxy
//! curl -H 'X-Forwarded-For: 203.0.113.7' http://127.0.0.1:8080/
//! ```
//!
//! Apache: `ProxyPass "/" "http://127.0.0.1:8080/"`. nginx:
//! `proxy_pass http://127.0.0.1:8080;` with
//! `proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;`.
//! Press Enter to drain and stop.

use std::time::Duration;

fn main() -> Result<(), serve_net::ServeError> {
    let server = serve_net::http_behind_proxy("127.0.0.1:8080", "127.0.0.1, ::1", |req, reply| {
        let client = req
            .client()
            .addr()
            .map_or_else(|| "unknown".to_owned(), |a| a.to_string());
        reply.text(
            200,
            &format!("you are {client}, asking for {}\n", req.path()),
        );
    })?;
    if let Some(local) = server.local_addr() {
        println!("listening on http://{local}/ behind a local proxy (press Enter to stop)");
    }

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

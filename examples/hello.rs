//! The smallest server: a page, a health check, and a 404.
//!
//! ```text
//! cargo run --example hello
//! curl -i http://127.0.0.1:8080/
//! ```
//!
//! Press Enter to drain and stop. serve-net handles no signals itself; a real
//! program calls `DrainHandle::drain` from its own signal handling or service
//! manager.

use std::time::Duration;

fn main() -> Result<(), serve_net::ServeError> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_owned());
    let server = serve_net::http(addr.as_str(), |req, reply| match req.path() {
        "/" => {
            reply.html(
                200,
                "<!doctype html><title>hello</title><h1>Hello from serve-net</h1>",
            );
        }
        "/health" => {
            reply.text(200, "ok");
        }
        _ => {
            reply.text(404, "not found");
        }
    })?;
    if let Some(local) = server.local_addr() {
        println!("listening on http://{local}/ (press Enter to stop)");
    }

    let drain = server.drain_handle();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        drain.drain(Duration::from_secs(5));
    });

    let report = server.wait();
    println!(
        "stopped in {:?}: {} requests finished during the drain, {} connections closed gracefully, {} killed",
        report.elapsed, report.requests_finished, report.graceful, report.killed
    );
    Ok(())
}

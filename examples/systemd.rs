//! A server under systemd with `Type=notify-reload` and a watchdog, for the
//! systemd CI job (`.github/systemd/run.sh`).
//!
//! serve-net handles no signals. This program reads commands on stdin: a
//! line `reload` reloads the configuration (systemd hears `RELOADING=1`, then
//! `READY=1`), and `stop` or the end of input drains. Under systemd a small
//! shell wrapper (`.github/systemd/serve.sh`) turns `SIGHUP` and `SIGTERM`
//! into those lines; a real program maps the signals in its own code.

use std::io::BufRead;
use std::sync::Arc;
use std::time::Duration;

use serve_net::{
    KeepAlive, ListenerSpec, NoNotify, Notify, Running, Server, ServerConfig, SystemdNotify,
    WatchdogPinger,
};

const ADDR: &str = "127.0.0.1:9101";

/// The configuration; `generation` changes the idle timeout, so every reload
/// has something to apply.
fn config(generation: u32) -> ServerConfig {
    let mut c = ServerConfig::new();
    c.listeners.push(ListenerSpec::tcp("web", ADDR));
    c.keep_alive = KeepAlive::new().idle(Duration::from_secs(20 + u64::from(generation % 2)));
    c
}

fn start<N: Notify>(notify: N) -> Result<Running, serve_net::ServeError> {
    Server::builder()
        .config(config(0))
        .notify(notify)
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok\n");
        }))
}

fn main() -> Result<(), serve_net::ServeError> {
    let running = match SystemdNotify::from_env() {
        Some(n) => start(n)?,
        None => start(NoNotify)?,
    };
    println!("listening on http://{ADDR}/");
    // The watchdog: pings only while every worker is healthy.
    let pinger = match (
        SystemdNotify::watchdog_interval(),
        SystemdNotify::from_env(),
    ) {
        (Some(interval), Some(n)) => Some(WatchdogPinger::start(
            &running,
            Arc::new(n) as Arc<dyn Notify>,
            interval,
        )),
        _ => None,
    };

    let mut generation = 0;
    for line in std::io::stdin().lock().lines() {
        match line.as_deref().map(str::trim) {
            Ok("reload") => {
                generation += 1;
                match running.reload(config(generation)) {
                    Ok(summary) => println!("reloaded: {summary:?}"),
                    Err(e) => eprintln!("reload refused: {e}"),
                }
            }
            Ok("stop") | Err(_) => break,
            Ok(_) => {}
        }
    }
    drop(pinger);
    let report = running.drain(Duration::from_secs(5));
    println!(
        "stopped: {} graceful, {} killed, {} requests finished during the drain",
        report.graceful, report.killed, report.requests_finished
    );
    Ok(())
}

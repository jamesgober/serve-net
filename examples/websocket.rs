//! A WebSocket echo: a page at `/` that opens a socket to `/ws` and shows what
//! comes back.
//!
//! ```text
//! cargo run --example websocket --features ws
//! open http://127.0.0.1:8080/
//! ```
//!
//! Press Enter to drain and stop.

use std::sync::Arc;
use std::time::Duration;

use serve_net::{
    Envelope, HeadAction, HttpHead, HttpReply, HttpRequest, HttpService, ListenerSpec, Outcome,
    Server, WebSocketConn, WebSocketService, WebSocketTarget, WsMessage,
};

const PAGE: &str = r#"<!doctype html>
<title>serve-net WebSocket</title>
<input id="m" value="hello" autofocus> <button id="b">send</button>
<pre id="log"></pre>
<script>
const log = (t) => { document.getElementById("log").textContent += t + "\n"; };
const ws = new WebSocket(`ws://${location.host}/ws`);
ws.onopen = () => log("open");
ws.onmessage = (e) => log("< " + e.data);
ws.onclose = (e) => log("closed " + e.code);
document.getElementById("b").onclick = () => {
  const m = document.getElementById("m").value;
  ws.send(m);
  log("> " + m);
};
</script>
"#;

/// Echoes every message.
struct Echo;

impl WebSocketService for Echo {
    fn message(&self, _env: &Envelope, message: WsMessage<'_>, conn: &mut WebSocketConn<'_>) {
        // A full output means the client is not reading: drop the echo.
        let _ = match message {
            WsMessage::Text(t) => conn.send_text(t),
            WsMessage::Binary(b) => conn.send_binary(b),
            _ => Ok(()),
        };
    }
}

struct Site {
    echo: Arc<dyn WebSocketService>,
}

impl HttpService for Site {
    fn head(&self, head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
        match head.path() {
            "/ws" => HeadAction::Upgrade(WebSocketTarget::shared(Arc::clone(&self.echo)).into()),
            _ => HeadAction::Proceed,
        }
    }

    fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        match req.path() {
            "/" => reply.html(200, PAGE),
            _ => reply.text(404, "not found"),
        };
        Outcome::Done
    }
}

fn main() -> Result<(), serve_net::ServeError> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_owned());
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", addr.as_str()))
        .serve(Site {
            echo: Arc::new(Echo),
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
        "stopped in {:?}: {} connections closed gracefully, {} killed",
        report.elapsed, report.graceful, report.killed
    );
    Ok(())
}

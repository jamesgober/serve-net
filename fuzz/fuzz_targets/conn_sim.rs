//! The connection state machine fed arbitrary bytes in arbitrary pieces.
//! Oracle: never panics; `done` runs exactly once per `head`; the service
//! never runs more often than heads were seen.

#![no_main]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use serve_net::testing::TestConnection;
use serve_net::{Envelope, Finished, HeadAction, HttpHead, HttpReply, HttpRequest, HttpService, Outcome};

#[derive(Default)]
struct Counts {
    heads: AtomicU32,
    calls: AtomicU32,
    dones: AtomicU32,
}

struct Site(Arc<Counts>);

impl HttpService for Site {
    fn head(&self, head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
        self.0.heads.fetch_add(1, Ordering::Relaxed);
        match head.path().len() % 7 {
            0 => HeadAction::refuse(403),
            _ => HeadAction::Proceed,
        }
    }
    fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        self.0.calls.fetch_add(1, Ordering::Relaxed);
        reply.text(200, req.path());
        if req.body_bytes().len() % 5 == 1 { Outcome::Close } else { Outcome::Done }
    }
    fn done(&self, _env: &Envelope, _finished: &Finished) {
        self.0.dones.fetch_add(1, Ordering::Relaxed);
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&step, input)) = data.split_first() else { return };
    let counts = Arc::new(Counts::default());
    let mut conn = TestConnection::new(Site(counts.clone()));
    let size = usize::from(step % 64) + 1;
    let mut out = Vec::new();
    for piece in input.chunks(size) {
        if !conn.is_open() {
            break;
        }
        conn.send_into(piece, &mut out);
    }
    drop(conn);
    let heads = counts.heads.load(Ordering::Relaxed);
    assert!(counts.calls.load(Ordering::Relaxed) <= heads);
    assert_eq!(counts.dones.load(Ordering::Relaxed), heads, "done exactly once per head");
});

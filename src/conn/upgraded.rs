//! The machine after a protocol switch (`HeadAction::Upgrade`).
//!
//! The 101 goes out first and ends the upgrade request (`done` with
//! `RequestEnd::Upgraded`, admission released). From then on the connection
//! waits in the idle phase, so a drain or the transport's idle timeout closes
//! it, and serve-net's exchange deadline paces pings.

#[cfg(feature = "ws")]
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(feature = "ws")]
use std::sync::Arc;
#[cfg(feature = "ws")]
use std::time::{Duration, Instant};

use super::{Conn, Io, Step};
#[cfg(feature = "ws")]
use super::{Phase, State};
#[cfg(feature = "ws")]
use crate::engine::{Engine, RefusalKind};
use crate::engine::{Family, Service};
#[cfg(feature = "ws")]
use crate::error::EndReason;
use crate::hooks::Hooks;
#[cfg(feature = "ws")]
use crate::hooks::{Checkpoint, CheckpointAt, Observer, Verdict};
#[cfg(feature = "ws")]
use crate::net::{CloseReason, ControlState};
use crate::service::UpgradeTarget;
#[cfg(feature = "ws")]
use crate::service::{RequestEnd, UpgradedConn};
#[cfg(feature = "ws")]
use crate::stats::Count;
#[cfg(feature = "ws")]
use crate::ws::{WebSocketConn, WsLink, WsMessage};

/// How long a close handshake serve-net started waits for the client's Close.
#[cfg(feature = "ws")]
const CLOSE_WAIT: Duration = Duration::from_secs(5);
/// Message capacity a connection keeps between messages.
#[cfg(feature = "ws")]
const MESSAGE_RETAIN: usize = 64 * 1024;

/// What a message's checks decided.
#[cfg(feature = "ws")]
enum Gate {
    Deliver,
    Closed,
    Abort,
}

#[cfg(not(feature = "ws"))]
impl<F: Family, S: Service<F>, H: Hooks> Conn<F, S, H> {
    pub(super) fn switch<I: Io>(&mut self, _io: &mut I, target: UpgradeTarget) -> Step {
        match target {}
    }
    pub(super) fn upgraded_data<I: Io>(&mut self, _io: &mut I, input: &[u8]) -> (usize, Step) {
        (input.len(), Step::Close)
    }
    pub(super) fn upgraded_peer_closed<I: Io>(&mut self, _io: &mut I) -> Step {
        Step::Close
    }
    pub(super) fn upgraded_timer<I: Io>(&mut self, _io: &mut I) -> Step {
        Step::Close
    }
    pub(super) fn upgraded_closed(&mut self) {}
}

#[cfg(feature = "ws")]
impl<F: Family, S: Service<F>, H: Hooks> Conn<F, S, H> {
    /// The upgrade request passed admission and the checkpoints: check the
    /// handshake, write the 101, and put the new protocol in place.
    pub(super) fn switch<I: Io>(&mut self, io: &mut I, target: UpgradeTarget) -> Step {
        let shared = Arc::clone(&self.shared);
        let UpgradeTarget::WebSocket(ws) = &target;
        let tables = Arc::clone(&self.tables);
        let Some(compiled) = tables
            .profiles
            .compiled(ws.profile.unwrap_or(self.env.profile))
        else {
            shared
                .observer
                .service_error(&self.env, "upgrade to a profile that does not exist");
            self.end = Some(EndReason::Refused);
            return self.refuse(
                io,
                RefusalKind::Internal,
                500,
                None,
                RequestEnd::Refused(RefusalKind::Internal),
            );
        };
        let max_pending = ws
            .max_pending
            .unwrap_or(compiled.limits.max_pending_output as usize);
        let now = io.now();
        let unix = self.unix_secs(now);
        self.out.clear(self.wipe);
        self.out_sent = 0;
        if let Err(e) = shared
            .family
            .upgrade(&mut self.engine, &target, unix, &mut self.out)
        {
            if e.kind == RefusalKind::Internal {
                shared.observer.service_error(
                    &self.env,
                    "an upgrade the request did not offer, or a subprotocol it did not offer",
                );
                self.end = Some(EndReason::Refused);
            } else {
                self.end = Some(EndReason::ProtocolError);
            }
            return self.refuse(io, e.kind, e.status, None, RequestEnd::Refused(e.kind));
        }
        if let Some(r) = self.request.as_mut() {
            r.status = Some(101);
            r.bytes_out += self.out.len() as u64;
        }
        // The engine switched; what followed the request is the new protocol's.
        let _ = self.engine.poll();
        let mut link = WsLink::new(ws, max_pending, now);
        link.backlog
            .extend_from_slice(self.engine.remaining_input());
        self.ws = Some(Box::new(link));
        self.state = State::Upgraded;
        self.upgraded_data(io, &[]).1
    }

    /// Input arrived, or the transport took what was queued.
    pub(super) fn upgraded_data<I: Io>(&mut self, io: &mut I, input: &[u8]) -> (usize, Step) {
        let Some(mut link) = self.ws.take() else {
            return (input.len(), Step::Close);
        };
        let r = self.ws_drive(io, &mut link, input);
        self.ws = Some(link);
        r
    }

    /// The client closed its sending side: whatever it sent is handled, then
    /// the connection ends.
    pub(super) fn upgraded_peer_closed<I: Io>(&mut self, io: &mut I) -> Step {
        if let Some(link) = self.ws.as_mut() {
            link.conn.feed_eof();
            link.finish = true;
        }
        self.end.get_or_insert(EndReason::ClientClose);
        match self.upgraded_data(io, &[]).1 {
            Step::Read => Step::Close,
            other => other,
        }
    }

    /// The exchange deadline: a ping is due, the client did not answer one,
    /// or a close handshake ran out of time.
    pub(super) fn upgraded_timer<I: Io>(&mut self, io: &mut I) -> Step {
        let Some(mut link) = self.ws.take() else {
            return Step::Close;
        };
        let now = io.now();
        let due = link.last_input + link.interval;
        let step = if link.finish || link.conn.state() != ws_net::State::Open {
            self.end.get_or_insert(EndReason::ClientClose);
            Step::Close
        } else if now < due {
            link.timer = due;
            Self::ws_idle(io, &link);
            Step::Read
        } else if link.ping_at.is_some() {
            // A whole interval since the ping, and not one byte back.
            self.end = Some(EndReason::Transport(CloseReason::Idle));
            Step::Close
        } else {
            let _ = link.conn.send_ping(&[]);
            link.ping_at = Some(now);
            link.timer = now + link.interval;
            if self.ws_push(io, &mut link) {
                Self::ws_idle(io, &link);
                Step::Read
            } else {
                Step::Write
            }
        };
        self.ws = Some(link);
        step
    }

    /// The connection closed: the service hears it once.
    pub(super) fn upgraded_closed(&mut self) {
        let Some(mut link) = self.ws.take() else {
            return;
        };
        if link.opened {
            link.service.closed(&self.env, link.code);
        }
        link.clear_message(self.wipe, 0);
        if self.wipe {
            crate::engine::wipe_bytes(&mut link.backlog);
        }
    }

    fn ws_drive<I: Io>(&mut self, io: &mut I, link: &mut WsLink, input: &[u8]) -> (usize, Step) {
        // The 101 first, then the upgrade request is done.
        while self.out_sent < self.out.len() {
            let n = io.push(&self.out.as_slice()[self.out_sent..]);
            if n == 0 {
                io.set_phase(Phase::Sending);
                return (0, Step::Write);
            }
            self.out_sent += n;
            self.count(io, Count::BytesOut(n as u64));
        }
        if self.out_sent > 0 {
            self.out.clear(self.wipe);
            self.out.shrink(0);
            self.out_sent = 0;
        }
        if self.request.is_some() {
            self.finish_request(RequestEnd::Upgraded);
            self.ws_open(io, link);
        }
        if !self.ws_push(io, link) {
            return (0, Step::Write);
        }
        let consumed = if link.finish {
            input.len()
        } else {
            match self.ws_read(io, link, input) {
                Some(n) => n,
                None => return (input.len(), self.kill_upgraded()),
            }
        };
        if !self.ws_push(io, link) {
            return (consumed, Step::Write);
        }
        if link.finish || link.conn.state() == ws_net::State::Closed {
            self.state = State::Closed;
            return (consumed, Step::Close);
        }
        Self::ws_idle(io, link);
        (consumed, Step::Read)
    }

    /// Waits for input in the idle phase (a drain or the idle timeout closes
    /// the connection there), with the link's deadline.
    fn ws_idle<I: Io>(io: &mut I, link: &WsLink) {
        io.set_phase(Phase::Idle);
        io.set_exchange_deadline(Some(link.timer));
    }

    /// After the 101 left: `upgraded`, the HTTP engine's memory back, the
    /// timers, and `opened`.
    fn ws_open<I: Io>(&mut self, io: &mut I, link: &mut WsLink) {
        link.opened = true;
        let shared = Arc::clone(&self.shared);
        let conn = UpgradedConn {
            env: self.env.clone(),
            protocol: "websocket",
            subprotocol: link.subprotocol,
        };
        shared.service.upgraded(&self.env, conn);
        // The HTTP engine and the body buffer are not used again.
        self.engine.reset();
        let _ = self.engine.shrink_idle();
        self.body = Vec::new();
        let now = io.now();
        link.last_input = now;
        link.timer = now + link.interval;
        io.set_idle_timeout(Some(link.interval * 2 + CLOSE_WAIT));
        Self::ws_idle(io, link);
        let catch = shared.catch_panics;
        let env = &self.env;
        let mut wc = WebSocketConn {
            conn: &mut link.conn,
            max_pending: link.max_pending,
        };
        let service = &*link.service;
        let panicked = if catch {
            catch_unwind(AssertUnwindSafe(|| service.opened(env, &mut wc))).is_err()
        } else {
            service.opened(env, &mut wc);
            false
        };
        if panicked {
            self.ws_panicked(link);
        }
    }

    /// Feeds input to ws-net and hands every complete message to the
    /// service. Stops while the client is not reading. `None`: a checkpoint
    /// killed the connection.
    fn ws_read<I: Io>(&mut self, io: &mut I, link: &mut WsLink, input: &[u8]) -> Option<usize> {
        let mut consumed = 0;
        loop {
            if link.conn.output().len() >= link.max_pending {
                break;
            }
            let fed = if !link.backlog.is_empty() {
                let n = link.conn.feed(&link.backlog);
                link.backlog.drain(..n);
                n
            } else if consumed < input.len() {
                let n = link.conn.feed(&input[consumed..]);
                consumed += n;
                n
            } else {
                0
            };
            if fed > 0 {
                link.last_input = io.now();
                link.ping_at = None;
            }
            let text = match link.conn.poll() {
                Ok(ws_net::Event::Text(t)) => {
                    link.text.clear();
                    link.text.push_str(t);
                    true
                }
                Ok(ws_net::Event::Binary(b)) => {
                    link.scratch.clear();
                    link.scratch.extend_from_slice(b);
                    false
                }
                Ok(ws_net::Event::Close(frame)) => {
                    link.code = Some(frame.map_or(1005, |f| f.code.as_u16()));
                    self.end.get_or_insert(EndReason::ClientClose);
                    link.finish = true;
                    break;
                }
                Ok(ws_net::Event::Closed) => {
                    link.finish = true;
                    break;
                }
                Ok(ws_net::Event::NeedInput) => {
                    if fed == 0 {
                        break;
                    }
                    continue;
                }
                Ok(_) => continue,
                Err(e) => {
                    // ws-net queued the Close with the right code.
                    link.code = e.close_code().map(|c| c.as_u16());
                    self.end = Some(EndReason::ProtocolError);
                    link.finish = true;
                    break;
                }
            };
            match self.ws_gate(link) {
                Gate::Deliver => {}
                Gate::Closed => break,
                Gate::Abort => return None,
            }
            self.ws_deliver(link, text);
            if link.conn.state() != ws_net::State::Open {
                // The service closed: wait for the client's Close, briefly.
                link.timer = io.now() + CLOSE_WAIT;
                break;
            }
        }
        Some(consumed)
    }

    /// The checks a request passes before its service call, for each
    /// message: the control line, then the checkpoint.
    fn ws_gate(&mut self, link: &mut WsLink) -> Gate {
        let critical = self
            .tables
            .profiles
            .compiled(self.env.profile)
            .is_some_and(|c| c.profile.critical);
        let refused = match self.checkpoint() {
            ControlState::Draining | ControlState::Stopping => {
                self.end = Some(EndReason::Drained);
                Some((ws_net::CloseCode::GOING_AWAY, "server draining"))
            }
            ControlState::Lockdown if !critical => {
                self.end = Some(EndReason::Shed);
                Some((ws_net::CloseCode::TRY_AGAIN_LATER, "try again later"))
            }
            ControlState::Shed { from_lane } if self.env.lane >= from_lane => {
                self.end = Some(EndReason::Shed);
                Some((ws_net::CloseCode::TRY_AGAIN_LATER, "try again later"))
            }
            _ => None,
        };
        if let Some((code, reason)) = refused {
            self.ws_close(link, code, reason);
            link.timer = Instant::now() + CLOSE_WAIT;
            return Gate::Closed;
        }
        match self
            .shared
            .checkpoint
            .check(&self.env, CheckpointAt::BeforeDispatch)
        {
            Verdict::Pass => Gate::Deliver,
            Verdict::Deny { .. } => {
                self.end = Some(EndReason::Denied);
                self.ws_close(link, ws_net::CloseCode::POLICY_VIOLATION, "denied");
                link.timer = Instant::now() + CLOSE_WAIT;
                Gate::Closed
            }
            Verdict::Kill => Gate::Abort,
        }
    }

    /// One service call with the message in `scratch`.
    fn ws_deliver(&mut self, link: &mut WsLink, text: bool) {
        let catch = self.shared.catch_panics;
        let panicked = {
            let WsLink {
                conn,
                service,
                scratch,
                text: text_buf,
                max_pending,
                ..
            } = &mut *link;
            let message = if text {
                WsMessage::Text(text_buf)
            } else {
                WsMessage::Binary(scratch)
            };
            let mut wc = WebSocketConn {
                conn,
                max_pending: *max_pending,
            };
            let service = &**service;
            let env = &self.env;
            if catch {
                catch_unwind(AssertUnwindSafe(|| service.message(env, message, &mut wc))).is_err()
            } else {
                service.message(env, message, &mut wc);
                false
            }
        };
        link.clear_message(self.wipe, MESSAGE_RETAIN);
        if panicked {
            self.ws_panicked(link);
        }
    }

    fn ws_panicked(&mut self, link: &mut WsLink) {
        self.shared
            .observer
            .service_error(&self.env, "the service panicked");
        self.end = Some(EndReason::ServicePanic);
        self.ws_close(link, ws_net::CloseCode::INTERNAL_ERROR, "");
        link.timer = Instant::now() + CLOSE_WAIT;
    }

    fn ws_close(&mut self, link: &mut WsLink, code: ws_net::CloseCode, reason: &str) {
        if link.conn.state() == ws_net::State::Open {
            let _ = link.conn.close(code, reason);
            link.code = Some(code.as_u16());
        }
    }

    /// Hands ws-net's output to the transport; `false` while some waits.
    fn ws_push<I: Io>(&mut self, io: &mut I, link: &mut WsLink) -> bool {
        loop {
            let out = link.conn.output();
            if out.is_empty() {
                return true;
            }
            let n = io.push(out);
            if n == 0 {
                io.set_phase(Phase::Sending);
                return false;
            }
            link.conn.advance_output(n);
            self.count(io, Count::BytesOut(n as u64));
        }
    }

    fn kill_upgraded(&mut self) -> Step {
        self.end = Some(EndReason::CheckpointKill);
        self.state = State::Closed;
        Step::Abort
    }
}

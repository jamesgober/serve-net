//! HTTP/2 over h2-net (D-120): one connection, many streams, each stream an
//! exchange that runs the same steps as an HTTP/1.1 request.
//!
//! h2-net does the protocol: frames, HPACK, flow control, settings, and its
//! attack budgets. This module drives it from the connection's machine:
//! every request goes through the family's head, the service's `head`,
//! the control line, admission, and the checkpoints, and is answered from
//! its reply parts. Without the `h2` feature the entry points below close
//! the connection; nothing selects HTTP/2 then.

#[cfg(not(feature = "h2"))]
use super::{Conn, Io, Step, Timeout};
#[cfg(not(feature = "h2"))]
use crate::engine::{Family, Service};
#[cfg(not(feature = "h2"))]
use crate::hooks::Hooks;

#[cfg(feature = "h2")]
mod imp {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use h2_net::ErrorCode;
    use h2_net::conn::{Config, ConnError, Event, ServerConn};
    use h2_net::frame::StreamId;
    use http_net::OwnedRequestHead;

    use super::super::exchange::{self, Admission, Exchange, Gate, Start};
    use super::super::{Conn, Io, Phase, State, Step, Timeout};
    use crate::client::Scheme;
    use crate::config::EngineChoice;
    use crate::engine::{Family, HeadContext, RefusalKind, Service};
    use crate::error::EndReason;
    use crate::hooks::{Checkpoint, CheckpointAt, Hooks, Observer, Shed, Verdict};
    use crate::net::ControlState;
    use crate::reply::{SlotState, WAKE_REPLY};
    use crate::service::{BodyPolicy, Outcome, RequestEnd};
    use crate::stats::Count;

    /// Exchanges kept for reuse after their stream ends.
    const SPARE: usize = 16;

    pub(crate) struct H2Link<F: Family> {
        conn: ServerConn,
        streams: BTreeMap<u32, H2Stream<F>>,
        spare: Vec<Exchange<F>>,
        dates: http_net::DateCache,
        /// The graceful `GOAWAY` went out.
        shutting_down: bool,
        /// Close once the output is written.
        closing: bool,
    }

    struct H2Stream<F: Family> {
        ex: Exchange<F>,
        head: OwnedRequestHead,
        policy: BodyPolicy,
        phase: Phase2,
        /// The peer sent `END_STREAM`.
        peer_ended: bool,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Phase2 {
        /// Reading the body, until the request deadline.
        Reading,
        /// Waiting for a deferred reply.
        Parked { until: Instant },
        /// The head left; `sent` bytes of the body went out.
        Sending { sent: usize },
    }

    impl<F: Family, S: Service<F>, H: Hooks> Conn<F, S, H> {
        /// Whether this connection speaks HTTP/2: the profile picks the engine
        /// by ALPN, the handshake said `h2`, and the family speaks HTTP/2.
        pub(crate) fn wants_h2(&self) -> bool {
            let by_alpn = self
                .compiled()
                .is_some_and(|c| c.profile.engine == EngineChoice::Http1OrH2ByAlpn);
            let h2 = self
                .tls
                .as_deref()
                .and_then(|t| t.alpn())
                .is_some_and(|p| p == b"h2");
            by_alpn && h2
        }

        /// Whether this connection's profile takes cleartext HTTP/2 when the
        /// first bytes are its preface.
        pub(crate) fn by_preface(&self) -> bool {
            self.compiled()
                .is_some_and(|c| c.profile.engine == EngineChoice::Http1OrH2ByPreface)
        }

        /// Prior knowledge (RFC 9113 section 3.3): on a profile that takes
        /// cleartext HTTP/2, the first 24 bytes say which protocol this
        /// connection speaks. `None` means HTTP/1.1 goes on.
        pub(crate) fn h2_sniff<I: Io>(
            &mut self,
            io: &mut I,
            input: &[u8],
        ) -> Option<(usize, Step)> {
            if !self.by_preface() {
                return None;
            }
            match http_net::is_h2_preface(input) {
                None => Some((0, Step::Read)),
                Some(true) => {
                    let step = self.h2_open(io);
                    if matches!(step, Step::Close | Step::Abort) {
                        return Some((input.len(), step));
                    }
                    let (n, step) = self.h2_drive(io, input);
                    self.count(io, Count::BytesIn(n as u64));
                    Some((n, step))
                }
                Some(false) => None,
            }
        }

        /// Opens the HTTP/2 side: the server's settings go out at once.
        pub(crate) fn h2_open<I: Io>(&mut self, io: &mut I) -> Step {
            let limits = self.limits();
            let header_list = u32::try_from(limits.engine.max_header_bytes()).unwrap_or(u32::MAX);
            let config = Config::new()
                .max_header_list_size(header_list)
                .stream_window(u32::try_from(limits.max_body.min(256 * 1024)).unwrap_or(256 * 1024))
                .max_output(limits.max_pending_output);
            self.h2 = Some(Box::new(H2Link {
                conn: ServerConn::new(config),
                streams: BTreeMap::new(),
                spare: Vec::new(),
                dates: http_net::DateCache::new(),
                shutting_down: false,
                closing: false,
            }));
            self.state = State::Http2;
            io.set_phase(Phase::Idle);
            io.set_exchange_deadline(None);
            self.h2_drive(io, &[]).1
        }

        /// Takes the link out, runs `f` on it, and puts it back.
        fn with_link<R>(&mut self, f: impl FnOnce(&mut Self, &mut H2Link<F>) -> R) -> Option<R> {
            let mut link = self.h2.take()?;
            let r = f(self, &mut link);
            self.h2 = Some(link);
            Some(r)
        }

        /// Bytes arrived (or none, to run what is pending).
        pub(crate) fn h2_drive<I: Io>(&mut self, io: &mut I, input: &[u8]) -> (usize, Step) {
            self.with_link(|this, link| {
                let mut taken = 0;
                loop {
                    let n = link.conn.feed(input.get(taken..).unwrap_or_default());
                    taken += n;
                    if let Err(e) = this.h2_events(io, link) {
                        this.h2_failed(link, e);
                        break;
                    }
                    if n == 0 || taken >= input.len() {
                        break;
                    }
                }
                this.h2_control(link);
                this.h2_resume_sending(io, link);
                (taken, this.h2_flush(io, link))
            })
            .unwrap_or((input.len(), Step::Close))
        }

        /// Hands every event h2-net has to the right stream.
        fn h2_events<I: Io>(&mut self, io: &mut I, link: &mut H2Link<F>) -> Result<(), ConnError> {
            loop {
                let mut release = None;
                let mut complete = None;
                match link.conn.poll()? {
                    None => return Ok(()),
                    Some(Event::Request {
                        stream,
                        head,
                        end_stream,
                        ..
                    }) => self.h2_request(io, link, stream, head, end_stream),
                    Some(Event::Data {
                        stream,
                        data,
                        end_stream,
                    }) => {
                        release = Some((stream, data.len()));
                        let id = stream.value();
                        if let Some(st) = link.streams.get_mut(&id) {
                            let over = take_body(st, data);
                            if end_stream {
                                st.peer_ended = true;
                            }
                            complete = Some((id, over, end_stream));
                        }
                    }
                    Some(Event::Trailers { stream, .. }) => {
                        let id = stream.value();
                        if let Some(st) = link.streams.get_mut(&id) {
                            st.peer_ended = true;
                            complete = Some((id, false, true));
                        }
                    }
                    Some(Event::Reset { stream, .. }) => {
                        self.h2_end_stream(link, stream.value(), RequestEnd::PeerGone);
                    }
                    Some(_) => {}
                }
                if let Some((stream, n)) = release {
                    // The bytes were copied (or dropped): the window opens again.
                    link.conn.release(stream, n);
                }
                if let Some((id, over, ended)) = complete {
                    if over {
                        self.h2_refuse(
                            io,
                            link,
                            id,
                            RefusalKind::TooLarge,
                            413,
                            None,
                            RequestEnd::Refused(RefusalKind::TooLarge),
                        );
                    } else if ended
                        && link
                            .streams
                            .get(&id)
                            .is_some_and(|s| s.phase == Phase2::Reading)
                    {
                        self.h2_dispatch(io, link, id);
                    }
                }
            }
        }

        /// A new request: the head view, `head`, admission, the checkpoint.
        fn h2_request<I: Io>(
            &mut self,
            io: &mut I,
            link: &mut H2Link<F>,
            stream: StreamId,
            head: OwnedRequestHead,
            end_stream: bool,
        ) {
            let id = stream.value();
            let now = io.now();
            let control = self.checkpoint();
            if control == ControlState::Stopping {
                link.conn.reset(stream, ErrorCode::REFUSED_STREAM);
                return;
            }
            self.refresh_tables();
            let limits = self.limits();
            let shared = &*self.shared;
            let tables = &*self.tables;
            let compiled = tables.profiles.compiled(self.ex.env.profile);
            let default_policy = compiled.map_or(
                BodyPolicy::Buffer {
                    max: limits.max_body,
                },
                |c| c.body,
            );
            let mut ex = link
                .spare
                .pop()
                .unwrap_or_else(|| Exchange::new(self.ex.env.clone(), shared.family.new_parts()));
            let decided = {
                let listener = tables.listeners.get(self.listener).and_then(Option::as_ref);
                let cx = HeadContext {
                    client: &self.client,
                    proxy: listener.and_then(|l| l.proxy.as_ref()),
                    declared_scheme: listener.map_or(Scheme::Http, |l| l.declared_scheme),
                    resolved: None,
                    proxy_facts: self.proxy_facts.as_deref(),
                    tls: self.tls.as_deref(),
                    sni_check: listener.is_none_or(|l| l.sni_check),
                };
                let view = head.view();
                let head_len = view.raw().bytes().len() as u64;
                match shared.family.h2_head(view, &cx) {
                    None => None,
                    Some(Err(e)) => Some(Err(e)),
                    Some(Ok((fhead, client))) => {
                        self.seq = self.seq.saturating_add(1);
                        let start = Start {
                            seq: id,
                            client,
                            lane: compiled.map_or(crate::config::DEFAULT_LANE, |c| c.profile.lane),
                            arrived: now,
                            deadline: now + limits.request_timeout,
                            head_reads: 1,
                            head_len,
                            max_body: limits.max_body,
                            policy: default_policy,
                        };
                        Some(Ok(ex.begin(shared, &fhead, start)))
                    }
                }
            };
            let decision = match decided {
                None => {
                    // A family without HTTP/2 never gets here (`wants_h2`).
                    link.conn.reset(stream, ErrorCode::INTERNAL_ERROR);
                    link.spare.push(ex);
                    return;
                }
                Some(Err(e)) => {
                    // Refused before `head` (a misdirected request, a bad
                    // forwarded header): the status, and the stream ends.
                    let _ = link.conn.send_headers(stream, e.status, &[], true);
                    if !end_stream {
                        link.conn.reset(stream, ErrorCode::NO_ERROR);
                    }
                    let draining = self.draining_now(io);
                    self.count(
                        io,
                        Count::Refused {
                            kind: e.kind,
                            draining,
                        },
                    );
                    link.spare.push(ex);
                    return;
                }
                Some(Ok(d)) => d,
            };
            let declared = head.view().content_length();
            let gate = Gate {
                control,
                critical: compiled.is_some_and(|c| c.profile.critical),
                worker: io.worker(),
                now,
                declared,
            };
            let admission = ex.admit(shared, decision, default_policy, &gate);
            let st = H2Stream {
                ex,
                head,
                policy: default_policy,
                phase: Phase2::Reading,
                peer_ended: end_stream,
            };
            let _ = link.streams.insert(id, st);
            match admission {
                Admission::Proceed(BodyPolicy::Stream) => {
                    // A streamed body over HTTP/2 comes in a later step.
                    if let Some(st) = link.streams.get(&id) {
                        self.shared.observer.service_error(
                            &st.ex.env,
                            "a streamed request body over HTTP/2 is not supported yet",
                        );
                    }
                    self.h2_refuse(
                        io,
                        link,
                        id,
                        RefusalKind::Internal,
                        501,
                        None,
                        RequestEnd::Refused(RefusalKind::Internal),
                    );
                }
                Admission::Proceed(policy) => {
                    if let Some(st) = link.streams.get_mut(&id) {
                        st.policy = policy;
                    }
                    if end_stream {
                        self.h2_dispatch(io, link, id);
                    }
                }
                Admission::Upgrade(_) => {
                    // Upgrades do not exist in HTTP/2 (WebSocket over HTTP/2,
                    // RFC 8441, is later).
                    self.h2_refuse(
                        io,
                        link,
                        id,
                        RefusalKind::Internal,
                        501,
                        None,
                        RequestEnd::Refused(RefusalKind::Internal),
                    );
                }
                Admission::Refuse {
                    kind,
                    status,
                    retry_after,
                    end,
                    ..
                } => self.h2_refuse(io, link, id, kind, status, retry_after, end),
                Admission::Kill => self.h2_kill(link),
            }
        }

        /// The request is complete: the checkpoint, the service, the reply.
        fn h2_dispatch<I: Io>(&mut self, io: &mut I, link: &mut H2Link<F>, id: u32) {
            let _ = self.checkpoint();
            let shared = &*self.shared;
            let Some(st) = link.streams.get_mut(&id) else {
                return;
            };
            match shared
                .checkpoint
                .check(&st.ex.env, CheckpointAt::BeforeDispatch)
            {
                Verdict::Pass => {}
                Verdict::Deny { status } => {
                    self.h2_refuse(
                        io,
                        link,
                        id,
                        RefusalKind::Denied,
                        status,
                        None,
                        RequestEnd::Denied,
                    );
                    return;
                }
                Verdict::Kill => {
                    self.h2_kill(link);
                    return;
                }
            }
            let (connection, wipe) = (self.connection, self.wipe);
            st.ex
                .ready_reply(shared, || io.waker(), connection, wipe, false);
            let listener = self
                .tables
                .listeners
                .get(self.listener)
                .and_then(Option::as_ref);
            let cx = HeadContext {
                client: &self.client,
                proxy: listener.and_then(|l| l.proxy.as_ref()),
                declared_scheme: listener.map_or(Scheme::Http, |l| l.declared_scheme),
                resolved: Some(st.ex.env.client),
                proxy_facts: self.proxy_facts.as_deref(),
                tls: self.tls.as_deref(),
                sni_check: listener.is_none_or(|l| l.sni_check),
            };
            let discard = matches!(st.policy, BodyPolicy::Discard { .. });
            let called = match shared.family.h2_head(st.head.view(), &cx) {
                Some(Ok((fhead, _))) => {
                    let body: &[u8] = if discard { &[] } else { &st.ex.body };
                    let request = shared.family.request(fhead, body);
                    Some(exchange::call(
                        shared,
                        &request,
                        &st.ex.env,
                        &mut st.ex.reply,
                    ))
                }
                _ => None,
            };
            let Some(called) = called else {
                st.ex.reply.link = None;
                self.h2_refuse(
                    io,
                    link,
                    id,
                    RefusalKind::Internal,
                    500,
                    None,
                    RequestEnd::Refused(RefusalKind::Internal),
                );
                return;
            };
            if called.panicked {
                st.ex.reply.link = None;
                shared
                    .observer
                    .service_error(&st.ex.env, "the service panicked");
                self.h2_refuse(
                    io,
                    link,
                    id,
                    RefusalKind::Internal,
                    500,
                    None,
                    RequestEnd::ServicePanic,
                );
                return;
            }
            if st.ex.reply.pool_full {
                st.ex.reply.pool_full = false;
                st.ex.reply.link = None;
                let shed = Shed {
                    retry_after: None,
                    close: false,
                };
                shared.observer.request_shed(&st.ex.env, &shed);
                self.h2_refuse(
                    io,
                    link,
                    id,
                    RefusalKind::Busy,
                    503,
                    Some(Duration::from_secs(1)),
                    RequestEnd::Shed,
                );
                return;
            }
            if st.ex.env.lane_late {
                shared
                    .observer
                    .service_error(&st.ex.env, "lane set after head");
            }
            if st.ex.reply.deferred {
                if called.outcome != Outcome::Deferred {
                    shared.observer.service_error(
                        &st.ex.env,
                        "reply deferred but call did not return Deferred",
                    );
                }
                let worker = st.ex.env.worker.get();
                let cap = self.limits().max_deferred_per_worker;
                if !self.shared.counters.park(worker, cap) {
                    self.h2_cancel_deferred(link, id);
                    if let Some(st) = link.streams.get_mut(&id) {
                        st.ex.reply.link = None;
                        let shed = Shed {
                            retry_after: None,
                            close: true,
                        };
                        self.shared.observer.request_shed(&st.ex.env, &shed);
                    }
                    self.h2_refuse(
                        io,
                        link,
                        id,
                        RefusalKind::Busy,
                        503,
                        Some(Duration::from_secs(1)),
                        RequestEnd::Shed,
                    );
                    return;
                }
                let until = io.now() + self.limits().reply_timeout;
                if let Some(st) = link.streams.get_mut(&id) {
                    st.ex.parked = true;
                    st.phase = Phase2::Parked { until };
                }
                self.count(io, Count::Deferred);
                // A completion that arrived while `call` ran is picked up now.
                if let Some(generation) = link.streams.get(&id).map(|s| s.ex.generation) {
                    self.h2_take_completion(io, link, id, generation);
                }
                return;
            }
            match called.outcome {
                Outcome::Deferred => {
                    if let Some(st) = link.streams.get(&id) {
                        self.shared
                            .observer
                            .service_error(&st.ex.env, "Deferred returned without reply.defer()");
                    }
                    self.h2_refuse(
                        io,
                        link,
                        id,
                        RefusalKind::Internal,
                        500,
                        None,
                        RequestEnd::Refused(RefusalKind::Internal),
                    );
                }
                outcome => self.h2_respond(io, link, id, outcome == Outcome::Close),
            }
        }

        /// Writes the reply the service built: the head, then as much of the
        /// body as the windows allow.
        fn h2_respond<I: Io>(&mut self, io: &mut I, link: &mut H2Link<F>, id: u32, close: bool) {
            let Some(st) = link.streams.get(&id) else {
                return;
            };
            match self
                .shared
                .checkpoint
                .check(&st.ex.env, CheckpointAt::BeforeReply)
            {
                Verdict::Pass => {}
                Verdict::Deny { status } => {
                    self.h2_refuse(
                        io,
                        link,
                        id,
                        RefusalKind::Denied,
                        status,
                        None,
                        RequestEnd::Denied,
                    );
                    return;
                }
                Verdict::Kill => {
                    self.h2_kill(link);
                    return;
                }
            }
            let unix = self.unix_secs(io.now());
            let shared = &*self.shared;
            let Some(reply) = shared.family.h2_reply(&st.ex.reply.parts) else {
                return;
            };
            if reply.invalid || reply.streamed {
                let why = if reply.invalid {
                    "reply could not be encoded"
                } else {
                    "a streamed reply over HTTP/2 is not supported yet"
                };
                shared.observer.service_error(&st.ex.env, why);
                self.h2_refuse(
                    io,
                    link,
                    id,
                    RefusalKind::Internal,
                    500,
                    None,
                    RequestEnd::Refused(RefusalKind::Internal),
                );
                return;
            }
            let date = link.dates.update(unix).as_str().as_bytes().to_vec();
            let retry = reply.retry_after.map(|s| s.to_string());
            let mut fields: Vec<(&str, &[u8])> = Vec::with_capacity(reply.fields.len() + 3);
            fields.extend(reply.fields.iter().copied());
            if !fields.iter().any(|(n, _)| n.eq_ignore_ascii_case("date")) {
                fields.push(("date", &date));
            }
            if let Some(r) = &retry {
                fields.push(("retry-after", r.as_bytes()));
            }
            let body_len = reply.body.len();
            let len = body_len.to_string();
            // A HEAD reply keeps its content-length; h2-net ends it with the
            // head and sends no body.
            if !fields
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("content-length"))
            {
                fields.push(("content-length", len.as_bytes()));
            }
            let status = reply.status;
            let close = close || reply.close;
            let stream = StreamId::masked(id);
            let ended = body_len == 0;
            if link
                .conn
                .send_headers(stream, status, &fields, ended)
                .is_err()
            {
                self.h2_end_stream(link, id, RequestEnd::Refused(RefusalKind::Internal));
                return;
            }
            if let Some(st) = link.streams.get_mut(&id) {
                if let Some(r) = st.ex.request.as_mut() {
                    r.status = Some(status);
                    r.bytes_out = body_len as u64;
                }
                st.phase = Phase2::Sending { sent: 0 };
            }
            if close && !link.shutting_down {
                link.conn.shutdown();
                link.shutting_down = true;
                self.end.get_or_insert(EndReason::ServiceClosed);
            }
            self.h2_send_body(link, id);
        }

        /// Sends what the windows allow of a reply's body; ends the request
        /// once all of it is out.
        fn h2_send_body(&mut self, link: &mut H2Link<F>, id: u32) {
            let shared = &*self.shared;
            let Some(st) = link.streams.get_mut(&id) else {
                return;
            };
            let Phase2::Sending { sent } = st.phase else {
                return;
            };
            let Some(reply) = shared.family.h2_reply(&st.ex.reply.parts) else {
                return;
            };
            let body = reply.body;
            let stream = StreamId::masked(id);
            let done = if body.is_empty() {
                true
            } else {
                let rest = body.get(sent..).unwrap_or_default();
                match link.conn.send_data(stream, rest, true) {
                    Ok(n) => {
                        st.phase = Phase2::Sending { sent: sent + n };
                        n == rest.len()
                    }
                    Err(_) => true,
                }
            };
            if done {
                self.h2_end_stream(link, id, RequestEnd::Replied);
            }
        }

        /// Continues every reply waiting for window or output room.
        fn h2_resume_sending<I: Io>(&mut self, _io: &mut I, link: &mut H2Link<F>) {
            let waiting: Vec<u32> = link
                .streams
                .iter()
                .filter(|(_, s)| matches!(s.phase, Phase2::Sending { .. }))
                .map(|(id, _)| *id)
                .collect();
            for id in waiting {
                self.h2_send_body(link, id);
            }
        }

        /// Refuses one request with `status`; the connection goes on.
        #[allow(clippy::too_many_arguments)]
        fn h2_refuse<I: Io>(
            &mut self,
            io: &mut I,
            link: &mut H2Link<F>,
            id: u32,
            kind: RefusalKind,
            status: u16,
            retry_after: Option<Duration>,
            end: RequestEnd,
        ) {
            let stream = StreamId::masked(id);
            let status = if (400..=599).contains(&status) {
                status
            } else {
                500
            };
            let retry = retry_after.map(|d| d.as_secs().max(1).to_string());
            let mut fields: Vec<(&str, &[u8])> = vec![("content-length", b"0")];
            if let Some(r) = &retry {
                fields.push(("retry-after", r.as_bytes()));
            }
            let _ = link.conn.send_headers(stream, status, &fields, true);
            let peer_ended = link.streams.get(&id).is_none_or(|s| s.peer_ended);
            if !peer_ended {
                // Ask the client to stop sending the body (RFC 9113 8.1).
                link.conn.reset(stream, ErrorCode::NO_ERROR);
            }
            if let Some(st) = link.streams.get_mut(&id) {
                if let Some(r) = st.ex.request.as_mut() {
                    r.status = Some(status);
                }
            }
            let draining = self.draining_now(io);
            self.count(io, Count::Refused { kind, draining });
            self.h2_end_stream(link, id, end);
        }

        /// Ends a stream's request: `done`, the observer, the release.
        fn h2_end_stream(&mut self, link: &mut H2Link<F>, id: u32, end: RequestEnd) {
            let Some(mut st) = link.streams.remove(&id) else {
                return;
            };
            if st.ex.parked {
                st.ex.parked = false;
                self.shared.counters.unpark(st.ex.env.worker.get());
            }
            if let Some(slot) = &st.ex.defer {
                let left = core::mem::replace(&mut *slot.lock(), SlotState::Idle);
                if let SlotState::Completed(_, mut parts, _) = left {
                    self.shared.family.clear_parts(&mut parts, self.wipe);
                }
            }
            let live = matches!(
                self.checkpoint(),
                ControlState::Draining | ControlState::Stopping
            );
            self.draining_seen |= live;
            st.ex
                .finish(&self.shared, end, self.draining_seen, self.wipe);
            self.shared
                .family
                .clear_parts(&mut st.ex.reply.parts, self.wipe);
            if link.spare.len() < SPARE {
                // A spare keeps its own deferral slot: an old token's
                // completion carries an old generation and is ignored.
                link.spare.push(st.ex);
            }
        }

        fn h2_cancel_deferred(&mut self, link: &mut H2Link<F>, id: u32) {
            if let Some(st) = link.streams.get_mut(&id) {
                if let Some(slot) = &st.ex.defer {
                    let left = core::mem::replace(&mut *slot.lock(), SlotState::Idle);
                    if let SlotState::Completed(_, mut parts, _) = left {
                        self.shared.family.clear_parts(&mut parts, self.wipe);
                    }
                }
                if st.ex.parked {
                    st.ex.parked = false;
                    self.shared.counters.unpark(st.ex.env.worker.get());
                }
            }
        }

        /// A checkpoint killed the connection.
        fn h2_kill(&mut self, link: &mut H2Link<F>) {
            self.end = Some(EndReason::CheckpointKill);
            let ids: Vec<u32> = link.streams.keys().copied().collect();
            for id in ids {
                self.h2_end_stream(link, id, RequestEnd::Killed);
            }
            link.conn.close(ErrorCode::INTERNAL_ERROR);
            link.closing = true;
        }

        /// h2-net ended the connection; its `GOAWAY` is in the output.
        fn h2_failed(&mut self, link: &mut H2Link<F>, e: ConnError) {
            self.end.get_or_insert(if e.is_abuse() {
                EndReason::Abuse
            } else {
                EndReason::ProtocolError
            });
            let ids: Vec<u32> = link.streams.keys().copied().collect();
            for id in ids {
                self.h2_end_stream(link, id, RequestEnd::PeerGone);
            }
            link.closing = true;
        }

        /// A drain sends the graceful `GOAWAY`; a stop ends at once.
        fn h2_control(&mut self, link: &mut H2Link<F>) {
            if link.closing {
                return;
            }
            match self.checkpoint() {
                ControlState::Stopping => {
                    self.end.get_or_insert(EndReason::Drained);
                    let ids: Vec<u32> = link.streams.keys().copied().collect();
                    for id in ids {
                        self.h2_end_stream(link, id, RequestEnd::Drained);
                    }
                    link.conn.close(ErrorCode::NO_ERROR);
                    link.closing = true;
                }
                ControlState::Draining if !link.shutting_down => {
                    link.conn.shutdown();
                    link.shutting_down = true;
                    self.end.get_or_insert(EndReason::Drained);
                }
                _ => {}
            }
        }

        /// Hands h2-net's output to the transport, and sets the phase and the
        /// deadline the open streams call for.
        fn h2_flush<I: Io>(&mut self, io: &mut I, link: &mut H2Link<F>) -> Step {
            loop {
                let out = link.conn.output();
                if out.is_empty() {
                    break;
                }
                let n = io.push(out);
                if n == 0 {
                    break;
                }
                self.count(io, Count::BytesOut(n as u64));
                link.conn.consume_output(n);
            }
            let pending = !link.conn.output().is_empty();
            if !pending && (link.closing || link.conn.is_done()) {
                self.end.get_or_insert(EndReason::ClientClose);
                self.state = State::Closed;
                return Step::Close;
            }
            // The earliest deadline among the streams: a body that stops
            // arriving, or a reply that never comes.
            let mut deadline: Option<Instant> = None;
            let mut reading = false;
            let mut parked = false;
            for st in link.streams.values() {
                let at = match st.phase {
                    Phase2::Reading => {
                        reading = true;
                        Some(st.ex.env.deadline)
                    }
                    Phase2::Parked { until } => {
                        parked = true;
                        Some(until)
                    }
                    Phase2::Sending { .. } => None,
                };
                if let Some(at) = at {
                    deadline = Some(deadline.map_or(at, |d| d.min(at)));
                }
            }
            if reading {
                io.set_phase(Phase::Receiving);
            } else {
                io.set_phase(Phase::Idle);
            }
            // A reply the service is still making must not look like an idle
            // client: the idle wait outlasts the reply deadline.
            io.set_idle_timeout(if parked {
                Some(self.limits().reply_timeout + Duration::from_secs(1))
            } else {
                None
            });
            io.set_exchange_deadline(deadline);
            if pending { Step::Write } else { Step::Read }
        }

        /// A deferred reply arrived for this connection.
        pub(crate) fn h2_wake<I: Io>(&mut self, io: &mut I, token: u32, value: u64) -> Step {
            self.with_link(|this, link| {
                if token == WAKE_REPLY {
                    let id = link
                        .streams
                        .iter()
                        .find(|(_, s)| {
                            matches!(s.phase, Phase2::Parked { .. }) && s.ex.generation == value
                        })
                        .map(|(id, _)| *id);
                    if let Some(id) = id {
                        this.h2_take_completion(io, link, id, value);
                    }
                }
                this.h2_control(link);
                this.h2_resume_sending(io, link);
                this.h2_flush(io, link)
            })
            .unwrap_or(Step::Close)
        }

        /// Takes a deferred reply from the stream's slot, if it is there.
        fn h2_take_completion<I: Io>(
            &mut self,
            io: &mut I,
            link: &mut H2Link<F>,
            id: u32,
            generation: u64,
        ) {
            let Some(st) = link.streams.get_mut(&id) else {
                return;
            };
            let Some(slot) = st.ex.defer.clone() else {
                return;
            };
            let taken = {
                let mut state = slot.lock();
                match core::mem::replace(&mut *state, SlotState::Idle) {
                    SlotState::Completed(g, parts, close) if g == generation => {
                        Some(Some((parts, close)))
                    }
                    SlotState::Dropped(g) if g == generation => Some(None),
                    other => {
                        *state = other;
                        None
                    }
                }
            };
            let Some(taken) = taken else {
                return;
            };
            if st.ex.parked {
                st.ex.parked = false;
                self.shared.counters.unpark(st.ex.env.worker.get());
            }
            match taken {
                Some((parts, close)) => {
                    let mut old = core::mem::replace(&mut st.ex.reply.parts, parts);
                    self.shared.family.clear_parts(&mut old, self.wipe);
                    self.h2_respond(io, link, id, close);
                }
                None => {
                    self.end.get_or_insert(EndReason::ReplyDropped);
                    self.h2_refuse(
                        io,
                        link,
                        id,
                        RefusalKind::Internal,
                        500,
                        None,
                        RequestEnd::Dropped,
                    );
                }
            }
        }

        /// A deadline passed: every stream past its own deadline is answered.
        pub(crate) fn h2_timer<I: Io>(&mut self, io: &mut I) -> Step {
            self.with_link(|this, link| {
                let now = io.now();
                let late: Vec<(u32, Phase2)> = link
                    .streams
                    .iter()
                    .filter_map(|(id, s)| match s.phase {
                        Phase2::Reading if s.ex.env.deadline <= now => Some((*id, s.phase)),
                        Phase2::Parked { until } if until <= now => Some((*id, s.phase)),
                        _ => None,
                    })
                    .collect();
                for (id, phase) in late {
                    match phase {
                        Phase2::Reading => this.h2_refuse(
                            io,
                            link,
                            id,
                            RefusalKind::BodyTimeout,
                            408,
                            None,
                            RequestEnd::TimedOut,
                        ),
                        _ => {
                            this.h2_cancel_deferred(link, id);
                            this.h2_refuse(
                                io,
                                link,
                                id,
                                RefusalKind::ReplyTimeout,
                                503,
                                None,
                                RequestEnd::TimedOut,
                            );
                        }
                    }
                }
                this.h2_flush(io, link)
            })
            .unwrap_or(Step::Close)
        }

        /// The peer closed its side: nothing more can arrive or be read.
        pub(crate) fn h2_peer_closed<I: Io>(&mut self, _io: &mut I) -> Step {
            let _ = self.with_link(|this, link| {
                let ids: Vec<u32> = link.streams.keys().copied().collect();
                for id in ids {
                    this.h2_end_stream(link, id, RequestEnd::PeerGone);
                }
            });
            self.end
                .get_or_insert(EndReason::Transport(crate::net::CloseReason::PeerClosed));
            self.state = State::Closed;
            Step::Close
        }

        /// The connection closed; every open request ends.
        pub(crate) fn h2_closed(&mut self, transport: bool) {
            let end = if transport {
                RequestEnd::PeerGone
            } else {
                RequestEnd::Killed
            };
            let _ = self.with_link(|this, link| {
                let ids: Vec<u32> = link.streams.keys().copied().collect();
                for id in ids {
                    this.h2_end_stream(link, id, end);
                }
            });
            self.h2 = None;
        }

        /// The output drained: continue what waits for room.
        pub(crate) fn h2_writable<I: Io>(&mut self, io: &mut I) -> Step {
            self.h2_drive(io, &[]).1
        }

        /// A timer for the HTTP/2 side.
        pub(crate) fn h2_timeout<I: Io>(&mut self, io: &mut I, which: Timeout) -> Step {
            match which {
                Timeout::Transport(reason) => {
                    self.end.get_or_insert(EndReason::Transport(reason));
                    Step::Close
                }
                Timeout::Exchange | Timeout::Park => self.h2_timer(io),
            }
        }
    }

    /// Takes a piece of a request body under the stream's policy; returns
    /// whether it went over the policy's limit.
    fn take_body<F: Family>(st: &mut H2Stream<F>, data: &[u8]) -> bool {
        let n = data.len() as u64;
        let Some(r) = st.ex.request.as_mut() else {
            return false;
        };
        r.bytes_in = r.bytes_in.saturating_add(n);
        match st.policy {
            BodyPolicy::Buffer { max } => {
                if st.ex.body.len() as u64 + n > max {
                    return true;
                }
                st.ex.body.extend_from_slice(data);
                false
            }
            BodyPolicy::Discard { max } => r.bytes_in.saturating_sub(r.head_bytes) > max,
            BodyPolicy::Stream => false,
        }
    }
}

#[cfg(feature = "h2")]
pub(crate) use imp::H2Link;

/// Without the feature: nothing selects HTTP/2, and these are never reached.
#[cfg(not(feature = "h2"))]
impl<F: Family, S: Service<F>, H: Hooks> Conn<F, S, H> {
    pub(crate) fn wants_h2(&self) -> bool {
        false
    }
    pub(crate) fn h2_sniff<I: Io>(&mut self, _io: &mut I, _input: &[u8]) -> Option<(usize, Step)> {
        None
    }
    pub(crate) fn h2_open<I: Io>(&mut self, _io: &mut I) -> Step {
        Step::Close
    }
    pub(crate) fn h2_drive<I: Io>(&mut self, _io: &mut I, input: &[u8]) -> (usize, Step) {
        (input.len(), Step::Close)
    }
    pub(crate) fn h2_wake<I: Io>(&mut self, _io: &mut I, _token: u32, _value: u64) -> Step {
        Step::Close
    }
    pub(crate) fn h2_peer_closed<I: Io>(&mut self, _io: &mut I) -> Step {
        Step::Close
    }
    pub(crate) fn h2_closed(&mut self, _transport: bool) {}
    pub(crate) fn h2_writable<I: Io>(&mut self, _io: &mut I) -> Step {
        Step::Close
    }
    pub(crate) fn h2_timeout<I: Io>(&mut self, _io: &mut I, _which: Timeout) -> Step {
        Step::Close
    }
}

//! One request's state, from its head to its end: the envelope the service
//! sees, the request record, the body, the reply, and the deferral slot,
//! with the steps that do not depend on the protocol (D-119).
//!
//! An HTTP/1.1 connection holds one exchange and reuses it for every
//! request. A protocol that carries several requests at once on one
//! connection holds one per request in flight. The steps here decide;
//! writing the answer is the protocol's.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{InCall, Release, Request, Shared};
use crate::client::ClientInfo;
use crate::engine::{EngineError, Family, RefusalKind, Service};
use crate::envelope::{Envelope, RequestSeq};
use crate::error::EndReason;
use crate::hooks::Dispatch;
use crate::hooks::{Admit, Checkpoint, CheckpointAt, Hooks, Observer, Shed, Verdict};
use crate::net::ConnectionId;
use crate::net::{ControlState, Lane, WorkerId};
use crate::reply::{DeferSlot, Link, Reply, Waker};
use crate::service::{
    BodyPolicy, Finished, HeadAction, Outcome, RequestEnd, UpgradeTarget, WorkClass,
};
use crate::stats::Count;

pub(crate) struct Exchange<F: Family> {
    /// The envelope the service sees (`seq` and the per-request facts are
    /// set at each head).
    pub(crate) env: Envelope,
    /// The request between its head and its end; `None` between requests.
    pub(crate) request: Option<Request>,
    pub(crate) body: Vec<u8>,
    pub(crate) reply: Reply<F>,
    /// Made at the first deferral and kept for the exchange's life.
    pub(crate) defer: Option<Arc<DeferSlot<F::Parts>>>,
    /// Counts dispatches: a deferral is woken by the generation it was
    /// made in, so a stale completion is told apart.
    pub(crate) generation: u64,
    /// This exchange holds one of its worker's parked-request places.
    pub(crate) parked: bool,
}

/// The facts of a new request that come from its connection and protocol.
pub(crate) struct Start {
    pub(crate) seq: u32,
    pub(crate) client: ClientInfo,
    pub(crate) lane: Lane,
    pub(crate) arrived: Instant,
    pub(crate) deadline: Instant,
    pub(crate) head_reads: u16,
    pub(crate) head_len: u64,
    pub(crate) max_body: u64,
    pub(crate) policy: BodyPolicy,
}

/// What `head` decided, before admission.
pub(crate) struct HeadDecision {
    pub(crate) action: HeadAction,
    pub(crate) panicked: bool,
    pub(crate) upgrade_refused: Option<EngineError>,
}

/// What `head`, the control line, admission, and the admitted checkpoint
/// decided about a request. Nothing is written yet.
pub(crate) enum Admission {
    /// Read the body under this policy.
    Proceed(BodyPolicy),
    /// The service asked for an upgrade, and it passed admission.
    Upgrade(UpgradeTarget),
    /// Refuse the request with this status; `reason` is how the connection
    /// ends when the protocol closes it.
    Refuse {
        kind: RefusalKind,
        status: u16,
        retry_after: Option<Duration>,
        end: RequestEnd,
        reason: EndReason,
    },
    /// A checkpoint asked for the connection to die.
    Kill,
}

/// The connection-wide facts admission reads.
pub(crate) struct Gate {
    pub(crate) control: ControlState,
    pub(crate) critical: bool,
    pub(crate) worker: WorkerId,
    pub(crate) now: Instant,
    /// The body length the head declared.
    pub(crate) declared: Option<u64>,
}

/// What a service call returned.
pub(crate) struct Called {
    pub(crate) outcome: Outcome,
    /// The call panicked (and the server catches panics): the outcome is
    /// `Close` and the reply must not be sent.
    pub(crate) panicked: bool,
}

/// Runs the service for one request, through the dispatch hook: the worker
/// is marked in a call, a panic is caught when the server catches panics,
/// and a debug build reports a `Quick` service that took over 1 ms.
pub(crate) fn call<F: Family, S: Service<F>, H: Hooks>(
    shared: &Shared<F, S, H>,
    request: &F::Request<'_>,
    env: &Envelope,
    reply: &mut Reply<F>,
) -> Called {
    // The misdeclared-service check is a debug build's: a release build
    // reads no clock here.
    let started = cfg!(debug_assertions).then(Instant::now);
    let in_call = InCall::enter(&shared.counters, env.worker.get());
    let mut run = || shared.dispatch.run(&shared.service, request, env, reply);
    let mut panicked = false;
    let outcome = if shared.catch_panics {
        // The reply is cleared before the refusal is written, so a
        // half-built one never leaves.
        let caught = catch_unwind(AssertUnwindSafe(&mut run));
        panicked = caught.is_err();
        caught.unwrap_or(Outcome::Close)
    } else {
        run()
    };
    drop(in_call);
    if let Some(started) = started {
        let took = started.elapsed();
        if <S as Service<F>>::WORK == WorkClass::Quick && took > Duration::from_millis(1) {
            shared.observer.service_misdeclared(env, took);
        }
    }
    Called { outcome, panicked }
}

impl<F: Family> Exchange<F> {
    /// Readies the reply for a dispatch: the next generation, the deferral
    /// link (the slot is made once per exchange, and the reply keeps its
    /// link between calls, so only a deferral, which moves the link into its
    /// token, costs a clone), and the parts cleared, or moved to the next
    /// part of a streamed reply.
    pub(crate) fn ready_reply<S: Service<F>, H: Hooks>(
        &mut self,
        shared: &Shared<F, S, H>,
        waker: impl FnOnce() -> Arc<dyn Waker>,
        connection: ConnectionId,
        wipe: bool,
        streaming: bool,
    ) {
        self.generation = self.generation.wrapping_add(1);
        match self.reply.link.as_mut() {
            Some(link) => link.generation = self.generation,
            None => {
                let slot = match &self.defer {
                    Some(s) => Arc::clone(s),
                    None => {
                        let mut s = DeferSlot::new(waker(), connection);
                        if let (true, Some(job)) = (wipe, &self.reply.job) {
                            s = s.wiping(Arc::new(crate::hooks::JobWiper(Arc::clone(job))));
                        }
                        let s = Arc::new(s);
                        self.defer = Some(Arc::clone(&s));
                        s
                    }
                };
                self.reply.link = Some(Link {
                    slot,
                    generation: self.generation,
                });
            }
        }
        self.reply.deferred = false;
        if streaming {
            shared.family.next_stream_part(&mut self.reply.parts, wipe);
        } else {
            shared.family.clear_parts(&mut self.reply.parts, wipe);
        }
    }

    pub(crate) fn new(env: Envelope, parts: F::Parts) -> Self {
        Self {
            env,
            request: None,
            body: Vec::new(),
            reply: Reply::new(parts),
            defer: None,
            generation: 0,
            parked: false,
        }
    }

    /// Starts a request: the envelope's per-request facts, the record (made
    /// before `head` runs, so `done` follows every `head`, a panicking one
    /// included, D-111), the observer, and `head` itself.
    pub(crate) fn begin<S: Service<F>, H: Hooks>(
        &mut self,
        shared: &Shared<F, S, H>,
        head: &F::Head<'_>,
        start: Start,
    ) -> HeadDecision {
        self.env.seq = RequestSeq(start.seq);
        self.env.client = start.client;
        self.env.lane = start.lane;
        self.env.lane_open = true;
        self.env.lane_late = false;
        self.env.arrived = start.arrived;
        self.env.deadline = start.deadline;
        self.env.user_word = 0;
        self.request = Some(Request {
            head_reads: start.head_reads,
            admitted: None,
            head_at: start.arrived,
            admitted_at: None,
            bytes_in: start.head_len,
            head_bytes: start.head_len,
            bytes_in_limit: start.head_len.saturating_add(start.max_body),
            policy: start.policy,
            discard_over: false,
            status: None,
            bytes_out: 0,
        });
        shared.observer.request_head(&self.env);
        let env = &mut self.env;
        let mut panicked = false;
        let action = if shared.catch_panics {
            let caught = catch_unwind(AssertUnwindSafe(|| shared.service.head(head, env)));
            panicked = caught.is_err();
            caught.unwrap_or(HeadAction::Proceed)
        } else {
            shared.service.head(head, env)
        };
        self.env.lane_open = false;
        // An upgrade is judged on the head view, where the trusted authority
        // is known (a WebSocket origin).
        let upgrade_refused = match &action {
            HeadAction::Upgrade(target) => shared.family.upgrade_allowed(head, target).err(),
            _ => None,
        };
        HeadDecision {
            action,
            panicked,
            upgrade_refused,
        }
    }

    /// After `head`: a panic, the service's action, the control line (shed,
    /// lockdown), admission, the admitted checkpoint, and a declared body
    /// over a buffered policy's limit. The observer calls and the record's
    /// admission facts happen here; writing the answer is the caller's.
    pub(crate) fn admit<S: Service<F>, H: Hooks>(
        &mut self,
        shared: &Shared<F, S, H>,
        decision: HeadDecision,
        default_policy: BodyPolicy,
        gate: &Gate,
    ) -> Admission {
        if decision.panicked {
            shared
                .observer
                .service_error(&self.env, "the service panicked in head");
            return Admission::Refuse {
                kind: RefusalKind::Internal,
                status: 500,
                retry_after: None,
                end: RequestEnd::ServicePanic,
                reason: EndReason::ServicePanic,
            };
        }
        let mut upgrade = None;
        let policy = match decision.action {
            HeadAction::Proceed => default_policy,
            HeadAction::ProceedWith(p) => p,
            HeadAction::Upgrade(target) => {
                if let Some(e) = decision.upgrade_refused {
                    return Admission::Refuse {
                        kind: e.kind,
                        status: e.status,
                        retry_after: None,
                        end: RequestEnd::Refused(e.kind),
                        reason: EndReason::Refused,
                    };
                }
                // The upgrade request carries no body: a declared one is 413.
                upgrade = Some(target);
                BodyPolicy::Buffer { max: 0 }
            }
            HeadAction::Refuse {
                status,
                retry_after,
            } => {
                return Admission::Refuse {
                    kind: RefusalKind::Service,
                    status,
                    retry_after,
                    end: RequestEnd::Refused(RefusalKind::Service),
                    reason: EndReason::Refused,
                };
            }
        };
        let refused_by_control = match gate.control {
            ControlState::Shed { from_lane } => self.env.lane >= from_lane,
            ControlState::Lockdown => !gate.critical,
            _ => false,
        };
        if refused_by_control {
            let shed = Shed {
                retry_after: None,
                close: true,
            };
            shared.observer.request_shed(&self.env, &shed);
            return Admission::Refuse {
                kind: RefusalKind::Busy,
                status: 503,
                retry_after: Some(Duration::from_secs(1)),
                end: RequestEnd::Shed,
                reason: EndReason::Shed,
            };
        }
        match shared.admit.admit(gate.worker, &self.env) {
            Ok(a) => {
                if let Some(r) = self.request.as_mut() {
                    r.admitted = Some(a);
                    r.admitted_at = Some(gate.now);
                }
                shared.observer.request_admitted(&self.env);
            }
            Err(shed) => {
                shared.observer.request_shed(&self.env, &shed);
                let retry = shed.retry_after.or(Some(Duration::from_secs(1)));
                return Admission::Refuse {
                    kind: RefusalKind::Busy,
                    status: 503,
                    retry_after: retry,
                    end: RequestEnd::Shed,
                    reason: EndReason::Shed,
                };
            }
        }
        match shared.checkpoint.check(&self.env, CheckpointAt::Admitted) {
            Verdict::Pass => {}
            Verdict::Deny { status } => {
                return Admission::Refuse {
                    kind: RefusalKind::Denied,
                    status,
                    retry_after: None,
                    end: RequestEnd::Denied,
                    reason: EndReason::Denied,
                };
            }
            Verdict::Kill => return Admission::Kill,
        }
        if let (BodyPolicy::Buffer { max }, Some(len)) = (policy, gate.declared) {
            if len > max {
                return Admission::Refuse {
                    kind: RefusalKind::TooLarge,
                    status: 413,
                    retry_after: None,
                    end: RequestEnd::Refused(RefusalKind::TooLarge),
                    reason: EndReason::ProtocolError,
                };
            }
        }
        if let Some(r) = self.request.as_mut() {
            r.policy = policy;
        }
        match upgrade {
            Some(target) => Admission::Upgrade(target),
            None => Admission::Proceed(policy),
        }
    }

    /// Ends the request, once: the counters, `done` (its panic caught when
    /// the server catches panics), the observer, and admission's release,
    /// which a guard makes certain even on an unwind (D-111). `draining`
    /// says whether a drain was seen on the connection.
    pub(crate) fn finish<S: Service<F>, H: Hooks>(
        &mut self,
        shared: &Shared<F, S, H>,
        end: RequestEnd,
        draining: bool,
        wipe: bool,
    ) {
        let Some(r) = self.request.take() else { return };
        let worker = self.env.worker.get();
        let counters = &shared.counters;
        counters.count(worker, Count::Requests);
        match end {
            RequestEnd::Replied => counters.count(worker, Count::Replied),
            RequestEnd::Shed => counters.count(worker, Count::Shed),
            RequestEnd::Denied => counters.count(worker, Count::Denied),
            RequestEnd::TimedOut => counters.count(worker, Count::TimedOut),
            _ => {}
        }
        if draining {
            counters.count(worker, Count::FinishedDraining);
        }
        let finished = Finished {
            head_reads: r.head_reads,
            status: r.status,
            end,
            bytes_in: r.bytes_in,
            bytes_out: r.bytes_out,
            head_at: r.head_at,
            admitted_at: r.admitted_at,
            replied_at: if end == RequestEnd::Replied {
                Some(Instant::now())
            } else {
                None
            },
        };
        // Released when this scope ends, after `done` and the observer, and
        // on an unwind out of either: a permit is never lost (D-111).
        let release = Release {
            admit: &shared.admit,
            env: &self.env,
            admitted: r.admitted,
        };
        if shared.catch_panics {
            let done = catch_unwind(AssertUnwindSafe(|| {
                shared.service.done(&self.env, &finished);
            }));
            if done.is_err() {
                shared
                    .observer
                    .service_error(&self.env, "the service panicked in done");
            }
        } else {
            shared.service.done(&self.env, &finished);
        }
        shared.observer.request_finished(&self.env, &finished);
        drop(release);
        if wipe {
            crate::engine::wipe_bytes(&mut self.body);
        }
        self.body.clear();
    }
}

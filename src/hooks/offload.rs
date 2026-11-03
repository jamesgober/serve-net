//! Moving slow services off the worker, onto a bounded pool the owner runs.

use core::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Duration;

use crate::engine::{DetachedRequest, Family, Service};
use crate::envelope::Envelope;
use crate::hooks::Dispatch;
use crate::reply::{CompleteError, Reply, ReplyToken};
use crate::service::{Outcome, WorkClass};

/// How long a job waits before it tries a full completion queue again.
const COMPLETE_RETRY: Duration = Duration::from_millis(1);

/// A bounded pool of threads, owned by the deployment.
pub trait OffloadPool: Send + Sync + 'static {
    /// Queues `job`, or hands it back when the pool is full. Never blocks.
    ///
    /// # Errors
    ///
    /// The job itself: serve-net answers the busy refusal.
    fn try_submit(&self, class: WorkClass, job: OffloadJob) -> Result<(), OffloadJob>;
}

/// One request on its way to a pool thread: an owned copy of the request and
/// the envelope, the reply, and a handle on the service. Dropping a job
/// without running it answers the 500 refusal.
pub struct OffloadJob {
    class: WorkClass,
    body: Box<dyn JobBody>,
}

impl OffloadJob {
    /// How the service behaves.
    #[must_use]
    pub fn class(&self) -> WorkClass {
        self.class
    }

    /// Calls the service and completes the reply; on the pool's thread. A
    /// panicking service answers the 500 refusal and leaves the thread alive.
    pub fn run(self) {
        self.body.run();
    }
}

impl fmt::Debug for OffloadJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OffloadJob")
            .field("class", &self.class)
            .finish_non_exhaustive()
    }
}

/// A job with its family erased.
trait JobBody: Send {
    fn run(self: Box<Self>);
    /// The pool refused it: nothing is waiting on the reply any more.
    fn cancel(self: Box<Self>);
}

/// A service behind an `Arc`, for a job that outlives the worker's call.
pub(crate) trait JobService<F: Family>: Send + Sync {
    fn detach(&self, request: &F::Request<'_>) -> Option<Box<dyn DetachedRequest<F>>>;
    fn call(&self, request: &F::Request<'_>, env: &Envelope, reply: &mut Reply<F>) -> Outcome;
    fn panicked(&self, env: &Envelope);
}

struct Job<F: Family> {
    request: Box<dyn DetachedRequest<F>>,
    env: Envelope,
    token: ReplyToken<F>,
    service: Arc<dyn JobService<F>>,
}

impl<F: Family> JobBody for Job<F> {
    fn run(self: Box<Self>) {
        let Job {
            request,
            env,
            mut token,
            service,
        } = *self;
        if token.is_cancelled() {
            return;
        }
        token.link_reply();
        let reply = token.reply();
        let mut outcome = None;
        let called = catch_unwind(AssertUnwindSafe(|| {
            request.with(&mut |r| outcome = Some(service.call(r, &env, &mut *reply)));
        }));
        if called.is_err() {
            service.panicked(&env);
            return;
        }
        if token.reply().deferred {
            // The service took the reply further: its own token answers now.
            token.release();
            return;
        }
        let close = match outcome {
            Some(Outcome::Done) => false,
            Some(Outcome::Close) => true,
            // `Deferred` with no `defer`: the dropped token answers 500.
            _ => return,
        };
        loop {
            match token.finish(close) {
                Err(CompleteError::Busy(back)) => token = back,
                _ => return,
            }
            std::thread::sleep(COMPLETE_RETRY);
        }
    }

    fn cancel(self: Box<Self>) {
        let mut job = *self;
        job.token.release_to_idle();
    }
}

/// Runs `Quick` services on the worker and hands `Io` and `Cpu` services to a
/// bounded pool: the job carries owned copies of the head, the body, and the
/// envelope, and the reply comes back through the connection's reply slot. A
/// full pool hands the job back and the client gets the busy refusal, so the
/// queue is never unbounded.
///
/// On the blocking driver the worker's thread parks until the reply arrives,
/// and the pool bounds how many slow calls run at once. On a reactor the
/// worker goes straight back to its other connections. A family without
/// [`Family::detach`], and a test connection, run the service on the worker.
///
/// Cost: about 2-10 microseconds per offloaded request for the hand-off and
/// the completion, plus a copy of the head and the body [est.]. `Quick`
/// services pay nothing. Security: the copy holds request data for as long as
/// the job lives; a wiping profile does not reach it.
///
/// ```
/// use serve_net::{Offload, OffloadJob, OffloadPool, WorkClass};
/// use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
///
/// struct Pool(SyncSender<OffloadJob>);
///
/// impl OffloadPool for Pool {
///     fn try_submit(&self, _class: WorkClass, job: OffloadJob) -> Result<(), OffloadJob> {
///         self.0.try_send(job).map_err(|e| match e {
///             TrySendError::Full(job) | TrySendError::Disconnected(job) => job,
///         })
///     }
/// }
///
/// let (jobs, queue) = sync_channel::<OffloadJob>(64);
/// std::thread::spawn(move || queue.into_iter().for_each(OffloadJob::run));
/// let dispatch = Offload::new(Pool(jobs));
/// # let _ = dispatch;
/// ```
#[derive(Debug)]
pub struct Offload<P: OffloadPool> {
    pool: P,
}

impl<P: OffloadPool> Offload<P> {
    /// Offloads to `pool`.
    pub fn new(pool: P) -> Self {
        Offload { pool }
    }
}

impl<P: OffloadPool> Dispatch for Offload<P> {
    fn run<F: Family, S: Service<F>>(
        &self,
        service: &S,
        request: &F::Request<'_>,
        env: &Envelope,
        reply: &mut Reply<F>,
    ) -> Outcome {
        if S::WORK == WorkClass::Quick {
            return service.call(request, env, reply);
        }
        let detached = reply
            .job
            .as_ref()
            .and_then(|job| Some((job.detach(request)?, Arc::clone(job))));
        let Some((copy, handle)) = detached else {
            return service.call(request, env, reply);
        };
        let job = OffloadJob {
            class: S::WORK,
            body: Box::new(Job {
                request: copy,
                env: env.clone(),
                token: reply.defer(),
                service: handle,
            }),
        };
        match self.pool.try_submit(S::WORK, job) {
            Ok(()) => Outcome::Deferred,
            Err(job) => {
                job.body.cancel();
                reply.deferred = false;
                reply.pool_full = true;
                Outcome::Close
            }
        }
    }
}

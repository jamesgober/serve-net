//! Building a server: configuration, hooks, and validation.

use std::sync::Arc;
use std::time::Duration;

use crate::config::validate::{self, Draft};
use crate::config::{KeepAlive, Limits, ListenerSpec, Profile, ProfileId, ProfileTable, Workers};
use crate::error::{ConfigErrors, ConfigWarning};
use crate::hooks::{Admit, Checkpoint, DefaultHooks, Dispatch, HookSet, Hooks, Observer};
use crate::net::ControlLine;

/// The entry point for Tier 2.
///
/// ```
/// use serve_net::{ListenerSpec, Server};
/// let built = Server::builder().listener(ListenerSpec::tcp("web", "127.0.0.1:0")).build()?;
/// assert!(built.warnings().is_empty());
/// # Ok::<(), serve_net::ConfigErrors>(())
/// ```
#[derive(Debug)]
pub struct Server;

impl Server {
    /// A builder with no listeners, no profiles (profile 0 is created with
    /// defaults when none is added), default limits, workers, and hooks.
    pub fn builder() -> ServerBuilder {
        ServerBuilder {
            config: Config {
                listeners: Vec::new(),
                profiles: Vec::new(),
                limits: Limits::new(),
                keep_alive: KeepAlive::new(),
                workers: Workers::auto(),
                drain_deadline: Duration::from_secs(30),
                accept_failure_window: Duration::from_secs(30),
                has_control: false,
            },
            hooks: DefaultHooks::default(),
        }
    }
}

/// Everything a builder holds except the hooks.
struct Config {
    listeners: Vec<ListenerSpec>,
    profiles: Vec<Profile>,
    limits: Limits,
    keep_alive: KeepAlive,
    workers: Workers,
    drain_deadline: Duration,
    accept_failure_window: Duration,
    has_control: bool,
}

/// Configuration and hooks; nothing is checked until [`build`](Self::build).
#[must_use]
pub struct ServerBuilder<H = DefaultHooks> {
    config: Config,
    hooks: H,
}

impl<H> ServerBuilder<H> {
    /// Adds a listener (at most 64).
    pub fn listener(mut self, spec: ListenerSpec) -> Self {
        self.config.listeners.push(spec);
        self
    }

    /// Adds a profile (at most 255).
    pub fn profile(mut self, profile: Profile) -> Self {
        self.config.profiles.push(profile);
        self
    }

    /// Server-wide limits; profiles may override them.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.config.limits = limits;
        self
    }

    /// Server-wide keep-alive; profiles may override it.
    pub fn keep_alive(mut self, policy: KeepAlive) -> Self {
        self.config.keep_alive = policy;
        self
    }

    /// The driver and worker count.
    pub fn workers(mut self, workers: Workers) -> Self {
        self.config.workers = workers;
        self
    }

    /// The deadline `wait` uses when a drain is requested without one (default 30 s).
    pub fn drain_deadline(mut self, deadline: Duration) -> Self {
        self.config.drain_deadline = deadline;
        self
    }

    /// How long a listener may report accept errors before health is
    /// `Degraded` (default 30 s).
    pub fn accept_failure_window(mut self, window: Duration) -> Self {
        self.config.accept_failure_window = window;
        self
    }
}

impl<A, D, K, O, C> ServerBuilder<HookSet<A, D, K, O, C>> {
    /// Replaces admission.
    pub fn admit<A2: Admit>(self, admit: A2) -> ServerBuilder<HookSet<A2, D, K, O, C>> {
        let HookSet {
            dispatch,
            checkpoint,
            observer,
            control,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                control,
            },
        }
    }

    /// Replaces dispatch.
    pub fn dispatch<D2: Dispatch>(self, dispatch: D2) -> ServerBuilder<HookSet<A, D2, K, O, C>> {
        let HookSet {
            admit,
            checkpoint,
            observer,
            control,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                control,
            },
        }
    }

    /// Replaces the revocation checkpoint.
    pub fn checkpoint<K2: Checkpoint>(
        self,
        checkpoint: K2,
    ) -> ServerBuilder<HookSet<A, D, K2, O, C>> {
        let HookSet {
            admit,
            dispatch,
            observer,
            control,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                control,
            },
        }
    }

    /// Replaces the observer.
    pub fn observer<O2: Observer>(self, observer: O2) -> ServerBuilder<HookSet<A, D, K, O2, C>> {
        let HookSet {
            admit,
            dispatch,
            checkpoint,
            control,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                control,
            },
        }
    }

    /// The process-wide control line, shared with net-io.
    pub fn control<C2: ControlLine>(
        mut self,
        line: Arc<C2>,
    ) -> ServerBuilder<HookSet<A, D, K, O, C2>> {
        self.config.has_control = true;
        let HookSet {
            admit,
            dispatch,
            checkpoint,
            observer,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                control: line,
            },
        }
    }
}

impl<H: Hooks> ServerBuilder<H> {
    /// Runs every validation rule and collects every failure; compiles the
    /// profile table. Nothing is bound and no thread is started.
    ///
    /// # Errors
    ///
    /// [`ConfigErrors`], never empty: every rule that failed.
    pub fn build(self) -> Result<Built<H>, ConfigErrors> {
        let c = self.config;
        let findings = validate::check(&Draft {
            listeners: &c.listeners,
            profiles: &c.profiles,
            limits: &c.limits,
            keep_alive: &c.keep_alive,
            workers: &c.workers,
            drain_deadline: c.drain_deadline,
            has_control: c.has_control,
        });
        if let Some(errors) = ConfigErrors::from_vec(findings.errors) {
            return Err(errors);
        }
        let mut profiles = c.profiles;
        if profiles.is_empty() {
            profiles.push(Profile::new(ProfileId(0)));
        }
        let table = ProfileTable::compile(&profiles, c.limits, c.keep_alive);
        Ok(Built {
            listeners: c.listeners,
            profiles: table,
            limits: c.limits,
            keep_alive: c.keep_alive,
            workers: c.workers,
            drain_deadline: c.drain_deadline,
            accept_failure_window: c.accept_failure_window,
            warnings: findings.warnings,
            hooks: self.hooks,
        })
    }
}

impl<H> core::fmt::Debug for ServerBuilder<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ServerBuilder")
            .field("listeners", &self.config.listeners)
            .field("profiles", &self.config.profiles.len())
            .finish()
    }
}

/// A validated configuration: nothing bound, no thread started.
#[must_use]
#[allow(dead_code)] // the fields are read by `bind` (waits for net-io 0.3)
pub struct Built<H: Hooks = DefaultHooks> {
    pub(crate) listeners: Vec<ListenerSpec>,
    pub(crate) profiles: ProfileTable,
    pub(crate) limits: Limits,
    pub(crate) keep_alive: KeepAlive,
    pub(crate) workers: Workers,
    pub(crate) drain_deadline: Duration,
    pub(crate) accept_failure_window: Duration,
    pub(crate) warnings: Vec<ConfigWarning>,
    pub(crate) hooks: H,
}

impl<H: Hooks> Built<H> {
    /// Values that are allowed but questionable.
    #[must_use]
    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    /// The compiled profile table.
    #[must_use]
    pub fn profiles(&self) -> &ProfileTable {
        &self.profiles
    }
}

impl<H: Hooks> core::fmt::Debug for Built<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Built")
            .field("listeners", &self.listeners)
            .field("warnings", &self.warnings)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Ingress, ProxySource, RuleId, TrustedProxies};

    #[test]
    fn test_build_reports_every_error_at_once() {
        let errors = Server::builder()
            .listener(ListenerSpec::tcp("Bad Name", "127.0.0.1:0"))
            .listener(
                ListenerSpec::tcp("site", "0.0.0.0:8080").ingress(Ingress::Proxy(
                    ProxySource::forwarded_for(TrustedProxies::loopback()),
                )),
            )
            .build()
            .unwrap_err();
        assert!(errors.has(RuleId::BadName));
        assert!(errors.has(RuleId::PublicBind));
    }

    #[test]
    fn test_build_creates_profile_zero() {
        let built = Server::builder()
            .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
            .build()
            .unwrap();
        assert_eq!(built.profiles().len(), 1);
        assert!(built.warnings().is_empty());
    }

    #[test]
    fn test_hook_setters_change_types() {
        let built = Server::builder()
            .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
            .admit(crate::InFlightCap::new().per_worker(8))
            .dispatch(crate::Inline)
            .checkpoint(crate::NoCheckpoint)
            .observer(crate::NoObserver)
            .control(Arc::new(crate::SimpleControl::new()))
            .profile(Profile::new(ProfileId(0)).critical(true))
            .build()
            .unwrap();
        assert!(built.warnings().is_empty());
    }
}

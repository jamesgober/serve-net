//! Changing a running server: listeners, profiles, limits, and ingress.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::validate::{self, Draft};
use crate::config::{
    Ingress, KeepAlive, Limits, ListenerSpec, Profile, ProfileId, ProfileTable, ProxyKind, Workers,
};
use crate::conn::session::{Cap, Runtime};
use crate::conn::{ListenerRuntime, Tables};
use crate::error::{ConfigError, ConfigErrors, RuleId, ServeError, Subject};
use crate::notify::Notifier;
use crate::running::ListenerMeta;
use crate::server::{connection_caps, file_owner, listener_meta, listener_runtime, net_listener};
use crate::snapshot::Snapshot;

/// A server's configuration as plain data, so a program can build it from
/// its own configuration format and pass the same value at start-up
/// (`ServerBuilder::config`) and at reload.
///
/// ```
/// use serve_net::{Limits, ListenerSpec, ServerConfig};
/// let mut config = ServerConfig::new();
/// config.listeners.push(ListenerSpec::tcp("web", "127.0.0.1:8080"));
/// config.limits = Limits::new().max_body(4 * 1024 * 1024);
/// # let _ = config;
/// ```
#[non_exhaustive]
#[derive(Debug)]
pub struct ServerConfig {
    /// The listeners, by name.
    pub listeners: Vec<ListenerSpec>,
    /// The profiles (profile 0 is created with defaults when empty).
    pub profiles: Vec<Profile>,
    /// Server-wide limits.
    pub limits: Limits,
    /// Server-wide keep-alive.
    pub keep_alive: KeepAlive,
    /// The driver and worker count; a reload may not change it.
    pub workers: Workers,
    /// The deadline for a drain asked for without one.
    pub drain_deadline: Duration,
}

impl ServerConfig {
    /// No listeners, no profiles, and the default limits, keep-alive, and
    /// workers.
    #[must_use]
    pub fn new() -> Self {
        ServerConfig {
            listeners: Vec::new(),
            profiles: Vec::new(),
            limits: Limits::new(),
            keep_alive: KeepAlive::new(),
            workers: Workers::auto(),
            drain_deadline: Duration::from_secs(30),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig::new()
    }
}

/// What a reload changed.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadSummary {
    /// Listeners bound by this reload.
    pub added: Vec<String>,
    /// Listeners closed by this reload.
    pub removed: Vec<String>,
    /// Listeners whose ingress, profile, or limits changed in place.
    pub changed: Vec<String>,
    /// Profiles in the new table.
    pub profiles: u16,
}

/// What a reload needs from the server it runs on.
pub(crate) struct Reloader {
    pub(crate) tables: Arc<Snapshot<Tables>>,
    pub(crate) rt: Arc<Runtime>,
    pub(crate) notifier: Arc<Notifier>,
    pub(crate) has_control: bool,
    pub(crate) has_tls: bool,
    pub(crate) workers: Workers,
    pub(crate) threads: u16,
}

/// What net-io fixed when the listener was bound: a reload cannot change it
/// in place.
pub(crate) fn transport_key(ingress: &Ingress) -> String {
    match ingress {
        Ingress::DirectTls => "tls".to_owned(),
        other => match other.proxy().map(|p| (p.kind, &p.trusted)) {
            Some((ProxyKind::ProxyProtocol(v), trusted)) => format!("proxy {v:?} {trusted:?}"),
            _ => "plain".to_owned(),
        },
    }
}

fn error(rule: RuleId, subject: Subject, message: &'static str, hint: &'static str) -> ConfigError {
    ConfigError::new(rule, subject, message, hint)
}

impl Reloader {
    /// Runs one reload; the caller holds the server's lock, so reloads are
    /// serialized.
    pub(crate) fn run(
        &self,
        server: &mut net_io::Server,
        metas: &mut Vec<Option<ListenerMeta>>,
        next: ServerConfig,
    ) -> Result<ReloadSummary, ServeError> {
        let ServerConfig {
            listeners,
            profiles,
            limits,
            keep_alive,
            workers,
            drain_deadline,
        } = next;

        // Validate everything first: nothing changes on failure.
        let findings = validate::check(&Draft {
            listeners: &listeners,
            profiles: &profiles,
            limits: &limits,
            keep_alive: &keep_alive,
            workers: &workers,
            drain_deadline,
            has_control: self.has_control,
            has_tls: self.has_tls,
        });
        let mut errors = findings.errors;
        let (soft, hard, threads) = connection_caps(&limits, &workers);
        if workers != self.workers || threads > self.threads {
            errors.push(error(
                RuleId::ReloadWorkers,
                Subject::Server,
                "a reload cannot change the workers, and these caps need more of them",
                "restart to change the workers, or keep max_connections within the pool",
            ));
        }
        let mut kept: Vec<(usize, ListenerSpec)> = Vec::new();
        let mut added: Vec<ListenerSpec> = Vec::new();
        for spec in listeners {
            let old = metas
                .iter()
                .enumerate()
                .find_map(|(id, m)| m.as_ref().filter(|m| m.name == spec.name).map(|m| (id, m)));
            match old {
                // The same socket: changes apply in place.
                Some((id, meta)) if meta.source == spec.source => {
                    if meta.transport != transport_key(&spec.ingress) {
                        errors.push(error(
                            RuleId::ReloadListener,
                            Subject::Listener(spec.name.clone()),
                            "PROXY trust and TLS are fixed when a listener is bound",
                            "add the listener under a new name and remove the old one, or restart",
                        ));
                    }
                    kept.push((id, spec));
                }
                // New, or a changed socket: a removal plus an addition.
                _ => added.push(spec),
            }
        }
        if let Some(errors) = ConfigErrors::from_vec(errors) {
            self.notifier.reload_refused(&errors);
            return Err(ServeError::Reload(errors));
        }

        let mut profiles = profiles;
        if profiles.is_empty() {
            profiles.push(Profile::new(ProfileId(0)));
        }
        let table = ProfileTable::compile(&profiles, limits, keep_alive);

        // Bind every added listener, door closed; on failure only these close.
        let mut bound: Vec<(usize, ListenerSpec)> = Vec::new();
        let in_use: Vec<String> = metas.iter().flatten().map(|m| m.name.clone()).collect();
        for spec in added {
            let result = net_listener(
                &spec,
                None,
                &table,
                &limits,
                hard,
                !in_use.contains(&spec.name),
            )
            .and_then(|net| {
                server
                    .add_listener(net)
                    .map_err(|source| ServeError::ReloadBind {
                        listener: spec.name.clone(),
                        source,
                    })
            });
            match result {
                Ok(id) => bound.push((usize::from(id.get()), spec)),
                Err(e) => {
                    for (id, _) in &bound {
                        if let Some(id) = listener_id(server, *id) {
                            let _ = server.remove_listener(id, Instant::now());
                        }
                    }
                    return Err(match e {
                        ServeError::Start(source) | ServeError::Bind { source, .. } => {
                            ServeError::ReloadBind {
                                listener: spec.name.clone(),
                                source,
                            }
                        }
                        other => other,
                    });
                }
            }
        }

        self.notifier.reloading(net_io::monotonic_micros());

        // The new tables: seats and caps carry over for what stays.
        let old = self.tables.load();
        let size = bound
            .iter()
            .map(|(id, _)| id + 1)
            .chain(kept.iter().map(|(id, _)| id + 1))
            .max()
            .unwrap_or(0)
            .max(metas.len());
        let mut listeners_table: Vec<Option<ListenerRuntime>> = (0..size).map(|_| None).collect();
        let mut next_metas: Vec<Option<ListenerMeta>> = (0..size).map(|_| None).collect();
        let mut changed = Vec::new();
        for (id, spec) in &kept {
            let Some(before) = old.listeners.get(*id).and_then(Option::as_ref) else {
                continue;
            };
            before.cap.set_limit(spec.max_connections);
            let mut runtime = listener_runtime(spec, Arc::clone(&before.cap));
            if let (Some(new), Some(was)) = (&mut runtime.unix, &before.unix) {
                new.same_user = Arc::clone(&was.same_user);
            }
            listeners_table[*id] = Some(runtime);
            let meta = listener_meta(spec);
            if metas[*id].as_ref().is_some_and(|m| !m.same_settings(&meta)) {
                changed.push(spec.name.clone());
            }
            next_metas[*id] = Some(meta);
        }
        let mut added_names = Vec::new();
        for (id, spec) in &bound {
            let runtime = listener_runtime(spec, Cap::new(spec.max_connections));
            if let Some(unix) = &runtime.unix {
                let info = server
                    .listeners()
                    .iter()
                    .find(|l| usize::from(l.id.get()) == *id);
                if let Some(net_io::Endpoint::Unix(path)) = info.map(|l| &l.local) {
                    if let Some(uid) = file_owner(path) {
                        let _ = unix.same_user.set(uid);
                    }
                }
            }
            listeners_table[*id] = Some(runtime);
            next_metas[*id] = Some(listener_meta(spec));
            added_names.push(spec.name.clone());
        }
        let profile_caps = table
            .iter()
            .enumerate()
            .map(|(i, p)| match old.profile_caps.get(i) {
                Some(cap) => {
                    cap.set_limit(p.budget.max_connections);
                    Arc::clone(cap)
                }
                None => Cap::new(p.budget.max_connections),
            })
            .collect();
        let profile_count = u16::try_from(table.len()).unwrap_or(u16::MAX);
        self.tables.publish(Tables {
            profiles: table,
            listeners: listeners_table,
            profile_caps,
        });
        self.rt.global.set_limit(Some(soft));

        // Open the new doors, then close and drain what left.
        let opened = server.open_door();
        let mut removed = Vec::new();
        let deadline = Instant::now() + drain_deadline;
        for (id, meta) in metas.iter().enumerate() {
            let Some(meta) = meta else { continue };
            if next_metas.get(id).is_some_and(Option::is_some) {
                continue;
            }
            removed.push(meta.name.clone());
            if let Some(id) = listener_id(server, id) {
                let _ = server.remove_listener(id, deadline);
            }
        }
        *metas = next_metas;
        opened.map_err(ServeError::Start)?;

        let summary = ReloadSummary {
            added: added_names,
            removed,
            changed,
            profiles: profile_count,
        };
        self.notifier.reloaded(&summary);
        Ok(summary)
    }
}

/// net-io's id for an index (ids are opaque; read back from the server).
fn listener_id(server: &net_io::Server, index: usize) -> Option<net_io::ListenerId> {
    server
        .listeners()
        .iter()
        .map(|l| l.id)
        .find(|id| usize::from(id.get()) == index)
}

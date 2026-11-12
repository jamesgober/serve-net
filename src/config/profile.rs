//! Profiles: which engine, lane, budget, and limits a listener's connections use.

use crate::config::{KeepAlive, Limits};
use crate::net::Lane;
use crate::service::BodyPolicy;

/// A profile's id: dense from 0, at most 255 profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ProfileId(pub u8);

impl ProfileId {
    /// Reserved; refused by `build` (rule `ProfileIdReserved`).
    pub const MAX: ProfileId = ProfileId(255);
}

/// A profile's share of the server. Unset values are unlimited within the
/// server's own caps; zero is refused (rule `ZeroCap`).
///
/// ```
/// use serve_net::Budget;
/// let admin = Budget::new().max_connections(50).max_in_flight(16);
/// assert_ne!(admin, Budget::new());
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    pub(crate) max_connections: Option<u32>,
    pub(crate) max_in_flight: Option<u32>,
    pub(crate) dedicated: Option<u16>,
}

impl Budget {
    /// No caps beyond the server's.
    #[must_use]
    pub const fn new() -> Self {
        Budget {
            max_connections: None,
            max_in_flight: None,
            dedicated: None,
        }
    }

    /// The profile's soft connection cap.
    #[must_use]
    pub const fn max_connections(mut self, n: u32) -> Self {
        self.max_connections = Some(n);
        self
    }

    /// Requests in flight for the profile, split across workers.
    #[must_use]
    pub const fn max_in_flight(mut self, n: u32) -> Self {
        self.max_in_flight = Some(n);
        self
    }

    /// Keeps `n` workers for this profile alone, so a flood on other
    /// listeners cannot take them (an admin listener). They come out of the
    /// server's soft connection cap: other profiles get the cap less every
    /// dedication, and this profile uses the rest of the cap once its own are
    /// taken. On the blocking driver a worker is a connection's thread, so
    /// this is a seat that stays free; it costs one atomic per connection on
    /// the profile, and capacity other profiles give up.
    #[must_use]
    pub const fn dedicated_workers(mut self, n: u16) -> Self {
        self.dedicated = Some(n);
        self
    }
}

/// The engine a profile's connections start with.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EngineChoice {
    /// HTTP/1.1 (http-net).
    #[default]
    Http1,
    /// The engine the TLS handshake's ALPN result names: HTTP/1.1 for
    /// `http/1.1` or no ALPN. `h2` needs the HTTP/2 engine, which is not
    /// built yet, so a connection that negotiated it is closed with
    /// `EndReason::NoEngine`; offer only `http/1.1` on the acceptor until
    /// then. Only on TLS listeners (`RuleId::AlpnWithoutTls`).
    Http1OrH2ByAlpn,
    /// A custom family given to `bind_family`; the ALPN ids it serves.
    Custom {
        /// The ALPN protocol ids offered on TLS listeners.
        alpn: &'static [&'static str],
    },
}

impl EngineChoice {
    /// Whether a connection whose TLS handshake negotiated `alpn` (`None`:
    /// no ALPN, or no TLS) has an engine under this choice. A `false` closes
    /// the connection before any byte reaches an engine (`EndReason::NoEngine`).
    pub(crate) fn admits(self, alpn: Option<&[u8]>) -> bool {
        match (self, alpn) {
            (_, None) => true,
            (EngineChoice::Http1 | EngineChoice::Http1OrH2ByAlpn, Some(p)) => {
                p == b"http/1.1" || p == b"http/1.0"
            }
            (EngineChoice::Custom { alpn: ids }, Some(p)) => {
                ids.iter().any(|id| id.as_bytes() == p)
            }
        }
    }
}

/// The answer to "connections on this listener use this engine, this lane,
/// this budget, these limits".
///
/// ```
/// use serve_net::{Budget, Lane, Limits, Profile, ProfileId};
/// use std::time::Duration;
///
/// let admin = Profile::new(ProfileId(1))
///     .lane(Lane(1))
///     .critical(true)
///     .budget(Budget::new().max_connections(50).max_in_flight(16))
///     .limits(Limits::new().request_timeout(Duration::from_secs(120)));
/// assert_eq!(admin.id(), ProfileId(1));
/// ```
#[must_use]
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub(crate) id: ProfileId,
    pub(crate) tag: u8,
    pub(crate) engine: EngineChoice,
    pub(crate) lane: Lane,
    pub(crate) budget: Budget,
    pub(crate) limits: Option<Limits>,
    pub(crate) keep_alive: Option<KeepAlive>,
    pub(crate) critical: bool,
    pub(crate) wipe_buffers: bool,
    pub(crate) body: Option<BodyPolicy>,
}

/// The default request lane.
pub(crate) const DEFAULT_LANE: Lane = Lane(4);

impl Profile {
    /// A profile with the defaults: HTTP/1.1, `Lane(4)`, no budget of its
    /// own, the server's limits and keep-alive, not critical, buffers wiped,
    /// bodies buffered up to `max_body`.
    pub fn new(id: ProfileId) -> Self {
        Profile {
            id,
            tag: 0,
            engine: EngineChoice::Http1,
            lane: DEFAULT_LANE,
            budget: Budget::new(),
            limits: None,
            keep_alive: None,
            critical: false,
            wipe_buffers: true,
            body: None,
        }
    }

    /// An opaque byte copied into every envelope (default 0).
    pub fn tag(mut self, tag: u8) -> Self {
        self.tag = tag;
        self
    }

    /// The engine connections start with.
    pub fn engine(mut self, engine: EngineChoice) -> Self {
        self.engine = engine;
        self
    }

    /// The default request lane.
    pub fn lane(mut self, lane: Lane) -> Self {
        self.lane = lane;
        self
    }

    /// The profile's share of the server.
    pub fn budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    /// Limits that replace the server's for this profile.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Keep-alive that replaces the server's for this profile.
    pub fn keep_alive(mut self, policy: KeepAlive) -> Self {
        self.keep_alive = Some(policy);
        self
    }

    /// Admitted under `Lockdown` (default false).
    pub fn critical(mut self, on: bool) -> Self {
        self.critical = on;
        self
    }

    /// Zero buffers when they return to a pool (default true).
    pub fn wipe_buffers(mut self, on: bool) -> Self {
        self.wipe_buffers = on;
        self
    }

    /// The body policy before `head` changes it.
    pub fn body(mut self, policy: BodyPolicy) -> Self {
        self.body = Some(policy);
        self
    }

    /// The profile's id.
    #[must_use]
    pub fn id(&self) -> ProfileId {
        self.id
    }

    /// Whether buffers are zeroed when they return to a pool: a custom
    /// family's engine reads it in `Family::engine` and zeroes the client's
    /// bytes it holds, as the HTTP engine does.
    #[must_use]
    pub fn wipes_buffers(&self) -> bool {
        self.wipe_buffers
    }
}

/// The compiled profiles, indexed by id.
#[derive(Debug)]
pub struct ProfileTable {
    pub(crate) profiles: Box<[Compiled]>,
}

/// A profile with the server's values filled in.
#[derive(Debug, Clone)]
pub(crate) struct Compiled {
    pub(crate) profile: Profile,
    pub(crate) limits: Limits,
    pub(crate) keep_alive: KeepAlive,
    pub(crate) body: BodyPolicy,
}

impl ProfileTable {
    /// The profile with this id.
    #[must_use]
    pub fn get(&self, id: ProfileId) -> Option<&Profile> {
        self.profiles.get(usize::from(id.0)).map(|c| &c.profile)
    }

    /// The number of profiles.
    #[must_use]
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// Whether the table is empty (never, after `build`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    /// Every profile, by id.
    pub fn iter(&self) -> impl Iterator<Item = &Profile> {
        self.profiles.iter().map(|c| &c.profile)
    }

    pub(crate) fn compiled(&self, id: ProfileId) -> Option<&Compiled> {
        self.profiles.get(usize::from(id.0))
    }

    /// Fills in server values; `profiles` must be dense from 0 (checked by validation).
    pub(crate) fn compile(
        profiles: &[Profile],
        limits: Limits,
        keep_alive: KeepAlive,
    ) -> ProfileTable {
        let mut sorted: Vec<&Profile> = profiles.iter().collect();
        sorted.sort_by_key(|p| p.id);
        let profiles = sorted
            .into_iter()
            .map(|p| {
                let limits = p.limits.unwrap_or(limits);
                let body = p.body.unwrap_or(BodyPolicy::Buffer {
                    max: limits.max_body,
                });
                Compiled {
                    profile: p.clone(),
                    limits,
                    keep_alive: p.keep_alive.unwrap_or(keep_alive),
                    body,
                }
            })
            .collect();
        ProfileTable { profiles }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_defaults() {
        let p = Profile::new(ProfileId(0));
        assert_eq!(p.lane, Lane(4));
        assert!(p.wipe_buffers);
        assert!(!p.critical);
        assert_eq!(p.engine, EngineChoice::Http1);
    }

    #[test]
    fn test_engine_admits_by_alpn() {
        for choice in [EngineChoice::Http1, EngineChoice::Http1OrH2ByAlpn] {
            assert!(choice.admits(None));
            assert!(choice.admits(Some(b"http/1.1")));
            assert!(choice.admits(Some(b"http/1.0")));
            // No HTTP/2 engine yet: h2 has nothing to run it.
            assert!(!choice.admits(Some(b"h2")));
            assert!(!choice.admits(Some(b"acme-tls/1")));
            assert!(
                !choice.admits(Some(b"HTTP/1.1")),
                "ALPN ids are exact bytes"
            );
        }
        let custom = EngineChoice::Custom {
            alpn: &["pg", "x-db/2"],
        };
        assert!(custom.admits(None));
        assert!(custom.admits(Some(b"x-db/2")));
        assert!(!custom.admits(Some(b"http/1.1")));
    }

    #[test]
    fn test_table_compiles_server_values() {
        let profiles = [
            Profile::new(ProfileId(1)).limits(Limits::new().max_body(10)),
            Profile::new(ProfileId(0)),
        ];
        let t = ProfileTable::compile(&profiles, Limits::new(), KeepAlive::new());
        assert_eq!(t.len(), 2);
        assert_eq!(t.get(ProfileId(0)).map(Profile::id), Some(ProfileId(0)));
        assert_eq!(
            t.compiled(ProfileId(1)).map(|c| c.body),
            Some(BodyPolicy::Buffer { max: 10 })
        );
        assert_eq!(
            t.compiled(ProfileId(0)).map(|c| c.body),
            Some(BodyPolicy::Buffer { max: 1 << 20 })
        );
        assert!(t.get(ProfileId(2)).is_none());
    }
}

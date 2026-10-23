//! The net-io items serve-net names in its own API.
//!
//! serve-net 0.3 depends on net-io 0.3, which is not published yet. Until it
//! is, this module defines the few net-io items serve-net's code and tests
//! need, with the shapes net-io's 0.2 design lock documents (net-io
//! `docs/API.md`). The day net-io 0.3 is on crates.io this module becomes
//! `pub use net_io::{...}` and nothing else in serve-net changes; serve-net
//! 0.3 is not released before that (dev/ROADMAP.md, v0.3.0).

use core::fmt;
use core::num::NonZeroU64;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// An importance class: lower is more important (net-io `Lane`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Lane(pub u8);

impl Lane {
    /// The most important lane, `Lane(0)`.
    pub const MOST_IMPORTANT: Lane = Lane(0);
}

/// A connection's id, never zero and never reused within a process (net-io
/// `ConnectionId`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(NonZeroU64);

impl ConnectionId {
    /// The id as a number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// An id from a number; `None` for 0.
    #[must_use]
    pub const fn new(id: u64) -> Option<ConnectionId> {
        match NonZeroU64::new(id) {
            Some(n) => Some(ConnectionId(n)),
            None => None,
        }
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A worker's index within one server (net-io `WorkerId`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkerId(u16);

impl WorkerId {
    /// The index.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    pub(crate) const fn from_index(index: u16) -> WorkerId {
        WorkerId(index)
    }
}

/// An address or CIDR text that did not parse (net-io `AddrError`).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddrError {
    /// Malformed text at this byte offset.
    Syntax {
        /// Byte offset of the problem.
        at: usize,
    },
    /// A prefix length above 32 (IPv4) or 128 (IPv6).
    PrefixTooLong {
        /// The family's maximum.
        max: u8,
    },
    /// Bits set below the prefix (`10.0.0.1/8`).
    HostBitsSet,
    /// More blocks than a list may hold.
    TooMany {
        /// The maximum.
        max: usize,
    },
    /// Empty text.
    Empty,
}

impl fmt::Display for AddrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AddrError::Syntax { at } => write!(f, "address or CIDR malformed at byte {at}"),
            AddrError::PrefixTooLong { max } => write!(f, "prefix length above {max}"),
            AddrError::HostBitsSet => f.write_str("host bits set below the prefix"),
            AddrError::TooMany { max } => write!(f, "more than {max} blocks"),
            AddrError::Empty => f.write_str("empty address list"),
        }
    }
}

impl std::error::Error for AddrError {}

/// A network block (net-io `Cidr`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// A block; the host bits must be zero.
    ///
    /// # Errors
    ///
    /// `PrefixTooLong`, `HostBitsSet`.
    pub fn new(ip: IpAddr, prefix: u8) -> Result<Cidr, AddrError> {
        let ip = normalize(ip);
        let max = if ip.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(AddrError::PrefixTooLong { max });
        }
        let masked = mask(ip, prefix);
        if masked != ip {
            return Err(AddrError::HostBitsSet);
        }
        Ok(Cidr {
            network: ip,
            prefix,
        })
    }

    /// A block from text: `10.0.0.0/8`, `2001:db8::/32`, or a bare address.
    ///
    /// # Errors
    ///
    /// `Empty`, `Syntax`, `PrefixTooLong`, `HostBitsSet`.
    pub fn parse(text: &str) -> Result<Cidr, AddrError> {
        if text.is_empty() {
            return Err(AddrError::Empty);
        }
        let (addr, prefix) = match text.find('/') {
            Some(at) => (&text[..at], Some((&text[at + 1..], at + 1))),
            None => (text, None),
        };
        let ip: IpAddr = addr.parse().map_err(|_| AddrError::Syntax { at: 0 })?;
        let ip = normalize(ip);
        let prefix = match prefix {
            None => {
                if ip.is_ipv4() {
                    32
                } else {
                    128
                }
            }
            Some((digits, at)) => {
                if digits.is_empty()
                    || digits.len() > 3
                    || !digits.bytes().all(|b| b.is_ascii_digit())
                    || (digits.len() > 1 && digits.starts_with('0'))
                {
                    return Err(AddrError::Syntax { at });
                }
                digits.parse::<u8>().map_err(|_| AddrError::Syntax { at })?
            }
        };
        Cidr::new(ip, prefix)
    }

    /// Whether `ip` (normalized first) is inside the block.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = normalize(ip);
        ip.is_ipv4() == self.network.is_ipv4() && mask(ip, self.prefix) == self.network
    }

    /// The network address.
    #[must_use]
    pub fn network(&self) -> IpAddr {
        self.network
    }

    /// The prefix length.
    #[must_use]
    pub fn prefix_len(&self) -> u8 {
        self.prefix
    }

    /// Whether this is an IPv4 block.
    #[must_use]
    pub fn is_ipv4(&self) -> bool {
        self.network.is_ipv4()
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let m = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(prefix))
            };
            IpAddr::V4(Ipv4Addr::from(bits & m))
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let m = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - u32::from(prefix))
            };
            IpAddr::V6(Ipv6Addr::from(bits & m))
        }
    }
}

/// An IPv4-mapped IPv6 address becomes the IPv4 address (net-io `normalize`).
#[must_use]
pub fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// The proxies whose claims about the client are believed (net-io
/// `TrustedProxies`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TrustedProxies {
    blocks: Vec<Cidr>,
}

impl TrustedProxies {
    /// The most blocks a list holds.
    pub const MAX: usize = 1024;

    /// A list from blocks (sorted and deduplicated).
    ///
    /// # Errors
    ///
    /// `TooMany`.
    pub fn new(blocks: &[Cidr]) -> Result<TrustedProxies, AddrError> {
        if blocks.len() > Self::MAX {
            return Err(AddrError::TooMany { max: Self::MAX });
        }
        let mut blocks = blocks.to_vec();
        blocks.sort_by_key(|b| (b.is_ipv4(), b.network, b.prefix));
        blocks.dedup();
        Ok(TrustedProxies { blocks })
    }

    /// A list from text separated by commas or whitespace.
    ///
    /// # Errors
    ///
    /// As [`Cidr::parse`], with the offset of the entry; `Empty` for no entry.
    pub fn parse(text: &str) -> Result<TrustedProxies, AddrError> {
        let mut blocks = Vec::new();
        let mut offset = 0;
        for part in text.split(|c: char| c == ',' || c.is_whitespace()) {
            if !part.is_empty() {
                let block = Cidr::parse(part).map_err(|e| match e {
                    AddrError::Syntax { at } => AddrError::Syntax { at: at + offset },
                    other => other,
                })?;
                blocks.push(block);
            }
            offset += part.len() + 1;
        }
        if blocks.is_empty() {
            return Err(AddrError::Empty);
        }
        TrustedProxies::new(&blocks)
    }

    /// An empty list: no proxy is trusted.
    #[must_use]
    pub fn none() -> TrustedProxies {
        TrustedProxies { blocks: Vec::new() }
    }

    /// `127.0.0.0/8` and `::1/128`.
    #[must_use]
    pub fn loopback() -> TrustedProxies {
        let v4 = Cidr {
            network: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 0)),
            prefix: 8,
        };
        let v6 = Cidr {
            network: IpAddr::V6(Ipv6Addr::LOCALHOST),
            prefix: 128,
        };
        TrustedProxies {
            blocks: vec![v6, v4],
        }
    }

    /// Whether `ip` is inside any block.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.blocks.iter().any(|b| b.contains(ip))
    }

    /// The number of blocks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The blocks.
    pub fn iter(&self) -> impl Iterator<Item = Cidr> + '_ {
        self.blocks.iter().copied()
    }
}

/// Which net-io driver runs connections (net-io `DriverKind`).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DriverKind {
    /// A bounded pool of threads, one connection per thread.
    Blocking,
    /// One worker per core over epoll, kqueue, or IOCP.
    Reactor,
    /// io_uring (Linux).
    Uring,
    /// The reactor where it is built and available, else the blocking pool.
    #[default]
    Auto,
}

/// The versions of the PROXY protocol a listener accepts (net-io `ProxyVersions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyVersions {
    /// Version 1 (text).
    V1,
    /// Version 2 (binary).
    V2,
    /// Either, chosen by the first byte.
    Both,
}

/// The state a control line carries (net-io `ControlState`).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlState {
    /// Normal operation.
    Normal,
    /// Refuse work in `from_lane` and every less important lane.
    Shed {
        /// The first lane refused.
        from_lane: Lane,
    },
    /// Only critical work.
    Lockdown,
    /// Drain with the line's deadline.
    Draining,
    /// Close everything now.
    Stopping,
}

/// A process-wide control signal (net-io `ControlLine`).
pub trait ControlLine: Send + Sync + 'static {
    /// Changes on every state change; an acquire load.
    fn epoch(&self) -> u64;
    /// The current state; read only when the epoch moved.
    fn state(&self) -> ControlState;
    /// The deadline that goes with `Draining`.
    fn deadline(&self) -> Option<Instant>;
}

/// The line that never changes (net-io `NoControl`).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoControl;

impl ControlLine for NoControl {
    #[inline]
    fn epoch(&self) -> u64 {
        0
    }
    fn state(&self) -> ControlState {
        ControlState::Normal
    }
    fn deadline(&self) -> Option<Instant> {
        None
    }
}

/// A control line set by the owner (net-io `SimpleControl`).
#[derive(Debug, Default)]
pub struct SimpleControl {
    epoch: AtomicU64,
    inner: Mutex<(Option<ControlState>, Option<Instant>)>,
}

impl SimpleControl {
    /// A line in `Normal`.
    #[must_use]
    pub fn new() -> Self {
        SimpleControl::default()
    }

    /// Sets the state and its deadline.
    pub fn set(&self, state: ControlState, deadline: Option<Instant>) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = (Some(state), deadline);
        drop(guard);
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

impl ControlLine for SimpleControl {
    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    fn state(&self) -> ControlState {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.0.unwrap_or(ControlState::Normal)
    }
    fn deadline(&self) -> Option<Instant> {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cidr_parse_valid_and_contains() {
        let c = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(c.contains("10.200.0.1".parse().unwrap()));
        assert!(!c.contains("11.0.0.1".parse().unwrap()));
        assert_eq!(c.to_string(), "10.0.0.0/8");
        assert_eq!(Cidr::parse("::1").unwrap().prefix_len(), 128);
    }

    #[test]
    fn test_cidr_parse_invalid_refused() {
        assert_eq!(Cidr::parse("10.0.0.1/8"), Err(AddrError::HostBitsSet));
        assert_eq!(
            Cidr::parse("10.0.0.0/33"),
            Err(AddrError::PrefixTooLong { max: 32 })
        );
        assert!(Cidr::parse("10.0.0.0/08").is_err());
        assert!(Cidr::parse("fe80::1%eth0").is_err());
        assert_eq!(Cidr::parse(""), Err(AddrError::Empty));
    }

    #[test]
    fn test_trusted_mapped_v6_matches_v4_block() {
        let t = TrustedProxies::parse("10.0.0.0/8").unwrap();
        assert!(t.contains("::ffff:10.0.0.1".parse().unwrap()));
        assert!(TrustedProxies::loopback().contains("::1".parse().unwrap()));
        assert!(TrustedProxies::parse(" , ").is_err());
    }

    #[test]
    fn test_simple_control_epoch_moves_on_set() {
        let c = SimpleControl::new();
        let e = c.epoch();
        c.set(ControlState::Shed { from_lane: Lane(2) }, None);
        assert!(c.epoch() > e);
        assert_eq!(c.state(), ControlState::Shed { from_lane: Lane(2) });
    }
}

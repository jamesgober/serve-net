//! The net-io items serve-net names in its own API (D-037): re-exported, so a
//! program that uses both crates sees one type for each.

pub use net_io::{
    AddrError, Cidr, ConnectionId, ControlLine, ControlState, DriverKind, Lane, NoControl,
    ProxyVersions, SimpleControl, TrustedProxies, WorkerId, normalize,
};

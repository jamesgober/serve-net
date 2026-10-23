//! Configurations decoded from arbitrary bytes. Oracle: `build` never panics,
//! and the same configuration gives the same errors in the same order.

#![no_main]

use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use serve_net::{Budget, Ingress, KeepAlive, Limits, ListenerSpec, Profile, ProfileId, ProxySource, Server, TrustedProxies};

fn configuration(data: &[u8]) -> serve_net::ServerBuilder {
    let mut builder = Server::builder();
    let mut chunks = data.chunks(6);
    for chunk in chunks.by_ref().take(8) {
        let get = |i: usize| chunk.get(i).copied().unwrap_or(0);
        let name = String::from_utf8_lossy(&chunk[..chunk.len().min(3)]).into_owned();
        let addr = format!("{}.{}.{}.{}:{}", get(0), get(1), get(2), get(3), u16::from(get(4)) * 7);
        let mut spec = ListenerSpec::tcp(&name, addr.as_str()).profile(ProfileId(get(5) % 3));
        if get(5) & 0x80 != 0 {
            let trust = format!("{}.0.0.0/{}", get(0), get(1) % 40);
            let trusted = TrustedProxies::parse(&trust).unwrap_or_else(|_| TrustedProxies::none());
            spec = spec.ingress(Ingress::Proxy(ProxySource::forwarded_for(trusted).max_hops(get(2))));
        }
        if get(4) & 1 == 1 {
            spec = spec.max_connections(u32::from(get(3)));
        }
        builder = builder.listener(spec);
    }
    for chunk in chunks.take(4) {
        let get = |i: usize| chunk.get(i).copied().unwrap_or(0);
        let limits = Limits::new()
            .head_timeout(Duration::from_millis(u64::from(get(0)) * 50))
            .request_timeout(Duration::from_millis(u64::from(get(1)) * 100))
            .max_pipelined_per_wake(u16::from(get(2)));
        let profile = Profile::new(ProfileId(get(3) % 4))
            .limits(limits)
            .keep_alive(KeepAlive::new().max_requests(u32::from(get(4))))
            .budget(Budget::new().max_in_flight(u32::from(get(5))));
        builder = builder.profile(profile);
    }
    builder
}

fuzz_target!(|data: &[u8]| {
    let first = configuration(data).build().err().map(|e| format!("{e}"));
    let second = configuration(data).build().err().map(|e| format!("{e}"));
    assert_eq!(first, second, "the same configuration gave different errors");
});

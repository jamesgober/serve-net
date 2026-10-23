//! The synchronization primitives serve-net's shared state uses: `std`'s in
//! normal builds, loom's under `cfg(loom)` so the models in `loom_tests` can
//! explore every interleaving.

#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicU32, AtomicU64};
#[cfg(loom)]
pub(crate) use loom::sync::{Mutex, MutexGuard};
#[cfg(not(loom))]
pub(crate) use std::sync::atomic::{AtomicU32, AtomicU64};
#[cfg(not(loom))]
pub(crate) use std::sync::{Mutex, MutexGuard};

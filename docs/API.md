# serve-net &mdash; API Reference

> Complete reference for every public item in `serve-net`, with examples.
> **Status: scaffold (`0.1.0`).** The public API is written here at `0.2.0` (design lock); see [`../dev/ROADMAP.md`](../dev/ROADMAP.md) and the planned shape in [`../dev/PLAN.md`](../dev/PLAN.md).

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>

## Table of contents

- [`VERSION`](#version)

## `VERSION`

```rust
pub const VERSION: &str;
```

The crate version, populated by Cargo at build time.

```rust
println!("serve-net {}", serve_net::VERSION);
```

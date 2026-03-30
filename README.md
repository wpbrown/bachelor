# bachelor

**bachelor** provides single-threaded async primitives (`!Send`, `!Sync`) for
thread-per-core (TPC) architectures.

## Why bachelor?

There are lots of great thread-safe async synchronization primitives available
in the Rust ecosystem. Thread safety comes at a cost — atomics, `Arc`,
cross-thread waker coordination — that we don't need to pay when running on a
single-threaded executor. `bachelor` provides `Rc`/`Cell`-based alternatives
that eliminate this overhead entirely.

Each primitive has a **two-level API**:

- **Core types** (e.g. `SpscChannel<T>`, `MpscLatchedSignal`) are plain structs
  with no `Rc` wrapping. They can be embedded directly inside your own shared
  types, letting multiple primitives share a single allocation. They require the
  caller to uphold some invariants.
- **Split-handle types** (e.g. `SpscChannelProducer<T>` /
  `SpscChannelConsumer<T>`) wrap a core type in `Rc` and use `&mut self` on
  async methods to enforce the single-waker contract at compile time. They are
  the convenient, hard-to-misuse entry point for most use cases.

## Primitives

| Core Type | Module | Topology | Description |
|-----------|--------|----------|-------------|
| `SpscChannel<T>` | `channel` | SPSC | Bounded async channel for single-producer, single-consumer communication. |
| `MpscChannel<T>` | `channel` | MPSC | Bounded async channel supporting multiple cloneable producers and a single consumer. |
| `SpmcBroadcast<T>` | `broadcast` | SPMC | Bounded broadcast channel where a single producer fans out each item to all subscribed consumers. Supports a visitor-based receive API to avoid cloning. |
| `MpscLatchedSignal` | `signal` | MPSC | Latched notification signal. Multiple producers can notify a single consumer. Signal persists indefinitely. |
| `MpscFiniteLatchedSignal` | `signal` | MPSC | Latched notification signal with finite lifetime. Auto-closes when all producers drop. |
| `MpmcLatchedSignal` | `signal` | MPMC | Latched notification signal with generation tracking for multiple consumers. Signal persists indefinitely. |
| `MpmcFiniteLatchedSignal` | `signal` | MPMC | Latched notification signal with generation tracking and finite lifetime. Auto-closes when all producers/sources drop. |
| `MpscWatchRef<T>` | `watch` | MPSC | Shared mutable value with change notification for a single consumer. |
| `MpmcWatchRef<T>` | `watch` | MPMC | Shared mutable value with per-consumer generation tracking and change notification. |

## Status

This crate is under active development.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.


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

| Core Type | Description |
|-----------|-------------|
| `SpscChannel<T>` | Bounded SPSC async channel. |
| `MpscChannel<T>` | Bounded MPSC async channel with multiple cloneable producers. |
| `SpmcBroadcast<T>` | Bounded SPMC broadcast channel that fans out each item to all subscribers. Supports visitor-based receive to avoid cloning. |
| `MpscLatchedSignal` | MPSC latched notification signal. Persists indefinitely. |
| `MpscFiniteLatchedSignal` | MPSC latched notification signal with finite lifetime. Auto-closes when all producers drop. |
| `MpmcLatchedSignal` | MPMC latched notification signal with per-consumer generation tracking. Persists indefinitely. |
| `MpmcFiniteLatchedSignal` | MPMC latched notification signal with generation tracking and finite lifetime. Auto-closes when all producers/sources drop. |
| `MpscWatchRef<T>` | MPSC shared mutable value with change notification. |
| `MpmcWatchRef<T>` | MPMC shared mutable value with per-consumer generation tracking and change notification. |

## Status

This crate is under active development.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.


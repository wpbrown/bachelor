//! # bachelor
//!
//! Pure single-threaded utilities optimized for thread-per-core executors.
//!
//! This crate is currently under active development.
//!
//! # Core types and split handles
//!
//! Each communication primitive in this crate is offered at two levels:
//!
//! **Core types** ([`SpscChannel`](channel::SpscChannel),
//! [`MpscChannel`](channel::MpscChannel),
//! [`SpmcBroadcast`](broadcast::SpmcBroadcast),
//! [`MpscLatchedSignal`](signal::MpscLatchedSignal), etc.) are plain
//! structs with no internal reference counting. Both the producer and
//! consumer sides operate on the same value through shared references.
//! You decide how to store and share the type: embed it in a larger
//! struct, place it in an `Rc`, or keep it on the stack. This is useful
//! when many primitives live in one shared allocation and the overhead
//! of a separate `Rc` per primitive is undesirable.
//!
//! **Split handles** ([`SpscChannelProducer`](channel::SpscChannelProducer) /
//! [`SpscChannelConsumer`](channel::SpscChannelConsumer),
//! [`SpmcBroadcastProducer`](broadcast::SpmcBroadcastProducer) /
//! [`SpmcBroadcastConsumer`](broadcast::SpmcBroadcastConsumer), etc.)
//! wrap a core type in an `Rc` and give each side its own handle.
//! Factory functions ([`channel::spsc_channel`], [`broadcast::spmc_broadcast`],
//! [`signal::mpsc_latched_signal`], [`watch::mpsc_watch`], etc.) return
//! these handle pairs. Split handles enforce usage rules at compile time
//! (see below) and manage lifetime automatically: dropping the last
//! producer closes the channel.
//!
//! Choose core types when you need control over allocation. Choose split
//! handles when convenience and compile-time correctness matter more.
//!
//! # Optional `stream` and `sink` support
//!
//! Enable the `stream` cargo feature for `futures-core` `Stream` integration:
//!
//! - [`SpscChannelConsumer`](`channel::SpscChannelConsumer`) implements
//!   `Stream<Item = T>`.
//! - [`MpscChannelConsumer`](`channel::MpscChannelConsumer`) implements
//!   `Stream<Item = T>`.
//! - [`SpmcBroadcastConsumer`](`broadcast::SpmcBroadcastConsumer`) implements
//!   `Stream<Item = T>` only when `T: Clone`.
//!
//! These streams yield buffered items first and then return `None` once the
//! primitive is closed and drained. The matching core polling APIs are
//! [`SpscChannel::poll_recv`](`channel::SpscChannel::poll_recv`) and
//! [`MpscChannel::poll_recv`](`channel::MpscChannel::poll_recv`). Broadcast
//! keeps its existing raw-key receive APIs; for non-clone payloads, use the
//! visitor-based `recv_ref` path instead of `Stream`.
//!
//! Enable the `sink` cargo feature for `futures-sink` `Sink` integration with
//! [`Closed`](`error::Closed`) as the error type:
//!
//! - [`SpscChannelProducer`](`channel::SpscChannelProducer`) implements `Sink<T>`.
//! - [`SpmcBroadcastProducer`](`broadcast::SpmcBroadcastProducer`) implements
//!   `Sink<T>`.
//! - MPSC uses [`MpscChannelProducer::into_sink`](`channel::MpscChannelProducer::into_sink`)
//!   to create an [`MpscChannelSink`](`channel::MpscChannelSink`), which then
//!   implements `Sink<T>`.
//!
//! The matching core send-readiness APIs are
//! [`SpscChannel::poll_ready_send`](`channel::SpscChannel::poll_ready_send`) and
//! [`SpmcBroadcast::poll_ready_send`](`broadcast::SpmcBroadcast::poll_ready_send`).
//! MPSC keeps sink waiter state in the split sink adaptor rather than on the
//! core type. For SPSC and SPMC, `poll_flush` is immediate because there is
//! no producer-side buffering beyond the channel queue itself. MPSC's
//! [`MpscChannelSink`](`channel::MpscChannelSink`) holds a one-item buffer to
//! avoid races between `poll_ready` and `start_send`, so its `poll_flush` may
//! return `Pending` while draining that buffer.
//!
//! # Single-waker contract
//!
//! Some async methods in this crate register a single waker for their
//! endpoint. If two futures targeting the same waker slot exist at the
//! same time, the second overwrites the first's waker and the first
//! will never be woken. This is a correctness hazard, not a
//! memory-safety issue.
//!
//! Each future clears its waker slot when dropped. Callers must
//! therefore ensure a previous future is dropped before creating a new
//! one for the same slot.
//!
//! Which methods are affected depends on the primitive. Each module's
//! documentation lists the specific methods that carry this constraint.
//!
//! Split handles prevent this at compile time: their async methods
//! take `&mut self`, so the borrow checker rejects a second call while
//! the first future is still alive.
//!
//! Core types take `&self` and cannot enforce this restriction. If you
//! use them directly, you must ensure that at most one async future is
//! active per waker slot. Each core-type method that carries this
//! requirement says so in its documentation.
//!
//! # User-managed consumer state
//!
//! Some low-level APIs expose caller-managed consumer state instead of
//! owned consumer handles.
//!
//! There are two patterns:
//!
//! - Raw subscription identity keys, such as broadcast consumer keys.
//!   These identify a live subscription inside the primitive. They are
//!   only valid while that subscription is still live. Reusing a stale
//!   key after unsubscribe is a caller bug and may panic.
//! - Consumer cursor keys, such as the MPMC signal and watch keys.
//!   These store per-consumer observation state in the key value itself
//!   and are passed back into `observe`. They do not require an explicit
//!   unsubscribe step, but they should still be treated as state owned by
//!   one logical consumer and used with the primitive that created them.
//!
//! Use these low-level APIs only when you need to store or manage
//! consumer state yourself. If you do not need that control, prefer the
//! split handle APIs, which manage consumer lifetime automatically and
//! avoid these manual contracts.

pub mod broadcast;
pub mod channel;
pub mod error;
pub mod signal;
pub mod watch;

mod core;

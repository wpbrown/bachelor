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
//! # Single-waker contract
//!
//! Some async methods in this crate register a single waker for their
//! endpoint. If two futures targeting the same waker slot exist at the
//! same time, the second overwrites the first's waker and the first
//! will never be woken. This is a correctness hazard, not a
//! memory-safety issue.
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

pub mod broadcast;
pub mod channel;
pub mod error;
pub mod signal;
pub mod watch;

mod core;

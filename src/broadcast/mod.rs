//! Bounded async broadcast channels.
//!
//! See the crate-level documentation on [core types vs split
//! handles](crate#core-types-and-split-handles).
//!
//! [`SpmcBroadcast`] has a single producer and multiple consumers.
//! The [single-waker contract](crate#single-waker-contract) applies to
//! `send` and [`SpmcBroadcast::poll_ready_send`] on the producer side and to
//! `recv` / `recv_ref` on each individual consumer. Enable the `stream` feature to get a
//! `futures-core` `Stream` implementation on [`SpmcBroadcastConsumer`]
//! when `T: Clone`, and enable the `sink` feature to get a `futures-sink`
//! `Sink` implementation on [`SpmcBroadcastProducer`]. Non-clone payloads
//! continue to use the visitor-based `recv_ref` APIs.

pub mod spmc;

pub use spmc::{
    SpmcBroadcast, SpmcBroadcastConsumer, SpmcBroadcastConsumerRef, SpmcBroadcastProducer,
    SpmcBroadcastSource, broadcast as spmc_broadcast,
};

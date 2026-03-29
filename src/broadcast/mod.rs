//! Bounded async broadcast channels.
//!
//! See the crate-level documentation on [core types vs split
//! handles](crate#core-types-and-split-handles).
//!
//! [`SpmcBroadcast`] has a single producer and multiple consumers.
//! The [single-waker contract](crate#single-waker-contract) applies to
//! `send` on the producer side and to `recv` / `recv_ref` on each
//! individual consumer.

pub mod spmc;

pub use spmc::{
    SpmcBroadcast, SpmcBroadcastConsumer, SpmcBroadcastConsumerRef, SpmcBroadcastProducer,
    SpmcBroadcastSource, broadcast as spmc_broadcast,
};

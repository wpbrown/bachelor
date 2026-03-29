//! Bounded async channels.
//!
//! See the crate-level documentation on [core types vs split
//! handles](crate#core-types-and-split-handles).
//!
//! ## SPSC
//!
//! [`SpscChannel`] has a single producer and a single consumer, so the
//! [single-waker contract](crate#single-waker-contract) applies to both
//! `send` and `recv`.
//!
//! ## MPSC
//!
//! [`MpscChannel`] has multiple producers (each `send` allocates its own
//! waker slot) and a single consumer. The [single-waker
//! contract](crate#single-waker-contract) applies to `recv` only.

pub mod mpsc;
pub mod spsc;

pub use mpsc::{MpscChannel, MpscChannelConsumer, MpscChannelProducer, channel as mpsc_channel};
pub use spsc::{SpscChannel, SpscChannelConsumer, SpscChannelProducer, channel as spsc_channel};

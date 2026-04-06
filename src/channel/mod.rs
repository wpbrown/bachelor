//! Bounded async channels.
//!
//! See the crate-level documentation on [core types vs split
//! handles](crate#core-types-and-split-handles).
//!
//! ## SPSC
//!
//! [`SpscChannel`] has a single producer and a single consumer, so the
//! [single-waker contract](crate#single-waker-contract) applies to both
//! `send`, `recv`, [`SpscChannel::poll_ready_send`], and
//! [`SpscChannel::poll_recv`]. Enable the `stream` feature to get a
//! `futures-core` `Stream` implementation on [`SpscChannelConsumer`],
//! and enable the `sink` feature to get a `futures-sink` `Sink`
//! implementation on [`SpscChannelProducer`]. The core type keeps the
//! lower-level polling APIs.
//!
//! ## MPSC
//!
//! [`MpscChannel`] has multiple producers (each `send` allocates its own
//! waker slot) and a single consumer. The [single-waker
//! contract](crate#single-waker-contract) applies to `recv` and
//! [`MpscChannel::poll_recv`] only. Enable the `stream` feature to get a
//! `futures-core` `Stream` implementation on [`MpscChannelConsumer`],
//! and enable the `sink` feature to get a `futures-sink` `Sink`
//! implementation on [`MpscChannelSink`]. Create that adaptor with
//! [`MpscChannelProducer::into_sink`]. Sink support stays off the core type
//! because producer waiter state is owned per producer-like handle.

pub mod mpsc;
pub mod spsc;

#[cfg(feature = "sink")]
pub use mpsc::MpscChannelSink;
pub use mpsc::{MpscChannel, MpscChannelConsumer, MpscChannelProducer, channel as mpsc_channel};
pub use spsc::{SpscChannel, SpscChannelConsumer, SpscChannelProducer, channel as spsc_channel};

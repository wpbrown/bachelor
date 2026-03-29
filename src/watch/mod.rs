//! Watch channels for async value observation.
//!
//! See the crate-level documentation on [core types vs split
//! handles](crate#core-types-and-split-handles).
//!
//! [`MpscWatchRef`] uses a single-consumer signal, so the [single-waker
//! contract](crate#single-waker-contract) applies to `observe` /
//! `changed`.
//!
//! [`MpmcWatchRef`] uses a multi-consumer signal. Its `observe` method
//! takes the consumer key by `&mut`, preventing two futures from sharing
//! the same key at compile time.

pub mod mpmc;
pub mod mpsc;

pub use mpmc::{
    MpmcWatchRef, MpmcWatchRefConsumer, MpmcWatchRefProducer, MpmcWatchRefSource,
    watch as mpmc_watch,
};
pub use mpsc::{MpscWatchRef, MpscWatchRefConsumer, MpscWatchRefProducer, watch as mpsc_watch};

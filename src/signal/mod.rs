//! Latched signals for async notification.
//!
//! See the crate-level documentation on [core types vs split
//! handles](crate#core-types-and-split-handles).
//!
//! The single-consumer signals ([`MpscLatchedSignal`] and
//! [`MpscFiniteLatchedSignal`]) store one waker, so the [single-waker
//! contract](crate#single-waker-contract) applies to `observe`.
//!
//! The multi-consumer signals ([`MpmcLatchedSignal`] and
//! [`MpmcFiniteLatchedSignal`]) allocate a per-future waker slot, but
//! each consumer key tracks its own generation. Their `observe` methods
//! take the key by `&mut`, preventing two futures from sharing the same
//! key at compile time.

pub mod mpmc_finite_latched;
pub mod mpmc_latched;
pub mod mpsc_finite_latched;
pub mod mpsc_latched;

pub use mpmc_finite_latched::{
    MpmcFiniteLatchedSignal, MpmcFiniteLatchedSignalConsumer, MpmcFiniteLatchedSignalProducer,
    MpmcFiniteLatchedSignalSource, Wait as MpmcFiniteLatchedWait,
    signal as mpmc_finite_latched_signal,
};
pub use mpmc_latched::{
    MpmcLatchedSignal, MpmcLatchedSignalConsumer, MpmcLatchedSignalProducer,
    MpmcLatchedSignalSource, Wait as MpmcLatchedWait, signal as mpmc_latched_signal,
};
pub use mpsc_finite_latched::{
    MpscFiniteLatchedSignal, MpscFiniteLatchedSignalConsumer, MpscFiniteLatchedSignalProducer,
    Wait as MpscFiniteLatchedWait, signal as mpsc_finite_latched_signal,
};
pub use mpsc_latched::{
    MpscLatchedSignal, MpscLatchedSignalConsumer, MpscLatchedSignalProducer,
    Wait as MpscLatchedWait, signal as mpsc_latched_signal,
};

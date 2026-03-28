pub mod mpmc;
pub mod mpsc;

pub use mpmc::{
    MpmcWatchRef, MpmcWatchRefConsumer, MpmcWatchRefProducer, MpmcWatchRefSource,
    watch as mpmc_watch,
};
pub use mpsc::{MpscWatchRef, MpscWatchRefConsumer, MpscWatchRefProducer, watch as mpsc_watch};

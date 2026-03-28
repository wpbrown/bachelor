pub mod bounded_queue;
pub mod broadcast_queue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvEffect {
    Unblocked,
    NoChange,
}

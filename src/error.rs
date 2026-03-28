use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

impl fmt::Display for Closed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("closed")
    }
}

impl std::error::Error for Closed {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrySendError<T> {
    Full(T),
    Closed(T),
}

impl<T> TrySendError<T> {
    pub fn into_inner(self) -> T {
        match self {
            Self::Full(item) | Self::Closed(item) => item,
        }
    }
}

impl<T> fmt::Display for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(_) => f.write_str("full"),
            Self::Closed(_) => f.write_str("closed"),
        }
    }
}

impl<T: fmt::Debug> std::error::Error for TrySendError<T> {}

#[derive(Debug)]
pub enum TryRecvRefError<F> {
    Empty(F),
    Closed,
}

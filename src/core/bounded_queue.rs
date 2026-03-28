use std::collections::VecDeque;
use std::num::NonZeroUsize;

use super::RecvEffect;

pub struct BoundedQueue<T> {
    buffer: VecDeque<T>,
    capacity: NonZeroUsize,
}

impl<T> BoundedQueue<T> {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity.get()),
            capacity,
        }
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.buffer.len() >= self.capacity.get()
    }

    #[cfg(test)]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    #[cfg(test)]
    #[inline]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn try_send(&mut self, item: T) -> Result<(), T> {
        if self.is_full() {
            return Err(item);
        }
        self.buffer.push_back(item);
        Ok(())
    }

    pub fn try_recv(&mut self) -> Option<(T, RecvEffect)> {
        let was_full = self.is_full();
        let item = self.buffer.pop_front()?;
        let effect = if was_full {
            RecvEffect::Unblocked
        } else {
            RecvEffect::NoChange
        };
        Some((item, effect))
    }

    pub fn shrink_to_fit(&mut self) {
        self.buffer.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    #[test]
    fn send_recv_roundtrip() {
        let mut q = BoundedQueue::new(nz(4));

        assert_eq!(q.try_send(1), Ok(()));
        assert_eq!(q.try_send(2), Ok(()));

        assert_eq!(q.try_recv(), Some((1, RecvEffect::NoChange)));
        assert_eq!(q.try_recv(), Some((2, RecvEffect::NoChange)));
        assert_eq!(q.try_recv(), None);
    }

    #[test]
    fn full_rejects_send() {
        let mut q = BoundedQueue::new(nz(2));

        assert_eq!(q.try_send(1), Ok(()));
        assert_eq!(q.try_send(2), Ok(()));
        assert_eq!(q.try_send(3), Err(3));
    }

    #[test]
    fn recv_frees_space() {
        let mut q = BoundedQueue::new(nz(2));

        assert_eq!(q.try_send(1), Ok(()));
        assert_eq!(q.try_send(2), Ok(()));
        assert!(q.is_full());

        assert_eq!(q.try_recv(), Some((1, RecvEffect::Unblocked)));
        assert!(!q.is_full());
        assert_eq!(q.try_send(3), Ok(()));
    }

    #[test]
    fn capacity_one() {
        let mut q = BoundedQueue::new(nz(1));

        assert_eq!(q.try_send(42), Ok(()));
        assert_eq!(q.try_send(43), Err(43));
        assert_eq!(q.try_recv(), Some((42, RecvEffect::Unblocked)));
        assert_eq!(q.try_send(43), Ok(()));
        assert_eq!(q.try_recv(), Some((43, RecvEffect::Unblocked)));
    }

    #[test]
    fn len_is_empty_is_full() {
        let mut q = BoundedQueue::new(nz(2));

        assert!(q.is_empty());
        assert!(!q.is_full());
        assert_eq!(q.len(), 0);

        q.try_send(1).unwrap();
        assert!(!q.is_empty());
        assert!(!q.is_full());
        assert_eq!(q.len(), 1);

        q.try_send(2).unwrap();
        assert!(!q.is_empty());
        assert!(q.is_full());
        assert_eq!(q.len(), 2);

        q.try_recv().unwrap();
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn shrink_is_behaviorally_transparent() {
        let mut q = BoundedQueue::new(nz(8));

        for i in 0..64 {
            assert_eq!(q.try_send(i), Ok(()));
            assert_eq!(q.try_recv().map(|(v, _)| v), Some(i));
        }

        q.shrink_to_fit();

        assert_eq!(q.try_send(999), Ok(()));
        assert_eq!(q.try_recv().map(|(v, _)| v), Some(999));
    }
}

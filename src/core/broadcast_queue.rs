use std::collections::VecDeque;
use std::num::NonZeroUsize;

use super::RecvEffect;
use slab::Slab;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConsumerKey(usize);

pub struct BroadcastQueue<T, C = ()> {
    buffer: VecDeque<T>,
    capacity: NonZeroUsize,
    tail: u64,
    cached_min_cursor: u64,
    consumers: Slab<ConsumerSlot<C>>,
}

#[derive(Debug)]
struct ConsumerSlot<C> {
    cursor: u64,
    context: C,
}

impl<T, C> BroadcastQueue<T, C> {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity.get()),
            capacity,
            tail: 0,
            cached_min_cursor: 0,
            consumers: Slab::new(),
        }
    }

    #[inline]
    fn head(&self) -> u64 {
        self.tail - self.buffer.len() as u64
    }

    fn compute_min_cursor(&self) -> u64 {
        self.consumers
            .iter()
            .map(|(_, slot)| slot.cursor)
            .min()
            .unwrap_or(self.tail)
    }

    fn gc(&mut self) {
        let head = self.head();
        let n = self.cached_min_cursor.saturating_sub(head) as usize;
        drop(self.buffer.drain(..n));
    }

    pub fn subscribe_with_context(&mut self, context: C) -> ConsumerKey {
        let cursor = self.tail;
        ConsumerKey(self.consumers.insert(ConsumerSlot { cursor, context }))
    }

    pub fn unsubscribe(&mut self, id: ConsumerKey) -> RecvEffect {
        debug_assert_eq!(
            self.cached_min_cursor,
            self.compute_min_cursor(),
            "cached_min_cursor != true minimum on entry to unsubscribe"
        );

        let slot = self.consumers.remove(id.0);
        let cursor = slot.cursor;

        let was_full = self.buffer.len() == self.capacity.get();

        if cursor == self.cached_min_cursor {
            self.cached_min_cursor = self.compute_min_cursor();
            self.gc();
        }

        if was_full && self.buffer.len() < self.capacity.get() {
            RecvEffect::Unblocked
        } else {
            RecvEffect::NoChange
        }
    }

    pub fn consumer_context_mut(&mut self, id: ConsumerKey) -> &mut C {
        &mut self.consumers[id.0].context
    }

    pub fn try_send(&mut self, item: T) -> Result<(), T> {
        if self.consumers.is_empty() {
            self.cached_min_cursor = self.tail;
            self.gc();
            drop(item);
            return Ok(());
        }

        self.gc();

        if self.buffer.len() >= self.capacity.get() {
            return Err(item);
        }

        self.buffer.push_back(item);
        self.tail = self
            .tail
            .checked_add(1)
            .expect("bounded queue tail sequence overflowed u64");

        Ok(())
    }

    pub(crate) fn for_each_consumer_mut(&mut self, mut visitor: impl FnMut(&mut C)) {
        for (_, slot) in self.consumers.iter_mut() {
            visitor(&mut slot.context);
        }
    }

    pub fn shrink_buffer_to_fit(&mut self) {
        self.gc();
        self.buffer.shrink_to_fit();
    }

    pub fn shrink_consumers_to_fit(&mut self) {
        self.consumers.shrink_to_fit();
    }
}

impl<T, C> BroadcastQueue<T, C> {
    pub fn try_recv_ref<F, R>(&mut self, id: ConsumerKey, visitor: F) -> Result<(R, RecvEffect), F>
    where
        F: FnOnce(&T) -> R,
    {
        debug_assert_eq!(
            self.cached_min_cursor,
            self.compute_min_cursor(),
            "cached_min_cursor != true minimum on entry to try_recv_ref"
        );

        let cursor = self.consumers[id.0].cursor;

        if cursor >= self.tail {
            return Err(visitor);
        }

        let head = self.head();
        debug_assert!(
            cursor >= head,
            "cursor ({cursor}) behind head ({head}) — invariant violated"
        );

        let result = visitor(&self.buffer[(cursor - head) as usize]);

        self.consumers[id.0].cursor = cursor
            .checked_add(1)
            .expect("bounded queue consumer cursor overflowed u64");

        let was_full = self.buffer.len() == self.capacity.get();

        if cursor == self.cached_min_cursor {
            self.cached_min_cursor = self.compute_min_cursor();
            self.gc();
        }

        let gc = if was_full && self.buffer.len() < self.capacity.get() {
            RecvEffect::Unblocked
        } else {
            RecvEffect::NoChange
        };

        Ok((result, gc))
    }
}

impl<T: Clone, C> BroadcastQueue<T, C> {
    pub fn try_recv(&mut self, id: ConsumerKey) -> Option<(T, RecvEffect)> {
        self.try_recv_ref(id, T::clone).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    fn new_queue(cap: usize) -> BroadcastQueue<i32> {
        BroadcastQueue::new(nz(cap))
    }

    fn sub<T>(q: &mut BroadcastQueue<T>) -> ConsumerKey {
        q.subscribe_with_context(())
    }

    #[test]
    fn send_recv_single_consumer() {
        let mut q = new_queue(4);
        let c = sub(&mut q);

        assert_eq!(q.try_send(1), Ok(()));
        assert_eq!(q.try_send(2), Ok(()));

        assert_eq!(q.try_recv(c), Some((1, RecvEffect::NoChange)));
        assert_eq!(q.try_recv(c), Some((2, RecvEffect::NoChange)));
        assert_eq!(q.try_recv(c), None);
    }

    #[test]
    fn lagged_consumer_blocks_producer() {
        let mut q = new_queue(2);
        let c = sub(&mut q);

        assert!(q.try_send(1).is_ok());
        assert!(q.try_send(2).is_ok());
        assert_eq!(q.try_send(3), Err(3));

        let (val, gc) = q.try_recv(c).unwrap();
        assert_eq!(val, 1);
        assert_eq!(gc, RecvEffect::Unblocked);

        assert!(q.try_send(3).is_ok());
    }

    #[test]
    fn drop_slow_consumer_unblocks() {
        let mut q = new_queue(2);
        let slow = sub(&mut q);
        let fast = sub(&mut q);

        assert!(q.try_send(10).is_ok());
        assert!(q.try_send(20).is_ok());

        q.try_recv(fast).unwrap();
        q.try_recv(fast).unwrap();

        assert_eq!(q.try_send(30), Err(30));

        assert_eq!(q.unsubscribe(slow), RecvEffect::Unblocked);
        assert!(q.try_send(30).is_ok());
    }

    #[test]
    fn new_subscriber_only_sees_new_items() {
        let mut q = new_queue(4);
        q.try_send(1).unwrap();
        q.try_send(2).unwrap();

        let c = sub(&mut q);

        q.try_send(3).unwrap();

        assert_eq!(q.try_recv(c), Some((3, RecvEffect::NoChange)));
        assert_eq!(q.try_recv(c), None);
    }

    #[test]
    fn no_consumers_sends_succeed() {
        let mut q = new_queue(2);

        for i in 0..100 {
            assert_eq!(q.try_send(i), Ok(()));
        }
    }

    #[test]
    fn multiple_consumers_different_positions() {
        let mut q = new_queue(4);
        let a = sub(&mut q);
        let b = sub(&mut q);

        for i in 0..4 {
            q.try_send(i).unwrap();
        }

        for i in 0..4 {
            let (val, _) = q.try_recv(a).unwrap();
            assert_eq!(val, i);
        }

        assert_eq!(q.try_send(99), Err(99));

        assert_eq!(q.try_recv(b).unwrap().0, 0);
        assert_eq!(q.try_recv(b).unwrap().0, 1);

        assert!(q.try_send(99).is_ok());
    }

    #[test]
    fn consumer_context_roundtrip() {
        let mut q: BroadcastQueue<i32, String> = BroadcastQueue::new(nz(4));
        let c = q.subscribe_with_context("alpha".to_owned());

        q.consumer_context_mut(c).push_str("-beta");
        assert_eq!(q.consumer_context_mut(c).as_str(), "alpha-beta");
    }

    #[test]
    fn subscribe_reuses_vacant_slots() {
        let mut q = new_queue(4);
        let a = sub(&mut q);
        let _b = sub(&mut q);

        q.unsubscribe(a);

        let c = sub(&mut q);

        assert_eq!(c.0, 0);
    }

    #[test]
    fn unsubscribe_all_then_resubscribe() {
        let mut q = new_queue(2);
        let a = sub(&mut q);

        q.try_send(1).unwrap();
        q.try_send(2).unwrap();

        q.unsubscribe(a);

        assert!(q.try_send(3).is_ok());

        let b = sub(&mut q);
        q.try_send(4).unwrap();

        assert_eq!(q.try_recv(b), Some((4, RecvEffect::NoChange)));
        assert_eq!(q.try_recv(b), None);
    }

    #[test]
    #[should_panic(expected = "invalid key")]
    fn double_unsubscribe_panics() {
        let mut q = new_queue(4);
        let c = sub(&mut q);
        q.unsubscribe(c);
        q.unsubscribe(c);
    }

    #[test]
    #[should_panic(expected = "invalid key")]
    fn recv_after_unsubscribe_panics() {
        let mut q = new_queue(4);
        let c = sub(&mut q);
        q.unsubscribe(c);
        q.try_recv(c);
    }

    #[test]
    fn capacity_one() {
        let mut q = new_queue(1);
        let c = sub(&mut q);

        assert!(q.try_send(42).is_ok());
        assert_eq!(q.try_send(43), Err(43));

        assert_eq!(q.try_recv(c), Some((42, RecvEffect::Unblocked)));
        assert!(q.try_send(43).is_ok());
    }

    #[test]
    fn three_consumers_staggered_reads() {
        let mut q = new_queue(3);
        let a = sub(&mut q);
        let b = sub(&mut q);
        let c = sub(&mut q);

        q.try_send(0).unwrap();
        q.try_send(1).unwrap();
        q.try_send(2).unwrap();

        for _ in 0..3 {
            q.try_recv(a).unwrap();
        }
        q.try_recv(b).unwrap();

        assert_eq!(q.try_send(3), Err(3));

        assert_eq!(q.unsubscribe(c), RecvEffect::Unblocked);

        assert!(q.try_send(3).is_ok());
    }

    #[test]
    fn shrink_is_behaviorally_transparent_after_churn() {
        let mut q = new_queue(8);

        for round in 0..64 {
            let a = sub(&mut q);
            let b = sub(&mut q);

            assert!(q.try_send(round).is_ok());
            assert_eq!(q.try_recv(a).map(|(v, _)| v), Some(round));
            assert_eq!(q.try_recv(b).map(|(v, _)| v), Some(round));

            assert_eq!(q.unsubscribe(a), RecvEffect::NoChange);
            assert_eq!(q.unsubscribe(b), RecvEffect::NoChange);
        }

        q.shrink_buffer_to_fit();
        q.shrink_consumers_to_fit();

        let c = sub(&mut q);
        assert!(q.try_send(999).is_ok());
        assert_eq!(q.try_recv(c), Some((999, RecvEffect::NoChange)));
        assert_eq!(q.try_recv(c), None);
    }

    struct NoCopy(Vec<u8>);

    #[test]
    fn try_recv_ref_single_consumer() {
        let mut q: BroadcastQueue<NoCopy> = BroadcastQueue::new(nz(4));
        let c = sub(&mut q);

        assert!(q.try_send(NoCopy(vec![1, 2, 3])).is_ok());
        assert!(q.try_send(NoCopy(vec![4, 5])).is_ok());

        let (val, gc) = q.try_recv_ref(c, |item| item.0.clone()).ok().unwrap();
        assert_eq!(val, vec![1, 2, 3]);
        assert_eq!(gc, RecvEffect::NoChange);

        let (val, _) = q.try_recv_ref(c, |item| item.0.len()).ok().unwrap();
        assert_eq!(val, 2);

        assert!(q.try_recv_ref(c, |_| ()).is_err());
    }

    #[test]
    fn try_recv_ref_returns_none_when_caught_up() {
        let mut q: BroadcastQueue<NoCopy> = BroadcastQueue::new(nz(4));
        let c = sub(&mut q);

        let mut called = false;
        let result = q.try_recv_ref(c, |_| {
            called = true;
        });
        assert!(result.is_err());
        assert!(!called);
    }

    #[test]
    fn try_recv_ref_gc_unblocks() {
        let mut q: BroadcastQueue<NoCopy> = BroadcastQueue::new(nz(2));
        let c = sub(&mut q);

        assert!(q.try_send(NoCopy(vec![10])).is_ok());
        assert!(q.try_send(NoCopy(vec![20])).is_ok());
        assert!(q.try_send(NoCopy(vec![30])).is_err());

        let (val, gc) = q.try_recv_ref(c, |item| item.0[0]).ok().unwrap();
        assert_eq!(val, 10);
        assert_eq!(gc, RecvEffect::Unblocked);

        assert!(q.try_send(NoCopy(vec![30])).is_ok());
    }

    #[test]
    fn try_recv_ref_multiple_consumers() {
        let mut q: BroadcastQueue<NoCopy> = BroadcastQueue::new(nz(4));
        let a = sub(&mut q);
        let b = sub(&mut q);

        assert!(q.try_send(NoCopy(vec![1])).is_ok());

        let (va, _) = q.try_recv_ref(a, |item| item.0.clone()).ok().unwrap();
        let (vb, _) = q.try_recv_ref(b, |item| item.0.clone()).ok().unwrap();
        assert_eq!(va, vec![1]);
        assert_eq!(vb, vec![1]);
    }
}

use crate::core::RecvEffect;
use crate::core::broadcast_queue::BroadcastQueue;
use crate::core::broadcast_queue::ConsumerKey;
use crate::error::{Closed, TryRecvRefError, TrySendError};
use smallvec::SmallVec;
use std::future::poll_fn;
use std::task::Poll;
use std::task::Waker;
use std::{cell::Cell, cell::RefCell, num::NonZeroUsize, rc::Rc};

/// Opaque raw consumer key returned by [`SpmcBroadcast::subscribe_raw`].
///
/// See the crate-level documentation on the [raw consumer key
/// contract](crate#user-managed-consumer-state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpmcBroadcastConsumerKey(ConsumerKey);

pub struct SpmcBroadcast<T> {
    queue: RefCell<BroadcastQueue<T, Option<Waker>>>,
    producer_waker: Cell<Option<Waker>>,
    closed: Cell<bool>,
}

pub struct SpmcBroadcastConsumerRef<'a, T> {
    channel: &'a SpmcBroadcast<T>,
    id: SpmcBroadcastConsumerKey,
}

struct Inner<T> {
    channel: SpmcBroadcast<T>,
    receiver_count: Cell<usize>,
}

pub struct SpmcBroadcastProducer<T> {
    inner: Rc<Inner<T>>,
}

pub struct SpmcBroadcastSource<T> {
    inner: Rc<Inner<T>>,
}

impl<T> Clone for SpmcBroadcastSource<T> {
    fn clone(&self) -> Self {
        self.inner
            .receiver_count
            .set(self.inner.receiver_count.get() + 1);
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

pub struct SpmcBroadcastConsumer<T> {
    inner: Rc<Inner<T>>,
    id: SpmcBroadcastConsumerKey,
}

pub fn broadcast<T>(capacity: NonZeroUsize) -> (SpmcBroadcastProducer<T>, SpmcBroadcastSource<T>) {
    let inner = Rc::new(Inner {
        channel: SpmcBroadcast::new(capacity),
        receiver_count: Cell::new(1),
    });
    let producer = SpmcBroadcastProducer {
        inner: Rc::clone(&inner),
    };
    let source = SpmcBroadcastSource { inner };
    (producer, source)
}

impl<T> SpmcBroadcast<T> {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            queue: RefCell::new(BroadcastQueue::new(capacity)),
            producer_waker: Cell::new(None),
            closed: Cell::new(false),
        }
    }

    pub fn close(&self) {
        self.closed.set(true);
        self.wake_all_consumers();
        if let Some(waker) = self.producer_waker.take() {
            waker.wake();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    pub fn shrink_buffer_to_fit(&self) {
        self.queue.borrow_mut().shrink_buffer_to_fit();
    }

    pub fn shrink_consumers_to_fit(&self) {
        self.queue.borrow_mut().shrink_consumers_to_fit();
    }

    pub fn shrink_to_fit(&self) {
        self.shrink_buffer_to_fit();
        self.shrink_consumers_to_fit();
    }

    pub fn try_send(&self, item: T) -> Result<(), TrySendError<T>> {
        if self.closed.get() {
            return Err(TrySendError::Closed(item));
        }
        let send_result = self.queue.borrow_mut().try_send(item);
        match send_result {
            Ok(()) => {
                self.wake_all_consumers();
                Ok(())
            }
            Err(item) => Err(TrySendError::Full(item)),
        }
    }

    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    pub async fn send(&self, item: T) -> Result<(), T> {
        let mut item = Some(item);

        poll_fn(|cx| {
            let send_item = item.take().expect("send polled after completion");

            match self.try_send(send_item) {
                Ok(()) => {
                    self.producer_waker.take();
                    Poll::Ready(Ok(()))
                }
                Err(TrySendError::Closed(returned_item)) => {
                    self.producer_waker.take();
                    Poll::Ready(Err(returned_item))
                }
                Err(TrySendError::Full(returned_item)) => {
                    item = Some(returned_item);
                    self.producer_waker.set(Some(cx.waker().clone()));
                    Poll::Pending
                }
            }
        })
        .await
    }

    fn register_consumer_waker(&self, id: SpmcBroadcastConsumerKey, waker: &Waker) {
        *self.queue.borrow_mut().consumer_context_mut(id.0) = Some(waker.clone());
    }

    fn wake_all_consumers(&self) {
        let wakers: SmallVec<[Waker; 16]> = {
            let mut queue = self.queue.borrow_mut();
            let mut collected = SmallVec::new();
            queue.for_each_consumer_mut(|context| {
                if let Some(waker) = context.take() {
                    collected.push(waker);
                }
            });
            collected
        };
        for waker in wakers {
            waker.wake();
        }
    }

    /// Creates a raw consumer subscription and returns its key.
    ///
    /// The returned key is valid until it is passed to
    /// [`unsubscribe_raw`](Self::unsubscribe_raw). Reusing that key after
    /// unsubscribe is a caller bug and may panic.
    ///
    /// Prefer [`subscribe_ref`](Self::subscribe_ref) or the split-handle
    /// API if you do not need to store subscription state manually.
    pub fn subscribe_raw(&self) -> SpmcBroadcastConsumerKey {
        SpmcBroadcastConsumerKey(self.queue.borrow_mut().subscribe_with_context(None))
    }

    /// Removes a raw consumer subscription.
    ///
    /// # Panics
    ///
    /// Panics if `id` is stale, already unsubscribed, or did not come
    /// from a live subscription on this broadcast.
    /// See the crate-level documentation on the [raw consumer key
    /// contract](crate#user-managed-consumer-state).
    pub fn unsubscribe_raw(&self, id: SpmcBroadcastConsumerKey) {
        let gc_effect = self.queue.borrow_mut().unsubscribe(id.0);
        if gc_effect == RecvEffect::Unblocked
            && let Some(waker) = self.producer_waker.take()
        {
            waker.wake();
        }
    }

    pub fn subscribe_ref(&self) -> SpmcBroadcastConsumerRef<'_, T> {
        let id = self.subscribe_raw();
        SpmcBroadcastConsumerRef { channel: self, id }
    }

    /// Attempts to receive the next item by passing a shared reference to the
    /// visitor closure, avoiding a clone.
    ///
    /// # Panics
    ///
    /// Panics if `id` is stale, already unsubscribed, or did not come
    /// from a live subscription on this broadcast.
    /// See the crate-level documentation on the [raw consumer key
    /// contract](crate#user-managed-consumer-state).
    ///
    /// Panics if `visitor` re-entrantly borrows this channel (e.g. calls
    /// [`try_recv_ref_raw`](Self::try_recv_ref_raw),
    /// [`try_recv_raw`](Self::try_recv_raw), [`try_send`](Self::try_send),
    /// or any other method that borrows the internal queue on the same
    /// instance). Use [`try_recv_raw`](Self::try_recv_raw) (`T: Clone`)
    /// if you need to access the channel inside the callback.
    pub fn try_recv_ref_raw<F, R>(
        &self,
        id: SpmcBroadcastConsumerKey,
        visitor: F,
    ) -> Result<R, TryRecvRefError<F>>
    where
        F: FnOnce(&T) -> R,
    {
        match self.queue.borrow_mut().try_recv_ref(id.0, visitor) {
            Ok((result, gc_effect)) => {
                if gc_effect == RecvEffect::Unblocked
                    && let Some(waker) = self.producer_waker.take()
                {
                    waker.wake();
                }
                Ok(result)
            }
            Err(visitor) => {
                if self.closed.get() {
                    Err(TryRecvRefError::Closed)
                } else {
                    Err(TryRecvRefError::Empty(visitor))
                }
            }
        }
    }

    /// Async version of [`try_recv_ref_raw`](Self::try_recv_ref_raw).
    ///
    /// # Panics
    ///
    /// Panics if `id` is stale, already unsubscribed, or did not come
    /// from a live subscription on this broadcast.
    /// See the crate-level documentation on the [raw consumer key
    /// contract](crate#user-managed-consumer-state).
    ///
    /// Panics if `visitor` re-entrantly borrows this channel.
    /// See [`try_recv_ref_raw`](Self::try_recv_ref_raw) for details.
    ///
    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    pub async fn recv_ref_raw<F, R>(
        &self,
        id: SpmcBroadcastConsumerKey,
        visitor: F,
    ) -> Result<R, Closed>
    where
        F: FnOnce(&T) -> R,
    {
        let mut visitor = Some(visitor);

        poll_fn(|cx| {
            let v = visitor
                .take()
                .expect("recv_ref_raw polled after completion");
            match self.try_recv_ref_raw(id, v) {
                Ok(result) => Poll::Ready(Ok(result)),
                Err(TryRecvRefError::Closed) => Poll::Ready(Err(Closed)),
                Err(TryRecvRefError::Empty(returned_visitor)) => {
                    visitor = Some(returned_visitor);
                    self.register_consumer_waker(id, cx.waker());
                    Poll::Pending
                }
            }
        })
        .await
    }
}

impl<T: Clone> SpmcBroadcast<T> {
    /// Attempts to receive the next item for a raw consumer subscription.
    ///
    /// # Panics
    ///
    /// Panics if `id` is stale, already unsubscribed, or did not come
    /// from a live subscription on this broadcast.
    /// See the crate-level documentation on the [raw consumer key
    /// contract](crate#user-managed-consumer-state).
    pub fn try_recv_raw(&self, id: SpmcBroadcastConsumerKey) -> Result<Option<T>, Closed> {
        match self.queue.borrow_mut().try_recv(id.0) {
            Some((item, gc_effect)) => {
                if gc_effect == RecvEffect::Unblocked
                    && let Some(waker) = self.producer_waker.take()
                {
                    waker.wake();
                }
                Ok(Some(item))
            }
            None => {
                if self.closed.get() {
                    Err(Closed)
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// # Panics
    ///
    /// Panics if `id` is stale, already unsubscribed, or did not come
    /// from a live subscription on this broadcast.
    /// See the crate-level documentation on the [raw consumer key
    /// contract](crate#user-managed-consumer-state).
    ///
    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    pub async fn recv_raw(&self, id: SpmcBroadcastConsumerKey) -> Result<T, Closed> {
        poll_fn(|cx| match self.try_recv_raw(id) {
            Ok(Some(item)) => Poll::Ready(Ok(item)),
            Err(Closed) => Poll::Ready(Err(Closed)),
            Ok(None) => {
                self.register_consumer_waker(id, cx.waker());
                Poll::Pending
            }
        })
        .await
    }
}

impl<'a, T> SpmcBroadcastConsumerRef<'a, T> {
    /// See [`SpmcBroadcast::try_recv_ref_raw`] for details.
    ///
    /// # Panics
    ///
    /// Panics if `visitor` re-entrantly borrows this channel.
    pub fn try_recv_ref<F, R>(&self, visitor: F) -> Result<R, TryRecvRefError<F>>
    where
        F: FnOnce(&T) -> R,
    {
        self.channel.try_recv_ref_raw(self.id, visitor)
    }

    /// See [`SpmcBroadcast::recv_ref_raw`] for details.
    ///
    /// # Panics
    ///
    /// Panics if `visitor` re-entrantly borrows this channel.
    pub async fn recv_ref<R>(&mut self, visitor: impl FnOnce(&T) -> R) -> Result<R, Closed> {
        self.channel.recv_ref_raw(self.id, visitor).await
    }
}

impl<'a, T: Clone> SpmcBroadcastConsumerRef<'a, T> {
    pub fn try_recv(&self) -> Result<Option<T>, Closed> {
        self.channel.try_recv_raw(self.id)
    }

    pub async fn recv(&mut self) -> Result<T, Closed> {
        self.channel.recv_raw(self.id).await
    }
}

impl<T> SpmcBroadcastConsumer<T> {
    /// See [`SpmcBroadcast::try_recv_ref_raw`] for details.
    ///
    /// # Panics
    ///
    /// Panics if `visitor` re-entrantly borrows this channel.
    pub fn try_recv_ref<F, R>(&self, visitor: F) -> Result<R, TryRecvRefError<F>>
    where
        F: FnOnce(&T) -> R,
    {
        self.inner.channel.try_recv_ref_raw(self.id, visitor)
    }

    /// See [`SpmcBroadcast::recv_ref_raw`] for details.
    ///
    /// # Panics
    ///
    /// Panics if `visitor` re-entrantly borrows this channel.
    pub async fn recv_ref<R>(&mut self, visitor: impl FnOnce(&T) -> R) -> Result<R, Closed> {
        self.inner.channel.recv_ref_raw(self.id, visitor).await
    }
}

impl<T: Clone> SpmcBroadcastConsumer<T> {
    pub fn try_recv(&self) -> Result<Option<T>, Closed> {
        self.inner.channel.try_recv_raw(self.id)
    }

    pub async fn recv(&mut self) -> Result<T, Closed> {
        self.inner.channel.recv_raw(self.id).await
    }
}

impl<T> SpmcBroadcastSource<T> {
    pub fn subscribe(&self) -> SpmcBroadcastConsumer<T> {
        self.inner
            .receiver_count
            .set(self.inner.receiver_count.get() + 1);
        let id = self.inner.channel.subscribe_raw();
        SpmcBroadcastConsumer {
            inner: Rc::clone(&self.inner),
            id,
        }
    }
}

impl<T> Drop for SpmcBroadcastSource<T> {
    fn drop(&mut self) {
        let count = self.inner.receiver_count.get() - 1;
        self.inner.receiver_count.set(count);
        if count == 0 {
            self.inner.channel.close();
        }
    }
}

impl<T> SpmcBroadcastProducer<T> {
    pub fn try_send(&self, item: T) -> Result<(), TrySendError<T>> {
        self.inner.channel.try_send(item)
    }

    pub async fn send(&mut self, item: T) -> Result<(), T> {
        self.inner.channel.send(item).await
    }

    pub fn shrink_buffer_to_fit(&self) {
        self.inner.channel.shrink_buffer_to_fit();
    }

    pub fn shrink_consumers_to_fit(&self) {
        self.inner.channel.shrink_consumers_to_fit();
    }

    pub fn shrink_to_fit(&self) {
        self.inner.channel.shrink_to_fit();
    }
}

impl<T> Drop for SpmcBroadcastProducer<T> {
    fn drop(&mut self) {
        self.inner.channel.close();
    }
}

impl<'a, T> Drop for SpmcBroadcastConsumerRef<'a, T> {
    fn drop(&mut self) {
        self.channel.unsubscribe_raw(self.id);
    }
}

impl<T> Drop for SpmcBroadcastConsumer<T> {
    fn drop(&mut self) {
        self.inner.channel.unsubscribe_raw(self.id);
        let count = self.inner.receiver_count.get() - 1;
        self.inner.receiver_count.set(count);
        if count == 0 {
            self.inner.channel.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::num::NonZeroUsize;
    use std::task::Context;

    use futures_executor::block_on;
    use futures_test::task::new_count_waker;
    use futures_util::pin_mut;

    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    #[test]
    fn recv_waiter_woken_by_send() {
        let channel = SpmcBroadcast::new(nz(2));
        let consumer = channel.subscribe_raw();

        let mut recv_fut = Box::pin(channel.recv_raw(consumer));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(recv_fut.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(wake_count.get(), 0);

        assert_eq!(channel.try_send(7), Ok(()));
        assert_eq!(wake_count.get(), 1);

        assert_eq!(recv_fut.as_mut().poll(&mut cx), Poll::Ready(Ok(7)));
    }

    #[test]
    fn producer_waiter_woken_by_recv_unblock() {
        let channel = SpmcBroadcast::new(nz(1));
        let consumer = channel.subscribe_raw();

        assert_eq!(channel.try_send(1), Ok(()));

        let mut send_fut = Box::pin(channel.send(2));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(channel.try_recv_raw(consumer), Ok(Some(1)));
        assert_eq!(wake_count.get(), 1);
        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn producer_waiter_woken_by_unsubscribe_unblock() {
        let channel = SpmcBroadcast::new(nz(1));
        let consumer = channel.subscribe_raw();

        assert_eq!(channel.try_send(1), Ok(()));

        let mut send_fut = Box::pin(channel.send(2));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Pending);
        channel.unsubscribe_raw(consumer);
        assert_eq!(wake_count.get(), 1);
        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn block_on_send_then_recv() {
        let channel = SpmcBroadcast::new(nz(2));
        let mut consumer = channel.subscribe_ref();

        block_on(async {
            channel.send(11).await.unwrap();
            assert_eq!(consumer.recv().await, Ok(11));
        });
    }

    #[test]
    fn send_blocks_on_full_queue_then_completes_after_lagger_drains() {
        let channel = SpmcBroadcast::new(nz(2));
        let lagger = channel.subscribe_raw();
        let fast = channel.subscribe_raw();

        assert_eq!(channel.try_send(1), Ok(()));
        assert_eq!(channel.try_send(2), Ok(()));

        assert_eq!(channel.try_recv_raw(fast), Ok(Some(1)));
        assert_eq!(channel.try_recv_raw(fast), Ok(Some(2)));
        assert_eq!(channel.try_send(3), Err(TrySendError::Full(3)));

        let send_fut = channel.send(3);
        pin_mut!(send_fut);
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(wake_count.get(), 0);

        assert_eq!(channel.try_recv_raw(lagger), Ok(Some(1)));
        assert_eq!(wake_count.get(), 1);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
        assert_eq!(channel.try_recv_raw(fast), Ok(Some(3)));
    }

    #[test]
    fn shrink_to_fit_is_behaviorally_transparent_after_churn() {
        let channel = SpmcBroadcast::new(nz(8));

        for round in 0..64 {
            let a = channel.subscribe_raw();
            let b = channel.subscribe_raw();

            assert_eq!(channel.try_send(round), Ok(()));
            assert_eq!(channel.try_recv_raw(a), Ok(Some(round)));
            assert_eq!(channel.try_recv_raw(b), Ok(Some(round)));

            channel.unsubscribe_raw(a);
            channel.unsubscribe_raw(b);
        }

        channel.shrink_to_fit();

        let mut consumer = channel.subscribe_ref();
        block_on(async {
            channel.send(1234).await.unwrap();
            assert_eq!(consumer.recv().await, Ok(1234));
        });
    }

    #[test]
    fn shrink_consumers_to_fit_is_behaviorally_transparent() {
        let channel: SpmcBroadcast<i32> = SpmcBroadcast::new(nz(8));
        let a = channel.subscribe_raw();
        let _b = channel.subscribe_raw();
        let _c = channel.subscribe_raw();

        channel.unsubscribe_raw(a);

        channel.shrink_consumers_to_fit();

        assert_eq!(channel.try_send(1), Ok(()));
    }

    #[derive(Debug, PartialEq)]
    struct NoCopy(Vec<u8>);

    #[test]
    fn try_recv_ref_raw_basic() {
        let channel = SpmcBroadcast::new(nz(4));
        let c = channel.subscribe_raw();

        assert!(channel.try_send(NoCopy(vec![1, 2, 3])).is_ok());

        let val = channel.try_recv_ref_raw(c, |item| item.0.clone());
        assert_eq!(val.ok(), Some(vec![1, 2, 3]));

        assert!(matches!(
            channel.try_recv_ref_raw(c, |_| ()),
            Err(TryRecvRefError::Empty(_))
        ));
        channel.unsubscribe_raw(c);
    }

    #[test]
    fn recv_ref_raw_waiter_woken_by_send() {
        let channel = SpmcBroadcast::new(nz(2));
        let consumer = channel.subscribe_raw();

        let mut recv_fut = Box::pin(channel.recv_ref_raw(consumer, |item: &NoCopy| item.0.clone()));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(recv_fut.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(wake_count.get(), 0);

        assert!(channel.try_send(NoCopy(vec![7])).is_ok());
        assert_eq!(wake_count.get(), 1);

        assert_eq!(recv_fut.as_mut().poll(&mut cx), Poll::Ready(Ok(vec![7])));
        channel.unsubscribe_raw(consumer);
    }

    #[test]
    fn producer_waiter_woken_by_try_recv_ref_raw_unblock() {
        let channel = SpmcBroadcast::new(nz(1));
        let consumer = channel.subscribe_raw();

        assert!(channel.try_send(NoCopy(vec![1])).is_ok());

        let mut send_fut = Box::pin(channel.send(NoCopy(vec![2])));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Pending);

        let val = channel.try_recv_ref_raw(consumer, |item| item.0.clone());
        assert_eq!(val.ok(), Some(vec![1]));
        assert_eq!(wake_count.get(), 1);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
        channel.unsubscribe_raw(consumer);
    }

    #[test]
    fn block_on_send_then_recv_ref() {
        let channel = SpmcBroadcast::new(nz(2));
        let mut consumer = channel.subscribe_ref();

        block_on(async {
            channel.send(NoCopy(vec![42])).await.unwrap();
            let val = consumer.recv_ref(|item: &NoCopy| item.0[0]).await;
            assert_eq!(val, Ok(42));
        });
    }

    #[test]
    fn consumer_owned_try_recv_ref() {
        let (producer, source) = broadcast(nz(4));
        let consumer = source.subscribe();

        assert!(producer.try_send(NoCopy(vec![10, 20])).is_ok());

        let val = consumer.try_recv_ref(|item| item.0.len());
        assert_eq!(val.ok(), Some(2));

        assert!(matches!(
            consumer.try_recv_ref(|_| ()),
            Err(TryRecvRefError::Empty(_))
        ));
    }

    #[test]
    fn channel_send_recv_clone() {
        let (producer, source) = broadcast(nz(4));
        let consumer = source.subscribe();

        assert_eq!(producer.try_send(1), Ok(()));
        assert_eq!(producer.try_send(2), Ok(()));

        assert_eq!(consumer.try_recv(), Ok(Some(1)));
        assert_eq!(consumer.try_recv(), Ok(Some(2)));
        assert_eq!(consumer.try_recv(), Ok(None));
    }

    #[test]
    fn channel_send_recv_ref_no_clone() {
        let (producer, source) = broadcast(nz(4));
        let consumer = source.subscribe();

        assert!(producer.try_send(NoCopy(vec![1, 2])).is_ok());

        let val = consumer.try_recv_ref(|item| item.0.clone());
        assert_eq!(val.ok(), Some(vec![1, 2]));
    }

    #[test]
    fn channel_async_send_recv() {
        let (mut producer, source) = broadcast(nz(2));
        let mut consumer = source.subscribe();

        block_on(async {
            producer.send(42).await.unwrap();
            assert_eq!(consumer.recv().await, Ok(42));
        });
    }

    #[test]
    fn channel_subscribe_additional_consumers() {
        let (producer, source) = broadcast(nz(4));
        let c1 = source.subscribe();
        let c2 = source.subscribe();
        let c3 = source.subscribe();

        assert_eq!(producer.try_send(7), Ok(()));

        assert_eq!(c1.try_recv(), Ok(Some(7)));
        assert_eq!(c2.try_recv(), Ok(Some(7)));
        assert_eq!(c3.try_recv(), Ok(Some(7)));
    }

    #[test]
    fn producer_owned_is_not_clone() {
        let (producer, _source) = broadcast::<i32>(nz(1));
        assert_eq!(producer.try_send(1), Ok(()));
    }

    #[test]
    fn close_then_drain_then_closed() {
        let channel = SpmcBroadcast::new(nz(4));
        let c = channel.subscribe_raw();

        assert_eq!(channel.try_send(1), Ok(()));
        assert_eq!(channel.try_send(2), Ok(()));

        channel.close();

        assert_eq!(channel.try_recv_raw(c), Ok(Some(1)));
        assert_eq!(channel.try_recv_raw(c), Ok(Some(2)));

        assert_eq!(channel.try_recv_raw(c), Err(Closed));

        channel.unsubscribe_raw(c);
    }

    #[test]
    fn close_wakes_pending_recv() {
        let channel = SpmcBroadcast::<i32>::new(nz(2));
        let c = channel.subscribe_raw();

        let mut recv_fut = Box::pin(channel.recv_raw(c));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(recv_fut.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(wake_count.get(), 0);

        channel.close();
        assert_eq!(wake_count.get(), 1);

        assert_eq!(recv_fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
        channel.unsubscribe_raw(c);
    }

    #[test]
    fn close_wakes_pending_send() {
        let channel = SpmcBroadcast::new(nz(1));
        let _c = channel.subscribe_raw();

        assert_eq!(channel.try_send(1), Ok(()));

        let mut send_fut = Box::pin(channel.send(2));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Pending);

        channel.close();
        assert_eq!(wake_count.get(), 1);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Ready(Err(2)));
    }

    #[test]
    fn producer_drop_closes_channel() {
        let (producer, source) = broadcast(nz(4));
        let consumer = source.subscribe();

        assert_eq!(producer.try_send(10), Ok(()));
        assert_eq!(producer.try_send(20), Ok(()));

        drop(producer);

        assert_eq!(consumer.try_recv(), Ok(Some(10)));
        assert_eq!(consumer.try_recv(), Ok(Some(20)));

        assert_eq!(consumer.try_recv(), Err(Closed));
    }

    #[test]
    fn double_close_is_idempotent() {
        let channel = SpmcBroadcast::<i32>::new(nz(2));
        channel.close();
        channel.close();
        assert!(channel.is_closed());
    }

    #[test]
    fn try_send_after_close_rejected() {
        let channel = SpmcBroadcast::new(nz(4));
        let _c = channel.subscribe_raw();

        channel.close();
        assert_eq!(channel.try_send(42), Err(TrySendError::Closed(42)));
    }

    #[test]
    fn close_recv_ref_drains_then_closed() {
        let channel = SpmcBroadcast::new(nz(4));
        let c = channel.subscribe_raw();

        assert!(channel.try_send(NoCopy(vec![1, 2])).is_ok());
        assert!(channel.try_send(NoCopy(vec![3])).is_ok());

        channel.close();

        let v1 = channel.try_recv_ref_raw(c, |item| item.0.clone());
        assert_eq!(v1.ok(), Some(vec![1, 2]));

        let v2 = channel.try_recv_ref_raw(c, |item| item.0.clone());
        assert_eq!(v2.ok(), Some(vec![3]));

        assert!(matches!(
            channel.try_recv_ref_raw(c, |_| ()),
            Err(TryRecvRefError::Closed)
        ));

        channel.unsubscribe_raw(c);
    }

    #[test]
    fn close_async_recv_drains_then_closed() {
        let (mut producer, source) = broadcast(nz(4));
        let mut consumer = source.subscribe();

        block_on(async {
            producer.send(1).await.unwrap();
            producer.send(2).await.unwrap();

            drop(producer);

            assert_eq!(consumer.recv().await, Ok(1));
            assert_eq!(consumer.recv().await, Ok(2));
            assert_eq!(consumer.recv().await, Err(Closed));
        });
    }

    #[test]
    fn close_async_recv_ref_drains_then_closed() {
        let channel = SpmcBroadcast::new(nz(4));
        let mut consumer = channel.subscribe_ref();

        block_on(async {
            channel.send(NoCopy(vec![7])).await.unwrap();
            channel.close();

            let val = consumer.recv_ref(|item: &NoCopy| item.0[0]).await;
            assert_eq!(val, Ok(7));

            let val = consumer.recv_ref(|_: &NoCopy| ()).await;
            assert_eq!(val, Err(Closed));
        });
    }
}

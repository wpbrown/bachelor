use crate::core::RecvEffect;
use crate::core::bounded_queue::BoundedQueue;
#[cfg(feature = "stream")]
use futures_core::Stream;
use slab::Slab;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

pub use crate::error::{Closed, TrySendError};
#[cfg(feature = "sink")]
pub use mpsc_channel_sink::MpscChannelSink;

pub struct MpscChannel<T> {
    queue: RefCell<BoundedQueue<T>>,
    consumer_waker: Cell<Option<Waker>>,
    producer_wakers: RefCell<Slab<Option<Waker>>>,
    closed: Cell<bool>,
}

pub struct MpscChannelProducer<T> {
    channel: Rc<MpscChannel<T>>,
}

pub struct MpscChannelConsumer<T> {
    channel: Rc<MpscChannel<T>>,
}

pub fn channel<T>(capacity: NonZeroUsize) -> (MpscChannelProducer<T>, MpscChannelConsumer<T>) {
    let channel = Rc::new(MpscChannel::new(capacity));
    let producer = MpscChannelProducer {
        channel: Rc::clone(&channel),
    };
    let consumer = MpscChannelConsumer { channel };
    (producer, consumer)
}

impl<T> MpscChannel<T> {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            queue: RefCell::new(BoundedQueue::new(capacity)),
            consumer_waker: Cell::new(None),
            producer_wakers: RefCell::new(Slab::new()),
            closed: Cell::new(false),
        }
    }

    pub fn close(&self) {
        self.closed.set(true);
        if let Some(waker) = self.consumer_waker.take() {
            waker.wake();
        }
        let slab = self.producer_wakers.take();
        for (_, slot) in slab {
            if let Some(waker) = slot {
                waker.wake();
            }
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    pub fn try_send(&self, item: T) -> Result<(), TrySendError<T>> {
        if self.closed.get() {
            return Err(TrySendError::Closed(item));
        }
        match self.queue.borrow_mut().try_send(item) {
            Ok(()) => {
                if let Some(waker) = self.consumer_waker.take() {
                    waker.wake();
                }
                Ok(())
            }
            Err(item) => Err(TrySendError::Full(item)),
        }
    }

    pub fn send(&self, item: T) -> Send<'_, T> {
        Send {
            channel: self,
            item: Some(item),
            waker_key: None,
        }
    }

    fn wake_one_producer(&self) {
        let waker = {
            let mut slab = self.producer_wakers.borrow_mut();
            slab.iter_mut().find_map(|(_, slot)| slot.take())
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn register_producer(&self, waker: Waker) -> usize {
        self.producer_wakers.borrow_mut().insert(Some(waker))
    }

    fn update_producer(&self, key: usize, waker: Waker) {
        self.producer_wakers.borrow_mut()[key] = Some(waker);
    }

    fn unregister_producer(&self, key: usize) {
        if self.closed.get() {
            return;
        }
        self.producer_wakers.borrow_mut().remove(key);
        if !self.queue.borrow().is_full() {
            self.wake_one_producer();
        }
    }

    pub fn try_recv(&self) -> Result<Option<T>, Closed> {
        match self.queue.borrow_mut().try_recv() {
            Some((item, effect)) => {
                if effect == RecvEffect::Unblocked {
                    self.wake_one_producer();
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

    fn poll_recv_result(&self, cx: &mut Context<'_>) -> Poll<Result<T, Closed>> {
        match self.try_recv() {
            Ok(Some(item)) => Poll::Ready(Ok(item)),
            Err(Closed) => Poll::Ready(Err(Closed)),
            Ok(None) => {
                self.consumer_waker.set(Some(cx.waker().clone()));
                Poll::Pending
            }
        }
    }

    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    pub fn recv(&self) -> Recv<'_, T> {
        Recv { channel: self }
    }

    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    ///
    /// Returns `Poll::Ready(None)` once the channel has been closed and all
    /// buffered items have been drained.
    pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.poll_recv_result(cx).map(Result::ok)
    }

    pub fn shrink_buffer_to_fit(&self) {
        self.queue.borrow_mut().shrink_to_fit();
    }

    pub fn shrink_producers_to_fit(&self) {
        self.producer_wakers.borrow_mut().shrink_to_fit();
    }

    pub fn shrink_to_fit(&self) {
        self.shrink_buffer_to_fit();
        self.shrink_producers_to_fit();
    }
}

pub struct Send<'a, T> {
    channel: &'a MpscChannel<T>,
    item: Option<T>,
    waker_key: Option<usize>,
}

/// `Send` only stores `T` by value and never creates a self-referential borrow.
impl<T> Unpin for Send<'_, T> {}

impl<T> Future for Send<'_, T> {
    type Output = Result<(), T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let send_item = this.item.take().expect("Send polled after completion");

        match this.channel.try_send(send_item) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(TrySendError::Closed(returned_item)) => Poll::Ready(Err(returned_item)),
            Err(TrySendError::Full(returned_item)) => {
                this.item = Some(returned_item);
                let waker = cx.waker().clone();
                match this.waker_key {
                    Some(key) => this.channel.update_producer(key, waker),
                    None => {
                        this.waker_key = Some(this.channel.register_producer(waker));
                    }
                }
                Poll::Pending
            }
        }
    }
}

impl<T> Drop for Send<'_, T> {
    fn drop(&mut self) {
        if let Some(key) = self.waker_key {
            self.channel.unregister_producer(key);
        }
    }
}

pub struct Recv<'a, T> {
    channel: &'a MpscChannel<T>,
}

impl<T> Future for Recv<'_, T> {
    type Output = Result<T, Closed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().channel.poll_recv_result(cx)
    }
}

impl<T> Drop for Recv<'_, T> {
    fn drop(&mut self) {
        self.channel.consumer_waker.take();
    }
}

impl<T> Clone for MpscChannelProducer<T> {
    fn clone(&self) -> Self {
        Self {
            channel: Rc::clone(&self.channel),
        }
    }
}

impl<T> MpscChannelProducer<T> {
    pub fn try_send(&self, item: T) -> Result<(), TrySendError<T>> {
        self.channel.try_send(item)
    }

    pub fn send(&self, item: T) -> Send<'_, T> {
        self.channel.send(item)
    }

    pub fn shrink_buffer_to_fit(&self) {
        self.channel.shrink_buffer_to_fit();
    }

    pub fn shrink_producers_to_fit(&self) {
        self.channel.shrink_producers_to_fit();
    }

    pub fn shrink_to_fit(&self) {
        self.channel.shrink_to_fit();
    }

    #[cfg(feature = "sink")]
    pub fn into_sink(self) -> mpsc_channel_sink::MpscChannelSink<T> {
        let channel = Rc::clone(&self.channel);
        drop(self);
        mpsc_channel_sink::MpscChannelSink::new(channel)
    }
}

impl<T> Drop for MpscChannelProducer<T> {
    fn drop(&mut self) {
        // The only Rc clones are held by producer-like send handles and the
        // single consumer. Count == 2 means self (about to drop) + consumer,
        // i.e. this is the last producer-like handle.
        if Rc::strong_count(&self.channel) == 2 {
            self.channel.close();
        }
    }
}

impl<T> MpscChannelConsumer<T> {
    pub fn try_recv(&self) -> Result<Option<T>, Closed> {
        self.channel.try_recv()
    }

    pub async fn recv(&mut self) -> Result<T, Closed> {
        self.channel.recv().await
    }
}

#[cfg(feature = "stream")]
impl<T> Stream for MpscChannelConsumer<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.channel.poll_recv(cx)
    }
}

impl<T> Drop for MpscChannelConsumer<T> {
    fn drop(&mut self) {
        self.channel.close();
    }
}

#[cfg(feature = "sink")]
mod mpsc_channel_sink {
    use super::*;
    use futures_sink::Sink;

    /// Feature-gated sink adaptor returned by [`MpscChannelProducer::into_sink`].
    ///
    /// This type owns the producer-side waiter slot required by the
    /// `futures-sink` `Sink` contract for MPSC sends.
    pub struct MpscChannelSink<T> {
        channel: Rc<MpscChannel<T>>,
        waker_key: Option<usize>,
        pending: Option<T>,
    }

    /// `MpscChannelSink` is never self-referential — safe to unpin unconditionally.
    impl<T> Unpin for MpscChannelSink<T> {}

    impl<T> MpscChannelSink<T> {
        pub(super) fn new(channel: Rc<MpscChannel<T>>) -> Self {
            Self {
                channel,
                waker_key: None,
                pending: None,
            }
        }

        /// Try to flush the buffered item into the channel.  Returns `Ok(true)`
        /// when the buffer is empty (either it was already empty or the item was
        /// successfully sent), `Ok(false)` when the queue is full and a buffered
        /// item could not be flushed, and `Err(Closed)` when a buffered item
        /// could not be flushed because the channel has been closed.
        fn flush_pending(&mut self) -> Result<bool, Closed> {
            let item = match self.pending.take() {
                Some(item) => item,
                None => return Ok(true),
            };
            match self.channel.try_send(item) {
                Ok(()) => Ok(true),
                Err(TrySendError::Closed(_)) => Err(Closed),
                Err(TrySendError::Full(item)) => {
                    self.pending = Some(item);
                    Ok(false)
                }
            }
        }

        fn register_waker(&mut self, cx: &mut Context<'_>) {
            let waker = cx.waker().clone();
            match self.waker_key {
                Some(key) => self.channel.update_producer(key, waker),
                None => {
                    self.waker_key = Some(self.channel.register_producer(waker));
                }
            }
        }

        fn teardown(&mut self) {
            if let Some(key) = self.waker_key.take() {
                self.channel.unregister_producer(key);
            }
            if Rc::strong_count(&self.channel) == 2 {
                self.channel.close();
            }
        }
    }

    impl<T> Sink<T> for MpscChannelSink<T> {
        type Error = Closed;

        fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            let this = self.get_mut();

            if this.flush_pending()? {
                if this.channel.closed.get() {
                    return Poll::Ready(Err(Closed));
                }
                return Poll::Ready(Ok(()));
            }

            // pending item could not be flushed – queue is full, register waker
            this.register_waker(cx);
            Poll::Pending
        }

        fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
            let this = self.get_mut();
            debug_assert!(
                this.pending.is_none(),
                "start_send called without poll_ready"
            );
            if this.channel.closed.get() {
                return Err(Closed);
            }
            this.pending = Some(item);
            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            let this = self.get_mut();

            if this.flush_pending()? {
                return Poll::Ready(Ok(()));
            }

            this.register_waker(cx);
            Poll::Pending
        }

        fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            let this = self.get_mut();

            let result = this.flush_pending();
            if !matches!(result, Ok(false)) {
                this.teardown();
                return Poll::Ready(result.map(|_| ()));
            }

            this.register_waker(cx);
            Poll::Pending
        }
    }

    impl<T> Drop for MpscChannelSink<T> {
        fn drop(&mut self) {
            self.teardown();
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
    #[cfg(feature = "sink")]
    use futures_sink::Sink;
    use futures_test::task::new_count_waker;
    #[cfg(feature = "stream")]
    use futures_util::StreamExt;

    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    #[test]
    fn try_send_try_recv_roundtrip() {
        let ch = MpscChannel::new(nz(4));

        assert_eq!(ch.try_send(1), Ok(()));
        assert_eq!(ch.try_send(2), Ok(()));

        assert_eq!(ch.try_recv(), Ok(Some(1)));
        assert_eq!(ch.try_recv(), Ok(Some(2)));
        assert_eq!(ch.try_recv(), Ok(None));
    }

    #[test]
    fn full_rejects_send() {
        let ch = MpscChannel::new(nz(2));

        assert_eq!(ch.try_send(1), Ok(()));
        assert_eq!(ch.try_send(2), Ok(()));
        assert_eq!(ch.try_send(3), Err(TrySendError::Full(3)));
    }

    #[test]
    fn consumer_waker_woken_by_send() {
        let ch = MpscChannel::new(nz(2));

        let mut recv_fut = Box::pin(ch.recv());
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(recv_fut.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(wake_count.get(), 0);

        assert_eq!(ch.try_send(7), Ok(()));
        assert_eq!(wake_count.get(), 1);

        assert_eq!(recv_fut.as_mut().poll(&mut cx), Poll::Ready(Ok(7)));
    }

    #[test]
    fn producer_waker_woken_by_recv_when_full() {
        let ch = MpscChannel::new(nz(1));

        assert_eq!(ch.try_send(1), Ok(()));

        let mut send_fut = Box::pin(ch.send(2));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(wake_count.get(), 0);

        assert_eq!(ch.try_recv(), Ok(Some(1)));
        assert_eq!(wake_count.get(), 1);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn multiple_blocked_producers_woken_one_at_a_time() {
        let ch = MpscChannel::new(nz(1));

        assert_eq!(ch.try_send(1), Ok(()));

        let mut send_a = Box::pin(ch.send(10));
        let (waker_a, wake_a) = new_count_waker();
        let mut cx_a = Context::from_waker(&waker_a);
        assert_eq!(send_a.as_mut().poll(&mut cx_a), Poll::Pending);

        let mut send_b = Box::pin(ch.send(20));
        let (waker_b, wake_b) = new_count_waker();
        let mut cx_b = Context::from_waker(&waker_b);
        assert_eq!(send_b.as_mut().poll(&mut cx_b), Poll::Pending);

        assert_eq!(ch.try_recv(), Ok(Some(1)));
        assert_eq!(wake_a.get(), 1);
        assert_eq!(wake_b.get(), 0);

        assert_eq!(send_a.as_mut().poll(&mut cx_a), Poll::Ready(Ok(())));

        assert_eq!(ch.try_recv(), Ok(Some(10)));
        assert_eq!(wake_b.get(), 1);

        assert_eq!(send_b.as_mut().poll(&mut cx_b), Poll::Ready(Ok(())));
        assert_eq!(ch.try_recv(), Ok(Some(20)));
    }

    #[test]
    fn dropped_send_future_cascades_wake() {
        let ch = MpscChannel::new(nz(1));

        assert_eq!(ch.try_send(1), Ok(()));

        let mut send_a = Box::pin(ch.send(10));
        let (waker_a, _wake_a) = new_count_waker();
        let mut cx_a = Context::from_waker(&waker_a);
        assert_eq!(send_a.as_mut().poll(&mut cx_a), Poll::Pending);

        let mut send_b = Box::pin(ch.send(20));
        let (waker_b, wake_b) = new_count_waker();
        let mut cx_b = Context::from_waker(&waker_b);
        assert_eq!(send_b.as_mut().poll(&mut cx_b), Poll::Pending);

        assert_eq!(ch.try_recv(), Ok(Some(1)));

        drop(send_a);

        assert_eq!(wake_b.get(), 1);

        assert_eq!(send_b.as_mut().poll(&mut cx_b), Poll::Ready(Ok(())));
    }

    #[test]
    fn dropped_send_future_no_spurious_wake_when_full() {
        let ch = MpscChannel::new(nz(1));

        assert_eq!(ch.try_send(1), Ok(()));

        let mut send_a = Box::pin(ch.send(10));
        let (waker_a, _) = new_count_waker();
        let mut cx_a = Context::from_waker(&waker_a);
        assert_eq!(send_a.as_mut().poll(&mut cx_a), Poll::Pending);

        let mut send_b = Box::pin(ch.send(20));
        let (waker_b, wake_b) = new_count_waker();
        let mut cx_b = Context::from_waker(&waker_b);
        assert_eq!(send_b.as_mut().poll(&mut cx_b), Poll::Pending);

        drop(send_a);
        assert_eq!(wake_b.get(), 0);

        assert_eq!(ch.try_recv(), Ok(Some(1)));
        assert_eq!(wake_b.get(), 1);
        assert_eq!(send_b.as_mut().poll(&mut cx_b), Poll::Ready(Ok(())));
    }

    #[test]
    fn close_then_drain_then_closed() {
        let ch = MpscChannel::new(nz(4));

        assert_eq!(ch.try_send(1), Ok(()));
        assert_eq!(ch.try_send(2), Ok(()));

        ch.close();

        assert_eq!(ch.try_recv(), Ok(Some(1)));
        assert_eq!(ch.try_recv(), Ok(Some(2)));
        assert_eq!(ch.try_recv(), Err(Closed));
    }

    #[test]
    fn close_wakes_pending_recv() {
        let ch = MpscChannel::<i32>::new(nz(2));

        let mut recv_fut = Box::pin(ch.recv());
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(recv_fut.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(wake_count.get(), 0);

        ch.close();
        assert_eq!(wake_count.get(), 1);

        assert_eq!(recv_fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn close_wakes_all_pending_sends() {
        let ch = MpscChannel::new(nz(1));

        assert_eq!(ch.try_send(1), Ok(()));

        let mut send_a = Box::pin(ch.send(2));
        let (waker_a, wake_a) = new_count_waker();
        let mut cx_a = Context::from_waker(&waker_a);
        assert_eq!(send_a.as_mut().poll(&mut cx_a), Poll::Pending);

        let mut send_b = Box::pin(ch.send(3));
        let (waker_b, wake_b) = new_count_waker();
        let mut cx_b = Context::from_waker(&waker_b);
        assert_eq!(send_b.as_mut().poll(&mut cx_b), Poll::Pending);

        ch.close();
        assert_eq!(wake_a.get(), 1);
        assert_eq!(wake_b.get(), 1);

        assert_eq!(send_a.as_mut().poll(&mut cx_a), Poll::Ready(Err(2)));
        assert_eq!(send_b.as_mut().poll(&mut cx_b), Poll::Ready(Err(3)));
    }

    #[test]
    fn try_send_after_close_rejected() {
        let ch = MpscChannel::new(nz(4));
        ch.close();
        assert_eq!(ch.try_send(42), Err(TrySendError::Closed(42)));
    }

    #[test]
    fn double_close_is_idempotent() {
        let ch = MpscChannel::<i32>::new(nz(2));
        ch.close();
        ch.close();
        assert!(ch.is_closed());
    }

    #[test]
    fn channel_send_recv() {
        let (producer, consumer) = channel(nz(4));

        assert_eq!(producer.try_send(1), Ok(()));
        assert_eq!(producer.try_send(2), Ok(()));

        assert_eq!(consumer.try_recv(), Ok(Some(1)));
        assert_eq!(consumer.try_recv(), Ok(Some(2)));
        assert_eq!(consumer.try_recv(), Ok(None));
    }

    #[test]
    fn channel_async_send_recv() {
        let (producer, mut consumer) = channel(nz(2));

        block_on(async {
            producer.send(42).await.unwrap();
            assert_eq!(consumer.recv().await, Ok(42));
        });
    }

    #[test]
    fn producer_is_clone() {
        let (p1, consumer) = channel(nz(4));
        let p2 = p1.clone();

        assert_eq!(p1.try_send(1), Ok(()));
        assert_eq!(p2.try_send(2), Ok(()));

        assert_eq!(consumer.try_recv(), Ok(Some(1)));
        assert_eq!(consumer.try_recv(), Ok(Some(2)));
    }

    #[test]
    fn dropping_one_producer_does_not_close() {
        let (p1, consumer) = channel(nz(4));
        let p2 = p1.clone();

        drop(p1);

        assert_eq!(p2.try_send(99), Ok(()));
        assert_eq!(consumer.try_recv(), Ok(Some(99)));
    }

    #[test]
    fn last_producer_drop_closes_channel() {
        let (p1, consumer) = channel(nz(4));
        let p2 = p1.clone();

        assert_eq!(p1.try_send(10), Ok(()));
        assert_eq!(p2.try_send(20), Ok(()));

        drop(p1);
        drop(p2);

        assert_eq!(consumer.try_recv(), Ok(Some(10)));
        assert_eq!(consumer.try_recv(), Ok(Some(20)));
        assert_eq!(consumer.try_recv(), Err(Closed));
    }

    #[test]
    fn consumer_drop_closes_channel() {
        let (producer, consumer) = channel::<i32>(nz(4));

        drop(consumer);

        assert_eq!(producer.try_send(1), Err(TrySendError::Closed(1)));
    }

    #[test]
    fn close_async_recv_drains_then_closed() {
        let (producer, mut consumer) = channel(nz(4));

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
    fn consumer_owned_is_not_clone() {
        let (_producer, consumer) = channel::<i32>(nz(1));
        assert_eq!(consumer.try_recv(), Ok(None));
    }

    #[test]
    fn multiple_producers_async() {
        let (p1, mut consumer) = channel(nz(4));
        let p2 = p1.clone();

        block_on(async {
            p1.send(1).await.unwrap();
            p2.send(2).await.unwrap();
            p1.send(3).await.unwrap();

            assert_eq!(consumer.recv().await, Ok(1));
            assert_eq!(consumer.recv().await, Ok(2));
            assert_eq!(consumer.recv().await, Ok(3));
        });
    }

    #[test]
    fn send_blocks_on_full_then_completes_after_recv() {
        let (producer, mut consumer) = channel(nz(2));

        block_on(async {
            producer.send(1).await.unwrap();
            producer.send(2).await.unwrap();

            let mut send_fut = Box::pin(producer.send(3));
            let (waker, wake_count) = new_count_waker();
            let mut cx = Context::from_waker(&waker);

            assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Pending);

            assert_eq!(consumer.recv().await, Ok(1));
            assert_eq!(wake_count.get(), 1);

            assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
        });
    }

    #[test]
    fn shrink_to_fit_is_behaviorally_transparent() {
        let (producer, mut consumer) = channel(nz(8));

        block_on(async {
            for i in 0..64 {
                producer.send(i).await.unwrap();
                assert_eq!(consumer.recv().await, Ok(i));
            }

            producer.shrink_to_fit();

            producer.send(999).await.unwrap();
            assert_eq!(consumer.recv().await, Ok(999));
        });
    }

    #[test]
    fn dropped_recv_future_clears_waker() {
        let ch = MpscChannel::<i32>::new(nz(2));

        let mut recv_fut = Box::pin(ch.recv());
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(recv_fut.as_mut().poll(&mut cx), Poll::Pending);
        drop(recv_fut);

        // Waker was cleared by Drop, so close should not wake it.
        ch.close();
        assert_eq!(wake_count.get(), 0);
    }

    #[test]
    fn poll_recv_returns_none_after_close_and_drain() {
        let ch = MpscChannel::new(nz(2));

        assert_eq!(ch.try_send(1), Ok(()));
        ch.close();

        let (waker, _wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(ch.poll_recv(&mut cx), Poll::Ready(Some(1)));
        assert_eq!(ch.poll_recv(&mut cx), Poll::Ready(None));
    }

    #[cfg(feature = "sink")]
    #[test]
    fn sink_poll_ready_pending_then_woken_by_recv() {
        let (producer, consumer) = channel(nz(1));
        let mut sink = producer.into_sink();

        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        // Fill queue via the sink
        assert_eq!(Pin::new(&mut sink).poll_ready(&mut cx), Poll::Ready(Ok(())));
        assert_eq!(Pin::new(&mut sink).start_send(1), Ok(()));
        // Flush 1 into the queue, then buffer a second item
        assert_eq!(Pin::new(&mut sink).poll_ready(&mut cx), Poll::Ready(Ok(())));
        assert_eq!(Pin::new(&mut sink).start_send(2), Ok(()));
        // Can't flush 2 — queue full
        assert_eq!(Pin::new(&mut sink).poll_ready(&mut cx), Poll::Pending);
        assert_eq!(wake_count.get(), 0);

        assert_eq!(consumer.try_recv(), Ok(Some(1)));
        assert_eq!(wake_count.get(), 1);

        // Now flush succeeds
        assert_eq!(Pin::new(&mut sink).poll_ready(&mut cx), Poll::Ready(Ok(())));
        assert_eq!(consumer.try_recv(), Ok(Some(2)));
    }

    #[cfg(feature = "sink")]
    #[test]
    fn sink_producers_wake_one_at_a_time() {
        let (producer_a, consumer) = channel(nz(1));
        let producer_b = producer_a.clone();
        let mut sink_a = producer_a.into_sink();
        let mut sink_b = producer_b.into_sink();

        let (waker_a, wake_a) = new_count_waker();
        let mut cx_a = Context::from_waker(&waker_a);
        let (waker_b, wake_b) = new_count_waker();
        let mut cx_b = Context::from_waker(&waker_b);

        // Fill queue and buffer an extra item through sink_a
        assert_eq!(
            Pin::new(&mut sink_a).poll_ready(&mut cx_a),
            Poll::Ready(Ok(()))
        );
        assert_eq!(Pin::new(&mut sink_a).start_send(1), Ok(()));
        assert_eq!(
            Pin::new(&mut sink_a).poll_ready(&mut cx_a),
            Poll::Ready(Ok(()))
        );
        assert_eq!(Pin::new(&mut sink_a).start_send(10), Ok(()));
        // sink_a can't flush 10 — queue full
        assert_eq!(Pin::new(&mut sink_a).poll_ready(&mut cx_a), Poll::Pending);

        // Buffer an item through sink_b, also can't flush
        assert_eq!(
            Pin::new(&mut sink_b).poll_ready(&mut cx_b),
            Poll::Ready(Ok(()))
        );
        assert_eq!(Pin::new(&mut sink_b).start_send(20), Ok(()));
        assert_eq!(Pin::new(&mut sink_b).poll_ready(&mut cx_b), Poll::Pending);

        // Consume one — wakes the first registered producer (sink_a)
        assert_eq!(consumer.try_recv(), Ok(Some(1)));
        assert_eq!(wake_a.get(), 1);
        assert_eq!(wake_b.get(), 0);

        // sink_a flushes 10 into the queue
        assert_eq!(
            Pin::new(&mut sink_a).poll_ready(&mut cx_a),
            Poll::Ready(Ok(()))
        );

        // Consume 10 — wakes the next producer (sink_b)
        assert_eq!(consumer.try_recv(), Ok(Some(10)));
        assert_eq!(wake_b.get(), 1);

        // sink_b flushes 20 into the queue
        assert_eq!(
            Pin::new(&mut sink_b).poll_ready(&mut cx_b),
            Poll::Ready(Ok(()))
        );
        assert_eq!(consumer.try_recv(), Ok(Some(20)));
    }

    #[cfg(feature = "sink")]
    #[test]
    fn sink_flush_is_immediately_ready() {
        let (producer, consumer) = channel(nz(2));
        let mut sink = producer.into_sink();

        let (waker, _wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(Pin::new(&mut sink).poll_ready(&mut cx), Poll::Ready(Ok(())));
        assert_eq!(Pin::new(&mut sink).start_send(9), Ok(()));
        assert_eq!(Pin::new(&mut sink).poll_flush(&mut cx), Poll::Ready(Ok(())));

        assert_eq!(consumer.try_recv(), Ok(Some(9)));
    }

    #[cfg(feature = "sink")]
    #[test]
    fn sink_close_wakes_pending_recv() {
        let (producer, mut consumer) = channel::<i32>(nz(2));
        let mut sink = producer.into_sink();

        let mut recv_fut = Box::pin(consumer.recv());
        let (recv_waker, recv_wake_count) = new_count_waker();
        let mut recv_cx = Context::from_waker(&recv_waker);

        assert_eq!(recv_fut.as_mut().poll(&mut recv_cx), Poll::Pending);
        assert_eq!(recv_wake_count.get(), 0);

        let (sink_waker, _sink_wake_count) = new_count_waker();
        let mut sink_cx = Context::from_waker(&sink_waker);

        assert_eq!(
            Pin::new(&mut sink).poll_close(&mut sink_cx),
            Poll::Ready(Ok(()))
        );
        assert_eq!(recv_wake_count.get(), 1);
        assert_eq!(
            recv_fut.as_mut().poll(&mut recv_cx),
            Poll::Ready(Err(Closed))
        );
    }

    #[cfg(feature = "sink")]
    #[test]
    fn sink_ready_and_start_send_return_closed_after_close() {
        let (producer, consumer) = channel::<i32>(nz(2));
        drop(consumer);
        let mut sink = producer.into_sink();

        let (waker, _wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(
            Pin::new(&mut sink).poll_ready(&mut cx),
            Poll::Ready(Err(Closed))
        );
        assert_eq!(Pin::new(&mut sink).start_send(5), Err(Closed));
    }

    #[cfg(feature = "sink")]
    #[test]
    fn sink_poll_close_reports_closed_when_buffered_item_cannot_flush() {
        let (producer, consumer) = channel::<i32>(nz(2));
        let mut sink = producer.into_sink();

        let (waker, _wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        // Buffer an item via the normal poll_ready → start_send sequence.
        assert_eq!(Pin::new(&mut sink).poll_ready(&mut cx), Poll::Ready(Ok(())));
        assert_eq!(Pin::new(&mut sink).start_send(42), Ok(()));

        // Close the channel before the buffered item is flushed.
        drop(consumer);

        // poll_close should report the failure to flush the buffered item.
        assert_eq!(
            Pin::new(&mut sink).poll_close(&mut cx),
            Poll::Ready(Err(Closed))
        );
    }

    #[cfg(feature = "sink")]
    #[test]
    fn dropped_sink_waker_slot_wakes_next_producer() {
        let (producer_a, consumer) = channel(nz(1));
        let producer_b = producer_a.clone();
        let mut sink_a = producer_a.into_sink();
        let mut sink_b = producer_b.into_sink();

        let (waker_a, _wake_a) = new_count_waker();
        let mut cx_a = Context::from_waker(&waker_a);
        let (waker_b, wake_b) = new_count_waker();
        let mut cx_b = Context::from_waker(&waker_b);

        // Fill queue and buffer items to make both sinks pending
        assert_eq!(
            Pin::new(&mut sink_a).poll_ready(&mut cx_a),
            Poll::Ready(Ok(()))
        );
        assert_eq!(Pin::new(&mut sink_a).start_send(1), Ok(()));
        assert_eq!(
            Pin::new(&mut sink_a).poll_ready(&mut cx_a),
            Poll::Ready(Ok(()))
        );
        assert_eq!(Pin::new(&mut sink_a).start_send(10), Ok(()));
        assert_eq!(Pin::new(&mut sink_a).poll_ready(&mut cx_a), Poll::Pending);

        assert_eq!(
            Pin::new(&mut sink_b).poll_ready(&mut cx_b),
            Poll::Ready(Ok(()))
        );
        assert_eq!(Pin::new(&mut sink_b).start_send(20), Ok(()));
        assert_eq!(Pin::new(&mut sink_b).poll_ready(&mut cx_b), Poll::Pending);

        // Consume and drop sink_a — should wake sink_b
        assert_eq!(consumer.try_recv(), Ok(Some(1)));
        drop(sink_a);

        assert_eq!(wake_b.get(), 1);
        assert_eq!(
            Pin::new(&mut sink_b).poll_ready(&mut cx_b),
            Poll::Ready(Ok(()))
        );
    }

    #[cfg(feature = "stream")]
    #[test]
    fn stream_pending_then_woken_by_send() {
        let (producer, mut consumer) = channel(nz(2));

        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(Pin::new(&mut consumer).poll_next(&mut cx), Poll::Pending);
        assert_eq!(wake_count.get(), 0);

        assert_eq!(producer.try_send(7), Ok(()));
        assert_eq!(wake_count.get(), 1);

        assert_eq!(
            Pin::new(&mut consumer).poll_next(&mut cx),
            Poll::Ready(Some(7))
        );
    }

    #[cfg(feature = "stream")]
    #[test]
    fn stream_drains_then_returns_none_after_last_producer_drop() {
        let (producer, mut consumer) = channel(nz(4));
        let producer_clone = producer.clone();

        assert_eq!(producer.try_send(1), Ok(()));
        assert_eq!(producer_clone.try_send(2), Ok(()));
        drop(producer);
        drop(producer_clone);

        block_on(async {
            assert_eq!(consumer.next().await, Some(1));
            assert_eq!(consumer.next().await, Some(2));
            assert_eq!(consumer.next().await, None);
        });
    }

    #[cfg(feature = "stream")]
    #[test]
    fn dropped_stream_consumer_clears_waker_slot() {
        let (producer, mut consumer) = channel::<i32>(nz(2));

        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(Pin::new(&mut consumer).poll_next(&mut cx), Poll::Pending);
        assert_eq!(wake_count.get(), 0);

        drop(consumer);
        assert_eq!(wake_count.get(), 1);

        drop(producer);
        assert_eq!(wake_count.get(), 1);
    }
}

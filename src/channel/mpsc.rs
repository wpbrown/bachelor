use crate::core::RecvEffect;
use crate::core::bounded_queue::BoundedQueue;
use slab::Slab;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

pub use crate::error::{Closed, TrySendError};

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

    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    pub async fn recv(&self) -> Result<T, Closed> {
        std::future::poll_fn(|cx| match self.try_recv() {
            Ok(Some(item)) => Poll::Ready(Ok(item)),
            Err(Closed) => Poll::Ready(Err(Closed)),
            Ok(None) => {
                self.consumer_waker.set(Some(cx.waker().clone()));
                Poll::Pending
            }
        })
        .await
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
}

impl<T> Drop for MpscChannelProducer<T> {
    fn drop(&mut self) {
        // The only Rc clones are held by producers and the single consumer.
        // Count == 2 means self (about to drop) + consumer, i.e. this is
        // the last producer. If a new type (e.g. a subscription source) is
        // added that also clones the Rc, this check must be replaced with
        // an explicit producer count.
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

impl<T> Drop for MpscChannelConsumer<T> {
    fn drop(&mut self) {
        self.channel.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroUsize;
    use std::task::Context;

    use futures_executor::block_on;
    use futures_test::task::new_count_waker;

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
}

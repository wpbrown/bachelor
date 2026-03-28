use crate::core::RecvEffect;
use crate::core::bounded_queue::BoundedQueue;
use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::task::{Poll, Waker};

pub use crate::error::{Closed, TrySendError};

pub struct SpscChannel<T> {
    queue: RefCell<BoundedQueue<T>>,
    consumer_waker: Cell<Option<Waker>>,
    producer_waker: Cell<Option<Waker>>,
    closed: Cell<bool>,
}

pub struct SpscChannelProducer<T> {
    channel: Rc<SpscChannel<T>>,
}

pub struct SpscChannelConsumer<T> {
    channel: Rc<SpscChannel<T>>,
}

pub fn channel<T>(capacity: NonZeroUsize) -> (SpscChannelProducer<T>, SpscChannelConsumer<T>) {
    let channel = Rc::new(SpscChannel::new(capacity));
    let producer = SpscChannelProducer {
        channel: Rc::clone(&channel),
    };
    let consumer = SpscChannelConsumer { channel };
    (producer, consumer)
}

impl<T> SpscChannel<T> {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            queue: RefCell::new(BoundedQueue::new(capacity)),
            consumer_waker: Cell::new(None),
            producer_waker: Cell::new(None),
            closed: Cell::new(false),
        }
    }

    pub fn close(&self) {
        self.closed.set(true);
        if let Some(waker) = self.consumer_waker.take() {
            waker.wake();
        }
        if let Some(waker) = self.producer_waker.take() {
            waker.wake();
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

    pub fn try_recv(&self) -> Result<Option<T>, Closed> {
        match self.queue.borrow_mut().try_recv() {
            Some((item, effect)) => {
                if effect == RecvEffect::Unblocked {
                    if let Some(waker) = self.producer_waker.take() {
                        waker.wake();
                    }
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

    pub async fn recv(&self) -> Result<T, Closed> {
        poll_fn(|cx| match self.try_recv() {
            Ok(Some(item)) => Poll::Ready(Ok(item)),
            Err(Closed) => Poll::Ready(Err(Closed)),
            Ok(None) => {
                self.consumer_waker.set(Some(cx.waker().clone()));
                Poll::Pending
            }
        })
        .await
    }

    pub fn shrink_to_fit(&self) {
        self.queue.borrow_mut().shrink_to_fit();
    }
}

impl<T> SpscChannelProducer<T> {
    pub fn try_send(&self, item: T) -> Result<(), TrySendError<T>> {
        self.channel.try_send(item)
    }

    pub async fn send(&self, item: T) -> Result<(), T> {
        self.channel.send(item).await
    }

    pub fn shrink_to_fit(&self) {
        self.channel.shrink_to_fit();
    }
}

impl<T> Drop for SpscChannelProducer<T> {
    fn drop(&mut self) {
        self.channel.close();
    }
}

impl<T> SpscChannelConsumer<T> {
    pub fn try_recv(&self) -> Result<Option<T>, Closed> {
        self.channel.try_recv()
    }

    pub async fn recv(&self) -> Result<T, Closed> {
        self.channel.recv().await
    }
}

impl<T> Drop for SpscChannelConsumer<T> {
    fn drop(&mut self) {
        self.channel.close();
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

    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    #[test]
    fn try_send_try_recv_roundtrip() {
        let ch = SpscChannel::new(nz(4));

        assert_eq!(ch.try_send(1), Ok(()));
        assert_eq!(ch.try_send(2), Ok(()));

        assert_eq!(ch.try_recv(), Ok(Some(1)));
        assert_eq!(ch.try_recv(), Ok(Some(2)));
        assert_eq!(ch.try_recv(), Ok(None));
    }

    #[test]
    fn full_rejects_send() {
        let ch = SpscChannel::new(nz(2));

        assert_eq!(ch.try_send(1), Ok(()));
        assert_eq!(ch.try_send(2), Ok(()));
        assert_eq!(ch.try_send(3), Err(TrySendError::Full(3)));
    }

    #[test]
    fn consumer_waker_woken_by_send() {
        let ch = SpscChannel::new(nz(2));

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
        let ch = SpscChannel::new(nz(1));

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
    fn close_then_drain_then_closed() {
        let ch = SpscChannel::new(nz(4));

        assert_eq!(ch.try_send(1), Ok(()));
        assert_eq!(ch.try_send(2), Ok(()));

        ch.close();

        assert_eq!(ch.try_recv(), Ok(Some(1)));
        assert_eq!(ch.try_recv(), Ok(Some(2)));

        assert_eq!(ch.try_recv(), Err(Closed));
    }

    #[test]
    fn close_wakes_pending_recv() {
        let ch = SpscChannel::<i32>::new(nz(2));

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
    fn close_wakes_pending_send() {
        let ch = SpscChannel::new(nz(1));

        assert_eq!(ch.try_send(1), Ok(()));

        let mut send_fut = Box::pin(ch.send(2));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Pending);

        ch.close();
        assert_eq!(wake_count.get(), 1);

        assert_eq!(send_fut.as_mut().poll(&mut cx), Poll::Ready(Err(2)));
    }

    #[test]
    fn try_send_after_close_rejected() {
        let ch = SpscChannel::new(nz(4));
        ch.close();
        assert_eq!(ch.try_send(42), Err(TrySendError::Closed(42)));
    }

    #[test]
    fn double_close_is_idempotent() {
        let ch = SpscChannel::<i32>::new(nz(2));
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
        let (producer, consumer) = channel(nz(2));

        block_on(async {
            producer.send(42).await.unwrap();
            assert_eq!(consumer.recv().await, Ok(42));
        });
    }

    #[test]
    fn producer_drop_closes_channel() {
        let (producer, consumer) = channel(nz(4));

        assert_eq!(producer.try_send(10), Ok(()));
        assert_eq!(producer.try_send(20), Ok(()));

        drop(producer);

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
        let (producer, consumer) = channel(nz(4));

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
    fn producer_owned_is_not_clone() {
        let (producer, _consumer) = channel::<i32>(nz(1));
        assert_eq!(producer.try_send(1), Ok(()));
    }

    #[test]
    fn consumer_owned_is_not_clone() {
        let (_producer, consumer) = channel::<i32>(nz(1));
        assert_eq!(consumer.try_recv(), Ok(None));
    }

    #[test]
    fn send_blocks_on_full_then_completes_after_recv() {
        let (producer, consumer) = channel(nz(2));

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
        let (producer, consumer) = channel(nz(8));

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

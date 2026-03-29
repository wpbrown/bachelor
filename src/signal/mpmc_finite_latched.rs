use slab::Slab;
use smallvec::SmallVec;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use crate::error::Closed;

const CLOSED_BIT: u64 = 1;
const STEP_SIZE: u64 = 2;

pub struct MpmcFiniteLatchedSignalConsumerKey {
    last_generation: u64,
}

pub struct MpmcFiniteLatchedSignal {
    state: Cell<u64>,
    wakers: RefCell<Slab<Waker>>,
}

impl Default for MpmcFiniteLatchedSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl MpmcFiniteLatchedSignal {
    pub fn new() -> Self {
        Self {
            state: Cell::new(0),
            wakers: RefCell::new(Slab::new()),
        }
    }

    pub fn generation(&self) -> u64 {
        self.state.get() & !CLOSED_BIT
    }

    pub fn is_closed(&self) -> bool {
        (self.state.get() & CLOSED_BIT) != 0
    }

    pub fn notify(&self) -> Result<(), Closed> {
        if self.is_closed() {
            return Err(Closed);
        }
        self.state
            .set(self.state.get().checked_add(STEP_SIZE).expect(
                "finite latched signal generation overflowed u64",
            ));
        self.wake_all();
        Ok(())
    }

    pub fn close(&self) {
        self.state.set(self.state.get() | CLOSED_BIT);
        self.wake_all();
    }

    pub fn subscribe(&self) -> MpmcFiniteLatchedSignalConsumerKey {
        MpmcFiniteLatchedSignalConsumerKey {
            last_generation: 0,
        }
    }

    pub fn subscribe_forward(&self) -> MpmcFiniteLatchedSignalConsumerKey {
        MpmcFiniteLatchedSignalConsumerKey {
            last_generation: self.generation(),
        }
    }

    pub fn observe<'a, 'b>(
        &'a self,
        key: &'b mut MpmcFiniteLatchedSignalConsumerKey,
    ) -> Wait<'a, 'b> {
        Wait {
            signal: self,
            key,
            waker_key: None,
        }
    }

    pub fn shrink_to_fit(&self) {
        self.wakers.borrow_mut().shrink_to_fit();
    }

    fn wake_all(&self) {
        let wakers: SmallVec<[Waker; 16]> = {
            let mut slab = self.wakers.borrow_mut();
            slab.iter_mut().map(|(_, w)| w.clone()).collect()
        };
        for waker in wakers {
            waker.wake();
        }
    }
}

pub struct Wait<'a, 'b> {
    signal: &'a MpmcFiniteLatchedSignal,
    key: &'b mut MpmcFiniteLatchedSignalConsumerKey,
    waker_key: Option<usize>,
}

impl Future for Wait<'_, '_> {
    type Output = Result<(), Closed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let state = this.signal.state.get();
        let generation = state & !CLOSED_BIT;

        if generation != this.key.last_generation {
            this.key.last_generation = generation;
            Poll::Ready(Ok(()))
        } else if (state & CLOSED_BIT) != 0 {
            Poll::Ready(Err(Closed))
        } else {
            let waker = cx.waker().clone();
            let mut wakers = this.signal.wakers.borrow_mut();
            match this.waker_key {
                Some(key) => wakers[key] = waker,
                None => this.waker_key = Some(wakers.insert(waker)),
            }
            Poll::Pending
        }
    }
}

impl Drop for Wait<'_, '_> {
    fn drop(&mut self) {
        if let Some(key) = self.waker_key {
            self.signal.wakers.borrow_mut().remove(key);
        }
    }
}

use std::rc::Rc;

struct Inner {
    signal: MpmcFiniteLatchedSignal,
    producer_count: Cell<usize>,
}

impl Inner {
    /// Number of receiver-side Rc holders (sources + consumers),
    /// assuming `self` is about to be dropped by one of them.
    fn receiver_count_after_drop(self: &Rc<Self>) -> usize {
        Rc::strong_count(self) - 1 - self.producer_count.get()
    }
}

pub fn signal() -> (
    MpmcFiniteLatchedSignalProducer,
    MpmcFiniteLatchedSignalSource,
) {
    let inner = Rc::new(Inner {
        signal: MpmcFiniteLatchedSignal::new(),
        producer_count: Cell::new(1),
    });
    (
        MpmcFiniteLatchedSignalProducer {
            inner: Rc::clone(&inner),
        },
        MpmcFiniteLatchedSignalSource { inner },
    )
}

#[derive(Clone)]
pub struct MpmcFiniteLatchedSignalSource {
    inner: Rc<Inner>,
}

impl MpmcFiniteLatchedSignalSource {
    pub fn subscribe(&self) -> MpmcFiniteLatchedSignalConsumer {
        let key = self.inner.signal.subscribe();
        MpmcFiniteLatchedSignalConsumer {
            inner: Rc::clone(&self.inner),
            key,
        }
    }

    pub fn subscribe_forward(&self) -> MpmcFiniteLatchedSignalConsumer {
        let key = self.inner.signal.subscribe_forward();
        MpmcFiniteLatchedSignalConsumer {
            inner: Rc::clone(&self.inner),
            key,
        }
    }
}

impl Drop for MpmcFiniteLatchedSignalSource {
    fn drop(&mut self) {
        if self.inner.receiver_count_after_drop() == 0 {
            self.inner.signal.close();
        }
    }
}

pub struct MpmcFiniteLatchedSignalProducer {
    inner: Rc<Inner>,
}

impl Clone for MpmcFiniteLatchedSignalProducer {
    fn clone(&self) -> Self {
        self.inner
            .producer_count
            .set(self.inner.producer_count.get() + 1);
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl MpmcFiniteLatchedSignalProducer {
    pub fn notify(&self) -> Result<(), Closed> {
        self.inner.signal.notify()
    }

    pub fn close(&self) {
        self.inner.signal.close();
    }
}

impl Drop for MpmcFiniteLatchedSignalProducer {
    fn drop(&mut self) {
        let count = self.inner.producer_count.get() - 1;
        self.inner.producer_count.set(count);
        if count == 0 {
            self.inner.signal.close();
        }
    }
}

pub struct MpmcFiniteLatchedSignalConsumer {
    inner: Rc<Inner>,
    key: MpmcFiniteLatchedSignalConsumerKey,
}

impl MpmcFiniteLatchedSignalConsumer {
    pub fn observe(&mut self) -> Wait<'_, '_> {
        self.inner.signal.observe(&mut self.key)
    }

    pub fn observe_forward(&mut self) -> Wait<'_, '_> {
        self.key.last_generation = self.inner.signal.generation();
        self.inner.signal.observe(&mut self.key)
    }
}

impl Drop for MpmcFiniteLatchedSignalConsumer {
    fn drop(&mut self) {
        if self.inner.receiver_count_after_drop() == 0 {
            self.inner.signal.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll};

    use futures_test::task::new_count_waker;

    #[test]
    fn notify_resolves_ok() {
        let sig = MpmcFiniteLatchedSignal::new();
        let mut key = sig.subscribe_forward();

        let mut fut = Box::pin(sig.observe(&mut key));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        sig.notify().unwrap();
        assert_eq!(wake_count.get(), 1);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn close_resolves_err() {
        let sig = MpmcFiniteLatchedSignal::new();
        let mut key = sig.subscribe_forward();

        let mut fut = Box::pin(sig.observe(&mut key));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        sig.close();
        assert_eq!(wake_count.get(), 1);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn changed_before_closed_delivers_ok_first() {
        let sig = MpmcFiniteLatchedSignal::new();
        let mut key = sig.subscribe_forward();

        sig.notify().unwrap();
        sig.close();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        let mut fut = Box::pin(sig.observe(&mut key));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
        drop(fut);

        let mut fut = Box::pin(sig.observe(&mut key));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn multi_consumer_independence() {
        let sig = MpmcFiniteLatchedSignal::new();
        let mut k1 = sig.subscribe_forward();
        let mut k2 = sig.subscribe_forward();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        sig.notify().unwrap();

        let mut f1 = Box::pin(sig.observe(&mut k1));
        assert_eq!(f1.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
        drop(f1);

        let mut f2 = Box::pin(sig.observe(&mut k2));
        assert_eq!(f2.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
        drop(f2);

        let mut f1 = Box::pin(sig.observe(&mut k1));
        assert_eq!(f1.as_mut().poll(&mut cx), Poll::Pending);
        drop(f1);

        let mut f2 = Box::pin(sig.observe(&mut k2));
        assert_eq!(f2.as_mut().poll(&mut cx), Poll::Pending);
    }

    #[test]
    fn subscribe_vs_subscribe_forward() {
        let sig = MpmcFiniteLatchedSignal::new();
        sig.notify().unwrap();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        let mut k_old = sig.subscribe();
        let mut fut = Box::pin(sig.observe(&mut k_old));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut k_new = sig.subscribe_forward();
        let mut fut = Box::pin(sig.observe(&mut k_new));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
    }

    #[test]
    fn lsb_isolation_generation_and_closed() {
        let sig = MpmcFiniteLatchedSignal::new();

        assert_eq!(sig.generation(), 0);
        assert!(!sig.is_closed());

        sig.notify().unwrap();
        assert_eq!(sig.generation(), 2);
        assert!(!sig.is_closed());

        sig.close();
        assert_eq!(sig.generation(), 2);
        assert!(sig.is_closed());

        assert_eq!(sig.notify(), Err(Closed));
        assert_eq!(sig.generation(), 2);
        assert!(sig.is_closed());
    }

    #[test]
    fn drop_clears_waker_slot() {
        let sig = MpmcFiniteLatchedSignal::new();
        let mut key = sig.subscribe_forward();

        let mut fut = Box::pin(sig.observe(&mut key));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
        drop(fut);

        sig.notify().unwrap();
        assert_eq!(wake_count.get(), 0);
    }

    #[test]
    fn notify_after_close_is_noop() {
        let sig = MpmcFiniteLatchedSignal::new();
        let mut key = sig.subscribe_forward();
        sig.close();
        assert_eq!(sig.notify(), Err(Closed));

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        let mut fut = Box::pin(sig.observe(&mut key));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn waker_slab_is_cleaned_up() {
        let sig = MpmcFiniteLatchedSignal::new();
        let mut key = sig.subscribe_forward();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        for _ in 0..10 {
            let mut fut = Box::pin(sig.observe(&mut key));
            assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
            drop(fut);
        }

        sig.shrink_to_fit();

        sig.notify().unwrap();
        let mut fut = Box::pin(sig.observe(&mut key));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }
}

use slab::Slab;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use crate::error::Closed;

const CLOSED_BIT: usize = 1;
const STEP_SIZE: usize = 2;

pub struct MpmcFiniteLatchedSignalConsumerKey {
    last_generation: Cell<usize>,
}

pub struct MpmcFiniteLatchedSignal {
    state: Cell<usize>,
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

    pub fn generation(&self) -> usize {
        self.state.get() & !CLOSED_BIT
    }

    pub fn is_closed(&self) -> bool {
        (self.state.get() & CLOSED_BIT) != 0
    }

    pub fn notify(&self) {
        self.state.set(self.state.get().wrapping_add(STEP_SIZE));
        self.wake_all();
    }

    pub fn close(&self) {
        self.state.set(self.state.get() | CLOSED_BIT);
        self.wake_all();
    }

    pub fn subscribe(&self) -> MpmcFiniteLatchedSignalConsumerKey {
        MpmcFiniteLatchedSignalConsumerKey {
            last_generation: Cell::new(0),
        }
    }

    pub fn subscribe_forward(&self) -> MpmcFiniteLatchedSignalConsumerKey {
        MpmcFiniteLatchedSignalConsumerKey {
            last_generation: Cell::new(self.generation()),
        }
    }

    pub fn observe<'a>(&'a self, key: &'a MpmcFiniteLatchedSignalConsumerKey) -> Wait<'a> {
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
        let wakers = self.wakers.borrow();
        for (_, waker) in wakers.iter() {
            waker.wake_by_ref();
        }
    }
}

pub struct Wait<'a> {
    signal: &'a MpmcFiniteLatchedSignal,
    key: &'a MpmcFiniteLatchedSignalConsumerKey,
    waker_key: Option<usize>,
}

impl Future for Wait<'_> {
    type Output = Result<(), Closed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let state = this.signal.state.get();
        let generation = state & !CLOSED_BIT;

        if generation != this.key.last_generation.get() {
            this.key.last_generation.set(generation);
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

impl Drop for Wait<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.waker_key {
            self.signal.wakers.borrow_mut().remove(key);
        }
    }
}

use std::rc::Rc;

pub fn signal() -> (
    MpmcFiniteLatchedSignalProducer,
    MpmcFiniteLatchedSignalSource,
) {
    let inner = Rc::new(MpmcFiniteLatchedSignal::new());
    (
        MpmcFiniteLatchedSignalProducer {
            inner: inner.clone(),
        },
        MpmcFiniteLatchedSignalSource { inner },
    )
}

#[derive(Clone)]
pub struct MpmcFiniteLatchedSignalSource {
    inner: Rc<MpmcFiniteLatchedSignal>,
}

impl MpmcFiniteLatchedSignalSource {
    pub fn subscribe(&self) -> MpmcFiniteLatchedSignalConsumer {
        let key = self.inner.subscribe();
        MpmcFiniteLatchedSignalConsumer {
            inner: self.inner.clone(),
            key,
        }
    }

    pub fn subscribe_forward(&self) -> MpmcFiniteLatchedSignalConsumer {
        let key = self.inner.subscribe_forward();
        MpmcFiniteLatchedSignalConsumer {
            inner: self.inner.clone(),
            key,
        }
    }
}

#[derive(Clone)]
pub struct MpmcFiniteLatchedSignalProducer {
    inner: Rc<MpmcFiniteLatchedSignal>,
}

impl MpmcFiniteLatchedSignalProducer {
    pub fn notify(&self) {
        self.inner.notify();
    }

    pub fn close(&self) {
        self.inner.close();
    }
}

pub struct MpmcFiniteLatchedSignalConsumer {
    inner: Rc<MpmcFiniteLatchedSignal>,
    key: MpmcFiniteLatchedSignalConsumerKey,
}

impl MpmcFiniteLatchedSignalConsumer {
    pub fn observe(&self) -> Wait<'_> {
        self.inner.observe(&self.key)
    }

    pub fn observe_forward(&self) -> Wait<'_> {
        self.key.last_generation.set(self.inner.generation());
        self.inner.observe(&self.key)
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
        let key = sig.subscribe_forward();

        let mut fut = Box::pin(sig.observe(&key));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        sig.notify();
        assert_eq!(wake_count.get(), 1);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn close_resolves_err() {
        let sig = MpmcFiniteLatchedSignal::new();
        let key = sig.subscribe_forward();

        let mut fut = Box::pin(sig.observe(&key));
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
        let key = sig.subscribe_forward();

        sig.notify();
        sig.close();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        let mut fut = Box::pin(sig.observe(&key));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut fut = Box::pin(sig.observe(&key));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn multi_consumer_independence() {
        let sig = MpmcFiniteLatchedSignal::new();
        let k1 = sig.subscribe_forward();
        let k2 = sig.subscribe_forward();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        sig.notify();

        let mut f1 = Box::pin(sig.observe(&k1));
        assert_eq!(f1.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut f2 = Box::pin(sig.observe(&k2));
        assert_eq!(f2.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut f1 = Box::pin(sig.observe(&k1));
        assert_eq!(f1.as_mut().poll(&mut cx), Poll::Pending);

        let mut f2 = Box::pin(sig.observe(&k2));
        assert_eq!(f2.as_mut().poll(&mut cx), Poll::Pending);
    }

    #[test]
    fn subscribe_vs_subscribe_forward() {
        let sig = MpmcFiniteLatchedSignal::new();
        sig.notify();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        let k_old = sig.subscribe();
        let mut fut = Box::pin(sig.observe(&k_old));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let k_new = sig.subscribe_forward();
        let mut fut = Box::pin(sig.observe(&k_new));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
    }

    #[test]
    fn lsb_isolation_generation_and_closed() {
        let sig = MpmcFiniteLatchedSignal::new();

        assert_eq!(sig.generation(), 0);
        assert!(!sig.is_closed());

        sig.notify();
        assert_eq!(sig.generation(), 2);
        assert!(!sig.is_closed());

        sig.close();
        assert_eq!(sig.generation(), 2);
        assert!(sig.is_closed());

        sig.notify();
        assert_eq!(sig.generation(), 4);
        assert!(sig.is_closed());
    }

    #[test]
    fn drop_clears_waker_slot() {
        let sig = MpmcFiniteLatchedSignal::new();
        let key = sig.subscribe_forward();

        let mut fut = Box::pin(sig.observe(&key));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
        drop(fut);

        sig.notify();
        assert_eq!(wake_count.get(), 0);
    }

    #[test]
    fn waker_slab_is_cleaned_up() {
        let sig = MpmcFiniteLatchedSignal::new();
        let key = sig.subscribe_forward();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        for _ in 0..10 {
            let mut fut = Box::pin(sig.observe(&key));
            assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
            drop(fut);
        }

        sig.shrink_to_fit();

        sig.notify();
        let mut fut = Box::pin(sig.observe(&key));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }
}

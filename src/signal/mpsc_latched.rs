use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

/// A single-consumer latched signal for async notification.
///
/// See the [single-waker contract](crate#single-waker-contract)
/// before using this type directly.
pub struct MpscLatchedSignal {
    waker: Cell<Option<Waker>>,
    signaled: Cell<bool>,
}

impl Default for MpscLatchedSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl MpscLatchedSignal {
    pub fn new() -> Self {
        Self {
            waker: Cell::new(None),
            signaled: Cell::new(false),
        }
    }

    pub fn notify(&self) {
        self.signaled.set(true);
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    /// Returns a future that resolves when the signal is (or has been) notified.
    ///
    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    pub fn observe(&self) -> Wait<'_> {
        Wait { signal: self }
    }

    /// Like [`observe`](Self::observe), but ignores any prior signal state.
    ///
    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    pub fn observe_forward(&self) -> Wait<'_> {
        self.signaled.set(false);
        self.observe()
    }
}

pub struct Wait<'a> {
    signal: &'a MpscLatchedSignal,
}

impl<'a> Future for Wait<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let signal = this.signal;

        if signal.signaled.get() {
            signal.signaled.set(false);
            Poll::Ready(())
        } else {
            signal.waker.set(Some(cx.waker().clone()));
            Poll::Pending
        }
    }
}

impl<'a> Drop for Wait<'a> {
    fn drop(&mut self) {
        self.signal.waker.take();
    }
}

pub fn signal() -> (MpscLatchedSignalProducer, MpscLatchedSignalConsumer) {
    let inner = Rc::new(MpscLatchedSignal::new());
    (
        MpscLatchedSignalProducer {
            inner: inner.clone(),
        },
        MpscLatchedSignalConsumer { inner },
    )
}

#[derive(Clone)]
pub struct MpscLatchedSignalProducer {
    inner: Rc<MpscLatchedSignal>,
}

impl MpscLatchedSignalProducer {
    pub fn notify(&self) {
        self.inner.notify();
    }
}

pub struct MpscLatchedSignalConsumer {
    inner: Rc<MpscLatchedSignal>,
}

impl MpscLatchedSignalConsumer {
    pub fn observe(&mut self) -> Wait<'_> {
        self.inner.observe()
    }

    pub fn observe_forward(&mut self) -> Wait<'_> {
        self.inner.observe_forward()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures_test::task::new_count_waker;

    #[test]
    fn observe_forward_ignores_prior_signal() {
        let sig = MpscLatchedSignal::new();
        sig.notify();

        let mut fut = Box::pin(sig.observe_forward());
        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        sig.notify();
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(()));
    }

    #[test]
    fn observe_forward_does_not_clear_pending_waiter() {
        let sig = MpscLatchedSignal::new();

        let mut fut = Box::pin(sig.observe());
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        let ignored = sig.observe_forward();

        sig.notify();
        assert_eq!(wake_count.get(), 1);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(()));

        drop(ignored);
    }
}

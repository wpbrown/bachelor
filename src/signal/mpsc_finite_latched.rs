use bitflags::bitflags;
use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use crate::error::Closed;

bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct Flags: u8 {
        const CHANGED = 0b01;
        const CLOSED  = 0b10;
    }
}

/// A single-consumer latched signal with finite lifetime semantics.
///
/// See the [single-waker contract](crate#single-waker-contract)
/// before using this type directly.
pub struct MpscFiniteLatchedSignal {
    state: Cell<Flags>,
    waker: Cell<Option<Waker>>,
}

impl Default for MpscFiniteLatchedSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl MpscFiniteLatchedSignal {
    pub fn new() -> Self {
        Self {
            state: Cell::new(Flags::empty()),
            waker: Cell::new(None),
        }
    }

    pub fn notify(&self) -> Result<(), Closed> {
        if self.state.get().contains(Flags::CLOSED) {
            return Err(Closed);
        }
        self.state.set(self.state.get() | Flags::CHANGED);
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
        Ok(())
    }

    pub fn close(&self) {
        self.state.set(self.state.get() | Flags::CLOSED);
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.state.get().contains(Flags::CLOSED)
    }

    /// Returns a future that resolves when the signal changes or closes.
    ///
    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    pub fn observe(&self) -> Wait<'_> {
        Wait { signal: self }
    }

    /// Like [`observe`](Self::observe), but ignores any prior change state.
    ///
    /// If the signal is already closed, the returned future still resolves
    /// to [`Err(Closed)`](Err).
    ///
    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    pub fn observe_forward(&self) -> Wait<'_> {
        self.state.set(self.state.get() - Flags::CHANGED);
        self.observe()
    }
}

pub struct Wait<'a> {
    signal: &'a MpscFiniteLatchedSignal,
}

impl Future for Wait<'_> {
    type Output = Result<(), Closed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let signal = self.get_mut().signal;
        let state = signal.state.get();

        if state.contains(Flags::CHANGED) {
            signal.state.set(state - Flags::CHANGED);
            Poll::Ready(Ok(()))
        } else if state.contains(Flags::CLOSED) {
            Poll::Ready(Err(Closed))
        } else {
            signal.waker.set(Some(cx.waker().clone()));
            Poll::Pending
        }
    }
}

impl Drop for Wait<'_> {
    fn drop(&mut self) {
        self.signal.waker.take();
    }
}

use std::rc::Rc;

pub fn signal() -> (
    MpscFiniteLatchedSignalProducer,
    MpscFiniteLatchedSignalConsumer,
) {
    let inner = Rc::new(MpscFiniteLatchedSignal::new());
    (
        MpscFiniteLatchedSignalProducer {
            inner: inner.clone(),
        },
        MpscFiniteLatchedSignalConsumer { inner },
    )
}

#[derive(Clone)]
pub struct MpscFiniteLatchedSignalProducer {
    inner: Rc<MpscFiniteLatchedSignal>,
}

impl MpscFiniteLatchedSignalProducer {
    pub fn notify(&self) -> Result<(), Closed> {
        self.inner.notify()
    }

    pub fn close(&self) {
        self.inner.close();
    }
}

impl Drop for MpscFiniteLatchedSignalProducer {
    fn drop(&mut self) {
        // The only Rc clones are held by producers and the single consumer.
        // Count == 2 means self (about to drop) + consumer, i.e. this is
        // the last producer. If a new type (e.g. a subscription source) is
        // added that also clones the Rc, this check must be replaced with
        // an explicit producer count.
        if Rc::strong_count(&self.inner) == 2 {
            self.inner.close();
        }
    }
}

pub struct MpscFiniteLatchedSignalConsumer {
    inner: Rc<MpscFiniteLatchedSignal>,
}

impl MpscFiniteLatchedSignalConsumer {
    pub fn observe(&mut self) -> Wait<'_> {
        self.inner.observe()
    }

    pub fn observe_forward(&mut self) -> Wait<'_> {
        self.inner.observe_forward()
    }
}

impl Drop for MpscFiniteLatchedSignalConsumer {
    fn drop(&mut self) {
        self.inner.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll};

    use futures_test::task::new_count_waker;

    #[test]
    fn notify_resolves_ok() {
        let sig = MpscFiniteLatchedSignal::new();

        let mut fut = Box::pin(sig.observe());
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        sig.notify().unwrap();
        assert_eq!(wake_count.get(), 1);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn close_resolves_err() {
        let sig = MpscFiniteLatchedSignal::new();

        let mut fut = Box::pin(sig.observe());
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        sig.close();
        assert_eq!(wake_count.get(), 1);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn changed_before_closed_delivers_ok_first() {
        let sig = MpscFiniteLatchedSignal::new();

        sig.notify().unwrap();
        sig.close();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        let mut fut = Box::pin(sig.observe());
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut fut = Box::pin(sig.observe());
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn changed_and_closed_between_polls() {
        let sig = MpscFiniteLatchedSignal::new();

        let mut fut = Box::pin(sig.observe());
        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        sig.notify().unwrap();
        sig.close();

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut fut2 = Box::pin(sig.observe());
        assert_eq!(fut2.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn pre_signaled_changed_resolves_immediately() {
        let sig = MpscFiniteLatchedSignal::new();
        sig.notify().unwrap();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        let mut fut = Box::pin(sig.observe());
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn observe_forward_ignores_prior_change() {
        let sig = MpscFiniteLatchedSignal::new();
        sig.notify().unwrap();

        let mut fut = Box::pin(sig.observe_forward());
        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        sig.notify().unwrap();
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn observe_forward_preserves_closed_state() {
        let sig = MpscFiniteLatchedSignal::new();
        sig.notify().unwrap();
        sig.close();

        let mut fut = Box::pin(sig.observe_forward());
        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn observe_forward_does_not_clear_pending_waiter() {
        let sig = MpscFiniteLatchedSignal::new();

        let mut fut = Box::pin(sig.observe());
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        let ignored = sig.observe_forward();

        sig.notify().unwrap();
        assert_eq!(wake_count.get(), 1);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        drop(ignored);
    }

    #[test]
    fn drop_clears_waker() {
        let sig = MpscFiniteLatchedSignal::new();

        let mut fut = Box::pin(sig.observe());
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
        drop(fut);

        sig.notify().unwrap();
        assert_eq!(wake_count.get(), 0);
    }

    #[test]
    fn notify_after_close_is_noop() {
        let sig = MpscFiniteLatchedSignal::new();
        sig.close();
        assert_eq!(sig.notify(), Err(Closed));

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        let mut fut = Box::pin(sig.observe());
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn multiple_changes_coalesce() {
        let sig = MpscFiniteLatchedSignal::new();
        sig.notify().unwrap();
        sig.notify().unwrap();
        sig.notify().unwrap();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        let mut fut = Box::pin(sig.observe());
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut fut2 = Box::pin(sig.observe());
        assert_eq!(fut2.as_mut().poll(&mut cx), Poll::Pending);
    }
}

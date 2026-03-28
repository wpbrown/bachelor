use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

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

    pub fn reset(&self) {
        self.signaled.set(false);
        self.waker.take();
    }

    pub fn observe(&self) -> Wait<'_> {
        Wait { signal: self }
    }

    pub fn observe_forward(&self) -> Wait<'_> {
        self.reset();
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
    pub fn observe(&self) -> Wait<'_> {
        self.inner.observe()
    }

    pub fn observe_forward(&self) -> Wait<'_> {
        self.inner.observe_forward()
    }
}

use slab::Slab;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

pub struct MpmcLatchedSignalConsumerKey {
    last_generation: usize,
}

pub struct MpmcLatchedSignal {
    generation: Cell<usize>,
    wakers: RefCell<Slab<Waker>>,
}

impl Default for MpmcLatchedSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl MpmcLatchedSignal {
    pub fn new() -> Self {
        Self {
            generation: Cell::new(0),
            wakers: RefCell::new(Slab::new()),
        }
    }

    pub fn generation(&self) -> usize {
        self.generation.get()
    }

    pub fn notify(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        let wakers = self.wakers.borrow();
        for (_, waker) in wakers.iter() {
            waker.wake_by_ref();
        }
    }

    pub fn subscribe(&self) -> MpmcLatchedSignalConsumerKey {
        MpmcLatchedSignalConsumerKey {
            last_generation: 0,
        }
    }

    pub fn subscribe_forward(&self) -> MpmcLatchedSignalConsumerKey {
        MpmcLatchedSignalConsumerKey {
            last_generation: self.generation.get(),
        }
    }

    pub fn observe<'a, 'b>(
        &'a self,
        key: &'b mut MpmcLatchedSignalConsumerKey,
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
}

pub struct Wait<'a, 'b> {
    signal: &'a MpmcLatchedSignal,
    key: &'b mut MpmcLatchedSignalConsumerKey,
    waker_key: Option<usize>,
}

impl Future for Wait<'_, '_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let current = this.signal.generation.get();

        if current != this.key.last_generation {
            this.key.last_generation = current;
            Poll::Ready(())
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

pub fn signal() -> (MpmcLatchedSignalProducer, MpmcLatchedSignalSource) {
    let inner = Rc::new(MpmcLatchedSignal::new());
    (
        MpmcLatchedSignalProducer {
            inner: inner.clone(),
        },
        MpmcLatchedSignalSource { inner },
    )
}

#[derive(Clone)]
pub struct MpmcLatchedSignalSource {
    inner: Rc<MpmcLatchedSignal>,
}

impl MpmcLatchedSignalSource {
    pub fn subscribe(&self) -> MpmcLatchedSignalConsumer {
        let key = self.inner.subscribe();
        MpmcLatchedSignalConsumer {
            inner: self.inner.clone(),
            key,
        }
    }

    pub fn subscribe_forward(&self) -> MpmcLatchedSignalConsumer {
        let key = self.inner.subscribe_forward();
        MpmcLatchedSignalConsumer {
            inner: self.inner.clone(),
            key,
        }
    }
}

#[derive(Clone)]
pub struct MpmcLatchedSignalProducer {
    inner: Rc<MpmcLatchedSignal>,
}

impl MpmcLatchedSignalProducer {
    pub fn notify(&self) {
        self.inner.notify();
    }
}

pub struct MpmcLatchedSignalConsumer {
    inner: Rc<MpmcLatchedSignal>,
    key: MpmcLatchedSignalConsumerKey,
}

impl MpmcLatchedSignalConsumer {
    pub fn observe(&mut self) -> Wait<'_, '_> {
        self.inner.observe(&mut self.key)
    }

    pub fn observe_forward(&mut self) -> Wait<'_, '_> {
        self.key.last_generation = self.inner.generation();
        self.inner.observe(&mut self.key)
    }
}

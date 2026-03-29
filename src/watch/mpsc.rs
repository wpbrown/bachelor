use crate::error::Closed;
use crate::signal::mpsc_finite_latched::{self, MpscFiniteLatchedSignal};
use std::cell::RefCell;
use std::rc::Rc;

pub struct MpscWatchRef<T> {
    data: RefCell<T>,
    signal: MpscFiniteLatchedSignal,
}

impl<T> MpscWatchRef<T> {
    pub fn new(initial: T) -> Self {
        Self {
            data: RefCell::new(initial),
            signal: MpscFiniteLatchedSignal::new(),
        }
    }

    /// Applies `f` to the stored value and notifies the consumer.
    ///
    /// # Panics
    ///
    /// Panics if `f` re-entrantly borrows this watch (e.g. calls
    /// [`set`](Self::set), [`view`](Self::view), [`get`](Self::get),
    /// or [`update`](Self::update) on the same instance).
    /// Use [`get`](Self::get) / [`set`](Self::set) before or after the
    /// closure if you need additional access.
    pub fn update(&self, f: impl FnOnce(&mut T)) -> Result<(), Closed> {
        if self.signal.is_closed() {
            return Err(Closed);
        }
        f(&mut self.data.borrow_mut());
        self.signal.notify()
    }

    /// Passes a shared reference to the stored value into `f` and returns
    /// the result.
    ///
    /// # Panics
    ///
    /// Panics if `f` re-entrantly mutably borrows this watch (e.g. calls
    /// [`update`](Self::update) or [`set`](Self::set) on the same
    /// instance). Read-only calls such as [`get`](Self::get) or nested
    /// [`view`](Self::view) calls are fine.
    pub fn view<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.data.borrow())
    }

    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.data.borrow().clone()
    }

    /// Replaces the stored value and notifies the consumer.
    ///
    /// This is the non-closure counterpart of [`update`](Self::update).
    /// Together with [`get`](Self::get), it provides a way to read and
    /// write without the re-entrancy constraints of the closure-based
    /// API.
    ///
    /// # Panics
    ///
    /// Panics if called while the value is borrowed, e.g. from inside a
    /// [`view`](Self::view) or [`update`](Self::update) closure on the
    /// same instance.
    pub fn set(&self, value: T) -> Result<(), Closed> {
        if self.signal.is_closed() {
            return Err(Closed);
        }
        *self.data.borrow_mut() = value;
        self.signal.notify()
    }

    pub fn close(&self) {
        self.signal.close();
    }

    pub fn is_closed(&self) -> bool {
        self.signal.is_closed()
    }

    /// Returns a future that resolves on the next change or closure.
    ///
    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    pub fn observe(&self) -> mpsc_finite_latched::Wait<'_> {
        self.signal.observe()
    }

    /// Like [`observe`](Self::observe), but ignores any already-latched
    /// change state.
    ///
    /// If the watch is already closed, the returned future still resolves
    /// to [`Err(Closed)`](Err).
    ///
    /// Callers must uphold the [single-waker contract](crate#single-waker-contract).
    pub fn observe_forward(&self) -> mpsc_finite_latched::Wait<'_> {
        self.signal.observe_forward()
    }
}

pub struct MpscWatchRefProducer<T> {
    inner: Rc<MpscWatchRef<T>>,
}

impl<T> Clone for MpscWatchRefProducer<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<T> MpscWatchRefProducer<T> {
    /// See [`MpscWatchRef::update`] for details.
    ///
    /// # Panics
    ///
    /// Panics if `f` re-entrantly borrows this watch.
    pub fn update(&self, f: impl FnOnce(&mut T)) -> Result<(), Closed> {
        self.inner.update(f)
    }

    /// See [`MpscWatchRef::view`] for details.
    ///
    /// # Panics
    ///
    /// Panics if `f` re-entrantly mutably borrows this watch.
    pub fn view<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.inner.view(f)
    }

    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.inner.get()
    }

    /// See [`MpscWatchRef::set`].
    pub fn set(&self, value: T) -> Result<(), Closed> {
        self.inner.set(value)
    }
}

impl<T> Drop for MpscWatchRefProducer<T> {
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

pub struct MpscWatchRefConsumer<T> {
    inner: Rc<MpscWatchRef<T>>,
}

impl<T> MpscWatchRefConsumer<T> {
    pub async fn changed(&mut self) -> Result<(), Closed> {
        self.inner.observe().await
    }

    pub async fn changed_forward(&mut self) -> Result<(), Closed> {
        self.inner.observe_forward().await
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// See [`MpscWatchRef::view`] for details.
    ///
    /// # Panics
    ///
    /// Panics if `f` re-entrantly mutably borrows this watch.
    pub fn view<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.inner.view(f)
    }

    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.inner.get()
    }
}

impl<T> Drop for MpscWatchRefConsumer<T> {
    fn drop(&mut self) {
        self.inner.close();
    }
}

pub fn watch<T>(initial: T) -> (MpscWatchRefProducer<T>, MpscWatchRefConsumer<T>) {
    let inner = Rc::new(MpscWatchRef::new(initial));
    let producer = MpscWatchRefProducer {
        inner: Rc::clone(&inner),
    };
    let consumer = MpscWatchRefConsumer { inner };
    (producer, consumer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll};

    use futures_executor::block_on;
    use futures_test::task::new_count_waker;

    #[test]
    fn update_and_view() {
        let w = MpscWatchRef::new(0);
        w.update(|v| *v = 42).unwrap();
        assert_eq!(w.view(|v| *v), 42);
    }

    #[test]
    fn get_clones_value() {
        let w = MpscWatchRef::new(String::from("hello"));
        assert_eq!(w.get(), "hello");
        w.update(|v| v.push_str(" world")).unwrap();
        assert_eq!(w.get(), "hello world");
    }

    #[test]
    fn observe_resolves_on_update() {
        let w = MpscWatchRef::new(0);

        let mut fut = Box::pin(w.observe());
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        w.update(|v| *v = 1).unwrap();
        assert_eq!(wake_count.get(), 1);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn observe_forward_ignores_prior_change() {
        let w = MpscWatchRef::new(0);
        w.update(|v| *v = 1).unwrap();

        let mut fut = Box::pin(w.observe_forward());
        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        w.update(|v| *v = 2).unwrap();
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn observe_forward_preserves_closed_state() {
        let w = MpscWatchRef::new(0);
        w.update(|v| *v = 1).unwrap();
        w.close();

        let mut fut = Box::pin(w.observe_forward());
        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn close_resolves_err() {
        let w = MpscWatchRef::new(0);

        let mut fut = Box::pin(w.observe());
        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        w.close();
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn update_then_close_delivers_ok_then_err() {
        let w = MpscWatchRef::new(0);

        let mut fut = Box::pin(w.observe());
        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        w.update(|v| *v = 99).unwrap();
        w.close();

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut fut2 = Box::pin(w.observe());
        assert_eq!(fut2.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));

        assert_eq!(w.get(), 99);
    }

    #[test]
    fn channel_update_and_changed() {
        block_on(async {
            let (tx, mut rx) = watch(0);

            tx.update(|v| *v = 42).unwrap();
            rx.changed().await.unwrap();

            assert_eq!(rx.get(), 42);
        });
    }

    #[test]
    fn producer_drop_closes_channel() {
        block_on(async {
            let (tx, mut rx) = watch(0);

            tx.update(|v| *v = 1).unwrap();
            drop(tx);

            rx.changed().await.unwrap();
            assert_eq!(rx.get(), 1);

            assert_eq!(rx.changed().await, Err(Closed));
        });
    }

    #[test]
    fn producer_clone_keeps_channel_open() {
        block_on(async {
            let (tx1, mut rx) = watch(0);
            let tx2 = tx1.clone();

            drop(tx1);

            tx2.update(|v| *v = 7).unwrap();
            rx.changed().await.unwrap();
            assert_eq!(rx.get(), 7);
        });
    }

    #[test]
    fn last_producer_clone_drop_closes() {
        block_on(async {
            let (tx1, mut rx) = watch(0);
            let tx2 = tx1.clone();

            drop(tx1);
            drop(tx2);

            assert_eq!(rx.changed().await, Err(Closed));
        });
    }

    #[test]
    fn consumer_view_delegates() {
        let (tx, rx) = watch(String::from("init"));
        tx.update(|v| *v = String::from("updated")).unwrap();
        assert_eq!(rx.view(|v| v.len()), 7);
    }

    #[test]
    fn producer_view_and_get() {
        let (tx, _rx) = watch(100);
        assert_eq!(tx.get(), 100);
        assert_eq!(tx.view(|v| *v + 1), 101);
    }

    #[test]
    fn changed_wakes_pending_consumer() {
        let (tx, mut rx) = watch(0);

        let mut fut = Box::pin(rx.changed());
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(wake_count.get(), 0);

        tx.update(|v| *v = 1).unwrap();
        assert_eq!(wake_count.get(), 1);
    }

    #[test]
    fn changed_forward_ignores_prior_change() {
        let (tx, mut rx) = watch(0);
        tx.update(|v| *v = 1).unwrap();

        let mut fut = Box::pin(rx.changed_forward());
        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        tx.update(|v| *v = 2).unwrap();
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }
}

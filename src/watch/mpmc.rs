use crate::error::Closed;
use crate::signal::mpmc_finite_latched::{
    self, MpmcFiniteLatchedSignal, MpmcFiniteLatchedSignalConsumerKey,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct MpmcWatchRefConsumerKey(MpmcFiniteLatchedSignalConsumerKey);

pub struct MpmcWatchRef<T> {
    data: RefCell<T>,
    signal: MpmcFiniteLatchedSignal,
}

impl<T> MpmcWatchRef<T> {
    pub fn new(initial: T) -> Self {
        Self {
            data: RefCell::new(initial),
            signal: MpmcFiniteLatchedSignal::new(),
        }
    }

    pub fn update(&self, f: impl FnOnce(&mut T)) {
        f(&mut self.data.borrow_mut());
        self.signal.notify();
    }

    pub fn view<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.data.borrow())
    }

    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.data.borrow().clone()
    }

    pub fn close(&self) {
        self.signal.close();
    }

    pub fn is_closed(&self) -> bool {
        self.signal.is_closed()
    }

    pub fn subscribe(&self) -> MpmcWatchRefConsumerKey {
        MpmcWatchRefConsumerKey(self.signal.subscribe())
    }

    pub fn subscribe_forward(&self) -> MpmcWatchRefConsumerKey {
        MpmcWatchRefConsumerKey(self.signal.subscribe_forward())
    }

    pub fn observe<'a>(
        &'a self,
        key: &'a MpmcWatchRefConsumerKey,
    ) -> mpmc_finite_latched::Wait<'a> {
        self.signal.observe(&key.0)
    }

    pub fn shrink_to_fit(&self) {
        self.signal.shrink_to_fit();
    }
}

struct Inner<T> {
    watch: MpmcWatchRef<T>,
    producer_count: Cell<usize>,
}

pub struct MpmcWatchRefProducer<T> {
    inner: Rc<Inner<T>>,
}

impl<T> Clone for MpmcWatchRefProducer<T> {
    fn clone(&self) -> Self {
        self.inner
            .producer_count
            .set(self.inner.producer_count.get() + 1);
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<T> MpmcWatchRefProducer<T> {
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        self.inner.watch.update(f);
    }

    pub fn view<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.inner.watch.view(f)
    }

    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.inner.watch.get()
    }

    pub fn shrink_to_fit(&self) {
        self.inner.watch.shrink_to_fit();
    }
}

impl<T> Drop for MpmcWatchRefProducer<T> {
    fn drop(&mut self) {
        let count = self.inner.producer_count.get() - 1;
        self.inner.producer_count.set(count);
        if count == 0 {
            self.inner.watch.close();
        }
    }
}

#[derive(Clone)]
pub struct MpmcWatchRefSource<T> {
    inner: Rc<Inner<T>>,
}

impl<T> MpmcWatchRefSource<T> {
    pub fn subscribe(&self) -> MpmcWatchRefConsumer<T> {
        let key = self.inner.watch.subscribe();
        MpmcWatchRefConsumer {
            inner: Rc::clone(&self.inner),
            key,
        }
    }

    pub fn subscribe_forward(&self) -> MpmcWatchRefConsumer<T> {
        let key = self.inner.watch.subscribe_forward();
        MpmcWatchRefConsumer {
            inner: Rc::clone(&self.inner),
            key,
        }
    }
}

pub struct MpmcWatchRefConsumer<T> {
    inner: Rc<Inner<T>>,
    key: MpmcWatchRefConsumerKey,
}

impl<T> MpmcWatchRefConsumer<T> {
    pub async fn changed(&self) -> Result<(), Closed> {
        self.inner.watch.observe(&self.key).await
    }

    pub fn is_closed(&self) -> bool {
        self.inner.watch.is_closed()
    }

    pub fn view<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.inner.watch.view(f)
    }

    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.inner.watch.get()
    }
}

pub fn watch<T>(initial: T) -> (MpmcWatchRefProducer<T>, MpmcWatchRefSource<T>) {
    let inner = Rc::new(Inner {
        watch: MpmcWatchRef::new(initial),
        producer_count: Cell::new(1),
    });
    let producer = MpmcWatchRefProducer {
        inner: Rc::clone(&inner),
    };
    let source = MpmcWatchRefSource { inner };
    (producer, source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll};

    use futures_executor::block_on;
    use futures_test::task::new_count_waker;

    #[test]
    fn update_and_view() {
        let w = MpmcWatchRef::new(0);
        w.update(|v| *v = 42);
        assert_eq!(w.view(|v| *v), 42);
    }

    #[test]
    fn get_clones_value() {
        let w = MpmcWatchRef::new(String::from("hello"));
        assert_eq!(w.get(), "hello");
        w.update(|v| v.push_str(" world"));
        assert_eq!(w.get(), "hello world");
    }

    #[test]
    fn observe_resolves_on_update() {
        let w = MpmcWatchRef::new(0);
        let key = w.subscribe_forward();

        let mut fut = Box::pin(w.observe(&key));
        let (waker, wake_count) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        w.update(|v| *v = 1);
        assert_eq!(wake_count.get(), 1);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn close_resolves_err() {
        let w = MpmcWatchRef::new(0);
        let key = w.subscribe_forward();

        let mut fut = Box::pin(w.observe(&key));
        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        w.close();
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));
    }

    #[test]
    fn update_then_close_delivers_ok_then_err() {
        let w = MpmcWatchRef::new(0);
        let key = w.subscribe_forward();

        let mut fut = Box::pin(w.observe(&key));
        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

        w.update(|v| *v = 99);
        w.close();

        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut fut2 = Box::pin(w.observe(&key));
        assert_eq!(fut2.as_mut().poll(&mut cx), Poll::Ready(Err(Closed)));

        assert_eq!(w.get(), 99);
    }

    #[test]
    fn multi_consumer_independence() {
        let w = MpmcWatchRef::new(0);
        let k1 = w.subscribe_forward();
        let k2 = w.subscribe_forward();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        w.update(|v| *v = 1);

        let mut f1 = Box::pin(w.observe(&k1));
        assert_eq!(f1.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut f2 = Box::pin(w.observe(&k2));
        assert_eq!(f2.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut f1 = Box::pin(w.observe(&k1));
        assert_eq!(f1.as_mut().poll(&mut cx), Poll::Pending);
    }

    #[test]
    fn channel_update_and_changed() {
        block_on(async {
            let (tx, src) = watch(0);
            let rx = src.subscribe_forward();

            tx.update(|v| *v = 42);
            rx.changed().await.unwrap();

            assert_eq!(rx.get(), 42);
        });
    }

    #[test]
    fn producer_drop_closes_channel() {
        block_on(async {
            let (tx, src) = watch(0);
            let rx = src.subscribe_forward();

            tx.update(|v| *v = 1);
            drop(tx);

            rx.changed().await.unwrap();
            assert_eq!(rx.get(), 1);

            assert_eq!(rx.changed().await, Err(Closed));
        });
    }

    #[test]
    fn producer_clone_keeps_channel_open() {
        block_on(async {
            let (tx1, src) = watch(0);
            let rx = src.subscribe_forward();
            let tx2 = tx1.clone();

            drop(tx1);

            tx2.update(|v| *v = 7);
            rx.changed().await.unwrap();
            assert_eq!(rx.get(), 7);
        });
    }

    #[test]
    fn last_producer_clone_drop_closes() {
        block_on(async {
            let (tx1, src) = watch(0);
            let rx = src.subscribe_forward();
            let tx2 = tx1.clone();

            drop(tx1);
            drop(tx2);

            assert_eq!(rx.changed().await, Err(Closed));
        });
    }

    #[test]
    fn source_subscribe_tracks_independently() {
        block_on(async {
            let (tx, src) = watch(0);
            let rx1 = src.subscribe_forward();

            tx.update(|v| *v = 1);
            rx1.changed().await.unwrap();
            assert_eq!(rx1.get(), 1);

            let rx2 = src.subscribe_forward();

            tx.update(|v| *v = 2);
            rx1.changed().await.unwrap();
            rx2.changed().await.unwrap();
            assert_eq!(rx1.get(), 2);
            assert_eq!(rx2.get(), 2);
        });
    }

    #[test]
    fn subscribe_forward_does_not_see_prior_unseen() {
        let (tx, src) = watch(0);
        let rx1 = src.subscribe_forward();

        tx.update(|v| *v = 1);

        let rx2 = src.subscribe_forward();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        let mut f1 = Box::pin(rx1.changed());
        assert_eq!(f1.as_mut().poll(&mut cx), Poll::Ready(Ok(())));

        let mut f2 = Box::pin(rx2.changed());
        assert_eq!(f2.as_mut().poll(&mut cx), Poll::Pending);
    }

    #[test]
    fn subscribe_sees_prior_changes() {
        let (tx, src) = watch(0);

        tx.update(|v| *v = 1);

        let rx = src.subscribe();

        let (waker, _) = new_count_waker();
        let mut cx = Context::from_waker(&waker);

        let mut f = Box::pin(rx.changed());
        assert_eq!(f.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    #[test]
    fn changed_wakes_all_consumers() {
        let (tx, src) = watch(0);
        let rx1 = src.subscribe_forward();
        let rx2 = src.subscribe_forward();

        let mut f1 = Box::pin(rx1.changed());
        let (waker1, wake1) = new_count_waker();
        let mut cx1 = Context::from_waker(&waker1);
        assert_eq!(f1.as_mut().poll(&mut cx1), Poll::Pending);

        let mut f2 = Box::pin(rx2.changed());
        let (waker2, wake2) = new_count_waker();
        let mut cx2 = Context::from_waker(&waker2);
        assert_eq!(f2.as_mut().poll(&mut cx2), Poll::Pending);

        tx.update(|v| *v = 1);

        assert_eq!(wake1.get(), 1);
        assert_eq!(wake2.get(), 1);
    }

    #[test]
    fn producer_view_and_get() {
        let (tx, _src) = watch(100);
        assert_eq!(tx.get(), 100);
        assert_eq!(tx.view(|v| *v + 1), 101);
    }

    #[test]
    fn consumer_view_delegates() {
        let (tx, src) = watch(String::from("init"));
        let rx = src.subscribe_forward();
        tx.update(|v| *v = String::from("updated"));
        assert_eq!(rx.view(|v| v.len()), 7);
    }

    #[test]
    fn shrink_to_fit_is_transparent() {
        block_on(async {
            let (tx, src) = watch(0);
            let rx = src.subscribe_forward();

            for i in 0..10 {
                tx.update(|v| *v = i);
                rx.changed().await.unwrap();
            }

            tx.shrink_to_fit();

            tx.update(|v| *v = 999);
            rx.changed().await.unwrap();
            assert_eq!(rx.get(), 999);
        });
    }
}

use crossbeam_epoch::{Guard, Pointer, Shared};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Atomic<T> {
    data: AtomicUsize,
    _marker: PhantomData<*mut T>,
}

unsafe impl<T: Send + Sync> Send for Atomic<T> {}
unsafe impl<T: Send + Sync> Sync for Atomic<T> {}

impl<T> Atomic<T> {
    pub fn new(t: T) -> Self {
        let ptr = Box::into_raw(Box::new(t));
        Self {
            data: AtomicUsize::new(ptr as usize),
            _marker: PhantomData,
        }
    }

    pub fn load<'g>(&self, _guard: &'g Guard) -> Shared<'g, T> {
        let curr = self.data.load(Ordering::SeqCst);
        unsafe { Shared::from_usize(curr) }
    }
}

pub fn cas2<T0, T1>(
    addr0: &Atomic<T0>,
    addr1: &Atomic<T1>,
    exp0: Shared<'_, T0>,
    exp1: Shared<'_, T1>,
    new0: Shared<'_, T0>,
    new1: Shared<'_, T1>,
) -> bool {
    false
}

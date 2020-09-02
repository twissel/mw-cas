use crossbeam_epoch::{Atomic, Shared};
use std::sync::atomic::{AtomicPtr};

pub struct RDCSSDescriptor<T0, T1> {
    addr0: AtomicPtr<Atomic<T0>>,
    addr1: AtomicPtr<RDCSSAtomic<T1>>,
}

pub struct RDCSSAtomic<T>(Atomic<T>);

pub struct RDCSSShared<'g, T>(Shared<'g, T>);

impl<T0, T1> RDCSSDescriptor<T0, T1> {
    pub fn new_ptr<P>(
        addr0: &Atomic<T0>,
        expected0: Shared<'_, T0>,
        addr1: &RDCSSAtomic<T1>,
        expected1: RDCSSShared<'_, T1>,
        new2: RDCSSShared<'_, T1>,
    ) -> Self {
        let addr0 = AtomicPtr::new(addr0 as *const Atomic<T0> as *mut _);
        Self {
            addr0,
            addr1: unimplemented!()
        }
    }
}

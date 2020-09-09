use crate::descriptor::MarkedPtr;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

pub(crate) struct AtomicMarkedPtr(AtomicUsize);
impl AtomicMarkedPtr {
    pub fn null() -> Self {
        Self(AtomicUsize::new(0))
    }

    pub fn from_usize(ptr: usize) -> Self {
        Self(AtomicUsize::new(ptr))
    }

    pub fn load(&self) -> MarkedPtr {
        MarkedPtr::from_usize(self.0.load(Ordering::SeqCst))
    }

    pub fn store(&self, ptr: MarkedPtr) {
        self.0.store(ptr.into_usize(), Ordering::SeqCst);
    }

    pub fn compare_exchange(
        &self,
        expected: MarkedPtr,
        new: MarkedPtr,
    ) -> Result<MarkedPtr, MarkedPtr> {
        let exchanged = self.0.compare_exchange(
            expected.into_usize(),
            new.into_usize(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        match exchanged {
            Ok(new) => Ok(MarkedPtr::from_usize(new)),
            Err(err) => Err(MarkedPtr::from_usize(err)),
        }
    }
}

pub(crate) struct PtrCell(AtomicPtr<AtomicMarkedPtr>);

impl PtrCell {
    pub fn empty() -> Self {
        Self(AtomicPtr::default())
    }

    pub fn store(&self, ptr: &AtomicMarkedPtr) {
        self.0.store(ptr as *const _ as *mut _, Ordering::SeqCst);
    }

    pub fn load<'a>(&self) -> &'a AtomicMarkedPtr {
        let ptr = self.0.load(Ordering::SeqCst);
        unsafe { &*ptr }
    }
}

use crate::descriptor::{DescriptorPtr, SeqNumberGenerator};
use crate::thread_local::ThreadLocal;
use crossbeam_epoch::{Atomic, Guard, Owned, Pointer, Shared};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

const DESCRIPTOR_MARKER: usize = 1;

struct PerThreadRDCSSDescriptor<T0, T1> {
    addr0: AtomicPtr<Atomic<T0>>,
    addr1: AtomicPtr<()>,
    exp0: AtomicUsize,
    exp1: AtomicUsize,
    new: AtomicUsize,

    seq_number: SeqNumberGenerator,
    _mark: std::marker::PhantomData<*mut T1>,
}

struct PerThreadsDescriptorFields<T0, T1> {
    addr0: *const Atomic<T0>,
    _mark: std::marker::PhantomData<*mut T1>,
}

unsafe impl<T0, T1> Send for PerThreadRDCSSDescriptor<T0, T1> {}

impl<T0, T1> PerThreadRDCSSDescriptor<T0, T1> {
    pub fn new() -> Self {
        Self {
            addr0: AtomicPtr::default(),
            addr1: AtomicPtr::default(),
            exp0: AtomicUsize::new(0),
            exp1: AtomicUsize::new(0),
            new: AtomicUsize::new(0),
            seq_number: SeqNumberGenerator::new(),
            _mark: std::marker::PhantomData,
        }
    }
}

#[repr(transparent)]
pub struct RDCSSAtomic<T>(Atomic<T>);

impl<T> RDCSSAtomic<T> {
    pub fn new(atom: Atomic<T>) -> Self {
        RDCSSAtomic(atom)
    }

    pub fn load<'g>(&self, guard: &'g Guard) -> RDCSSShared<'g, T> {
        let s = self.0.load(Ordering::SeqCst, &guard);
        RDCSSShared(s)
    }
}

#[repr(transparent)]
pub struct RDCSSOwned<T>(Owned<T>);

impl<T> RDCSSOwned<T> {
    pub fn new(t: T) -> Self {
        Self(Owned::new(t))
    }

    pub fn into_shared<'g>(self, guard: &'g Guard) -> RDCSSShared<'g, T> {
        RDCSSShared(self.0.into_shared(guard))
    }
}

pub struct RDCSSShared<'g, T>(Shared<'g, T>);

impl<'g, T> Clone for RDCSSShared<'g, T> {
    fn clone(&self) -> Self {
        RDCSSShared(self.0.clone())
    }
}

impl<'g, T> Copy for RDCSSShared<'g, T> {}

pub struct RDCSSDescriptor<T0, T1> {
    per_thread_descriptors: ThreadLocal<PerThreadRDCSSDescriptor<T0, T1>>,
}

impl<T0, T1> RDCSSDescriptor<T0, T1>
where
    T0: 'static,
    T1: 'static,
{
    pub fn new() -> Self {
        Self {
            per_thread_descriptors: ThreadLocal::new(),
        }
    }

    pub fn new_ptr(
        &self,
        addr0: &Atomic<T0>,
        addr1: &RDCSSAtomic<T1>,
        exp0: Shared<'_, T1>,
        exp1: RDCSSShared<'_, T1>,
        new1: RDCSSShared<'_, T1>,
    ) -> DescriptorPtr {
        let (thread_id, per_thread_descriptor) = self
            .per_thread_descriptors
            .get_or_insert_with(PerThreadRDCSSDescriptor::new);
        per_thread_descriptor.seq_number.inc();
        per_thread_descriptor
            .addr0
            .store(addr0 as *const Atomic<T0> as *mut _, Ordering::SeqCst);
        per_thread_descriptor
            .addr1
            .store(addr1 as *const RDCSSAtomic<T1> as *mut _, Ordering::SeqCst);

        per_thread_descriptor
            .exp0
            .store(exp0.into_usize(), Ordering::SeqCst);
        per_thread_descriptor
            .exp1
            .store(exp1.0.into_usize(), Ordering::SeqCst);
        per_thread_descriptor
            .new
            .store(new1.0.into_usize(), Ordering::SeqCst);

        let new_seq = per_thread_descriptor.seq_number.inc();
        DescriptorPtr::new(thread_id, new_seq).with_mark(DESCRIPTOR_MARKER)
    }

    pub fn rdcss<'g>(
        &self,
        addr0: &Atomic<T0>,
        addr1: &RDCSSAtomic<T1>,
        exp0: Shared<'_, T1>,
        exp1: RDCSSShared<'_, T1>,
        new1: RDCSSShared<'_, T1>,
        guard: &'g Guard,
    ) -> RDCSSShared<'g, T1> {
        let des_ptr = self.new_ptr(addr0, addr1, exp0, exp1, new1);
        let addr1 = &addr1.0 as *const Atomic<T1> as *const Atomic<()>;
        unsafe {
            let exp1: Shared<()> = Shared::from_usize(exp1.0.into_usize());
            let des_ptr_shared = Shared::from_usize(des_ptr.into_usize());
            loop {
                let installed =
                    &(*addr1).compare_and_set(exp1, des_ptr_shared, Ordering::SeqCst, &guard);
                match installed {
                    Ok(installed) => {
                        self.rdcss_help(des_ptr);
                        return RDCSSShared(Shared::from_usize(installed.into_usize()));
                    }
                    Err(e) => {
                        let curr_ptr = e.current.into_usize();
                        let curr = DescriptorPtr::from_usize(curr_ptr);
                        if is_marked(curr) {
                            self.rdcss_help(curr);
                            continue;
                        } else {
                            let new = RDCSSShared(Shared::from_usize(curr_ptr));
                            return new;
                        }
                    }
                }
            }
        }
    }

    fn rdcss_help(&self, des: DescriptorPtr) {
        unimplemented!()
    }

    fn read_fields(&self, des: DescriptorPtr) -> Result<PerThreadsDescriptorFields<T0, T1>, ()> {
        unimplemented!()
    }
}

pub fn is_marked(ptr: DescriptorPtr) -> bool {
    ptr.mark() == DESCRIPTOR_MARKER
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdcss::RDCSSDescriptor;
    use crossbeam_epoch::pin;

    #[test]
    fn test_descriptor() {
        let g = pin();
        let atom = Atomic::new(5);
        let atom_exp = atom.load(Ordering::SeqCst, &g);
        let rdcss_atom = RDCSSAtomic::new(Atomic::new(5));
        let rdcss_exp = rdcss_atom.load(&g);
        let des = RDCSSDescriptor::<usize, usize>::new();
        let rdcss_new = RDCSSOwned::new(10).into_shared(&g);
        let ptr = des.new_ptr(&atom, &rdcss_atom, atom_exp, rdcss_exp, rdcss_new);
        assert!(is_marked(ptr));
        let r = des.rdcss(&atom, &rdcss_atom, atom_exp, rdcss_exp, rdcss_new, &g);
    }
}

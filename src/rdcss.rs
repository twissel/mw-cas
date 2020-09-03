use crate::descriptor::{DescriptorPtr, SeqNumberGenerator};
use crate::thread_local::ThreadLocal;
use crossbeam_epoch::{Atomic, Guard, Owned, Pointer, Shared};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

const DESCRIPTOR_MARKER: usize = 1;

struct PerThreadRDCSSDescriptor<T0, T1> {
    addr0: AtomicPtr<Atomic<T0>>,
    addr1: AtomicPtr<Atomic<T1>>,
    exp0: AtomicUsize,
    exp1: AtomicUsize,
    new: AtomicUsize,

    seq_number: SeqNumberGenerator,
    _mark: std::marker::PhantomData<*mut T1>,
}

unsafe impl<T0, T1> Send for PerThreadRDCSSDescriptor<T0, T1> {}
unsafe impl<T0, T1> Sync for PerThreadRDCSSDescriptor<T0, T1> {}

impl<T0, T1> PerThreadRDCSSDescriptor<T0, T1> {
    fn new() -> Self {
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

    fn read_fields(&self) -> PerThreadsDescriptorFields<'_, T0, T1> {
        unsafe {
            let addr0: *mut Atomic<T0> = self.addr0.load(Ordering::SeqCst);
            let addr1: *mut Atomic<T1> = self.addr1.load(Ordering::SeqCst);
            let exp0: Shared<T0> = Shared::from_usize(self.exp0.load(Ordering::SeqCst));
            let exp1: Shared<T1> = Shared::from_usize(self.exp1.load(Ordering::SeqCst));
            let new: Shared<T1> = Shared::from_usize(self.new.load(Ordering::SeqCst));
            PerThreadsDescriptorFields {
                addr0: &*addr0,
                addr1: &*addr1,
                exp0,
                exp1,
                new,
            }
        }
    }
}

struct PerThreadsDescriptorFields<'g, T0, T1> {
    addr0: &'g Atomic<T0>,
    addr1: &'g Atomic<T1>,
    exp0: Shared<'g, T0>,
    exp1: Shared<'g, T1>,
    new: Shared<'g, T1>,
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

#[derive(PartialEq, Eq, Debug)]
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
            .store(&addr1.0 as *const Atomic<T1> as *mut _, Ordering::SeqCst);

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
        unsafe {
            let des_ptr_shared = Shared::from_usize(des_ptr.into_usize());
            loop {
                let installed =
                    addr1
                        .0
                        .compare_and_set(exp1.0, des_ptr_shared, Ordering::SeqCst, &guard);
                match installed {
                    Ok(_) => {
                        self.rdcss_help(des_ptr, guard);
                        return RDCSSShared(Shared::from_usize(exp1.0.into_usize()));
                    }
                    Err(e) => {
                        let curr_ptr = e.current.into_usize();
                        let curr = DescriptorPtr::from_usize(curr_ptr);
                        if is_marked(curr) {
                            self.rdcss_help(curr, guard);
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

    fn rdcss_help<'g>(&self, des: DescriptorPtr, guard: &Guard) {
        let fields = self.read_fields(des);
        unsafe {
            if let Ok(fields) = fields {
                let curr0 = fields.addr0.load(Ordering::SeqCst, guard);
                if curr0 == fields.exp0 {
                    fields.addr1.compare_and_set(
                        Shared::from_usize(des.into_usize()),
                        fields.new,
                        Ordering::SeqCst,
                        guard,
                    );
                } else {
                    fields.addr1.compare_and_set(
                        Shared::from_usize(des.into_usize()),
                        fields.exp1,
                        Ordering::SeqCst,
                        guard,
                    );
                }
            }
        }
    }

    fn read_fields(
        &self,
        des: DescriptorPtr,
    ) -> Result<PerThreadsDescriptorFields<'_, T0, T1>, ()> {
        let tid = des.tid();
        let seq = des.seq();
        let curr_thread_desciptor = self
            .per_thread_descriptors
            .get_for_thread(tid)
            .expect("Missing thread descriptor");
        if seq != curr_thread_desciptor.seq_number.current() {
            Err(())
        } else {
            let fields = curr_thread_desciptor.read_fields();
            if seq != curr_thread_desciptor.seq_number.current() {
                Err(())
            } else {
                Ok(fields)
            }
        }
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
        let swapped = des.rdcss(&atom, &rdcss_atom, atom_exp, rdcss_exp, rdcss_new, &g);
        assert_eq!(swapped, rdcss_exp);
    }
}

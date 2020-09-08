use crate::descriptor::{MarkedPtr, SeqNumber};
use crate::ptr::{AtomicMarkedPtr, PtrCell};
use crate::rdcss::RDCSS_DESCRIPTOR;
use crate::thread_local::ThreadLocal;
use crossbeam_epoch::{Guard, Pointer, Shared};
use crossbeam_utils::{Backoff, CachePadded};
use once_cell::sync::Lazy;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

static CAS2_DESCRIPTOR: Lazy<Cas2Descriptor> = Lazy::new(|| Cas2Descriptor::new());

#[repr(transparent)]
pub struct Atomic<T> {
    data: AtomicMarkedPtr,
    _marker: PhantomData<*mut T>,
}

unsafe impl<T: Send + Sync> Send for Atomic<T> {}
unsafe impl<T: Send + Sync> Sync for Atomic<T> {}

pub fn cas2<T0, T1>(
    addr0: &Atomic<T0>,
    addr1: &Atomic<T1>,
    exp0: Shared<'_, T0>,
    exp1: Shared<'_, T1>,
    new0: Shared<'_, T0>,
    new1: Shared<'_, T1>,
) -> bool {
    let descriptor_ptr = CAS2_DESCRIPTOR.make_descriptor(addr0, addr1, exp0, exp1, new0, new1);
    CAS2_DESCRIPTOR.cas2_help(descriptor_ptr, false)
}

impl<T> Atomic<T> {
    pub fn new(t: T) -> Self {
        let ptr = Box::into_raw(Box::new(t));
        Self {
            data: AtomicMarkedPtr::from_usize(ptr as usize),
            _marker: PhantomData,
        }
    }

    pub fn load<'g>(&self, _guard: &'g Guard) -> Shared<'g, T> {
        loop {
            let curr = RDCSS_DESCRIPTOR.read(&self.data);
            if curr.mark() == Cas2Descriptor::MARK {
                CAS2_DESCRIPTOR.cas2_help(curr, true);
            } else {
                return unsafe { Shared::from_usize(curr.into_usize()) };
            }
        }
    }
}

struct Cas2Descriptor {
    map: ThreadLocal<CachePadded<PerThreadCas2Descriptor>>,
}

impl Cas2Descriptor {
    const MARK: usize = 2;
    pub fn new() -> Self {
        Self {
            map: ThreadLocal::new(),
        }
    }

    pub fn make_descriptor<T0, T1>(
        &self,
        addr0: &Atomic<T0>,
        addr1: &Atomic<T1>,
        exp0: Shared<'_, T0>,
        exp1: Shared<'_, T1>,
        new0: Shared<'_, T0>,
        new1: Shared<'_, T1>,
    ) -> MarkedPtr {
        let (tid, per_thread_descriptor) = CAS2_DESCRIPTOR
            .map
            .get_or_insert_with(|| CachePadded::new(PerThreadCas2Descriptor::empty()));

        // invalidate current descriptor
        per_thread_descriptor.inc_seq();

        // sort and store addresses
        let (addr0_entry, addr1_entry) =
            if addr0 as *const Atomic<T0> as usize <= addr1 as *const Atomic<T1> as usize {
                (
                    &per_thread_descriptor.entries[0],
                    &per_thread_descriptor.entries[1],
                )
            } else {
                (
                    &per_thread_descriptor.entries[1],
                    &per_thread_descriptor.entries[1],
                )
            };
        addr0_entry.addr.store(&addr0.data);
        addr0_entry.exp.store(exp0.into());
        addr0_entry.new.store(new0.into());

        addr1_entry.addr.store(&addr1.data);
        addr1_entry.exp.store(exp1.into());
        addr1_entry.new.store(new1.into());

        // make descriptor fully initialized
        per_thread_descriptor.inc_seq();
        let current_seq_num = per_thread_descriptor.status.load().seq_number();

        // create a ptr for descriptor
        MarkedPtr::new(tid, current_seq_num).with_mark(Self::MARK)
    }

    pub fn cas2_help(&self, descriptor_ptr: MarkedPtr, help: bool) -> bool {
        let descriptor_seq = descriptor_ptr.seq();
        // load current descriptor
        let descriptor = self
            .map
            .get_for_thread(descriptor_ptr.tid())
            .expect("Missing thread  CAS2 descriptor");

        // Phase 1: try to install descriptor in all entries
        // Only if des has status == UNDECIDED && descriptor is still valid
        let descriptor_current_status = descriptor.status.load();
        if descriptor_seq == descriptor_current_status.seq_number()
            && descriptor_current_status.status() == Cas2DescriptorStatus::UNDECIDED
        {
            let mut new_status = Cas2DescriptorStatus::succeeded(descriptor_seq);
            let start = if help { 1 } else { 0 };
            let backoff = Backoff::new();
            'entry_loop: for entry in &descriptor.entries[start..] {
                'install_loop: loop {
                    let entry_addr = entry.addr.load();
                    let entry_exp = entry.exp.load();
                    let swapped = RDCSS_DESCRIPTOR.rdcss(
                        &descriptor.status,
                        entry_addr,
                        descriptor_current_status,
                        entry_exp,
                        descriptor_ptr,
                    );

                    if swapped.mark() == Cas2Descriptor::MARK && swapped != descriptor_ptr {
                        if backoff.is_completed() {
                            self.cas2_help(swapped, true);
                        } else {
                            backoff.spin();
                        }
                        continue 'install_loop;
                    } else {
                        if swapped != entry_exp {
                            new_status = new_status.set_failed();
                            break 'entry_loop;
                        } else {
                            break 'install_loop;
                        }
                    }
                }
            }
            let expected_status = descriptor_current_status;
            let descriptor_current_status = descriptor.status.load();
            if descriptor_current_status.status() == Cas2DescriptorStatus::UNDECIDED
                && expected_status.seq_number() == descriptor_current_status.seq_number()
            {
                let _ = descriptor
                    .status
                    .compare_exchange(expected_status, new_status);
            }

            let descriptor_current_status = descriptor.status.load();
            let succeeded = descriptor_current_status.status() == Cas2DescriptorStatus::SUCCEEDED;
            for entry in &descriptor.entries {
                let addr = entry.addr.load();
                let new = if succeeded {
                    entry.new.load()
                } else {
                    entry.exp.load()
                };
                let descriptor_current_status = descriptor.status.load();
                if descriptor_current_status.seq_number() == descriptor_seq {
                    let _ = addr.compare_exchange(descriptor_ptr, new);
                } else {
                    return false;
                }
            }
            succeeded
        } else {
            // thread we was trying to help, already finished this operation
            false
        }
    }
}

struct PerThreadCas2Descriptor {
    pub entries: [Entry; 2],
    pub status: Cas2DescriptorStatusCell,
}

impl PerThreadCas2Descriptor {
    fn empty() -> Self {
        Self {
            status: Cas2DescriptorStatusCell::new(),
            entries: [Entry::empty(), Entry::empty()],
        }
    }

    fn inc_seq(&self) {
        let seq_num = self.status.load().seq_number().inc();
        self.status.store(Cas2DescriptorStatus::undecided(seq_num))
    }
}

pub struct Cas2DescriptorStatusCell(AtomicUsize);

impl Cas2DescriptorStatusCell {
    pub fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    pub fn load(&self) -> Cas2DescriptorStatus {
        Cas2DescriptorStatus(self.0.load(Ordering::SeqCst))
    }

    pub fn store(&self, status: Cas2DescriptorStatus) {
        self.0.store(status.0, Ordering::Relaxed);
    }

    pub fn compare_exchange(
        &self,
        expected_status: Cas2DescriptorStatus,
        new_status: Cas2DescriptorStatus,
    ) -> Result<Cas2DescriptorStatus, Cas2DescriptorStatus> {
        let swapped = self.0.compare_exchange(
            expected_status.0,
            new_status.0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        swapped
            .map(Cas2DescriptorStatus::from_usize)
            .map_err(Cas2DescriptorStatus::from_usize)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Cas2DescriptorStatus(usize);

impl Cas2DescriptorStatus {
    pub const UNDECIDED: usize = 0;
    pub const SUCCEEDED: usize = 1;
    pub const FAILED: usize = 2;
    const NUM_STATUS_BITS: usize = 8;

    fn undecided(seq_num: SeqNumber) -> Self {
        let seq_num = seq_num.as_usize() << Self::NUM_STATUS_BITS;
        Self(seq_num | Self::UNDECIDED)
    }

    fn succeeded(seq_num: SeqNumber) -> Self {
        let seq_num = seq_num.as_usize() << Self::NUM_STATUS_BITS;
        Self(seq_num | Self::SUCCEEDED)
    }

    fn failed(seq_num: SeqNumber) -> Self {
        let seq_num = seq_num.as_usize() << Self::NUM_STATUS_BITS;
        Self(seq_num | Self::FAILED)
    }

    fn set_failed(self) -> Cas2DescriptorStatus {
        Self::failed(self.seq_number())
    }

    fn seq_number(&self) -> SeqNumber {
        let seq_num = self.0 >> Self::NUM_STATUS_BITS;
        SeqNumber::from_usize(seq_num)
    }

    fn status(&self) -> usize {
        self.0 & ((1 << 8) - 1)
    }

    fn from_usize(status: usize) -> Self {
        Self(status)
    }
}

struct Entry {
    addr: PtrCell,
    exp: AtomicMarkedPtr,
    new: AtomicMarkedPtr,
}

impl Entry {
    pub fn empty() -> Self {
        Self {
            addr: PtrCell::empty(),
            exp: AtomicMarkedPtr::null(),
            new: AtomicMarkedPtr::null(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crossbeam_epoch::{pin, Owned};

    #[test]
    fn test_mcas() {
        let g = pin();
        let atom0 = Atomic::new(0);
        let exp0 = atom0.load(&g);
        let new0 = Owned::new(1).into_shared(&g);

        let atom1 = Atomic::new(0);
        let exp1 = atom1.load(&g);
        let new1 = Owned::new(1).into_shared(&g);
        let succeeded = cas2(&atom0, &atom1, exp0, exp1, new0, new1);
        let new0 = atom0.load(&g);
        let new1 = atom1.load(&g);
        unsafe {
            assert_eq!(new0.as_ref().unwrap(), &1);
            assert_eq!(new1.as_ref().unwrap(), &1);
        }

        let succeeded = cas2(&atom0, &atom1, exp0, exp1, new0, new1);
        let curr0 = atom0.load(&g);
        let curr1 = atom0.load(&g);
        unsafe {
            assert_eq!(curr0.as_ref().unwrap(), &1);
            assert_eq!(curr0.as_ref().unwrap(), &1);
        }

        assert!(!succeeded);
    }
}

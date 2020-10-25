pub use crate::atomic::Atomic;
use crate::{
    atomic::{AtomicAddress, AtomicBits, Bits, Word},
    rdcss::RDCSS_DESCRIPTOR,
    sequence_number::SeqNumber,
    thread_local::ThreadLocal,
};
use arrayvec::ArrayVec;
use crossbeam_utils::Backoff;
use once_cell::sync::Lazy;
use std::sync::atomic::{fence, AtomicUsize as StdAtomicUsize, Ordering};

pub(crate) static CASN_DESCRIPTOR: Lazy<CasNDescriptor> = Lazy::new(CasNDescriptor::new);

#[allow(clippy::missing_safety_doc)]
pub unsafe fn cas2<T0, T1>(
    addr0: &Atomic<T0>,
    addr1: &Atomic<T1>,
    exp0: T0,
    exp1: T1,
    new0: T0,
    new1: T1,
) -> bool
where
    T0: Word,
    T1: Word,
{
    let entry0 = Entry {
        addr: addr0.as_atomic_bits(),
        exp: exp0.into(),
        new: new0.into(),
    };

    let entry1 = Entry {
        addr: addr1.as_atomic_bits(),
        exp: exp1.into(),
        new: new1.into(),
    };

    let mut entries = [entry0, entry1];
    let descriptor_ptr = CASN_DESCRIPTOR.make_descriptor(&mut entries);
    CASN_DESCRIPTOR.help(descriptor_ptr, false)
}

#[allow(clippy::missing_safety_doc)]
pub unsafe fn cas_n<T>(addresses: &[&Atomic<T>], expected: &[T], new: &[T]) -> bool
where
    T: Word,
{
    assert_eq!(addresses.len(), expected.len());
    assert_eq!(expected.len(), new.len());
    assert!(addresses.len() <= MAX_ENTRIES);
    let mut entries = ArrayVec::<[Entry<'_>; MAX_ENTRIES]>::new();
    for ((addr, exp), new) in addresses.iter().zip(expected).zip(new) {
        let entry = Entry {
            addr: addr.as_atomic_bits(),
            exp: (*exp).into(),
            new: (*new).into(),
        };
        entries.push(entry);
    }
    let descriptor_ptr = CASN_DESCRIPTOR.make_descriptor(&mut entries);
    CASN_DESCRIPTOR.help(descriptor_ptr, false)
}

pub(crate) struct CasNDescriptor {
    map: ThreadLocal<ThreadCasNDescriptor>,
}

impl CasNDescriptor {
    pub const MARK: usize = 2;

    pub fn new() -> Self {
        Self {
            map: ThreadLocal::new(),
        }
    }

    pub fn make_descriptor(&'static self, entries: &mut [Entry]) -> Bits {
        let (tid, per_thread_descriptor) = CASN_DESCRIPTOR.map.get();

        // invalidate current descriptor
        per_thread_descriptor.inc_seq();

        fence(Ordering::Release);

        // sort and store addresses
        per_thread_descriptor.store_entries(entries);
        // make descriptor fully initialized
        per_thread_descriptor.inc_seq();
        let current_seq_num = per_thread_descriptor
            .status
            .load(Ordering::SeqCst)
            .seq_number();

        // create a ptr for descriptor
        Bits::new_descriptor_ptr(tid, current_seq_num).with_mark(Self::MARK)
    }

    fn try_snapshot(
        &'static self,
        descriptor_ptr: Bits,
    ) -> Result<ThreadCasNDescriptorSnapshot, ()> {
        let thread_descriptor = self.map.get_for_thread(descriptor_ptr.tid());
        thread_descriptor.try_snapshot(descriptor_ptr.seq())
    }

    pub fn help(&'static self, descriptor_ptr: Bits, help_other: bool) -> bool {
        let descriptor_seq = descriptor_ptr.seq();

        // try to snapshot descriptor we was helping
        let descriptor_snapshot = self.try_snapshot(descriptor_ptr);
        match descriptor_snapshot {
            Ok(descriptor_snapshot) => {
                // Phase 1: try to install descriptor in all entries
                // Only if des has status == UNDECIDED
                let descriptor_current_status =
                    match descriptor_snapshot.try_read_status(descriptor_ptr) {
                        Ok(status) => status,
                        Err(_) => {
                            assert!(help_other);
                            return false;
                        },
                    };
                if descriptor_current_status.status() == CasNDescriptorStatus::UNDECIDED {
                    let mut new_status = CasNDescriptorStatus::succeeded(descriptor_seq);
                    let start = if help_other { 1 } else { 0 };
                    let backoff = Backoff::new();
                    'entry_loop: for entry in &descriptor_snapshot.entries[start..] {
                        'install_loop: loop {
                            let entry_addr = entry.addr;
                            let entry_exp = entry.exp;
                            let swapped = RDCSS_DESCRIPTOR.rdcss(
                                &descriptor_snapshot.status,
                                entry_addr,
                                descriptor_current_status,
                                entry_exp,
                                descriptor_ptr,
                            );

                            if swapped.mark() == CasNDescriptor::MARK
                                && swapped != descriptor_ptr
                            {
                                if backoff.is_completed() {
                                    self.help(swapped, true);
                                } else {
                                    backoff.spin();
                                }
                                continue 'install_loop;
                            } else if swapped != entry_exp {
                                new_status = new_status.set_failed();
                                break 'entry_loop;
                            } else {
                                break 'install_loop;
                            }
                        }
                    }
                    descriptor_snapshot.cas_status(descriptor_current_status, new_status);
                }
                let descriptor_current_status =
                    match descriptor_snapshot.try_read_status(descriptor_ptr) {
                        Ok(status) => status,
                        Err(()) => {
                            assert!(help_other);
                            return false;
                        },
                    };

                let succeeded =
                    descriptor_current_status.status() == CasNDescriptorStatus::SUCCEEDED;
                for entry in &descriptor_snapshot.entries {
                    let new = if succeeded { entry.new } else { entry.exp };
                    let _ = entry.addr.compare_exchange(descriptor_ptr, new);
                }
                succeeded
            },
            Err(_) => {
                assert!(help_other);
                // nothing to do, thread we was trying to help, already finished this operation.
                false
            },
        }
    }
}

const MAX_ENTRIES: usize = 8;

struct ThreadCasNDescriptor {
    pub entries: [AtomicEntry; MAX_ENTRIES],
    pub num_entries: StdAtomicUsize,
    pub status: AtomicCasNDescriptorStatus,
}

impl ThreadCasNDescriptor {
    fn new() -> Self {
        Self {
            status: AtomicCasNDescriptorStatus::new(),
            num_entries: StdAtomicUsize::new(0),
            entries: [
                AtomicEntry::empty(),
                AtomicEntry::empty(),
                AtomicEntry::empty(),
                AtomicEntry::empty(),
                AtomicEntry::empty(),
                AtomicEntry::empty(),
                AtomicEntry::empty(),
                AtomicEntry::empty(),
            ],
        }
    }

    // only thread who owns this descriptor is allowed to call this function
    fn inc_seq(&self) {
        let seq_num = self.status.load(Ordering::Relaxed).seq_number().inc();
        self.status
            .store(CasNDescriptorStatus::undecided(seq_num), Ordering::SeqCst)
    }

    fn try_snapshot(
        &self,
        seq_num: SeqNumber,
    ) -> Result<ThreadCasNDescriptorSnapshot, ()> {
        let current_seq_num = self.status.load(Ordering::SeqCst).seq_number();
        if current_seq_num == seq_num {
            let num_entries = self.num_entries.load(Ordering::Relaxed);

            assert!(num_entries >= 2);
            let entries = self.entries[0..num_entries]
                .iter()
                .map(|atomic_entry| atomic_entry.load())
                .collect();

            fence(Ordering::Acquire);
            if seq_num == self.status.load(Ordering::SeqCst).seq_number() {
                Ok(ThreadCasNDescriptorSnapshot {
                    entries,
                    status: &self.status,
                })
            } else {
                Err(())
            }
        } else {
            Err(())
        }
    }

    fn store_entries(&self, entries: &mut [Entry<'_>]) {
        entries.sort_by_key(|e| e.addr as *const AtomicBits);
        for (atomic_entry, entry) in self.entries.iter().zip(&*entries) {
            atomic_entry.store(entry);
        }
        self.num_entries.store(entries.len(), Ordering::Relaxed);
    }
}

impl Default for ThreadCasNDescriptor {
    fn default() -> Self {
        Self::new()
    }
}

struct ThreadCasNDescriptorSnapshot<'a> {
    entries: ArrayVec<[Entry<'a>; MAX_ENTRIES]>,
    status: &'a AtomicCasNDescriptorStatus,
}

impl ThreadCasNDescriptorSnapshot<'_> {
    fn try_read_status(&self, descriptor_ptr: Bits) -> Result<CasNDescriptorStatus, ()> {
        let status = self.status.load(Ordering::SeqCst);
        if status.seq_number() == descriptor_ptr.seq() {
            Ok(status)
        } else {
            Err(())
        }
    }

    fn cas_status(
        &self,
        expected_status: CasNDescriptorStatus,
        new_status: CasNDescriptorStatus,
    ) {
        assert_eq!(expected_status.status(), CasNDescriptorStatus::UNDECIDED);
        let current_status = self.status.load(Ordering::SeqCst);
        if current_status == expected_status {
            let _ = self.status.compare_exchange(expected_status, new_status);
        }
    }
}

pub struct AtomicCasNDescriptorStatus(StdAtomicUsize);

impl AtomicCasNDescriptorStatus {
    pub fn new() -> Self {
        Self(StdAtomicUsize::new(0))
    }

    pub fn load(&self, ordering: Ordering) -> CasNDescriptorStatus {
        CasNDescriptorStatus(self.0.load(ordering))
    }

    pub fn store(&self, status: CasNDescriptorStatus, ordering: Ordering) {
        self.0.store(status.0, ordering);
    }

    pub fn compare_exchange(
        &self,
        expected_status: CasNDescriptorStatus,
        new_status: CasNDescriptorStatus,
    ) -> Result<CasNDescriptorStatus, CasNDescriptorStatus> {
        let swapped = self.0.compare_exchange(
            expected_status.0,
            new_status.0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        swapped
            .map(CasNDescriptorStatus::from_usize)
            .map_err(CasNDescriptorStatus::from_usize)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct CasNDescriptorStatus(usize);

impl CasNDescriptorStatus {
    pub const FAILED: usize = 2;
    const NUM_STATUS_BITS: usize = 8;
    pub const SUCCEEDED: usize = 1;
    pub const UNDECIDED: usize = 0;

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

    fn set_failed(self) -> CasNDescriptorStatus {
        Self::failed(self.seq_number())
    }

    fn seq_number(self) -> SeqNumber {
        let seq_num = self.0 >> Self::NUM_STATUS_BITS;
        SeqNumber::from_usize(seq_num)
    }

    fn status(self) -> usize {
        self.0 & ((1 << Self::NUM_STATUS_BITS) - 1)
    }

    fn from_usize(status: usize) -> Self {
        Self(status)
    }
}

struct AtomicEntry {
    addr: AtomicAddress<AtomicBits>,
    exp: AtomicBits,
    new: AtomicBits,
}

impl AtomicEntry {
    fn empty() -> Self {
        Self {
            addr: AtomicAddress::empty(),
            exp: AtomicBits::empty(),
            new: AtomicBits::empty(),
        }
    }

    fn load<'a>(&self) -> Entry<'a> {
        let addr = unsafe { self.addr.load(Ordering::Relaxed) };
        let exp = self.exp.load(Ordering::Relaxed);
        let new = self.new.load(Ordering::Relaxed);
        Entry { addr, exp, new }
    }

    fn store(&self, e: &Entry) {
        self.addr.store(e.addr, Ordering::Relaxed);
        self.new.store(e.new, Ordering::Relaxed);
        self.exp.store(e.exp, Ordering::Relaxed);
    }
}

pub(crate) struct Entry<'a> {
    addr: &'a AtomicBits,
    exp: Bits,
    new: Bits,
}

#[cfg(test)]
mod test {
    use super::*;
    use crossbeam_epoch::{pin, Owned, Shared};
    use std::sync::Arc;

    #[test]
    fn test_mcas() {
        let g = pin();
        let atom0 = Atomic::new(std::ptr::null());
        let exp0 = atom0.load();
        let new0 = Owned::new(1).into_shared(&g);

        let atom1 = Atomic::new(std::ptr::null());
        let exp1 = atom1.load();
        let new1 = Owned::new(1).into_shared(&g);
        let succeeded =
            unsafe { cas2(&atom0, &atom1, exp0, exp1, new0.as_raw(), new1.as_raw()) };
        assert!(succeeded);
        let new0 = atom0.load();
        let new1 = atom1.load();
        unsafe {
            assert_eq!(&*new0, &1);
            assert_eq!(&*new1, &1);
        }

        let succeeded = unsafe { cas2(&atom0, &atom1, exp0, exp1, new0, new1) };
        let curr0 = atom0.load();
        let curr1 = atom0.load();
        unsafe {
            assert_eq!(&*curr0, &1);
            assert_eq!(&*curr1, &1);
        }

        assert!(!succeeded);
    }

    #[test]
    fn counter_test() {
        let mut handles = Vec::new();
        let counter = Arc::new((
            Atomic::<*const u64>::new(Box::into_raw(Box::new(0))),
            Atomic::<*const u64>::new(Box::into_raw(Box::new(0))),
        ));
        let max = 100_000;
        for _ in 0..8 {
            let counter = counter.clone();
            let h = std::thread::spawn(move || loop {
                unsafe {
                    let g = crossbeam_epoch::pin();
                    let curr_first_shared = Shared::from(counter.0.load() as *const _);
                    let curr_second_shared = Shared::from(counter.1.load() as *const _);
                    let curr_first = curr_first_shared.deref();
                    let curr_second = curr_second_shared.deref();
                    if *curr_first == max {
                        break;
                    }

                    let new_first: Shared<'_, u64> =
                        Owned::<u64>::new(*curr_first + 1).into_shared(&g);
                    let new_second: Shared<'_, u64> =
                        Owned::<u64>::new(*curr_second + 1).into_shared(&g);

                    if cas2(
                        &counter.0,
                        &counter.1,
                        curr_first_shared.as_raw(),
                        curr_second_shared.as_raw(),
                        new_first.as_raw(),
                        new_second.as_raw(),
                    ) {
                        g.defer_destroy(curr_first_shared);
                        g.defer_destroy(curr_second_shared);
                    } else {
                        new_first.into_owned();
                        new_second.into_owned();
                    }
                }
            });

            handles.push(h);
        }

        for h in handles {
            h.join().unwrap();
        }

        let counter = match Arc::try_unwrap(counter) {
            Ok(c) => c,
            Err(_) => panic!(),
        };
        unsafe {
            let first = counter.0.load();
            assert_eq!(*first, max);
            Box::from_raw(first as *mut u32);

            let second = counter.1.load();
            assert_eq!(*second, max);
            Box::from_raw(second as *mut u32);
        }
    }
}

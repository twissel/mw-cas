use self::traits::Atom;
use crate::casword::{AtomicCasWord, AtomicCasWordCell};
use crate::casword::{CasWord, SeqNumber};
use crate::rdcss::RDCSS_DESCRIPTOR;
use crate::thread_local::ThreadLocal;
use arrayvec::ArrayVec;
use crossbeam_epoch::{Guard, Owned, Pointer, Shared};
use crossbeam_utils::{Backoff, CachePadded};
use once_cell::sync::Lazy;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering};

static CAS2_DESCRIPTOR: Lazy<Cas2Descriptor> = Lazy::new(|| Cas2Descriptor::new());

pub struct Atomic<T> {
    data: AtomicCasWord,
    _marker: PhantomData<*mut T>,
}

impl<T> Atomic<T> {
    pub fn new(t: T) -> Self {
        let ptr = Box::into_raw(Box::new(t));
        Self {
            data: AtomicCasWord::from_usize(ptr as usize),
            _marker: PhantomData,
        }
    }

    pub fn load<'g>(&self, _guard: &'g Guard) -> Shared<'g, T> {
        Shared::from(self.load_word())
    }

    pub unsafe fn into_owned(self) -> Owned<T> {
        Owned::from_usize(self.data.into_inner())
    }
}

unsafe impl<T: Send + Sync> Send for Atomic<T> {}
unsafe impl<T: Send + Sync> Sync for Atomic<T> {}

pub struct AtomicUsize {
    data: AtomicCasWord,
}

impl AtomicUsize {
    pub fn new(t: usize) -> Self {
        Self {
            data: AtomicCasWord::from_usize(t),
        }
    }

    pub fn load(&self) -> usize {
        self.load_word().into()
    }
}

pub unsafe fn cas2<A0, A1>(
    addr0: &A0,
    addr1: &A1,
    exp0: <A0 as traits::Atom>::Word,
    exp1: <A1 as traits::Atom>::Word,
    new0: <A0 as traits::Atom>::Word,
    new1: <A1 as traits::Atom>::Word,
) -> bool
where
    for<'a> A0: traits::Atom<'a>,
    for<'a> A1: traits::Atom<'a>,
{
    let entry0 = Entry {
        addr: addr0.as_atomic_cas_word(),
        exp: exp0.into(),
        new: new0.into(),
    };

    let entry1 = Entry {
        addr: addr1.as_atomic_cas_word(),
        exp: exp1.into(),
        new: new1.into(),
    };

    let mut entries = [entry0, entry1];
    let descriptor_ptr = CAS2_DESCRIPTOR.make_descriptor(&mut entries);
    CAS2_DESCRIPTOR.cas2_help(descriptor_ptr, false)
}

pub unsafe fn cas_n<A>(
    addresses: &[&A],
    expected: &[<A as traits::Atom>::Word],
    new: &[<A as traits::Atom>::Word],
) -> bool
where
    for<'a> A: traits::Atom<'a>,
{
    assert_eq!(addresses.len(), expected.len());
    assert_eq!(expected.len(), new.len());
    assert!(addresses.len() <= MAX_ENTRIES);
    let mut entries = ArrayVec::<[Entry<'_>; MAX_ENTRIES]>::new();
    for ((addr, exp), new) in addresses.iter().zip(expected).zip(new) {
        let entry = Entry {
            addr: addr.as_atomic_cas_word(),
            exp: (*exp).into(),
            new: (*new).into(),
        };
        entries.push(entry);
    }
    let descriptor_ptr = CAS2_DESCRIPTOR.make_descriptor(&mut entries);
    CAS2_DESCRIPTOR.cas2_help(descriptor_ptr, false)
}

struct Cas2Descriptor {
    map: ThreadLocal<CachePadded<ThreadCas2Descriptor>>,
}

impl Cas2Descriptor {
    const MARK: usize = 2;
    pub fn new() -> Self {
        Self {
            map: ThreadLocal::new(),
        }
    }

    pub fn make_descriptor(&'static self, entries: &mut [Entry]) -> CasWord {
        let (tid, per_thread_descriptor) = CAS2_DESCRIPTOR
            .map
            .get_or_insert_with(|| CachePadded::new(ThreadCas2Descriptor::empty()));

        // invalidate current descriptor
        per_thread_descriptor.inc_seq();
        // sort and store addresses
        per_thread_descriptor.store_entries(entries);
        // make descriptor fully initialized
        per_thread_descriptor.inc_seq();
        let current_seq_num = per_thread_descriptor.status.load().seq_number();

        // create a ptr for descriptor
        CasWord::new_descriptor_ptr(tid, current_seq_num).with_mark(Self::MARK)
    }

    fn try_snapshot(
        &'static self,
        descriptor_ptr: CasWord,
    ) -> Result<ThreadCas2DescriptorSnapshot, ()> {
        let thread_descriptor = self.map.get_for_thread(descriptor_ptr.tid()).unwrap();
        thread_descriptor.try_snapshot(descriptor_ptr.seq())
    }

    pub fn cas2_help(&'static self, descriptor_ptr: CasWord, help_other: bool) -> bool {
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
                        }
                    };
                if descriptor_current_status.status() == Cas2DescriptorStatus::UNDECIDED {
                    let mut new_status = Cas2DescriptorStatus::succeeded(descriptor_seq);
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
                    descriptor_snapshot.cas_status(descriptor_current_status, new_status);
                }
                let descriptor_current_status =
                    match descriptor_snapshot.try_read_status(descriptor_ptr) {
                        Ok(status) => status,
                        Err(()) => {
                            assert!(help_other);
                            return false;
                        }
                    };

                let succeeded =
                    descriptor_current_status.status() == Cas2DescriptorStatus::SUCCEEDED;
                for entry in &descriptor_snapshot.entries {
                    let new = if succeeded { entry.new } else { entry.exp };
                    let _ = entry.addr.compare_exchange(descriptor_ptr, new);
                }
                succeeded
            }
            Err(_) => {
                assert!(help_other);
                // nothing to do, thread we was trying to help, already finished this operation.
                false
            }
        }
    }
}

const MAX_ENTRIES: usize = 8;

struct ThreadCas2Descriptor {
    pub entries: [AtomicEntry; MAX_ENTRIES],
    pub num_entries: StdAtomicUsize,
    pub status: Cas2DescriptorStatusCell,
}

impl ThreadCas2Descriptor {
    fn empty() -> Self {
        Self {
            status: Cas2DescriptorStatusCell::new(),
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

    fn inc_seq(&self) {
        let seq_num = self.status.load().seq_number().inc();
        self.status.store(Cas2DescriptorStatus::undecided(seq_num))
    }

    fn try_snapshot(&self, seq_num: SeqNumber) -> Result<ThreadCas2DescriptorSnapshot, ()> {
        let current_seq_num = self.status.load().seq_number();
        if current_seq_num == seq_num {
            let num_entries = self.num_entries.load(Ordering::SeqCst);

            assert!(num_entries >= 2);
            let entries = self.entries[0..num_entries]
                .iter()
                .map(|atomic_entry| atomic_entry.load())
                .collect();

            if seq_num == self.status.load().seq_number() {
                Ok(ThreadCas2DescriptorSnapshot {
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
        entries.sort_by_key(|e| e.addr as *const AtomicCasWord);
        for (atomic_entry, entry) in self.entries.iter().zip(&*entries) {
            atomic_entry.store(entry);
        }
        self.num_entries.store(entries.len(), Ordering::SeqCst);
    }
}

struct ThreadCas2DescriptorSnapshot<'a> {
    entries: ArrayVec<[Entry<'a>; MAX_ENTRIES]>,
    status: &'a Cas2DescriptorStatusCell,
}

impl ThreadCas2DescriptorSnapshot<'_> {
    fn try_read_status(&self, descriptor_ptr: CasWord) -> Result<Cas2DescriptorStatus, ()> {
        let status = self.status.load();
        if status.seq_number() == descriptor_ptr.seq() {
            Ok(status)
        } else {
            Err(())
        }
    }

    fn cas_status(&self, expected_status: Cas2DescriptorStatus, new_status: Cas2DescriptorStatus) {
        assert_eq!(expected_status.status(), Cas2DescriptorStatus::UNDECIDED);
        let current_status = self.status.load();
        if current_status == expected_status {
            let _ = self.status.compare_exchange(expected_status, new_status);
        }
    }
}

pub struct Cas2DescriptorStatusCell(StdAtomicUsize);

impl Cas2DescriptorStatusCell {
    pub fn new() -> Self {
        Self(StdAtomicUsize::new(0))
    }

    pub fn load(&self) -> Cas2DescriptorStatus {
        Cas2DescriptorStatus(self.0.load(Ordering::SeqCst))
    }

    pub fn store(&self, status: Cas2DescriptorStatus) {
        self.0.store(status.0, Ordering::SeqCst);
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
        self.0 & ((1 << Self::NUM_STATUS_BITS) - 1)
    }

    fn from_usize(status: usize) -> Self {
        Self(status)
    }
}

struct AtomicEntry {
    addr: AtomicCasWordCell,
    exp: AtomicCasWord,
    new: AtomicCasWord,
}

impl AtomicEntry {
    fn empty() -> Self {
        Self {
            addr: AtomicCasWordCell::empty(),
            exp: AtomicCasWord::null(),
            new: AtomicCasWord::null(),
        }
    }

    fn load<'a>(&self) -> Entry<'a> {
        let addr = self.addr.load();
        let exp = self.exp.load();
        let new = self.new.load();
        Entry { addr, exp, new }
    }

    fn store(&self, e: &Entry) {
        self.addr.store(e.addr);
        self.new.store(e.new);
        self.exp.store(e.exp);
    }
}

struct Entry<'a> {
    addr: &'a AtomicCasWord,
    exp: CasWord,
    new: CasWord,
}

pub mod traits {
    use crate::casword::{AtomicCasWord, CasWord};
    use crate::mwcas::{AtomicUsize, Cas2Descriptor, CAS2_DESCRIPTOR};
    use crate::rdcss::RDCSS_DESCRIPTOR;
    use crate::Atomic;
    use crossbeam_epoch::Shared;

    pub trait Atom<'a>: super::sealed::Sealed {
        type Word: Into<CasWord> + From<CasWord> + Copy;

        fn as_atomic_cas_word(&self) -> &AtomicCasWord;

        fn load_word(&self) -> CasWord {
            loop {
                let curr = RDCSS_DESCRIPTOR.read(self.as_atomic_cas_word());
                if curr.mark() == Cas2Descriptor::MARK {
                    CAS2_DESCRIPTOR.cas2_help(curr, true);
                } else {
                    return curr;
                }
            }
        }
    }

    impl<'a, T: 'a> Atom<'a> for Atomic<T> {
        type Word = Shared<'a, T>;

        fn as_atomic_cas_word(&self) -> &AtomicCasWord {
            &self.data
        }
    }

    impl Atom<'_> for AtomicUsize {
        type Word = usize;

        fn as_atomic_cas_word(&self) -> &AtomicCasWord {
            &self.data
        }
    }
}

mod sealed {
    pub trait Sealed {}

    impl<T> Sealed for super::Atomic<T> {}

    impl Sealed for super::AtomicUsize {}
}

#[cfg(test)]
mod test {
    use super::*;
    use crossbeam_epoch::{pin, Owned};
    use std::sync::Arc;

    #[test]
    fn test_mcas() {
        let g = pin();
        let atom0 = Atomic::new(0);
        let exp0 = atom0.load(&g);
        let new0 = Owned::new(1).into_shared(&g);

        let atom1 = Atomic::new(0);
        let exp1 = atom1.load(&g);
        let new1 = Owned::new(1).into_shared(&g);
        let succeeded = unsafe { cas2(&atom0, &atom1, exp0, exp1, new0, new1) };
        assert!(succeeded);
        let new0 = atom0.load(&g);
        let new1 = atom1.load(&g);
        unsafe {
            assert_eq!(new0.as_ref().unwrap(), &1);
            assert_eq!(new1.as_ref().unwrap(), &1);
        }

        let succeeded = unsafe { cas2(&atom0, &atom1, exp0, exp1, new0, new1) };
        let curr0 = atom0.load(&g);
        let curr1 = atom0.load(&g);
        unsafe {
            assert_eq!(curr0.as_ref().unwrap(), &1);
            assert_eq!(curr1.as_ref().unwrap(), &1);
        }

        assert!(!succeeded);
    }

    #[test]
    fn counter_test() {
        let mut handles = Vec::new();
        let counter = Arc::new((Atomic::new(0), Atomic::new(0)));
        let max = 100_000;
        for _ in 0..8 {
            let counter = counter.clone();
            let h = std::thread::spawn(move || loop {
                unsafe {
                    let g = crossbeam_epoch::pin();
                    let curr_first_shared = counter.0.load(&g);
                    let curr_second_shared = counter.1.load(&g);
                    let curr_first = curr_first_shared.deref();
                    let curr_second = curr_second_shared.deref();
                    if *curr_first == max {
                        break;
                    }

                    let new_first = Owned::new(*curr_first + 1).into_shared(&g);
                    let new_second = Owned::new(*curr_second + 1).into_shared(&g);

                    if cas2(
                        &counter.0,
                        &counter.1,
                        curr_first_shared,
                        curr_second_shared,
                        new_first,
                        new_second,
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
            let first = counter.0.into_owned();
            assert_eq!(*first, max);

            let second = counter.1.into_owned();
            assert_eq!(*second, max);
        }
    }
}

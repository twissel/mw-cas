use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::thread_local::ThreadId;
use crossbeam_epoch::Shared;

const SHIFT: usize = 2;
use crossbeam_epoch::Pointer;

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct CasWord(usize);

impl CasWord {
    pub fn new_descriptor_ptr(tid: ThreadId, seq: SeqNumber) -> Self {
        let tid = (tid.as_u16() as usize) << (SeqNumber::LENGTH + SHIFT);
        Self(tid | (seq.as_usize() << SHIFT))
    }

    pub fn tid(&self) -> ThreadId {
        ThreadId::from_u16((self.0 >> (SeqNumber::LENGTH + SHIFT)) as u16)
    }

    pub fn seq(&self) -> SeqNumber {
        let mask = (1usize << (SeqNumber::LENGTH + SHIFT)) - 1;
        let seq = (self.0 & mask) >> SHIFT;
        SeqNumber::from_usize(seq)
    }

    pub fn with_mark(self, mark: usize) -> Self {
        let bits = mark & (SHIFT + 1);
        let marked = self.0 | bits;
        Self(marked)
    }

    pub fn mark(self) -> usize {
        self.0 & (SHIFT + 1)
    }

    pub fn into_usize(self) -> usize {
        self.0
    }

    pub fn from_usize(raw: usize) -> Self {
        Self(raw)
    }
}

impl<T> From<Shared<'_, T>> for CasWord {
    fn from(s: Shared<'_, T>) -> Self {
        CasWord::from_usize(s.with_tag(0).into_usize())
    }
}

impl<T> From<CasWord> for Shared<'_, T> {
    fn from(w: CasWord) -> Self {
        unsafe { Shared::from_usize(w.into_usize()) }
    }
}

impl From<usize> for CasWord {
    fn from(r: usize) -> Self {
        CasWord(r << SHIFT)
    }
}

impl From<CasWord> for usize {
    fn from(w: CasWord) -> Self {
        w.into_usize() >> SHIFT
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct SeqNumber(usize);

impl SeqNumber {
    pub const LENGTH: usize = 48;
    pub fn inc(&self) -> SeqNumber {
        Self(self.0 + 1)
    }
}

#[derive(Debug)]
pub struct SeqNumberGenerator(AtomicUsize);

impl SeqNumberGenerator {
    pub fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    pub fn inc(&self) -> SeqNumber {
        let curr = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        SeqNumber(curr)
    }

    pub fn current(&self) -> SeqNumber {
        SeqNumber(self.0.load(Ordering::SeqCst))
    }
}

impl SeqNumber {
    pub fn as_usize(&self) -> usize {
        self.0
    }

    pub fn from_usize(v: usize) -> Self {
        Self(v)
    }
}

pub struct AtomicCasWord(AtomicUsize);

impl AtomicCasWord {
    pub fn null() -> Self {
        Self(AtomicUsize::new(0))
    }

    pub fn from_usize(d: usize) -> Self {
        Self(AtomicUsize::new(d))
    }

    pub fn into_inner(self) -> usize {
        self.0.into_inner()
    }

    pub fn load(&self) -> CasWord {
        CasWord::from_usize(self.0.load(Ordering::SeqCst))
    }

    pub fn store(&self, word: CasWord) {
        self.0.store(word.into_usize(), Ordering::SeqCst);
    }

    pub fn compare_exchange(&self, expected: CasWord, new: CasWord) -> Result<CasWord, CasWord> {
        let exchanged = self.0.compare_exchange(
            expected.into_usize(),
            new.into_usize(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        match exchanged {
            Ok(new) => Ok(CasWord::from_usize(new)),
            Err(err) => Err(CasWord::from_usize(err)),
        }
    }
}

pub struct AtomicAddress<T>(AtomicPtr<T>);

impl<T> AtomicAddress<T> {
    pub fn empty() -> Self {
        Self(AtomicPtr::default())
    }

    pub fn store(&self, ptr: &T) {
        self.0.store(ptr as *const _ as *mut _, Ordering::SeqCst);
    }

    // TODO: make this function unsafe
    pub fn load<'a>(&self) -> &'a T {
        let ptr = self.0.load(Ordering::SeqCst);
        unsafe { &*ptr }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_descriptor_ptr() {
        let seq_number = SeqNumber::from_usize(20000);
        let tid = ThreadId::from_u16(2u16.pow(14) - 1);
        let descriptor = CasWord::new_descriptor_ptr(tid, seq_number);
        assert_eq!(descriptor.tid(), tid);
        assert_eq!(descriptor.seq(), seq_number);

        let marked_descriptor = descriptor.with_mark(1);
        assert_eq!(marked_descriptor.mark(), 1);
        let marked_descriptor = descriptor.with_mark(2);
        assert_eq!(marked_descriptor.mark(), 2);
        assert_eq!(marked_descriptor.tid(), tid);
        assert_eq!(marked_descriptor.seq(), seq_number);
    }
}

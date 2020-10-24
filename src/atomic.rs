use crate::{
    mwcas::{CasNDescriptor, CASN_DESCRIPTOR},
    rdcss::RDCSS_DESCRIPTOR,
    sequence_number::SeqNumber,
    thread_local::ThreadId,
};
use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

#[repr(transparent)]
pub struct Atomic<T: Word> {
    v: UnsafeCell<T>,
}

impl<T: Word> Atomic<T> {
    pub fn new(t: T) -> Self {
        Self {
            v: UnsafeCell::new(t),
        }
    }

    pub fn load(&self) -> T {
        loop {
            let curr = RDCSS_DESCRIPTOR.read(self.as_atomic_bits());
            if curr.mark() == CasNDescriptor::MARK {
                CASN_DESCRIPTOR.help(curr, true);
            } else {
                return curr.into();
            }
        }
    }

    pub(crate) fn as_atomic_bits(&self) -> &AtomicBits {
        unsafe {
            let v = self.v.get() as *const T as *const AtomicBits;
            &*v
        }
    }
}

pub trait Word: sealed::Word + Into<Bits> + From<Bits> + Copy {}
impl<T> Word for *mut T {}

impl<T> From<*mut T> for Bits {
    fn from(ptr: *mut T) -> Self {
        Bits::from_usize(ptr as _)
    }
}

impl<T> From<Bits> for *mut T {
    fn from(bits: Bits) -> Self {
        bits.into_usize() as _
    }
}

impl<T> Word for *const T {}

impl<T> From<*const T> for Bits {
    fn from(ptr: *const T) -> Self {
        Bits::from_usize(ptr as _)
    }
}

impl<T> From<Bits> for *const T {
    fn from(bits: Bits) -> Self {
        bits.into_usize() as _
    }
}

impl Word for usize {}

impl From<usize> for Bits {
    fn from(int: usize) -> Self {
        Bits::from_usize(int << Bits::NUM_RESERVED_BITS)
    }
}

impl From<Bits> for usize {
    fn from(w: Bits) -> Self {
        w.into_usize() >> Bits::NUM_RESERVED_BITS
    }
}

unsafe impl<T: Word> Sync for Atomic<T> {}
unsafe impl<T: Word> Send for Atomic<T> {}

mod sealed {

    pub trait Word {}

    impl<T> Word for *mut T {}
    impl<T> Word for *const T {}
    impl Word for usize {}
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub struct Bits(usize);

impl Bits {
    pub const NUM_RESERVED_BITS: usize = 2;

    pub fn new_descriptor_ptr(tid: ThreadId, seq: SeqNumber) -> Self {
        let tid =
            (tid.as_u16() as usize) << (SeqNumber::LENGTH + Self::NUM_RESERVED_BITS);
        Self(tid | (seq.as_usize() << Self::NUM_RESERVED_BITS))
    }

    pub fn tid(self) -> ThreadId {
        ThreadId::from_u16(
            (self.0 >> (SeqNumber::LENGTH + Self::NUM_RESERVED_BITS)) as u16,
        )
    }

    pub fn seq(self) -> SeqNumber {
        let mask = (1usize << (SeqNumber::LENGTH + Self::NUM_RESERVED_BITS)) - 1;
        let seq = (self.0 & mask) >> Self::NUM_RESERVED_BITS;
        SeqNumber::from_usize(seq)
    }

    pub fn with_mark(self, mark: usize) -> Self {
        let bits = mark & (Self::NUM_RESERVED_BITS + 1);
        let marked = self.0 | bits;
        Self(marked)
    }

    pub fn mark(self) -> usize {
        self.0 & (Self::NUM_RESERVED_BITS + 1)
    }

    pub fn into_usize(self) -> usize {
        self.0
    }

    pub fn from_usize(raw: usize) -> Self {
        Self(raw)
    }
}

#[repr(transparent)]
pub struct AtomicBits(AtomicUsize);

impl AtomicBits {
    pub fn empty() -> Self {
        Self(AtomicUsize::new(0))
    }

    pub fn load(&self, ord: Ordering) -> Bits {
        Bits::from_usize(self.0.load(ord))
    }

    pub fn store(&self, word: Bits, ord: Ordering) {
        self.0.store(word.into_usize(), ord);
    }

    pub fn compare_exchange(&self, expected: Bits, new: Bits) -> Result<Bits, Bits> {
        let exchanged = self.0.compare_exchange(
            expected.into_usize(),
            new.into_usize(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        match exchanged {
            Ok(new) => Ok(Bits::from_usize(new)),
            Err(err) => Err(Bits::from_usize(err)),
        }
    }
}

pub struct AtomicAddress<T>(AtomicPtr<T>);

impl<T> AtomicAddress<T> {
    pub fn empty() -> Self {
        Self(AtomicPtr::default())
    }

    pub fn store(&self, ptr: &T, ordering: Ordering) {
        self.0.store(ptr as *const _ as *mut _, ordering);
    }

    // safety: store was called previously
    pub unsafe fn load<'a>(&self, ordering: Ordering) -> &'a T {
        let ptr = self.0.load(ordering);
        &*ptr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_descriptor_ptr() {
        let seq_number = SeqNumber::from_usize(20000);
        let tid = ThreadId::from_u16(2u16.pow(14) - 1);
        let descriptor = Bits::new_descriptor_ptr(tid, seq_number);
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

use crate::thread_local::ThreadId;
use std::sync::atomic::{AtomicUsize, Ordering};

const NUM_RESERVED_BITS: usize = 3;
const SEQ_NUMBER_LENGTH: usize = 50;

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct DescriptorPtr(usize);

impl DescriptorPtr {
    pub fn new(tid: ThreadId, seq: SeqNumber) -> Self {
        let tid = (tid.as_u16() as usize) << SEQ_NUMBER_LENGTH;
        Self(tid | (seq.as_usize() << NUM_RESERVED_BITS))
    }

    pub fn tid(&self) -> ThreadId {
        unsafe { ThreadId::from_u16((self.0 >> SEQ_NUMBER_LENGTH) as u16) }
    }

    pub fn seq(&self) -> SeqNumber {
        let mask = (1usize << (SEQ_NUMBER_LENGTH)) - 1;
        let seq = (self.0 & mask) >> 3;
        SeqNumber::from_usize(seq)
    }

    pub fn with_mark(self, mark: usize) -> Self {
        let bits = mark & NUM_RESERVED_BITS;
        let marked = self.0 | bits;
        Self(marked)
    }

    pub fn mark(&self) -> usize {
        self.0 & NUM_RESERVED_BITS
    }

    pub fn unmark(self) -> Self {
        Self(self.0 & !NUM_RESERVED_BITS)
    }

    pub fn into_usize(self) -> usize {
        self.0
    }

    pub unsafe fn from_usize(raw: usize) -> Self {
        Self(raw)
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct SeqNumber(usize);

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

    fn from_usize(v: usize) -> Self {
        Self(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_descriptor_ptr() {
        let seq_number = SeqNumber::from_usize(20000);
        let tid = unsafe { ThreadId::from_u16(2u16.pow(14) - 1) };
        let descriptor = DescriptorPtr::new(tid, seq_number);
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

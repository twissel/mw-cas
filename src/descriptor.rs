use crate::thread_local::ThreadId;
use std::sync::atomic::AtomicUsize;

#[repr(transparent)]
pub struct DescriptorPtr(usize);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct SeqNumber(usize);

pub struct SeqNumberGenerator(AtomicUsize);


impl SeqNumber {
    pub fn as_usize(&self) -> usize {
        self.0
    }

    fn from_usize(v: usize) -> Self {
        Self(v)
    }
}

impl DescriptorPtr {
    pub fn new(tid: ThreadId, seq: SeqNumber) -> Self {
        let tid = (tid.as_u16() as usize) << 50;
        Self(tid | seq.as_usize())
    }

    pub fn tid(&self) -> ThreadId {
        unsafe {
            ThreadId::from_u16((self.0 >> 50) as u16)
        }

    }

    pub fn seq(&self) -> SeqNumber {
        let mask = (1usize << 50) - 1;
        SeqNumber::from_usize(self.0 & mask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let seq_number = SeqNumber::from_usize(2usize.pow(50) - 1);
        let tid = unsafe { ThreadId::from_u16(2u16.pow(14) - 1)};
        let descriptor = DescriptorPtr::new(tid, seq_number);
        assert_eq!(descriptor.tid(), tid);
        assert_eq!(descriptor.seq(), seq_number);
    }
}
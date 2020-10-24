use std::sync::atomic::{AtomicUsize, Ordering};

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

    pub fn inc(&self, store_ordering: Ordering) -> SeqNumber {
        let new = self.0.load(Ordering::Relaxed) + 1;
        self.0.store(new, store_ordering);
        SeqNumber(new)
    }

    pub fn current(&self, ordering: Ordering) -> SeqNumber {
        SeqNumber(self.0.load(ordering))
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

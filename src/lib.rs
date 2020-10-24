#![cfg(target_pointer_width = "64")]

mod casword;
mod mwcas;
pub(crate) mod rdcss;
mod thread_local;

pub use mwcas::{cas2, cas_n, Atomic, AtomicUsize};

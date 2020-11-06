#![cfg(target_pointer_width = "64")]

mod atomic;
mod mwcas;
pub(crate) mod rdcss;
mod sequence_number;
mod thread_local;

#[cfg(test)]
mod list;

pub use mwcas::{cas2, cas_n, Atomic, CASN};

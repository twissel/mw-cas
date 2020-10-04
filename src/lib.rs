#![cfg(target_pointer_width = "64")]

pub(crate) mod rdcss;

mod casword;
mod thread_local;

mod mwcas;

pub use mwcas::{cas2, cas_n, Atomic};

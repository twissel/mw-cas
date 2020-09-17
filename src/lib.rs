#![cfg(target_pointer_width = "64")]

pub(crate) mod rdcss;

mod ptr;
mod thread_local;

mod mcas;

pub use mcas::{Atomic, cas2};

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}

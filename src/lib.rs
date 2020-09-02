//#![cfg(target_pointer_width = "64")]

pub(crate) mod thread_local;
pub(crate) mod descriptor;
pub(crate) mod rdcss;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}

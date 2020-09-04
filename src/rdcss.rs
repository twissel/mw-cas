use crate::descriptor::{DescriptorPtr, SeqNumberGenerator};
use crate::thread_local::ThreadLocal;
use crossbeam_epoch::{Atomic, Guard, Owned, Pointer, Shared};
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

const DESCRIPTOR_MARKER: usize = 1;

static RDCSS_DESCRIPTOR: Lazy<RDCSSDescriptor> = Lazy::new(|| RDCSSDescriptor::new());

struct PerThreadRDCSSDescriptor {
    status_location_cell: AtomicPtr<AtomicUsize>,
    data_location_cell: AtomicPtr<AtomicUsize>,
    expected_status_cell: AtomicUsize,
    expected_ptr_cell: AtomicUsize,
    kcas_ptr_cell: AtomicUsize,
    seq_number: SeqNumberGenerator,
}

impl PerThreadRDCSSDescriptor {
    fn new() -> Self {
        Self {
            status_location_cell: AtomicPtr::default(),
            data_location_cell: AtomicPtr::default(),
            expected_status_cell: AtomicUsize::new(0),
            expected_ptr_cell: AtomicUsize::new(0),
            kcas_ptr_cell: AtomicUsize::new(0),
            seq_number: SeqNumberGenerator::new(),
        }
    }

    fn read_fields(&self) -> PerThreadsDescriptorFields {
        unsafe {
            let addr0: *mut AtomicUsize = self.status_location_cell.load(Ordering::SeqCst);
            let addr1: *mut AtomicUsize = self.data_location_cell.load(Ordering::SeqCst);
            let exp0: usize = self.expected_status_cell.load(Ordering::SeqCst);
            let exp1: usize = self.expected_ptr_cell.load(Ordering::SeqCst);
            let new: usize = self.kcas_ptr_cell.load(Ordering::SeqCst);
            PerThreadsDescriptorFields {
                status_location: &*addr0,
                data_location: &*addr1,
                expected_status: exp0,
                expected_data_ptr: exp1,
                kcas_ptr: new,
            }
        }
    }
}

struct PerThreadsDescriptorFields<'g> {
    status_location: &'g AtomicUsize,
    data_location: &'g AtomicUsize,
    expected_status: usize,
    expected_data_ptr: usize,
    kcas_ptr: usize,
}

pub struct RDCSSDescriptor {
    per_thread_descriptors: ThreadLocal<PerThreadRDCSSDescriptor>,
}

impl RDCSSDescriptor {
    pub fn new() -> Self {
        Self {
            per_thread_descriptors: ThreadLocal::new(),
        }
    }

    pub fn new_ptr(
        &self,
        status_location: &AtomicUsize,
        data_location: &AtomicUsize,
        expected_status: usize,
        expected_data: usize,
        new_kcas_ptr: usize,
    ) -> DescriptorPtr {
        let (thread_id, per_thread_descriptor) = self
            .per_thread_descriptors
            .get_or_insert_with(PerThreadRDCSSDescriptor::new);
        per_thread_descriptor.seq_number.inc();
        per_thread_descriptor
            .status_location_cell
            .store(status_location as *const AtomicUsize as *mut _, Ordering::SeqCst);
        per_thread_descriptor
            .data_location_cell
            .store(data_location as *const AtomicUsize as *mut _, Ordering::SeqCst);

        per_thread_descriptor.expected_status_cell.store(expected_status, Ordering::SeqCst);
        per_thread_descriptor.expected_ptr_cell.store(expected_data, Ordering::SeqCst);
        per_thread_descriptor.kcas_ptr_cell.store(new_kcas_ptr, Ordering::SeqCst);

        let new_seq = per_thread_descriptor.seq_number.inc();
        DescriptorPtr::new(thread_id, new_seq).with_mark(DESCRIPTOR_MARKER)
    }

    pub fn rdcss<'g>(
        &self,
        status_location: &AtomicUsize,
        data_location: &AtomicUsize,
        expected_status: usize,
        expected_data_ptr: usize,
        new_kcas_ptr: usize,
    ) -> usize {
        let des_ptr = self.new_ptr(status_location, data_location, expected_status, expected_data_ptr, new_kcas_ptr);
        unsafe {
            loop {
                let installed = data_location.compare_exchange(
                    expected_data_ptr,
                    des_ptr.into_usize(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                match installed {
                    Ok(_) => {
                        self.rdcss_help(des_ptr);
                        return expected_data_ptr;
                    }
                    Err(new_curr) => {
                        let new_curr = DescriptorPtr::from_usize(new_curr);
                        if is_marked(new_curr) {
                            self.rdcss_help(new_curr);
                            continue;
                        } else {
                            return new_curr.into_usize();
                        }
                    }
                }
            }
        }
    }

    fn rdcss_help<'g>(&self, des: DescriptorPtr) {
        let fields = self.read_fields(des);
        unsafe {
            if let Ok(fields) = fields {
                let curr_status = fields.status_location.load(Ordering::SeqCst);
                if curr_status == fields.expected_status {
                    let _ = fields.data_location.compare_exchange(
                        des.into_usize(),
                        fields.kcas_ptr,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                } else {
                    let _ = fields.data_location.compare_exchange(
                        des.into_usize(),
                        fields.expected_data_ptr,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                }
            }
        }
    }

    fn read_fields(&self, des: DescriptorPtr) -> Result<PerThreadsDescriptorFields, ()> {
        let tid = des.tid();
        let seq = des.seq();
        let curr_thread_desciptor = self
            .per_thread_descriptors
            .get_for_thread(tid)
            .expect("Missing thread descriptor");
        if seq != curr_thread_desciptor.seq_number.current() {
            Err(())
        } else {
            let fields = curr_thread_desciptor.read_fields();
            if seq != curr_thread_desciptor.seq_number.current() {
                Err(())
            } else {
                Ok(fields)
            }
        }
    }
}

pub fn is_marked(ptr: DescriptorPtr) -> bool {
    ptr.mark() == DESCRIPTOR_MARKER
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdcss::RDCSSDescriptor;
    use crossbeam_epoch::pin;

    #[test]
    fn test_descriptor() {
        let g = pin();
        let atom = AtomicUsize::new(1000);
        let atom_exp = 1000;
        let rdcss_atom = AtomicUsize::new(10);
        let rdcss_exp = 10;
        let des = RDCSSDescriptor::new();
        let rdcss_new = 2000;
        let ptr = des.new_ptr(&atom, &rdcss_atom, atom_exp, rdcss_exp, rdcss_new);
        assert!(is_marked(ptr));
        let swapped = des.rdcss(&atom, &rdcss_atom, atom_exp, rdcss_exp, rdcss_new);
        assert_eq!(swapped, rdcss_exp);

        atom.store(10001, Ordering::SeqCst);
        let swapped = des.rdcss(&atom, &rdcss_atom, atom_exp, rdcss_exp, rdcss_new);
        assert_ne!(swapped, rdcss_exp);

        atom.store(1000, Ordering::SeqCst);
        rdcss_atom.store(11, Ordering::SeqCst);
        let swapped = des.rdcss(&atom, &rdcss_atom, atom_exp, rdcss_exp, rdcss_new);
        assert_ne!(swapped, rdcss_exp);
        assert_ne!(11, rdcss_exp);
    }
}

use crate::descriptor::{MarkedPtr, SeqNumberGenerator};
use crate::mcas::{Cas2DescriptorStatus, Cas2DescriptorStatusCell};
use crate::ptr::{AtomicMarkedPtr, PtrCell};
use crate::thread_local::ThreadLocal;
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicPtr, Ordering, fence};
use crossbeam_utils::CachePadded;

pub(crate) static RDCSS_DESCRIPTOR: Lazy<RDCSSDescriptor> = Lazy::new(|| RDCSSDescriptor::new());

struct PerThreadRDCSSDescriptor {
    status_location_cell: AtomicPtr<Cas2DescriptorStatusCell>,
    data_location_cell: PtrCell,
    expected_status_cell: Cas2DescriptorStatusCell,
    expected_ptr_cell: AtomicMarkedPtr,
    kcas_ptr_cell: AtomicMarkedPtr,
    seq_number: SeqNumberGenerator,
}

impl PerThreadRDCSSDescriptor {
    fn new() -> Self {
        Self {
            status_location_cell: AtomicPtr::default(),
            data_location_cell: PtrCell::empty(),
            expected_status_cell: Cas2DescriptorStatusCell::new(),
            expected_ptr_cell: AtomicMarkedPtr::null(),
            kcas_ptr_cell: AtomicMarkedPtr::null(),
            seq_number: SeqNumberGenerator::new(),
        }
    }

    fn read_fields(&self) -> PerThreadsDescriptorFields {
        unsafe {
            let status_location: *mut Cas2DescriptorStatusCell =
                self.status_location_cell.load(Ordering::SeqCst);
            let data_location: &AtomicMarkedPtr = self.data_location_cell.load();
            let expected_status: Cas2DescriptorStatus = self.expected_status_cell.load();
            let expected_data_ptr = self.expected_ptr_cell.load();
            let kcas_ptr = self.kcas_ptr_cell.load();
            PerThreadsDescriptorFields {
                status_location: &*status_location,
                data_location,
                expected_status,
                expected_data_ptr,
                kcas_ptr,
            }
        }
    }
}

struct PerThreadsDescriptorFields<'g> {
    status_location: &'g Cas2DescriptorStatusCell,
    data_location: &'g AtomicMarkedPtr,
    expected_status: Cas2DescriptorStatus,
    expected_data_ptr: MarkedPtr,
    kcas_ptr: MarkedPtr,
}

pub struct RDCSSDescriptor {
    per_thread_descriptors: ThreadLocal<CachePadded<PerThreadRDCSSDescriptor>>,
}

impl RDCSSDescriptor {
    pub const MARK: usize = 1;
    fn new() -> Self {
        Self {
            per_thread_descriptors: ThreadLocal::new(),
        }
    }

    fn make_descriptor(
        &self,
        status_location: &Cas2DescriptorStatusCell,
        data_location: &AtomicMarkedPtr,
        expected_status: Cas2DescriptorStatus,
        expected_data: MarkedPtr,
        new_kcas_ptr: MarkedPtr,
    ) -> MarkedPtr {
        let (thread_id, per_thread_descriptor) = self
            .per_thread_descriptors
            .get_or_insert_with(|| CachePadded::new(PerThreadRDCSSDescriptor::new()));
        per_thread_descriptor.seq_number.inc();
        fence(Ordering::SeqCst);
        per_thread_descriptor.status_location_cell.store(
            status_location as *const Cas2DescriptorStatusCell as *mut _,
            Ordering::SeqCst,
        );
        per_thread_descriptor
            .data_location_cell
            .store(data_location);

        per_thread_descriptor
            .expected_status_cell
            .store(expected_status);
        per_thread_descriptor
            .expected_ptr_cell
            .store(expected_data);
        per_thread_descriptor
            .kcas_ptr_cell
            .store(new_kcas_ptr);

        let new_seq = per_thread_descriptor.seq_number.inc();
        MarkedPtr::new(thread_id, new_seq).with_mark(Self::MARK)
    }

    pub(crate) fn rdcss(
        &self,
        status_location: &Cas2DescriptorStatusCell,
        data_location: &AtomicMarkedPtr,
        expected_status: Cas2DescriptorStatus,
        expected_data_ptr: MarkedPtr,
        new_kcas_ptr: MarkedPtr,
    ) -> MarkedPtr {
        let des_ptr = self.make_descriptor(
            status_location,
            data_location,
            expected_status,
            expected_data_ptr,
            new_kcas_ptr,
        );
        loop {
            let installed = data_location.compare_exchange(
                expected_data_ptr,
                des_ptr,
            );
            match installed {
                Ok(_) => {
                    self.rdcss_help(des_ptr);
                    return expected_data_ptr;
                }
                Err(new_curr) => {
                    if is_marked(new_curr) {
                        self.rdcss_help(new_curr);
                        continue;
                    } else {
                        return new_curr;
                    }
                }
            }
        }
    }

    fn rdcss_help<'g>(&self, des: MarkedPtr) {
        let fields = self.read_fields(des);
        if let Ok(fields) = fields {
            let curr_status = fields.status_location.load();
            if curr_status == fields.expected_status {
                let _ = fields.data_location.compare_exchange(
                    des,
                    fields.kcas_ptr,
                );
            } else {
                let _ = fields.data_location.compare_exchange(
                    des,
                    fields.expected_data_ptr,
                );
            }
        }
    }

    fn read_fields(&self, des: MarkedPtr) -> Result<PerThreadsDescriptorFields, ()> {
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

    pub(crate) fn read(&self, addr_loc: &AtomicMarkedPtr) -> MarkedPtr {
        loop {
            let ptr =  addr_loc.load();
            if is_marked(ptr) {
                self.rdcss_help(ptr);
            } else {
                return ptr;
            }
        }
    }
}

pub fn is_marked(ptr: MarkedPtr) -> bool {
    ptr.mark() == RDCSSDescriptor::MARK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdcss::RDCSSDescriptor;
    use crossbeam_epoch::pin;

    #[test]
    fn test_descriptor() {
        let g = pin();
        /*let atom = AtomicUsize::new(1000);
        let atom_exp = 1000;
        let rdcss_atom = AtomicUsize::new(10);
        let rdcss_exp = 10;
        let des = RDCSSDescriptor::new();
        let rdcss_new = 2000;
        let ptr = des.make_descriptor(&atom, &rdcss_atom, atom_exp, rdcss_exp, rdcss_new);
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
        assert_ne!(11, rdcss_exp);*/
    }
}

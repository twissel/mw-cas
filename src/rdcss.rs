use crate::{
    atomic::{AtomicAddress, AtomicBits, Bits},
    mwcas::{AtomicCasNDescriptorStatus, CasNDescriptorStatus},
    sequence_number::SeqNumberGenerator,
    thread_local::ThreadLocal,
};
use crossbeam_utils::{Backoff, CachePadded};
use once_cell::sync::Lazy;
use std::sync::atomic::{fence, Ordering};

pub(crate) static RDCSS_DESCRIPTOR: Lazy<RDCSSDescriptor> =
    Lazy::new(|| RDCSSDescriptor::new());

struct ThreadRDCSSDescriptor {
    status_address: AtomicAddress<AtomicCasNDescriptorStatus>,
    data_address: AtomicAddress<AtomicBits>,
    expected_status_cell: AtomicCasNDescriptorStatus,
    expected_ptr_cell: AtomicBits,
    kcas_ptr_cell: AtomicBits,
    seq_number: SeqNumberGenerator,
}

impl ThreadRDCSSDescriptor {
    fn new() -> Self {
        Self {
            status_address: AtomicAddress::empty(),
            data_address: AtomicAddress::empty(),
            expected_status_cell: AtomicCasNDescriptorStatus::new(),
            expected_ptr_cell: AtomicBits::empty(),
            kcas_ptr_cell: AtomicBits::empty(),
            seq_number: SeqNumberGenerator::new(),
        }
    }

    fn snapshot(&self) -> ThreadRDCSSDescriptorSnapshot {
        unsafe {
            let status_location: &AtomicCasNDescriptorStatus =
                self.status_address.load(Ordering::Relaxed);
            let data_location: &AtomicBits = self.data_address.load(Ordering::Relaxed);
            let expected_status: CasNDescriptorStatus =
                self.expected_status_cell.load(Ordering::Relaxed);
            let expected_data_ptr = self.expected_ptr_cell.load(Ordering::Relaxed);
            let kcas_ptr = self.kcas_ptr_cell.load(Ordering::Relaxed);
            ThreadRDCSSDescriptorSnapshot {
                status_location,
                data_location,
                expected_status,
                expected_data_ptr,
                kcas_ptr,
            }
        }
    }
}

impl Default for ThreadRDCSSDescriptor {
    fn default() -> Self {
        Self::new()
    }
}

struct ThreadRDCSSDescriptorSnapshot<'g> {
    status_location: &'static AtomicCasNDescriptorStatus,
    data_location: &'g AtomicBits,
    expected_status: CasNDescriptorStatus,
    expected_data_ptr: Bits,
    kcas_ptr: Bits,
}

pub struct RDCSSDescriptor {
    per_thread_descriptors: ThreadLocal<CachePadded<ThreadRDCSSDescriptor>>,
}

impl RDCSSDescriptor {
    pub const MARK: usize = 1;

    fn new() -> Self {
        Self {
            per_thread_descriptors: ThreadLocal::new(),
        }
    }

    fn make_descriptor(
        &'static self,
        status_ref: &'static AtomicCasNDescriptorStatus,
        data_ref: &AtomicBits,
        expected_status: CasNDescriptorStatus,
        expected_data: Bits,
        new_kcas_ptr: Bits,
    ) -> Bits {
        let (thread_id, per_thread_descriptor) = self.per_thread_descriptors.get();

        per_thread_descriptor.seq_number.inc(Ordering::Relaxed);
        fence(Ordering::Release);

        per_thread_descriptor
            .status_address
            .store(status_ref, Ordering::Relaxed);
        per_thread_descriptor
            .data_address
            .store(data_ref, Ordering::Relaxed);

        per_thread_descriptor
            .expected_status_cell
            .store(expected_status, Ordering::Relaxed);
        per_thread_descriptor
            .expected_ptr_cell
            .store(expected_data, Ordering::Relaxed);
        per_thread_descriptor
            .kcas_ptr_cell
            .store(new_kcas_ptr, Ordering::Relaxed);

        let new_seq = per_thread_descriptor.seq_number.inc(Ordering::Release);
        Bits::new_descriptor_ptr(thread_id, new_seq).with_mark(Self::MARK)
    }

    pub(crate) fn rdcss(
        &'static self,
        status_location: &'static AtomicCasNDescriptorStatus,
        data_location: &AtomicBits,
        expected_status: CasNDescriptorStatus,
        expected_data_ptr: Bits,
        new_kcas_ptr: Bits,
    ) -> Bits {
        let des_ptr = self.make_descriptor(
            status_location,
            data_location,
            expected_status,
            expected_data_ptr,
            new_kcas_ptr,
        );
        let backoff = Backoff::new();
        loop {
            let current = data_location.load(Ordering::SeqCst);
            if is_marked(current) {
                if backoff.is_completed() {
                    self.rdcss_help(des_ptr);
                } else {
                    backoff.spin();
                }
                continue;
            }
            if current != expected_data_ptr {
                return current;
            }
            let installed = data_location.compare_exchange(expected_data_ptr, des_ptr);
            if let Ok(_) = installed {
                self.rdcss_help(des_ptr);
                return expected_data_ptr;
            } else {
                backoff.reset();
            }
        }
    }

    fn rdcss_help(&self, des: Bits) {
        let snapshot = self.try_snapshot(des);
        if let Ok(snapshot) = snapshot {
            let curr_status = snapshot.status_location.load(Ordering::SeqCst);
            if curr_status == snapshot.expected_status {
                let _ = snapshot
                    .data_location
                    .compare_exchange(des, snapshot.kcas_ptr);
            } else {
                let _ = snapshot
                    .data_location
                    .compare_exchange(des, snapshot.expected_data_ptr);
            }
        }
    }

    fn try_snapshot(&self, des: Bits) -> Result<ThreadRDCSSDescriptorSnapshot, ()> {
        let tid = des.tid();
        let seq = des.seq();
        let curr_thread_descriptor = self.per_thread_descriptors.get_for_thread(tid);
        if seq != curr_thread_descriptor.seq_number.current(Ordering::Acquire) {
            Err(())
        } else {
            let fields = curr_thread_descriptor.snapshot();

            fence(Ordering::Acquire);
            if seq != curr_thread_descriptor.seq_number.current(Ordering::Relaxed) {
                Err(())
            } else {
                Ok(fields)
            }
        }
    }

    pub(crate) fn read(&self, addr_loc: &AtomicBits) -> Bits {
        loop {
            let ptr = addr_loc.load(Ordering::SeqCst);
            if is_marked(ptr) {
                self.rdcss_help(ptr);
            } else {
                return ptr;
            }
        }
    }
}

pub fn is_marked(ptr: Bits) -> bool {
    ptr.mark() == RDCSSDescriptor::MARK
}

#[cfg(test)]
mod tests {
    /*
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
        assert_ne!(11, rdcss_exp);
    }
    */
}

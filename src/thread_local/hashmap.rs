use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicPtr, Ordering};

enum ArrayOrKV<V> {
    Array([AtomicPtr<ArrayOrKV<V>>; 64]),
    KV { key: u16, val: V },
}

impl<V> ArrayOrKV<V> {
    fn as_kv(&self) -> (&u16, &V) {
        match self {
            ArrayOrKV::KV { ref key, ref val } => (key, val),
            ArrayOrKV::Array(_) => panic!("ArrayOrKV is ArrayOrKV::Array"),
        }
    }

    fn to_kv(self) -> (u16, V) {
        match self {
            ArrayOrKV::KV { key, val } => (key, val),
            ArrayOrKV::Array(_) => panic!("ArrayOrKV is ArrayOrKV::Array"),
        }
    }
}

pub struct TryInsertError<'a, V> {
    pub current: &'a V,
    pub not_inserted: V,
}

pub struct Uint14HashMap<V> {
    array: [AtomicPtr<ArrayOrKV<V>>; 256],
}

impl<V> Uint14HashMap<V>
where
    V: 'static + Send,
{
    pub fn new() -> Self {
        let mut data: [MaybeUninit<AtomicPtr<ArrayOrKV<V>>>; 256] =
            unsafe { MaybeUninit::uninit().assume_init() };
        for elem in &mut data[..] {
            *elem = MaybeUninit::new(AtomicPtr::default());
        }
        let data = unsafe { std::mem::transmute::<_, [AtomicPtr<ArrayOrKV<V>>; 256]>(data) };
        Self { array: data }
    }

    pub fn try_insert(&self, key: u16, val: V) -> Result<(), TryInsertError<V>> {
        let indx = fst_lvl_idx(key);
        let mut atom_ptr = &self.array[indx];
        let to_insert = Box::into_raw(Box::new(ArrayOrKV::KV { key, val }));
        loop {
            let ptr: *mut ArrayOrKV<V> = atom_ptr.load(Ordering::SeqCst);
            unsafe {
                if ptr.is_null() {
                    let inserted = atom_ptr.compare_exchange(
                        ptr,
                        to_insert,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                    match inserted {
                        Ok(_) => return Ok(()),
                        Err(_) => continue,
                    }
                } else {
                    match *ptr {
                        ArrayOrKV::KV {
                            key: curr_key,
                            ref val,
                        } => {
                            if curr_key == key {
                                let not_inserted = Box::from_raw(to_insert);
                                return Err(TryInsertError {
                                    current: val,
                                    not_inserted: not_inserted.to_kv().1,
                                });
                            } else {
                                try_expand_at(atom_ptr, ptr);
                            }
                        }
                        ArrayOrKV::Array(ref array) => {
                            atom_ptr = &array[scd_lvl_idx(key)];
                            continue;
                        }
                    }
                }
            }
        }
    }

    pub fn get(&self, key: u16) -> Option<&V>
    where V: Sync
    {
        let idx = fst_lvl_idx(key);
        let mut atom_ptr = &self.array[idx];
        loop {
            let ptr: *mut ArrayOrKV<V> = atom_ptr.load(Ordering::SeqCst);
            unsafe {
                if ptr.is_null() {
                    return None;
                } else {
                    match *ptr {
                        ArrayOrKV::KV {
                            key: curr_key,
                            ref val,
                        } => return if curr_key == key { Some(val) } else { None },
                        ArrayOrKV::Array(ref array) => {
                            atom_ptr = &array[scd_lvl_idx(key)];
                            continue;
                        }
                    }
                }
            }
        }
    }
}

fn try_expand_at<V>(at: &AtomicPtr<ArrayOrKV<V>>, curr: *mut ArrayOrKV<V>) -> *mut ArrayOrKV<V> {
    unsafe {
        let mut new_lvl: [MaybeUninit<AtomicPtr<ArrayOrKV<V>>>; 64] =
            MaybeUninit::uninit().assume_init();

        for elem in &mut new_lvl[..] {
            *elem = MaybeUninit::new(AtomicPtr::default());
        }
        let new_lvl = std::mem::transmute::<_, [AtomicPtr<ArrayOrKV<V>>; 64]>(new_lvl);
        let new_idx = match *curr {
            ArrayOrKV::Array(_) => unreachable!("wrong logic"),
            ArrayOrKV::KV { ref key, .. } => scd_lvl_idx(*key),
        };
        new_lvl[new_idx].store(curr, Ordering::SeqCst);
        let new_lvl_ptr = Box::into_raw(Box::new(ArrayOrKV::Array(new_lvl)));
        match at.compare_exchange(curr, new_lvl_ptr, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(o) => o,
            Err(e) => {
                let _ = unsafe { Box::from_raw(new_lvl_ptr) };
                e
            }
        }
    }
}

fn fst_lvl_idx(key: u16) -> usize {
    (key & ((1 << 8) - 1)) as usize
}

fn scd_lvl_idx(key: u16) -> usize {
    ((key >> 8) & ((1 << 6) - 1)) as usize
}

#[cfg(test)]
mod tests {
    use crate::thread_local::hashmap::Uint14HashMap;

    #[test]
    fn it_works() {
        let m = Uint14HashMap::new();
        for key in 0..10000 {
            assert!(m.try_insert(key, key).is_ok());
            assert_eq!(m.get(key), Some(&key));
        }
    }
}

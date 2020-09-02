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

unsafe impl <V: Send> Send for Uint14HashMap<V> {}
unsafe impl <V: Sync> Sync for Uint14HashMap<V> {}

impl<V> Drop for Uint14HashMap<V> {
    fn drop(&mut self) {
        for el in &mut self.array[..] {
            let ptr = el.load(Ordering::Relaxed);
            unsafe {
                if !ptr.is_null() {
                    let b = Box::from_raw(ptr);
                    match *b {
                        ArrayOrKV::Array(second_level) => {
                            for second_level_el in &second_level[..] {
                                let second_level_ptr = second_level_el.load(Ordering::Relaxed);
                                if !second_level_ptr.is_null() {
                                    let sb = Box::from_raw(second_level_ptr);
                                    match *sb {
                                        ArrayOrKV::Array(_) => {
                                            panic!("Second level element is array")
                                        }
                                        ArrayOrKV::KV { .. } => (),
                                    }
                                }
                            }
                        }
                        ArrayOrKV::KV { .. } => (),
                    }
                }
            }
        }
    }
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

    pub fn insert(&self, key: u16, val: V) -> &V {
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
                        Ok(_) => return (&*to_insert).as_kv().1,
                        Err(_) => continue,
                    }
                } else {
                    match *ptr {
                        ArrayOrKV::KV {
                            key: curr_key,
                            ref val,
                        } => {
                            if curr_key == key {
                                let _ = Box::from_raw(to_insert);
                                return val;
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

    // unsafe because `V` can be !Sync
    pub unsafe fn get(&self, key: u16) -> Option<&V>
where {
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
                let _ = Box::from_raw(new_lvl_ptr);
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
            assert_eq!(m.insert(key, key), &key);
            assert_eq!(unsafe { m.get(key) }, Some(&key));
        }
    }
}

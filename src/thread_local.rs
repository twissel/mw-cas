mod hashmap;
use hashmap::Uint14HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(1);

// 14bit thread id
pub struct ThreadId(u16);

const U14_MAX: u64 = 16383;

thread_local! {
       pub static THREAD_ID: ThreadId = ThreadId::new()
}

impl ThreadId {
    pub fn new() -> Self {
        let curr = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);
        if curr >= U14_MAX - 1 {
            panic!("More then 16.000 threads was created")
        } else {
            ThreadId(curr as u16)
        }
    }
}

pub struct ThreadLocal<V> {
    map: Uint14HashMap<V>,
}

impl<V> ThreadLocal<V>
where
    V: Send + 'static,
{
    pub fn get(&self) -> Option<&V> {
        let id = THREAD_ID.with(|id| id.0);

        // safety: safe as only one thread has access to V
        unsafe { self.map.get(id) }
    }

    pub fn get_or_insert_with<F>(&self, f: F) -> &V
    where
        F: FnOnce() -> V,
    {
        let id = THREAD_ID.with(|id| id.0);
        // safety: safe as only one thread has access to V
        let v = unsafe { self.map.get(id) };
        match v {
            Some(v) => v,
            None => self.map.insert(id, f()),
        }
    }

    pub fn get_for_thread(&self, thread_id: ThreadId) -> Option<&V>
    where
        V: Sync,
    {
        // safety: safe as V is Sync
        unsafe { self.map.get(thread_id.0) }
    }
}


#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}

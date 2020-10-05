mod hashmap;
use hashmap::Uint14HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(1);

// 14bit thread id
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
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

    pub fn as_u16(&self) -> u16 {
        self.0
    }

    pub fn from_u16(v: u16) -> Self {
        Self(v)
    }
}

pub struct ThreadLocal<V> {
    map: Uint14HashMap<V>,
}

impl<V> ThreadLocal<V>
where
    V: Send + 'static,
{
    pub fn new() -> Self {
        Self {
            map: Uint14HashMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn get(&self) -> Option<&V> {
        let id = THREAD_ID.with(|id| id.0);

        // safety: safe as only one thread has access to V
        unsafe { self.map.get(id) }
    }

    pub fn get_or_insert_with<F>(&self, f: F) -> (ThreadId, &V)
    where
        F: FnOnce() -> V,
    {
        let id = THREAD_ID.with(|id| *id);
        // safety: safe as only one thread has access to V
        let v = unsafe { self.map.get(id.0) };
        match v {
            Some(v) => (id, v),
            None => (id, self.map.insert(id.0, f())),
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
    use super::ThreadLocal;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn it_works() {
        let mut handles = Vec::new();
        let tls = Arc::new(ThreadLocal::<u64>::new());
        let h = thread::spawn({
            let tls = tls.clone();
            move || {
                let _ = tls.get_or_insert_with(|| 1);
                assert_eq!(tls.get().unwrap(), &1);
            }
        });
        handles.push(h);

        let h = thread::spawn({
            let tls = tls.clone();
            move || {
                let _ = tls.get_or_insert_with(|| 2);
                assert_eq!(tls.get().unwrap(), &2);
            }
        });
        handles.push(h);

        for h in handles {
            h.join().unwrap();
        }
    }
}

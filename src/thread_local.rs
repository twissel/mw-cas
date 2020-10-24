use crossbeam_utils::CachePadded;
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, Ordering};

pub const MAX_THREADS: usize = 1024;
static THREAD_IDS: Lazy<Vec<AtomicBool>> =
    Lazy::new(|| (0..MAX_THREADS).map(|_| AtomicBool::new(false)).collect());

thread_local! {
       static REG_ID: RegisteredThreadId = ThreadId::register();
       pub static THREAD_ID: ThreadId = ThreadId(REG_ID.with(|id| id.0));
}

// 14bit thread id
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct ThreadId(u16);

pub struct RegisteredThreadId(u16);

impl ThreadId {
    fn register() -> RegisteredThreadId {
        for (index, slot) in (&*THREAD_IDS).iter().enumerate() {
            let occupied = slot.load(Ordering::SeqCst);
            if !occupied {
                match slot.compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed) {
                    Ok(_) => return RegisteredThreadId(index as _),
                    Err(_) => {
                        continue;
                    }
                }
            }
        }
        panic!("no free slots left, all {} slots are used", MAX_THREADS);
    }

    pub fn as_u16(&self) -> u16 {
        self.0
    }

    pub fn from_u16(v: u16) -> Self {
        Self(v)
    }
}

impl Drop for RegisteredThreadId {
    fn drop(&mut self) {
        let ids = &*THREAD_IDS;
        ids[self.0 as usize].store(false, Ordering::SeqCst);
    }
}

pub struct ThreadLocal<V> {
    map: Vec<CachePadded<V>>,
}

impl<V> ThreadLocal<V>
where
    V: Send + 'static + Default,
{
    pub fn new() -> Self {
        Self {
            map: (0..MAX_THREADS)
                .map(|_| CachePadded::new(V::default()))
                .collect(),
        }
    }

    pub fn get(&self) -> (ThreadId, &V) {
        let id = THREAD_ID.with(|id| *id);

        // safety: safe as only one thread has access to V
        (id, &*self.map.get(id.0 as usize).unwrap())
    }


    pub fn get_for_thread(&self, thread_id: ThreadId) -> &V
    where
        V: Sync,
    {
        // safety: safe as V is Sync
        &*self.map.get(thread_id.0 as usize).unwrap()
    }
}

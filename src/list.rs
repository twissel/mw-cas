// Doubly linked list
use crate::{cas2, Atomic, CASN};
use crossbeam_epoch::{pin, Guard, Shared};

const DELETED_MARK: usize = 3;

pub struct List<T: Send + Sync + 'static> {
    head: *const Node<T>,
    tail: *const Node<T>,
}

impl<T: Send + Sync + 'static> List<T> {
    pub fn new() -> Self {
        let head = Box::into_raw(Box::new(Node::Head(Head {
            next: Atomic::new(std::ptr::null_mut()),
        }))) as *const Node<T>;

        let tail = Box::into_raw(Box::new(Node::Tail(Tail {
            prev: Atomic::new(std::ptr::null_mut()),
        }))) as *const Node<T>;

        unsafe {
            let exp = std::ptr::null();
            let head_next = &(*head).as_head().next;
            let tail_prev = &(*tail).as_tail().prev;
            let success = cas2(head_next, tail_prev, exp, exp, tail, head);
            assert!(success);
            List { head, tail }
        }
    }

    pub fn head(&self) -> &Head<T> {
        unsafe { (*self.head).as_head() }
    }

    pub fn tail(&self) -> &Tail<T> {
        unsafe { (*self.tail).as_tail() }
    }
}

pub enum Node<T: 'static> {
    Head(Head<T>),
    Tail(Tail<T>),
    Regular(RegularNode<T>),
}

impl<T: 'static> Node<T> {
    fn as_head(&self) -> &Head<T> {
        match self {
            Node::Head(h) => h,
            _ => panic!(),
        }
    }

    fn as_tail(&self) -> &Tail<T> {
        match self {
            Node::Tail(t) => t,
            _ => panic!(),
        }
    }
}

pub struct Head<T: 'static> {
    next: Atomic<*const Node<T>>,
}

impl<T: 'static> Head<T> {
    pub fn next<'g>(&'g self, _guard: &'g Guard) -> Shared<'g, Node<T>>{
        unsafe {
            let n = self.next.load();
            Shared::from(n)
        }
    }

    pub fn insert_after(&self, data: T) {
        let _guard = pin();
        loop {
            let head_next = self.next.load();
            let head_next_prev_ref = unsafe {
                match &*(head_next) {
                    Node::Tail(t) => &t.prev,
                    Node::Regular(r) => &r.prev,
                    _ => {
                        unreachable!()
                    }
                }
            };
            let head_next_prev = head_next_prev_ref.load();
        }
    }
}

pub struct Tail<T: 'static> {
    prev: Atomic<*const Node<T>>,
}

pub struct RegularNode<T: 'static> {
    next: Atomic<*const Node<T>>,
    prev: Atomic<*const Node<T>>,
}

impl<T: 'static> RegularNode<T> {
    fn next<'g>(&'g self, _guard: &'g Guard) -> Shared<'g, Node<T>> {
        let next = self.next.load();
        Shared::from(next)
    }

    fn prev<'g>(&'g self, _guard: &'g Guard) -> Shared<'g, Node<T>> {
        let next = self.prev.load();
        Shared::from(next)
    }
}

#[inline]
fn is_deleted<T>(ptr: *const Node<T>) -> bool {
    (ptr as usize & DELETED_MARK) == DELETED_MARK
}

#[inline]
fn mark_deleted<T>(ptr: *const Node<T>) -> * const Node<T> {
    (ptr as usize | DELETED_MARK) as *const _
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn list_basic() {
        let max_allowed_tag = (1 << std::mem::align_of::<Node<usize>>().trailing_zeros()) - 1;
        dbg!(max_allowed_tag);
        let g = pin();
        let list = List::<usize>::new();
        let head = list.head();
        //head.insert_after(0);
    }
}

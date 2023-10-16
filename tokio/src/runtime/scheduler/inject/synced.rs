#![cfg_attr(
    any(not(all(tokio_unstable, feature = "full")), target_family = "wasm"),
    allow(dead_code)
)]

use std::{cell::RefCell, collections::LinkedList, rc::Rc};

use crate::runtime::task;

pub(crate) struct Synced {
    /// True if the queue is closed.
    pub(super) is_closed: bool,

    /// Linked-list head.
    pub(super) head: Option<task::RawTask>,

    /// Linked-list tail.
    pub(super) tail: Option<task::RawTask>,
}

pub(crate) struct Synced2 {
    /// True if the queue is closed.
    pub(super) is_closed: bool,
    pub(super) list: LinkedList<Vec<task::RawTask>>,
    pub(super) current: Option<Vec<task::RawTask>>,
}

pub(crate) struct SyncedNode {
    tasks: Vec<task::RawTask>,
    /// Pointer to next node
    next: Option<Rc<RefCell<SyncedNode>>>,
}

unsafe impl Send for Synced2 {}
unsafe impl Sync for Synced2 {}

const TASKS_CAPACITY: usize = 16;

impl Synced2 {
    pub(super) fn pop<T: 'static>(&mut self) -> Option<task::Notified<T>> {
        loop {
            if let Some(current) = self.current.as_mut() {
                if let Some(task) = current.pop() {
                    // safety: a `Notified` is pushed into the queue and now it is popped!
                    return Some(unsafe { task::Notified::from_raw(task) });
                }
            }

            self.current = self.list.pop_front();
            if self.current.is_none() {
                return None;
            }
        }
    }

    pub(super)fn push(&mut self, task: task::RawTask) {
        if let Some(current) = self.current.as_mut() {
            if current.len() < TASKS_CAPACITY {
                current.push(task);
                return;
            }
        }

        let mut new_vec = Vec::with_capacity(TASKS_CAPACITY);
        new_vec.push(task);
        if let Some(current) = self.current.take() {
            self.list.push_back(current);
        }
        self.current = Some(new_vec);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.current.is_none() || self.current.as_ref().unwrap().is_empty()
    }
}

unsafe impl Send for Synced {}
unsafe impl Sync for Synced {}

impl Synced {
    pub(super) fn pop<T: 'static>(&mut self) -> Option<task::Notified<T>> {
        let task = self.head?;

        self.head = unsafe { task.get_queue_next() };

        if self.head.is_none() {
            self.tail = None;
        }

        unsafe { task.set_queue_next(None) };

        // safety: a `Notified` is pushed into the queue and now it is popped!
        Some(unsafe { task::Notified::from_raw(task) })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.head.is_none()
    }
}

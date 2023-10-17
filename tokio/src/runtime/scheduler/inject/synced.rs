#![cfg_attr(
    any(not(all(tokio_unstable, feature = "full")), target_family = "wasm"),
    allow(dead_code)
)]

use crate::runtime::task;
use std::ptr::NonNull;

pub(crate) struct Synced {
    /// True if the queue is closed.
    pub(super) is_closed: bool,
    pub(super) head: Option<NonNull<SyncedNode>>,
    pub(super) tail: Option<NonNull<SyncedNode>>,
}

pub(crate) struct SyncedNode {
    index: usize,
    tasks: Vec<task::RawTask>,
    /// Pointer to the next node
    next: Option<NonNull<SyncedNode>>,
}

impl SyncedNode {
    fn is_empty(&self) -> bool {
        self.index == self.tasks.len()
    }
}

unsafe impl Send for Synced {}
unsafe impl Sync for Synced {}

const TASKS_CAPACITY: usize = 16;

impl Synced {
    pub(super) fn pop<T: 'static>(&mut self) -> Option<task::Notified<T>> {
        let head = self.head.as_mut().map(|head| unsafe { head.as_mut() })?;
        let task = head.tasks[head.index];
        head.index += 1;

        if head.index == head.tasks.len() {
            // Safety: if the next is Some, the next must be non-null
            let new_head = unsafe { head.next.map(|next| NonNull::new_unchecked(next.as_ptr())) };
            let old_head = self.head.take().unwrap();
            self.head = new_head;

            if self.head.is_none() {
                self.tail = None;
            }
            // Safety: release the memory of the old head
            drop(unsafe { Box::from_raw(old_head.as_ptr()) });
        }

        Some(unsafe { task::Notified::from_raw(task) })
    }

    pub(super) fn push(&mut self, task: task::RawTask) {
        if let Some(mut tail) = self.tail {
            let tail_ref = unsafe { tail.as_mut() };
            if tail_ref.tasks.len() < tail_ref.tasks.capacity() {
                tail_ref.tasks.push(task);
                return;
            }
        }

        let mut next = Box::new(SyncedNode {
            index: 0,
            tasks: Vec::with_capacity(TASKS_CAPACITY),
            next: None,
        });
        next.tasks.push(task);
        let new_node_ptr = NonNull::from(Box::leak(next));

        if let Some(mut tail) = self.tail {
            // Safety: the current thread write it exclusively in the lock
            unsafe { tail.as_mut().next = Some(new_node_ptr) };
        }
        self.tail = Some(new_node_ptr);

        if self.head.is_none() {
            self.head = self.tail;
        }
    }

    pub(super) fn push_batch(&mut self, tasks: Vec<task::RawTask>) {
        let next = Box::new(SyncedNode {
            index: 0,
            tasks,
            next: None,
        });
        let new_node_ptr = NonNull::from(Box::leak(next));

        if let Some(mut tail) = self.tail {
            // Safety: the current thread write it exclusively
            unsafe { tail.as_mut().next = Some(new_node_ptr) };
        }
        self.tail = Some(new_node_ptr);

        if self.head.is_none() {
            self.head = self.tail;
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.head.is_none() || unsafe { self.head.unwrap().as_ref().is_empty() }
    }
}

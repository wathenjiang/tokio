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
        if let Some(head) = self.head {
            let mut head = unsafe { Box::from_raw(head.as_ptr()) };
            let task = head.tasks[head.index];
            head.index += 1;

            if head.index == TASKS_CAPACITY {
                // Safety: if the next is Some, the next must be non-null
                unsafe {
                    self.head = head.next.map(|next| NonNull::new_unchecked(next.as_ptr()));
                }
                if self.head.is_none() {
                    self.tail = None;
                }
                drop(head);
            } else {
                Box::leak(head);
            }
            return Some(unsafe { task::Notified::from_raw(task) });
        }
        None
    }

    pub(super) fn push(&mut self, task: task::RawTask) {
        if let Some(mut tail) = self.tail {
            let tail_ref = unsafe { tail.as_mut() };
            if tail_ref.tasks.len() < TASKS_CAPACITY {
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

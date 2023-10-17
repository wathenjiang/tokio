#![cfg_attr(
    any(not(all(tokio_unstable, feature = "full")), target_family = "wasm"),
    allow(dead_code)
)]

use std::ptr::NonNull;

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
    pub(super) head: Option<NonNull<SyncedNode>>,
    pub(super) tail: Option<NonNull<SyncedNode>>,
}

pub(crate) struct SyncedNode {
    index: usize,
    tasks: Vec<task::RawTask>,
    /// Pointer to next node
    next: Option<NonNull<SyncedNode>>,
}

impl SyncedNode {
    fn is_empty(&self) -> bool {
        self.index == self.tasks.len()
    }
}

unsafe impl Send for Synced2 {}
unsafe impl Sync for Synced2 {}

const TASKS_CAPACITY: usize = 16;

impl Synced2 {
    pub(super) fn pop<T: 'static>(&mut self) -> Option<task::Notified<T>> {
        if let Some(head) = self.head {
            // 将 head 指针理解为存储于 heap 上的 ListNode
            let mut head = unsafe { Box::from_raw(head.as_ptr()) };
            let task = head.tasks[head.index];
            head.index += 1;
            // head
            if head.index == TASKS_CAPACITY {
                unsafe {
                    self.head = head.next.map(|next| NonNull::new_unchecked(next.as_ptr()));
                    if self.head.is_none() {
                        self.tail = None;
                    }
                }
            }
            Box::leak(head);
            return Some(unsafe { task::Notified::from_raw(task) });
        }
        return None;
    }

    pub(super) fn push(&mut self, task: task::RawTask) {
        if let Some(mut tail) = self.tail {
            let tail_ref = unsafe { tail.as_mut() };
            if tail_ref.tasks.len() < TASKS_CAPACITY {
                tail_ref.tasks.push(task);
                return;
            }
        }

        let mut new_node = Box::new(SyncedNode {
            index: 0,
            tasks: Vec::with_capacity(TASKS_CAPACITY),
            next: None,
        });
        new_node.tasks.push(task);
        let new_node_ptr = NonNull::from(Box::leak(new_node));

        unsafe {
            // 新节点作为老节点的子节点，新节点转为新的 tail 节点
            if let Some(mut tail) = self.tail {
                tail.as_mut().next = Some(new_node_ptr);
            }
            self.tail = Some(new_node_ptr);
            // 如果 head 没有设置，那么为 head 进行设置
            if self.head.is_none() {
                self.head = self.tail;
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.head.is_none() || unsafe { self.head.unwrap().as_ref().is_empty() }
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

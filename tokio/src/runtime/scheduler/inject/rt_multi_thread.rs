use super::{Shared, Synced};

use crate::runtime::scheduler::Lock;
use crate::runtime::task;

use std::sync::atomic::Ordering::Release;

impl<'a> Lock<Synced> for &'a mut Synced {
    type Handle = &'a mut Synced;

    fn lock(self) -> Self::Handle {
        self
    }
}

impl AsMut<Synced> for Synced {
    fn as_mut(&mut self) -> &mut Synced {
        self
    }
}

impl<T: 'static> Shared<T> {
    /// Pushes several values into the queue.
    ///
    /// # Safety
    ///
    /// Must be called with the same `Synced` instance returned by `Inject::new`}
    #[inline]
    pub(crate) unsafe fn push_batch<L>(
        &self,
        shared: L,
        batch_tasks: Vec<task::Notified<T>>,
    ) where
        L: Lock<Synced>,
    {
        let num = batch_tasks.len();
        let mut synced = shared.lock();
        if synced.as_mut().is_closed {
            drop(synced);
            drop(batch_tasks);
            return;
        }
        let synced = synced.as_mut();
        for task in batch_tasks {
            synced.push(task.into_raw());
        }

        // Increment the count.
        //
        // safety: All updates to the len atomic are guarded by the mutex. As
        // such, a non-atomic load followed by a store is safe.
        let len = self.len.unsync_load();

        self.len.store(len + num, Release);
    }
}

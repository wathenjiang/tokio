// Enable dead_code / unreachable_pub here. It has been disabled in lib.rs for
// other code when running loom tests.
#![cfg_attr(loom, warn(dead_code, unreachable_pub))]

use self::noop_scheduler::NoopSchedule;
use self::unowned_wrapper::unowned;

mod noop_scheduler {
    use crate::runtime::task::{self, Task};

    /// `task::Schedule` implementation that does nothing, for testing.
    pub(crate) struct NoopSchedule;

    impl task::Schedule for NoopSchedule {
        fn release(&self, _task: &Task) -> Option<Task> {
            None
        }

        fn schedule(&self, _task: task::Notified) {
            unreachable!();
        }
    }
}

mod unowned_wrapper {
    use crate::runtime::task::{Id, JoinHandle, Notified};
    use crate::runtime::tests::NoopSchedule;

    #[cfg(all(tokio_unstable, feature = "tracing"))]
    pub(crate) fn unowned<T>(task: T) -> (Notified<NoopSchedule>, JoinHandle<T::Output>)
    where
        T: std::future::Future + Send + 'static,
        T::Output: Send + 'static,
    {
        use tracing::Instrument;
        let span = tracing::trace_span!("test_span");
        let task = task.instrument(span);
        let (task, handle) = crate::runtime::task::unowned(task, NoopSchedule, Id::next());
        (task.into_notified(), handle)
    }

    #[cfg(not(all(tokio_unstable, feature = "tracing")))]
    pub(crate) fn unowned<T>(task: T) -> (Notified, JoinHandle<T::Output>)
    where
        T: std::future::Future + Send + 'static,
        T::Output: Send + 'static,
    {
        let (task, handle) = crate::runtime::task::unowned(task, Some(NoopSchedule), Id::next());
        (task.into_notified(), handle)
    }
}

cfg_loom! {
    mod loom_blocking;
    mod loom_current_thread;
    mod loom_join_set;
    mod loom_local;
    mod loom_multi_thread;
    mod loom_multi_thread_alt;
    mod loom_oneshot;

    // Make sure debug assertions are enabled
    #[cfg(not(debug_assertions))]
    compile_error!("these tests require debug assertions to be enabled");
}

cfg_not_loom! {
    mod inject;
    mod queue;

    #[cfg(not(miri))]
    mod task_combinations;

    #[cfg(miri)]
    mod task;
}

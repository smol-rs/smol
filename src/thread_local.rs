//! The thread-local executor.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::thread::{self, ThreadId};

use crossbeam::queue::SegQueue;
use scoped_tls_hkt::scoped_thread_local;

use crate::io_event::IoEvent;
use crate::reactor::Reactor;
use crate::task::{Runnable, Task};
use crate::throttle;

// The thread-local executor.
//
// This thread-local is only set while inside `ThreadLocalExecutor::enter()`.
scoped_thread_local!(static EXECUTOR: ThreadLocalExecutor);

/// An executor for thread-local tasks.
///
/// Thread-local tasks are spawned by calling `Task::local()` and their futures do not have to
/// implement `Send`. They can only be run by the same thread that created them.
pub(crate) struct ThreadLocalExecutor {
    /// The main task queue.
    queue: RefCell<VecDeque<Runnable>>,

    /// When another thread wakes a task belonging to this executor, it goes into this queue.
    injector: Arc<SegQueue<Runnable>>,

    /// An I/O event that is triggered when another thread wakes a task belonging to this executor.
    event: IoEvent,
}

impl ThreadLocalExecutor {
    /// Creates a new thread-local executor.
    pub fn new() -> ThreadLocalExecutor {
        ThreadLocalExecutor {
            queue: RefCell::new(VecDeque::new()),
            injector: Arc::new(SegQueue::new()),
            event: IoEvent::new().expect("cannot create an `IoEvent`"),
        }
    }

    /// TODO
    pub fn enter<T>(&self, f: impl FnOnce() -> T) -> T {
        if EXECUTOR.is_set() {
            panic!("cannot run an executor inside another executor");
        }
        EXECUTOR.set(self, f)
    }

    /// TODO
    pub fn event(&self) -> &IoEvent {
        &self.event
    }

    /// Spawns a future onto this executor.
    ///
    /// Returns a `Task` handle for the spawned task.
    pub fn spawn<T: 'static>(future: impl Future<Output = T> + 'static) -> Task<T> {
        if !EXECUTOR.is_set() {
            panic!("cannot spawn a thread-local task if not inside an executor");
        }

        EXECUTOR.with(|ex| {
            // Why weak reference here? Injector may hold the task while the task's waker holds a
            // reference to the injector. So this reference must be weak to break the cycle.
            let injector = Arc::downgrade(&ex.injector);
            let event = ex.event.clone();
            let id = thread_id();

            // The function that schedules a runnable task.
            let schedule = move |runnable| {
                if thread_id() == id {
                    // If scheduling from the original thread, push into the main queue.
                    EXECUTOR.with(|ex| ex.queue.borrow_mut().push_back(runnable));
                } else if let Some(injector) = injector.upgrade() {
                    // If scheduling from a different thread, push into the injector queue.
                    injector.push(runnable);
                    // Trigger an I/O event to let the original thread know that a task has been
                    // scheduled. If that thread is inside epoll/kqueue/wepoll, an I/O event will
                    // wake it up.
                    event.set();
                }
            };

            // Create a task, schedule it, and return its `Task` handle.
            let (runnable, handle) = async_task::spawn_local(future, schedule, ());
            runnable.schedule();
            Task(Some(handle))
        })
    }

    /// Executes a batch of tasks and returns `true` if there are more tasks to run.
    pub fn execute(&self) -> bool {
        // Execute 4 series of 50 tasks.
        for _ in 0..4 {
            for _ in 0..50 {
                // Find the next task to run.
                match self.pop() {
                    None => {
                        // There are no more tasks to run.
                        return false;
                    }
                    Some(r) => {
                        // Run the task.
                        throttle::setup(|| r.run());
                    }
                }
            }

            // Poll the reactor and drain the injector queue. We do this occasionally to make
            // execution more fair to all tasks involved.
            self.fetch();
        }

        // There are likely more tasks to run.
        true
    }

    /// Finds the next task to run.
    fn pop(&self) -> Option<Runnable> {
        // Check if there is a task in the main queue.
        if let Some(r) = self.queue.borrow_mut().pop_front() {
            return Some(r);
        }
        // If not, fetch more tasks from the reactor or the injector queue.
        self.fetch();
        // Check the main queue again.
        self.queue.borrow_mut().pop_front()
    }

    /// Polls the reactor and moves all tasks from the injector queue into the main queue.
    fn fetch(&self) {
        // The reactor might wake tasks belonging to this executor.
        Reactor::get().poll().expect("failure while polling I/O");

        // Move tasks from the injector queue into the main queue.
        let mut queue = self.queue.borrow_mut();
        while let Ok(r) = self.injector.pop() {
            queue.push_back(r);
        }
    }
}

/// Same as `std::thread::current().id()`, but more efficient.
fn thread_id() -> ThreadId {
    thread_local! {
        static ID: ThreadId = thread::current().id();
    }

    ID.try_with(|id| *id)
        .unwrap_or_else(|_| thread::current().id())
}

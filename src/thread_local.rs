//! The thread-local executor.
//!
//! Tasks created by [`Task::local()`] go into this executor. Every thread calling
//! [`run()`][`crate::run()`] creates a thread-local executor. Tasks cannot be spawned onto a
//! thread-local executor if it is not running.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::thread::{self, ThreadId};

use concurrent_queue::ConcurrentQueue;
use scoped_tls_hkt::scoped_thread_local;

use crate::task::{Runnable, Task};

scoped_thread_local! {
    /// The thread-local executor.
    ///
    /// This thread-local is only set while inside [`LocalExecutor::enter()`].
    static EXECUTOR: LocalExecutor
}

/// An executor for thread-local tasks.
///
/// Thread-local tasks are spawned by calling [`Task::local()`] and their futures do not have to
/// implement [`Send`]. They can only be run by the same thread that created them.
pub(crate) struct LocalExecutor {
    /// The main task queue.
    queue: RefCell<VecDeque<Runnable>>,

    /// When another thread wakes a task belonging to this executor, it goes into this queue.
    injector: Arc<ConcurrentQueue<Runnable>>,

    callback: Arc<dyn Fn() + Send + Sync>,

    sleeping: Cell<bool>,

    ticks: Cell<usize>,
}

impl LocalExecutor {
    /// Creates a new thread-local executor.
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> LocalExecutor {
        LocalExecutor {
            queue: RefCell::new(VecDeque::new()),
            injector: Arc::new(ConcurrentQueue::unbounded()),
            callback: Arc::new(notify),
            sleeping: Cell::new(false),
            ticks: Cell::new(0),
        }
    }

    /// Enters the context of this executor.
    pub fn enter<T>(&self, f: impl FnOnce() -> T) -> T {
        // TODO(stjepang): Allow recursive executors.
        if EXECUTOR.is_set() {
            panic!("cannot run an executor inside another executor");
        }
        EXECUTOR.set(self, f)
    }

    /// Spawns a future onto this executor.
    ///
    /// Returns a [`Task`] handle for the spawned task.
    pub fn spawn<T: 'static>(future: impl Future<Output = T> + 'static) -> Task<T> {
        if !EXECUTOR.is_set() {
            panic!("cannot spawn a thread-local task if not inside an executor");
        }

        EXECUTOR.with(|ex| {
            // Why weak reference here? Injector may hold the task while the task's waker holds a
            // reference to the injector. So this reference must be weak to break the cycle.
            let injector = Arc::downgrade(&ex.injector);
            let callback = ex.callback.clone();
            let id = thread_id();

            // The function that schedules a runnable task when it gets woken up.
            let schedule = move |runnable| {
                if thread_id() == id {
                    // If scheduling from the original thread, push into the main queue.
                    EXECUTOR.with(|ex| ex.queue.borrow_mut().push_back(runnable));
                } else if let Some(injector) = injector.upgrade() {
                    // If scheduling from a different thread, push into the injector queue.
                    injector.push(runnable).unwrap();
                }

                // Trigger an I/O event to let the original thread know that a task has been
                // scheduled. If that thread is inside epoll/kqueue/wepoll, an I/O event will wake
                // it up.
                callback();
            };

            // Create a task, push it into the queue by scheduling it, and return its `Task` handle.
            let (runnable, handle) = async_task::spawn_local(future, schedule, ());
            runnable.schedule();
            Task(Some(handle))
        })
    }

    pub fn tick(&self) -> bool {
        match self.search() {
            None => {
                self.ticks.set(0);
                false
            }
            Some(r) => {
                self.ticks.set(self.ticks.get() + 1);
                if self.ticks.get() == 50 {
                    self.ticks.set(0);
                    self.fetch();
                }

                self.enter(|| r.run());
                true
            }
        }
    }

    /// Finds the next task to run.
    fn search(&self) -> Option<Runnable> {
        // Check if there is a task in the main queue.
        if let Some(r) = self.queue.borrow_mut().pop_front() {
            return Some(r);
        }

        // If not, fetch tasks from the injector queue.
        self.fetch();

        // Check the main queue again.
        self.queue.borrow_mut().pop_front()
    }

    /// Moves all tasks from the injector queue into the main queue.
    fn fetch(&self) {
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

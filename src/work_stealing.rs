//! The work-stealing executor.
//!
//! Tasks created by [`Task::spawn()`] go into this executor. Every thread calling [`run()`]
//! initializes a [`Worker`] that participates in work stealing, which is allowed to run any task
//! in this executor or in other workers. Since tasks can be stolen by any worker and thus move
//! from thread to thread, their futures must implement [`Send`].
//!
//! There is only one global instance of this type, accessible by [`Executor::get()`].
//!
//! [Work stealing] is a strategy that reduces contention in multi-threaded environments. If all
//! invocations of [`run()`] used the same global task queue all the time, they would contend on
//! the queue all the time, thus slowing the executor down.
//!
//! The solution is to have a separate queue for each invocation of [`run()`], called a "worker".
//! Each thread is primarily using its own worker. Once all tasks in the worker are exhausted, then
//! we look for tasks in the global queue, called "injector", or steal tasks from other workers.
//!
//! [`run()`]: crate::run()
//! [Work stealing]: https://en.wikipedia.org/wiki/Work_stealing

use std::cell::Cell;
use std::future::Future;
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use concurrent_queue::ConcurrentQueue;
use once_cell::sync::Lazy;
use scoped_tls_hkt::scoped_thread_local;
use slab::Slab;

use crate::task::{Runnable, Task};

scoped_thread_local! {
    /// The current thread's worker.
    ///
    /// Other threads may steal tasks from this worker through its associated stealer that was
    /// registered in the work-stealing executor.
    ///
    /// This thread-local is only set while inside [`Worker::enter()`].
    static WORKER: for<'a> &'a Worker<'a>
}

/// The global work-stealing executor.
pub(crate) struct Executor {
    /// When a thread that is not inside [`run()`][`crate::run()`] spawns or wakes a task, it goes
    /// into this queue.
    injector: ConcurrentQueue<Runnable>,

    /// Registered handles for stealing tasks from workers.
    stealers: RwLock<Slab<Arc<ConcurrentQueue<Runnable>>>>,

    notified: AtomicBool,
    sleepers: Mutex<Sleepers>,
}

struct Sleepers {
    count: usize,
    items: Vec<Arc<dyn Fn() + Send + Sync>>,
}

impl Executor {
    /// Returns a reference to the global work-stealing executor.
    pub fn get() -> &'static Executor {
        static EXECUTOR: Lazy<Executor> = Lazy::new(|| Executor {
            injector: ConcurrentQueue::unbounded(),
            stealers: RwLock::new(Slab::new()),
            notified: AtomicBool::new(true),
            sleepers: Mutex::new(Sleepers {
                count: 0,
                items: Vec::new(),
            }),
        });
        &EXECUTOR
    }

    fn notify(&self) {
        if !self
            .notified
            .compare_and_swap(false, true, Ordering::SeqCst)
        {
            let mut sleepers = self.sleepers.lock().unwrap();
            if sleepers.items.len() == sleepers.count {
                if let Some(callback) = sleepers.items.pop() {
                    let callback = callback.clone();
                    drop(sleepers);
                    callback();
                }
            }
        }
    }

    /// Spawns a future onto this executor.
    ///
    /// Returns a [`Task`] handle for the spawned task.
    pub fn spawn<T: Send + 'static>(
        &'static self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        // The function that schedules a runnable task when it gets woken up.
        let schedule = move |runnable| {
            if WORKER.is_set() {
                // If scheduling from a worker thread, push into the worker's queue.
                WORKER.with(|w| w.push(runnable));
            } else {
                // If scheduling from a non-worker thread, push into the injector queue.
                self.injector.push(runnable).unwrap();

                self.notify();
            }
        };

        // Create a task, push it into the queue by scheduling it, and return its `Task` handle.
        let (runnable, handle) = async_task::spawn(future, schedule, ());
        runnable.schedule();
        Task(Some(handle))
    }

    /// Registers a new worker.
    ///
    /// The worker will automatically deregister itself when dropped.
    pub fn worker(&self, notify: impl Fn() + Send + Sync + 'static) -> Worker<'_> {
        let mut stealers = self.stealers.write().unwrap();
        let vacant = stealers.vacant_entry();

        // Create a worker and put its stealer handle into the executor.
        let worker = Worker {
            key: vacant.key(),
            slot: Cell::new(None),
            queue: Arc::new(ConcurrentQueue::bounded(512)),
            executor: self,
            callback: Arc::new(notify),
            sleeping: Cell::new(false),
            ticks: Cell::new(0),
        };
        vacant.insert(worker.queue.clone());
        drop(stealers);

        worker
    }
}

/// A worker that participates in the work-stealing executor.
///
/// Each invocation of `run()` creates its own worker.
pub(crate) struct Worker<'a> {
    /// The ID of this worker obtained during registration.
    key: usize,

    /// A slot into which tasks go before entering the actual queue.
    ///
    /// Note that other workers cannot steal this task.
    slot: Cell<Option<Runnable>>,

    /// A queue of tasks.
    ///
    /// Other workers are able to steal tasks from this queue.
    queue: Arc<ConcurrentQueue<Runnable>>,

    /// The parent work-stealing executor.
    executor: &'a Executor,

    callback: Arc<dyn Fn() + Send + Sync>,

    sleeping: Cell<bool>,

    ticks: Cell<usize>,
}

impl Worker<'_> {
    /// Enters the context of this executor.
    fn enter<T>(&self, f: impl FnOnce() -> T) -> T {
        // TODO(stjepang): Allow recursive executors.
        if WORKER.is_set() {
            panic!("cannot run an executor inside another executor");
        }
        WORKER.set(self, f)
    }

    fn sleep(&self) -> bool {
        let sleeping = self.sleeping.get();
        self.sleeping.set(true);

        let mut sleepers = self.executor.sleepers.lock().unwrap();

        if sleeping {
            if sleepers
                .items
                .iter()
                .all(|i| !Arc::ptr_eq(i, &self.callback))
            {
                sleepers.items.push(self.callback.clone());
            }
        } else {
            sleepers.count += 1;
            sleepers.items.push(self.callback.clone());
        }

        if sleepers.count == 0 || sleepers.count > sleepers.items.len() {
            self.executor.notified.swap(true, Ordering::SeqCst);
        } else {
            self.executor.notified.swap(false, Ordering::SeqCst);
        }

        !sleeping
    }

    fn wake(&self) -> bool {
        if !self.sleeping.get() {
            return false;
        }

        self.sleeping.set(false);

        let mut sleepers = self.executor.sleepers.lock().unwrap();
        sleepers.count -= 1;
        sleepers.items.retain(|i| !Arc::ptr_eq(i, &self.callback)); // TODO: optimize

        if sleepers.count == 0 || sleepers.count > sleepers.items.len() {
            self.executor.notified.swap(true, Ordering::SeqCst);
        } else {
            self.executor.notified.swap(false, Ordering::SeqCst);
        }

        true
    }

    /// Executes a batch of tasks and returns `true` if there may be more tasks to run.
    pub fn tick(&self) -> bool {
        loop {
            match self.search() {
                None => {
                    if !self.sleep() {
                        // self.ticks.set(self.ticks.get().wrapping_add(1));
                        return false;
                    }
                }
                Some(r) => {
                    if !self.wake() {
                        self.executor.notify();
                    }

                    // Run the task.
                    if self.enter(|| r.run()) {
                        // The task was woken while it was running, which means it got
                        // scheduled the moment running completed. Therefore, it is now inside
                        // the slot and would be the next task to run.
                        //
                        // Instead of re-running the task in the next iteration, let's flush
                        // the slot in order to give other tasks a chance to run.
                        //
                        // This is a necessary step to ensure task yielding works as expected.
                        // If a task wakes itself and returns `Poll::Pending`, we don't want it
                        // to run immediately after that because that'd defeat the whole
                        // purpose of yielding.
                        self.flush_slot();
                    }

                    let ticks = self.ticks.get();
                    self.ticks.set(ticks.wrapping_add(1));
                    if ticks % 16 == 0 {
                        self.flush_slot();
                    }
                    if ticks % 64 == 0 {
                        if let Some(r) = self.steal_global() {
                            self.push(r);
                        }
                    }

                    return true;
                }
            }
        }
    }

    /// Pushes a task into this worker.
    fn push(&self, runnable: Runnable) {
        // Put the task into the slot.
        if let Some(r) = self.slot.replace(Some(runnable)) {
            // If the slot had a task, push it into the queue.
            if let Err(err) = self.queue.push(r) {
                self.executor.injector.push(err.into_inner()).unwrap();
            }
        }
    }

    /// Moves a task from the slot into the local queue.
    fn flush_slot(&self) {
        if let Some(r) = self.slot.take() {
            if let Err(err) = self.queue.push(r) {
                self.executor.injector.push(err.into_inner()).unwrap();
            }
        }
    }

    /// Finds the next task to run.
    fn search(&self) -> Option<Runnable> {
        // Check if there is a task in the slot.
        if let Some(r) = self.slot.take() {
            return Some(r);
        }

        // Check if there is a task in the queue.
        if let Ok(r) = self.queue.pop() {
            return Some(r);
        }

        // Try stealing from the injector queue.
        if let Some(r) = self.steal_global() {
            return Some(r);
        }

        // Try stealing from other workers.
        let stealers = self.executor.stealers.read().unwrap();

        // Pick a random starting point in the iterator list and rotate the list.
        let n = stealers.len();
        let start = fastrand::usize(..n);
        let iter = stealers.iter().chain(stealers.iter()).skip(start).take(n);

        // Remove this worker's stealer handle.
        let iter = iter.filter(|(k, _)| *k != self.key);
        let iter = iter.map(|(_, q)| q);

        // Try stealing from each worker in the list.
        for q in iter {
            let count = self
                .queue
                .capacity()
                .unwrap_or(usize::MAX)
                .min((q.len() + 1) / 2);

            // Steal half of the tasks from this worker.
            for _ in 0..count {
                if let Ok(r) = q.pop() {
                    self.push(r);
                } else {
                    break;
                }
            }

            // Check if there is a task in the slot.
            if let Some(r) = self.slot.take() {
                return Some(r);
            }
        }

        None
    }

    /// Steals tasks from the injector queue.
    fn steal_global(&self) -> Option<Runnable> {
        let count = self
            .queue
            .capacity()
            .unwrap_or(usize::MAX)
            .min((self.executor.injector.len() + 1) / 2);

        // Steal half of the tasks from the injector queue.
        for _ in 0..count {
            if let Ok(r) = self.executor.injector.pop() {
                self.push(r);
            } else {
                break;
            }
        }

        // If anything was stolen, a task must be in the slot.
        self.slot.take()
    }
}

impl Drop for Worker<'_> {
    fn drop(&mut self) {
        // Unregister the worker.
        self.executor.stealers.write().unwrap().remove(self.key);

        // Move the task in the slot into the injector queue.
        if let Some(r) = self.slot.take() {
            r.schedule();
        }

        // Move all tasks in this worker's queue into the injector queue.
        while let Ok(r) = self.queue.pop() {
            r.schedule();
        }

        self.wake();

        // This task will not search for tasks anymore and therefore won't notify other workers if
        // new tasks are found. Notify another worker to start searching right away.
        self.executor.notify();
    }
}

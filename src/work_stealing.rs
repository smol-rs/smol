//! The work-stealing executor.
//!
//! Tasks created by [`Task::spawn()`] go into this executor. Any thread calling [`run()`]
//! initializes a `Worker` that participates in work stealing, which is allowed to run any task in
//! this executor or in other workers.
//!
//! Since tasks can be stolen by any worker and thus move from thread to thread, their futures must
//! implement [`Send`].
//!
//! There is only one global instance of this type, accessible by [`WorkStealingExecutor::get()`].
//!
//! Work stealing is a strategy that reduces contention in a multi-threaded environment. If all
//! invocations of [`run()`] used the same global task queue all the time, they would constantly
//! "step on each other's toes", causing a lot of CPU cache traffic and too often waste time
//! retrying queue operations in compare-and-swap loops.
//!
//! The solution is to have a separate queue in each invocation of [`run()`], called a "worker".
//! Each thread is primarily using its own worker. Once there are no more tasks in the worker, we
//! either grab a batch of tasks from the main global queue, or steal tasks from other workers.
//! Of course, work-stealing still causes contention in some cases, but much less often.
//!
//! More about work stealing: https://en.wikipedia.org/wiki/Work_stealing

use std::cell::Cell;
use std::future::Future;
use std::num::Wrapping;
use std::panic;

use crossbeam::deque;
use crossbeam::sync::ShardedLock;
use once_cell::sync::Lazy;
use scoped_tls_hkt::scoped_thread_local;
use slab::Slab;

use crate::io_event::IoEvent;
use crate::task::{Runnable, Task};
use crate::throttle;

// The current thread's worker.
//
// Other threads may steal tasks from this worker through its associated stealer that was
// registered in the work-stealing executor.
//
// This thread-local is only set while inside `WorkStealingExecutor::enter()`.
scoped_thread_local!(static WORKER: for<'a> &'a Worker<'a>);

/// The global work-stealing executor.
pub(crate) struct WorkStealingExecutor {
    /// When a thread that is not inside `run()` spawns or wakes a task, it goes into this queue.
    injector: deque::Injector<Runnable>,

    /// Registered handles for stealing tasks from workers.
    stealers: ShardedLock<Slab<deque::Stealer<Runnable>>>,

    /// An I/O event that is triggered whenever there might be available tasks to run.
    event: IoEvent,
}

impl WorkStealingExecutor {
    /// Returns a reference to the global work-stealing executor.
    pub fn get() -> &'static WorkStealingExecutor {
        static EXECUTOR: Lazy<WorkStealingExecutor> = Lazy::new(|| WorkStealingExecutor {
            injector: deque::Injector::new(),
            stealers: ShardedLock::new(Slab::new()),
            event: IoEvent::new().expect("cannot create an `IoEvent`"),
        });
        &EXECUTOR
    }

    /// Returns the event indicating there is a scheduled task.
    pub fn event(&self) -> &IoEvent {
        &self.event
    }

    /// Spawns a future onto this executor.
    ///
    /// Returns a `Task` handle for the spawned task.
    pub fn spawn<T: Send + 'static>(
        &'static self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        // The function that schedules a runnable task.
        let schedule = move |runnable| {
            if WORKER.is_set() {
                // If scheduling from a worker thread, push into the worker's queue.
                WORKER.with(|w| w.push(runnable));
            } else {
                // If scheduling from a non-worker thread, push into the injector queue.
                self.injector.push(runnable);

                // Notify workers that there is a task in the injector queue.
                self.event.notify();
            }
        };

        // Create a task, schedule it, and return its `Task` handle.
        let (runnable, handle) = async_task::spawn(future, schedule, ());
        runnable.schedule();
        Task(Some(handle))
    }

    /// Registers a new worker.
    ///
    /// The worker will automatically deregister itself when dropped.
    pub fn worker(&self) -> Worker<'_> {
        let mut stealers = self.stealers.write().unwrap();
        let vacant = stealers.vacant_entry();

        // Create a worker and put its stealer handle into the executor.
        let worker = Worker {
            key: vacant.key(),
            slot: Cell::new(None),
            queue: deque::Worker::new_fifo(),
            executor: self,
        };
        vacant.insert(worker.queue.stealer());

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
    slot: Cell<Option<Runnable>>,

    /// A queue of tasks.
    ///
    /// Other workers are able to steal tasks from this queue.
    queue: deque::Worker<Runnable>,

    /// The parent work-stealing executor.
    executor: &'a WorkStealingExecutor,
}

impl Worker<'_> {
    /// Enters the context of this executor.
    pub fn enter<T>(&self, f: impl FnOnce() -> T) -> T {
        if WORKER.is_set() {
            panic!("cannot run an executor inside another executor");
        }
        WORKER.set(self, f)
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
                        // Notify other workers that there may be stealable tasks.
                        //
                        // This is necessary because `pop()` sometimes re-shuffles tasks between
                        // queues, which races with other workers looking for tasks. They might
                        // believe there are no tasks while there really are, so we notify here.
                        self.executor.event.notify();

                        // Run the task.
                        if throttle::setup(|| r.run()) {
                            // The task was scheduled while it was running, which means it got
                            // pushed into this worker. It is now inside the slot and would be the
                            // next task to run.
                            //
                            // Let's flush the slot in order to give other tasks a chance to run.
                            self.push(None);
                        }
                    }
                }
            }

            self.push(None);
            if let Some(r) = retry_steal(|| self.executor.injector.steal_batch_and_pop(&self.queue))
            {
                self.push(Some(r));
            }
        }

        // There are likely more tasks to run.
        true
    }

    /// Pushes a task into this worker.
    ///
    /// If the given task is `None`, the slot's contents gets replaced with `None`, moving its
    /// previously held task into the queue (if there was one).
    fn push(&self, runnable: impl Into<Option<Runnable>>) {
        // Put the task into the slot.
        if let Some(r) = self.slot.replace(runnable.into()) {
            // If the slot had a task, push it into the queue.
            self.queue.push(r);
        }
    }

    /// Finds the next task to run.
    fn pop(&self) -> Option<Runnable> {
        // Check if there is a task in the slot or in the queue.
        if let Some(r) = self.slot.take().or_else(|| self.queue.pop()) {
            return Some(r);
        }

        // Try stealing from the global queue.
        if let Some(r) = retry_steal(|| self.executor.injector.steal_batch_and_pop(&self.queue)) {
            return Some(r);
        }

        // Try stealing from other workers.
        let stealers = self.executor.stealers.read().unwrap();
        retry_steal(|| {
            // Pick a random starting point in the iterator list and rotate the list.
            let n = stealers.len();
            let start = fast_random(n);
            let iter = stealers.iter().chain(stealers.iter()).skip(start).take(n);

            // Remove this worker's stealer handle.
            let iter = iter.filter(|(k, _)| *k != self.key);

            // Try stealing from each worker in the list. Collecting stops as soon as we get a
            // `Steal::Success`. Otherwise, if any steal attempt resulted in a `Steal::Retry`,
            // that's the collected result and we'll retry from the beginning.
            iter.map(|(_, s)| s.steal_batch_and_pop(&self.queue))
                .collect()
        })
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
        while let Some(r) = self.queue.pop() {
            r.schedule();
        }
    }
}

/// Returns a random number in the interval `0..n`.
fn fast_random(n: usize) -> usize {
    thread_local! {
        static RNG: Cell<Wrapping<u32>> = Cell::new(Wrapping(1));
    }

    RNG.with(|rng| {
        // This is the 32-bit variant of Xorshift: https://en.wikipedia.org/wiki/Xorshift
        let mut x = rng.get();
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        rng.set(x);

        // This is a fast alternative to `x % n`:
        // https://lemire.me/blog/2016/06/27/a-fast-alternative-to-the-modulo-reduction/
        ((x.0 as u64).wrapping_mul(n as u64) >> 32) as usize
    })
}

/// Retries a steal operation for as long as it returns `Steal::Retry`.
fn retry_steal<T>(mut steal_op: impl FnMut() -> deque::Steal<T>) -> Option<T> {
    loop {
        match steal_op() {
            deque::Steal::Success(t) => return Some(t),
            deque::Steal::Empty => return None,
            deque::Steal::Retry => {}
        }
    }
}

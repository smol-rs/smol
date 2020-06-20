use std::cell::Cell;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, ThreadId};

use concurrent_queue::{ConcurrentQueue, PopError, PushError};
use scoped_tls::scoped_thread_local;
use slab::Slab;

use crate::task::{Runnable, Task};

scoped_thread_local! {
    static WORKER: Worker
}

/// State shared between [`Queue`] and [`Worker`].
struct Global {
    /// The global queue.
    queue: ConcurrentQueue<Runnable>,

    /// Shards of the global queue created by workers.
    shards: RwLock<Slab<Arc<ConcurrentQueue<Runnable>>>>,

    /// Set to `true` when a sleeping worker is notified or no workers are sleeping.
    notified: AtomicBool,

    /// A list of sleeping workers.
    sleepers: Mutex<Sleepers>,
}

impl Global {
    /// Notifies a sleeping worker.
    fn notify(&self) {
        if !self
            .notified
            .compare_and_swap(false, true, Ordering::SeqCst)
        {
            let callback = self.sleepers.lock().unwrap().notify();
            if let Some(cb) = callback {
                cb.call();
            }
        }
    }
}

/// A list of sleeping workers.
struct Sleepers {
    /// Number of sleeping workers (both notified and unnotified).
    count: usize,

    /// Callbacks of sleeping unnotified workers.
    ///
    /// A sleeping worker is notified when its callback is missing from this list.
    callbacks: Vec<Callback>,
}

impl Sleepers {
    /// Inserts a new sleeping worker.
    fn insert(&mut self, callback: &Callback) {
        self.count += 1;
        self.callbacks.push(callback.clone());
    }

    /// Re-inserts a sleeping worker's callback if it was notified.
    ///
    /// Returns `true` if the worker was notified.
    fn update(&mut self, callback: &Callback) -> bool {
        if self.callbacks.iter().all(|cb| cb != callback) {
            self.callbacks.push(callback.clone());
            true
        } else {
            false
        }
    }

    /// Removes a previously inserted sleeping worker.
    fn remove(&mut self, callback: &Callback) {
        self.count -= 1;
        for i in (0..self.callbacks.len()).rev() {
            if &self.callbacks[i] == callback {
                self.callbacks.remove(i);
                return;
            }
        }
    }

    /// Returns `true` if a sleeping worker is notified or no workers are sleeping.
    fn is_notified(&self) -> bool {
        self.count == 0 || self.count > self.callbacks.len()
    }

    /// Returns notification callback for a sleeping worker.
    ///
    /// If a worker was notified already or there are no workers, `None` will be returned.
    fn notify(&mut self) -> Option<Callback> {
        if self.callbacks.len() == self.count {
            self.callbacks.pop()
        } else {
            None
        }
    }
}

/// A queue for spawning tasks.
pub(crate) struct Queue {
    global: Arc<Global>,
}

impl Queue {
    /// Creates a new queue for spawning tasks.
    pub fn new() -> Queue {
        Queue {
            global: Arc::new(Global {
                queue: ConcurrentQueue::unbounded(),
                shards: RwLock::new(Slab::new()),
                notified: AtomicBool::new(true),
                sleepers: Mutex::new(Sleepers {
                    count: 0,
                    callbacks: Vec::new(),
                }),
            }),
        }
    }

    /// Spawns a future onto this queue.
    ///
    /// Returns a [`Task`] handle for the spawned task.
    pub fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        let global = self.global.clone();

        // The function that schedules a runnable task when it gets woken up.
        let schedule = move |runnable| {
            if WORKER.is_set() {
                WORKER.with(|w| {
                    if Arc::ptr_eq(&global, &w.global) {
                        if let Err(err) = w.shard.push(runnable) {
                            global.queue.push(err.into_inner()).unwrap();
                        }
                    } else {
                        global.queue.push(runnable).unwrap();
                    }
                });
            } else {
                global.queue.push(runnable).unwrap();
            }

            global.notify();
        };

        // Create a task, push it into the queue by scheduling it, and return its `Task` handle.
        let (runnable, handle) = async_task::spawn(future, schedule, ());
        runnable.schedule();
        Task(Some(handle))
    }

    /// Registers a new worker.
    ///
    /// The worker will automatically deregister itself when dropped.
    pub fn worker(&self, notify: impl Fn() + Send + Sync + 'static) -> Worker {
        let mut shards = self.global.shards.write().unwrap();
        let vacant = shards.vacant_entry();

        // Create a worker and put its stealer handle into the executor.
        let worker = Worker {
            key: vacant.key(),
            global: Arc::new(self.global.clone()),
            shard: SlotQueue {
                slot: Cell::new(None),
                queue: Arc::new(ConcurrentQueue::bounded(512)),
            },
            local: SlotQueue {
                slot: Cell::new(None),
                queue: Arc::new(ConcurrentQueue::unbounded()),
            },
            callback: Callback::new(notify),
            sleeping: Cell::new(false),
            ticker: Cell::new(0),
        };
        vacant.insert(worker.shard.queue.clone());

        worker
    }
}

impl Default for Queue {
    fn default() -> Queue {
        Queue::new()
    }
}

/// A worker that participates in the work-stealing executor.
///
/// Each invocation of `run()` creates its own worker.
pub(crate) struct Worker {
    /// The ID of this worker obtained during registration.
    key: usize,

    /// The global queue.
    global: Arc<Arc<Global>>,

    /// A shard of the global queue.
    shard: SlotQueue<Runnable>,

    /// Local queue for `!Send` tasks.
    local: SlotQueue<Runnable>,

    /// Callback invoked to wake this worker up.
    callback: Callback,

    /// Set to `true` when in sleeping state.
    ///
    /// States a worker can be in:
    /// 1) Woken.
    /// 2a) Sleeping and unnotified.
    /// 2b) Sleeping and notified.
    sleeping: Cell<bool>,

    /// Bumped every time a task is run.
    ticker: Cell<usize>,
}

impl Worker {
    /// Spawns a local future onto this executor.
    ///
    /// Returns a [`Task`] handle for the spawned task.
    pub fn spawn_local<T: 'static>(&self, future: impl Future<Output = T> + 'static) -> Task<T> {
        let queue = self.local.queue.clone();
        let callback = self.callback.clone();
        let id = thread_id();

        // The function that schedules a runnable task when it gets woken up.
        let schedule = move |runnable| {
            if thread_id() == id && WORKER.is_set() {
                WORKER.with(|w| {
                    if Arc::ptr_eq(&queue, &w.local.queue) {
                        w.local.push(runnable).unwrap();
                    } else {
                        queue.push(runnable).unwrap();
                    }
                });
            } else {
                queue.push(runnable).unwrap();
            }

            callback.call();
        };

        // Create a task, push it into the queue by scheduling it, and return its `Task` handle.
        let (runnable, handle) = async_task::spawn_local(future, schedule, ());
        runnable.schedule();
        Task(Some(handle))
    }

    /// Moves the worker into sleeping and unnotified state.
    ///
    /// Returns `true` if the worker was already sleeping and unnotified.
    fn sleep(&self) -> bool {
        let mut sleepers = self.global.sleepers.lock().unwrap();

        if self.sleeping.get() {
            // Already sleeping, check if notified.
            if !sleepers.update(&self.callback) {
                return false;
            }
        } else {
            // Move to sleeping state.
            sleepers.insert(&self.callback);
        }

        self.global
            .notified
            .swap(sleepers.is_notified(), Ordering::SeqCst);

        self.sleeping.set(true);
        true
    }

    /// Moves the worker into woken state.
    fn wake(&self) -> bool {
        if self.sleeping.get() {
            let mut sleepers = self.global.sleepers.lock().unwrap();
            sleepers.remove(&self.callback);

            self.global
                .notified
                .swap(sleepers.is_notified(), Ordering::SeqCst);
        }

        self.sleeping.replace(false)
    }

    /// Runs a single task and returns `true` if one was found.
    pub fn tick(&self) -> bool {
        loop {
            match self.search() {
                None => {
                    // Move to sleeping and unnotified state.
                    if !self.sleep() {
                        // If already sleeping and unnotified, return.
                        return false;
                    }
                }
                Some(r) => {
                    // Wake up.
                    if !self.wake() {
                        // If already woken, notify another worker.
                        self.global.notify();
                    }

                    // Bump the ticker.
                    let ticker = self.ticker.get();
                    self.ticker.set(ticker.wrapping_add(1));

                    // Flush slots to ensure fair task scheduling.
                    if ticker % 16 == 0 {
                        if let Err(err) = self.shard.flush() {
                            self.global.queue.push(err.into_inner()).unwrap();
                            self.global.notify();
                        }
                        self.local.flush().unwrap();
                    }

                    // Steal tasks from the global queue to ensure fair task scheduling.
                    if ticker % 64 == 0 {
                        self.shard.steal(&self.global.queue);
                    }

                    // Run the task.
                    if WORKER.set(self, || r.run()) {
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
                        if let Err(err) = self.shard.flush() {
                            self.global.queue.push(err.into_inner()).unwrap();
                            self.global.notify();
                        }
                        self.local.flush().unwrap();
                    }

                    return true;
                }
            }
        }
    }

    /// Finds the next task to run.
    fn search(&self) -> Option<Runnable> {
        if self.ticker.get() % 2 == 0 {
            // On even ticks, look into the local queue and then into the shard.
            if let Ok(r) = self.local.pop().or_else(|_| self.shard.pop()) {
                return Some(r);
            }
        } else {
            // On odd ticks, look into the shard and then into the local queue.
            if let Ok(r) = self.shard.pop().or_else(|_| self.local.pop()) {
                return Some(r);
            }
        }

        // Try stealing from the global queue.
        self.shard.steal(&self.global.queue);
        if let Ok(r) = self.shard.pop() {
            return Some(r);
        }

        // Try stealing from other shards.
        let shards = self.global.shards.read().unwrap();

        // Pick a random starting point in the iterator list and rotate the list.
        let n = shards.len();
        let start = fastrand::usize(..n);
        let iter = shards.iter().chain(shards.iter()).skip(start).take(n);

        // Remove this worker's shard.
        let iter = iter.filter(|(key, _)| *key != self.key);
        let iter = iter.map(|(_, shard)| shard);

        // Try stealing from each shard in the list.
        for shard in iter {
            self.shard.steal(shard);
            if let Ok(r) = self.shard.pop() {
                return Some(r);
            }
        }

        None
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Wake and unregister the worker.
        self.wake();
        self.global.shards.write().unwrap().remove(self.key);

        // Re-schedule remaining tasks in the shard.
        while let Ok(r) = self.shard.pop() {
            r.schedule();
        }
        // Notify another worker to start searching for tasks.
        self.global.notify();

        // TODO(stjepang): Close the local queue and empty it.
    }
}

/// A queue with a single-item slot in front of it.
struct SlotQueue<T> {
    slot: Cell<Option<T>>,
    queue: Arc<ConcurrentQueue<T>>,
}

impl<T> SlotQueue<T> {
    /// Pushes an item into the slot, overflowing the old item into the queue.
    fn push(&self, t: T) -> Result<(), PushError<T>> {
        match self.slot.replace(Some(t)) {
            None => Ok(()),
            Some(t) => self.queue.push(t),
        }
    }

    /// Pops an item from the slot, or queue if the slot is empty.
    fn pop(&self) -> Result<T, PopError> {
        match self.slot.take() {
            None => self.queue.pop(),
            Some(t) => Ok(t),
        }
    }

    /// Flushes the slot into the queue.
    fn flush(&self) -> Result<(), PushError<T>> {
        match self.slot.take() {
            None => Ok(()),
            Some(t) => self.queue.push(t),
        }
    }

    /// Steals some items from another queue.
    fn steal(&self, from: &ConcurrentQueue<T>) {
        // Flush the slot before stealing.
        if let Err(err) = self.flush() {
            self.slot.set(Some(err.into_inner()));
            return;
        }

        // Half of `from`'s length rounded up.
        let mut count = (from.len() + 1) / 2;

        if count > 0 {
            // Don't steal more than fits into the queue.
            if let Some(cap) = self.queue.capacity() {
                count = count.min(cap - self.queue.len());
            }

            // Steal tasks.
            for _ in 0..count {
                if let Ok(t) = from.pop() {
                    assert!(self.queue.push(t).is_ok());
                } else {
                    break;
                }
            }
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

#[derive(Clone)]
struct Callback(Arc<Box<dyn Fn() + Send + Sync>>);

impl Callback {
    fn new(f: impl Fn() + Send + Sync + 'static) -> Callback {
        Callback(Arc::new(Box::new(f)))
    }

    fn call(&self) {
        (self.0)();
    }
}

impl PartialEq for Callback {
    fn eq(&self, other: &Callback) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Callback {}

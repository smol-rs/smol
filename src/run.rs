//! Implementation of [`run()`].
//!
//! This function is the entry point to the smol executor.

use std::future::Future;
use std::task::{Context, Poll};
use std::thread;

use futures::future::{self, Either};

use crate::block_on;
use crate::context;
use crate::io_event::IoEvent;
use crate::reactor::{Reactor, ReactorLock};
use crate::thread_local::ThreadLocalExecutor;
use crate::throttle;
use crate::work_stealing::WorkStealingExecutor;

/// Runs executors and polls the reactor.
///
/// This function simultaneously runs the thread-local executor, runs the work-stealing
/// executor, and polls the reactor for I/O events and timers. At least one thread has to be
/// calling [`run()`] in order for futures waiting on I/O and timers to get notified.
///
/// # Examples
///
/// Single-threaded executor:
///
/// ```
/// smol::run(async {
///     println!("Hello from the smol executor!");
/// });
/// ```
///
/// Multi-threaded executor:
///
/// ```
/// use futures::future;
/// use smol::Task;
/// use std::thread;
///
/// // Same number of threads as there are CPU cores.
/// let num_threads = num_cpus::get().max(1);
///
/// // Create an executor thread pool.
/// for _ in 0..num_threads {
///     // A pending future is one that simply yields forever.
///     thread::spawn(|| smol::run(future::pending::<()>()));
/// }
///
/// // No need to `run()`, now we can just block on the main future.
/// smol::block_on(async {
///     Task::spawn(async {
///         println!("Hello from an executor thread!");
///     })
///     .await;
/// });
/// ```
///
/// Stoppable multi-threaded executor:
///
/// ```
/// use smol::Task;
/// use std::thread;
///
/// // Same number of threads as there are CPU cores.
/// let num_threads = num_cpus::get().max(1);
///
/// // A channel that sends the shutdown signal.
/// let (s, r) = piper::chan::<()>(0);
/// let mut threads = Vec::new();
///
/// // Create an executor thread pool.
/// for _ in 0..num_threads {
///     // Spawn an executor thread that waits for the shutdown signal.
///     let r = r.clone();
///     threads.push(thread::spawn(move || smol::run(r.recv())));
/// }
///
/// // No need to `run()`, now we can just block on the main future.
/// smol::block_on(async {
///     Task::spawn(async {
///         println!("Hello from an executor thread!");
///     })
///     .await;
/// });
///
/// // Send a shutdown signal.
/// drop(s);
///
/// // Wait for threads to finish.
/// for t in threads {
///     t.join().unwrap();
/// }
/// ```
pub fn run<T>(future: impl Future<Output = T>) -> T {
    // Create a thread-local executor and a worker in the work-stealing executor.
    let local = ThreadLocalExecutor::new();
    let ws_executor = WorkStealingExecutor::get();
    let worker = ws_executor.worker();
    let reactor = Reactor::get();

    // Create a waker that triggers an I/O event in the thread-local scheduler.
    let ev = local.event().clone();
    let waker = async_task::waker_fn(move || ev.notify());
    let cx = &mut Context::from_waker(&waker);
    futures::pin_mut!(future);

    // Set up tokio (if enabled) and the thread-locals before execution begins.
    let enter = context::enter;
    let enter = |f| local.enter(|| enter(f));
    let enter = |f| worker.enter(|| enter(f));

    enter(|| {
        // A list of I/O events that indicate there is work to do.
        let io_events = [local.event(), ws_executor.event()];

        // Number of times this thread has yielded because it didn't find any work.
        let mut yields = 0;

        // We run four components at the same time, treating them all fairly and making sure none
        // of them get starved:
        //
        // 1. `future` - the main future.
        // 2. `local - the thread-local executor.
        // 3. `worker` - the work-stealing executor.
        // 4. `reactor` - the reactor.
        //
        // When all four components are out of work, we block the current thread on
        // epoll/kevent/wepoll. If new work comes in that isn't naturally triggered by an I/O event
        // registered with `Async` handles, we use `IoEvent`s to simulate an I/O event that will
        // unblock the thread:
        //
        // - When the main future is woken, `local.event()` is triggered.
        // - When thread-local executor gets new work, `local.event()` is triggered.
        // - When work-stealing executor gets new work, `ws_executor.event()` is triggered.
        // - When a new earliest timer is registered, `reactor.event()` is triggered.
        //
        // This way we make sure that if any changes happen that might give us new work will
        // unblock epoll/kevent/wepoll and let us continue the loop.
        loop {
            // 1. Poll the main future.
            if let Poll::Ready(val) = throttle::setup(|| future.as_mut().poll(cx)) {
                return val;
            }

            // 2. Run a batch of tasks in the thread-local executor.
            let more_local = local.execute();
            // 3. Run a batch of tasks in the work-stealing executor.
            let more_worker = worker.execute();

            // 4. Poll the reactor.
            if let Some(reactor_lock) = reactor.try_lock() {
                yields = 0;
                react(reactor_lock, &io_events, more_local || more_worker);
                continue;
            }

            // If there is more work in the thread-local or the work-stealing executor, continue.
            if more_local || more_worker {
                yields = 0;
                continue;
            }

            // Yield a few times if no work is found.
            yields += 1;
            if yields <= 2 {
                thread::yield_now();
                continue;
            }

            // If still no work is found, stop yielding and block the thread.
            yields = 0;

            // Prepare for blocking until the reactor is locked or `local.event()` is triggered.
            //
            // Note that there is no need to wait for `ws_executor.event()`. If we lock the reactor
            // immediately, we'll check for the I/O event right after that anyway.
            //
            // If some other worker is holding the reactor locked, it will unlock it as soon as the
            // I/O event is triggered. Then, another worker will be allowed to lock the reactor,
            // and will unlock it if there is more work to do because every worker triggers the I/O
            // event whenever it finds a runnable task.
            let lock = reactor.lock();
            let notified = local.event().notified();
            futures::pin_mut!(lock);
            futures::pin_mut!(notified);

            // Block until either the reactor is locked or `local.event()` is triggered.
            if let Either::Left((reactor_lock, _)) = block_on(future::select(lock, notified)) {
                react(reactor_lock, &io_events, false);
            }
        }
    })
}

/// Polls or waits on the locked reactor.
///
/// If any of the I/O events are ready or there are more tasks to run, the reactor is polled.
/// Otherwise, the current thread waits on it until a timer fires or an I/O event occurs.
///
/// I/O events are cleared at the end of this function.
fn react(mut reactor_lock: ReactorLock<'_>, io_events: &[&IoEvent], mut more_tasks: bool) {
    // Clear all I/O events and check if any of them were triggered.
    for ev in io_events {
        if ev.clear() {
            more_tasks = true;
        }
    }

    if more_tasks {
        // If there might be more tasks to run, just poll without blocking.
        reactor_lock.poll().expect("failure while polling I/O");
    } else {
        // Otherwise, block until the first I/O event or a timer.
        reactor_lock.wait().expect("failure while waiting on I/O");

        // Clear all I/O events before dropping the lock. This is not really necessary, but
        // clearing flags here might prevent a redundant wakeup in the future.
        for ev in io_events {
            ev.clear();
        }
    }
}

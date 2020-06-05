//! Implementation of [`run()`].
//!
//! This function is the entry point to the smol executor.

use std::future::Future;
use std::task::{Context, Poll};
use std::thread;

use futures_util::future::{self, Either};

use crate::block_on;
use crate::context;
use crate::io_event::IoEvent;
use crate::reactor::{Reactor, ReactorLock};
use crate::thread_local::ThreadLocalExecutor;
use crate::throttle;
use crate::work_stealing::{WorkStealingExecutor, Worker};

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
/// // Run the thread-local and work-stealing executor on the current thread.
/// smol::run(async {
///     println!("Hello from the smol executor!");
/// });
/// ```
///
/// Multi-threaded executor:
///
/// ```no_run
/// use futures::future;
/// use smol::Task;
/// use std::thread;
///
/// // Same number of threads as there are CPU cores.
/// let num_threads = num_cpus::get().max(1);
///
/// // Run the thread-local and work-stealing executor on a thread pool.
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
    let event = IoEvent::new().expect("cannot create an `IoEvent`");
    let ws_executor = WorkStealingExecutor::get();
    let worker = ws_executor.worker();
    let reactor = Reactor::get();

    // Create a waker that triggers an I/O event in the thread-local scheduler.
    let ev = event.clone();
    let waker = async_task::waker_fn(move || ev.notify());
    let cx = &mut Context::from_waker(&waker);
    futures_util::pin_mut!(future);

    // Set up tokio (if enabled) and the thread-locals before execution begins.
    let enter = context::enter;
    let enter = |f| worker.enter(|| enter(f));

    enter(|| {
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
            if let Poll::Ready(val) = throttle::setup(|| future.as_mut().poll(cx)) {
                return val;
            }

            let more = worker.execute();

            if more {
                if let Some(mut reactor_lock) = reactor.try_lock() {
                    reactor_lock.poll().expect("failure while polling I/O");
                }
                continue;
            }

            // 4. Poll the reactor.
            if let Some(reactor_lock) = reactor.try_lock() {
                react(&worker, reactor_lock, &event);
                continue;
            }

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
            let notified = event.notified();
            futures_util::pin_mut!(lock);
            futures_util::pin_mut!(notified);

            // Block until either the reactor is locked or `local.event()` is triggered.
            if let Either::Left((reactor_lock, _)) = block_on(future::select(lock, notified)) {
                react(&worker, reactor_lock, &event);
            } else {
                // Clear `local.event()` because it was triggered.
                event.clear();
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
fn react(worker: &Worker<'_>, mut reactor_lock: ReactorLock<'_>, event: &IoEvent) {
    if event.clear() {
        // If there might be more tasks to run, just poll without blocking.
        reactor_lock.poll().expect("failure while polling I/O");
    } else {
        // Otherwise, block until the first I/O event or a timer.
        reactor_lock.wait().expect("failure while waiting on I/O");

        event.clear();
    }
}

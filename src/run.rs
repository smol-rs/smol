//! Implementation of [`run()`].
//!
//! This function is the entry point to the smol executor.

use std::future::Future;
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;

use futures_util::future::{self, Either};

use crate::block_on;
use crate::context;
use crate::io_event::IoEvent;
use crate::io_parking::{IoParker, IoUnparker};
use crate::reactor::{Reactor, ReactorLock};
use crate::thread_local::LocalExecutor;
use crate::throttle;
use crate::work_stealing::{Executor, Worker};

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
    let parker = IoParker::new();

    let unparker = parker.unparker();
    let worker = Executor::get().worker(move || unparker.unpark());

    let unparker = parker.unparker();
    let local = LocalExecutor::new(move || unparker.unpark());

    // Create a waker that triggers an I/O event in the thread-local scheduler.
    let unparker = parker.unparker();
    let waker = async_task::waker_fn(move || unparker.unpark());
    let cx = &mut Context::from_waker(&waker);
    futures_util::pin_mut!(future);

    // Set up tokio if enabled.
    // let mut enter = context::enter;

    loop {
        // Poll the main future.
        if let Poll::Ready(val) = throttle::setup(|| future.as_mut().poll(cx)) {
            return val;
        }

        let mut more_worker = true;
        let mut more_local = true;
        // enter(|| {
            for _ in 0..200 {
                if !worker.tick() {
                    more_worker = false;
                    break;
                }
            }

            for _ in 0..200 {
                if !local.tick() {
                    more_local = false;
                    break;
                }
            }
        // });

        if more_local || more_worker {
            // Process ready I/O events without blocking.
            parker.park_timeout(Duration::from_secs(0));
        } else {
            // Wait until unparked.
            parker.park();
        }
    }
}

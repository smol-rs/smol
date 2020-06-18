//! Implementation of [`run()`].
//!
//! This function is the entry point to the smol executor.

use std::future::Future;
use std::task::{Context, Poll};
use std::time::Duration;

use once_cell::sync::Lazy;

use crate::context;
use crate::multitask;
use crate::parking::Parker;
use scoped_tls::scoped_thread_local;

/// The global task queue.
pub(crate) static QUEUE: Lazy<multitask::Queue> = Lazy::new(|| multitask::Queue::new());

scoped_thread_local! {
    /// Thread-local worker queue.
    pub(crate) static WORKER: multitask::Worker
}

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
    let parker = Parker::new();

    let unparker = parker.unparker();
    let worker = QUEUE.worker(move || unparker.unpark());

    // Create a waker that triggers an I/O event in the thread-local scheduler.
    let unparker = parker.unparker();
    let waker = async_task::waker_fn(move || unparker.unpark());
    let cx = &mut Context::from_waker(&waker);
    futures_util::pin_mut!(future);

    // Set up tokio if enabled.
    context::enter(|| {
        WORKER.set(&worker, || {
            'start: loop {
                // Poll the main future.
                if let Poll::Ready(val) = future.as_mut().poll(cx) {
                    return val;
                }

                for _ in 0..200 {
                    if !worker.tick() {
                        parker.park();
                        continue 'start;
                    }
                }

                // Process ready I/O events without blocking.
                parker.park_timeout(Duration::from_secs(0));
            }
        })
    })
}

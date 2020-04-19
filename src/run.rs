use std::future::Future;
use std::task::{Context, Poll};

use futures::future::{self, Either};

use crate::block_on;
use crate::context;
use crate::reactor::Reactor;
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
/// TODO a thread-pool example with num_cpus::get().max(1)
/// TODO a stoppable thread-pool with channels
///
/// ```no_run
/// use futures::future;
/// use std::thread;
///
/// for _ in 0..num_cpus::get().max(1) {
///     thread::spawn(|| smol::run(future::pending::<()>()));
/// }
/// ```
pub fn run<T>(future: impl Future<Output = T>) -> T {
    // Create a thread-local executor and a worker in the work-stealing executor.
    let local = ThreadLocalExecutor::new();
    let ws_executor = WorkStealingExecutor::get();
    let worker = ws_executor.worker();

    // Create a waker that triggers an I/O event in the thread-local scheduler.
    let ev = local.event().clone();
    let waker = async_task::waker_fn(move || ev.set());
    let cx = &mut Context::from_waker(&waker);
    futures::pin_mut!(future);

    // Set up tokio (if enabled) and the thread-locals before execution begins.
    let enter = context::enter;
    let enter = |f| local.enter(|| enter(f));
    let enter = |f| worker.enter(|| enter(f));

    enter(|| {
        // We run four components at the same time, treating them all fairly and making sure none
        // of them get starved:
        //
        // 1. `future` - the main future.
        // 2. `local - the thread-local executor.
        // 3. `worker` - the work-stealing executor.
        // 4. `Reactor::get()` - the reactor.
        //
        // When all four components are out of work, we block the current thread on
        // epoll/kevent/wepoll. If new work comes in that isn't naturally triggered by an I/O event
        // registered with `Async` handles, we use `IoEvent`s to simulate an I/O event that will
        // unblock the thread:
        //
        // - When the main future is woken, `local.event` is triggered.
        // - When thread-local executor gets new work, `local.event` is triggered.
        // - When work-stealing executor gets new work, `ws_executor.event` is triggered.
        // - When a new earliest timer is registered, `Reactor::get().event` is triggered.
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
            Reactor::get().poll().expect("failure while polling I/O");

            // If there is more work in the thread-local or the work-stealing executor, continue
            // the loop.
            if more_local || more_worker {
                continue;
            }

            // Prepare for blocking until the reactor is locked or `local.event` is triggered.
            //
            // Note that there is no need to wait for `executor.event`. If the reactor is locked
            // immediately, we'll check for the I/O event right after that anyway.
            //
            // If some other worker is holding the reactor locked, it will be unblocked as soon as
            // the I/O event is triggered. Then, another worker will be allowed to lock the
            // reactor, and will be unblocked if there is more work to do. Every worker triggers
            // `worker.executor.event` each time it finds a runnable task.
            let lock = Reactor::get().lock();
            let ready = local.event().ready();
            futures::pin_mut!(lock);
            futures::pin_mut!(ready);

            // Block until either the reactor is locked or `local.event` is triggered.
            if let Either::Left((mut reactor, _)) = block_on(future::select(lock, ready)) {
                // Clear the two I/O events.
                let local_ev = local.event().clear();
                let ws_ev = ws_executor.event().clear();

                // If any of the two I/O events has been triggered, continue the loop.
                if local_ev || ws_ev {
                    continue;
                }

                // Block until an I/O event occurs.
                reactor.wait().expect("failure while waiting on I/O");
            }
        }
    })
}

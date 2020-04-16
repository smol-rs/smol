//! A small and fast async runtime.
//!
//! # Executors
//!
//! There are three executors that poll futures:
//!
//! 1. Thread-local executor for tasks created by [`Task::local()`].
//! 2. Work-stealing executor for tasks created by [`Task::spawn()`].
//! 3. Blocking executor for tasks created by [`Task::blocking()`], [`blocking!`], [`iter()`],
//!    [`reader()`] and [`writer()`].
//!
//! Blocking executor is the only one that spawns threads.
//!
//! # Reactor
//!
//! To wait for the next I/O event, the reactor calls [epoll] on Linux/Android, [kqueue] on
//! macOS/iOS/BSD, and [WSAPoll] on Windows.
//!
//! The [`Async`] type registers an I/O handle in the reactor and is able to convert its blocking
//! operations into async operations.
//!
//! The [`Timer`] type registers a timer in the reactor that will fire at the chosen point in
//! time.
//!
//! # Running
//!
//! Function [`run()`] simultaneously runs the thread-local executor, runs the work-stealing
//! executor, and polls the reactor for I/O events and timers. At least one thread has to be
//! calling [`run()`] in order for futures waiting on I/O and timers to get notified.
//!
//! If you want a multithreaded runtime, just call [`run()`] from multiple threads. See [here TODO
//! link to example] for an example.
//!
//! There is also [`block_on()`], which blocks the thread until a future completes, but it doesn't
//! do anything else besides that.
//!
//! Blocking tasks run in the background on a dedicated thread pool.
//!
//! # Examples
//!
//! Connect to a HTTP website, make a GET request, and pipe the response to the standard output:
//!
//! ```
//! use futures::prelude::*;
//! use smol::Async;
//! use std::net::TcpStream;
//!
//! fn main() -> std::io::Result<()> {
//!     smol::run(async {
//!         let mut stream = Async::<TcpStream>::connect("example.com:80").await?;
//!         let req = b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n";
//!         stream.write_all(req).await?;
//!
//!         let mut stdout = smol::writer(std::io::stdout());
//!         futures::io::copy(&stream, &mut stdout).await?;
//!         Ok(())
//!     })
//! }
//! ```
//!
//! Look inside the [examples] directory for more.
//!
//! Those examples show how to read a [file][read-file] or
//! [directory][read-directory], [spawn][process-run] a process or read its
//! [output][process-output], use timers to [sleep][timer-sleep] or set a
//! [timeout][timer-timeout], catch the [Ctrl-C][ctrl-c] signal and [gracefully shut down] TODO,
//! redirect standard [input to output][stdin-to-stdout].
//!
//! They include a simple TCP [client][tcp-client]/[server][tcp-server], a chat
//! [client][chat-client]/[server][chat-server] over TCP, a [web crawler][web-crawler], a simple
//! TLS [client][tls-client]/[server][tls-server], a simple
//! HTTP+TLS [client][simple-client]/[server][simple-server], a [hyper]
//! [client][hyper-client]/[server][hyper-server], an [async-h1]
//! [client][async-h1-client]/[server][async-h1-server], a WebSocket+TLS
//! [client][websocket-client]/[server][websocket-server].
//!
//! Even non-async libraries can be plugged into this runtime. See examples for [inotify],
//! [timerfd], [signal-hook], and [uds_windows].
//!
//! You can also mix this runtime with [async-std] and [tokio], or use runtime-specific
//! libraries like [reqwest].
//!
//! [epoll]: https://en.wikipedia.org/wiki/Epoll
//! [kqueue]: https://en.wikipedia.org/wiki/Kqueue
//! [WSAPoll]: https://docs.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-wsapoll
//!
//! [examples]: https://github.com/stjepang/smol/tree/master/examples
//! [async-h1]: https://docs.rs/async-h1
//! [hyper]: https://docs.rs/hyper
//! [www.rust-lang.org]: https://www.rust-lang.org/
//! [cat]: https://en.wikipedia.org/wiki/Cat_(Unix)
//!
//! [stdin-to-stdout]: https://github.com/stjepang/smol/blob/master/examples/stdin-to-stdout.rs
//! [ctrl-c]: https://github.com/stjepang/smol/blob/master/examples/ctrl-c.rs
//! [read-file]: https://github.com/stjepang/smol/blob/master/examples/read-file.rs
//! [read-directory]: https://github.com/stjepang/smol/blob/master/examples/read-directory.rs
//! [timer-sleep]: https://github.com/stjepang/smol/blob/master/examples/timer-sleep.rs
//! [timer-timeout]: https://github.com/stjepang/smol/blob/master/examples/timer-timeout.rs
//! [process-run]: https://github.com/stjepang/smol/blob/master/examples/process-run.rs
//! [process-output]: https://github.com/stjepang/smol/blob/master/examples/process-output.rs
//! [tcp-client]: https://github.com/stjepang/smol/blob/master/examples/tcp-client.rs
//! [tcp-server]: https://github.com/stjepang/smol/blob/master/examples/tcp-server.rs
//! [tls-client]: https://github.com/stjepang/smol/blob/master/examples/tls-client.rs
//! [tls-server]: https://github.com/stjepang/smol/blob/master/examples/tls-server.rs
//! [simple-client]: https://github.com/stjepang/smol/blob/master/examples/simple-client.rs
//! [simple-server]: https://github.com/stjepang/smol/blob/master/examples/simple-server.rs
//! [async-h1-client]: https://github.com/stjepang/smol/blob/master/examples/async-h1-client.rs
//! [async-h1-server]: https://github.com/stjepang/smol/blob/master/examples/async-h1-server.rs
//! [hyper-client]: https://github.com/stjepang/smol/blob/master/examples/hyper-client.rs
//! [hyper-server]: https://github.com/stjepang/smol/blob/master/examples/hyper-server.rs
//! [websocket-client]: https://github.com/stjepang/smol/blob/master/examples/websocket-client.rs
//! [websocket-server]: https://github.com/stjepang/smol/blob/master/examples/websocket-server.rs
//! [chat-client]: https://github.com/stjepang/smol/blob/master/examples/chat-client.rs
//! [chat-server]: https://github.com/stjepang/smol/blob/master/examples/chat-server.rs
//! [web-crawler]: https://github.com/stjepang/smol/blob/master/examples/web-crawler.rs
//! [inotify]: https://github.com/stjepang/smol/blob/master/examples/linux-inotify.rs
//! [timerfd]: https://github.com/stjepang/smol/blob/master/examples/linux-timerfd.rs
//! [signal-hook]: https://github.com/stjepang/smol/blob/master/examples/unix-signal.rs
//! [uds_windows]: https://github.com/stjepang/smol/blob/master/examples/windows-uds.rs

#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

#[cfg(not(any(
    target_os = "linux",     // epoll
    target_os = "android",   // epoll
    target_os = "macos",     // kqueue
    target_os = "ios",       // kqueue
    target_os = "freebsd",   // kqueue
    target_os = "netbsd",    // kqueue
    target_os = "openbsd",   // kqueue
    target_os = "dragonfly", // kqueue
    target_os = "windows",   // WSAPoll
)))]
compile_error!("smol does not support this target OS");

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Debug;
use std::io::{self, Read, Write};
use std::mem;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::num::Wrapping;
use std::panic;
use std::pin::Pin;
use std::sync::atomic::{self, AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::{
    os::unix::io::{AsRawFd, RawFd},
    os::unix::net::{SocketAddr as UnixSocketAddr, UnixDatagram, UnixListener, UnixStream},
    path::Path,
};

#[cfg(windows)]
use std::os::windows::io::{AsRawSocket, FromRawSocket, RawSocket};

use crossbeam::deque;
use crossbeam::queue::SegQueue;
use crossbeam::sync::{Parker, ShardedLock};
use futures::future::Either;
use futures::io::AllowStdIo;
use futures::prelude::*;
use once_cell::sync::Lazy;
use scoped_tls::scoped_thread_local;
use slab::Slab;
use socket2::{Domain, Protocol, Socket, Type};
use std::sync::{Condvar, Mutex, MutexGuard};

// TODO: explain the implementation and major components:
// - the Task struct
// - thread-local executor
// - work-stealing executor
// - reactor
// - Timer
// - Async
// - the IoEvent
// - blocking executor
// - sys module

// ---------- The task system ----------

/// A runnable future, ready for execution.
///
/// When a future is internally spawned using `async_task::spawn()` or `async_task::spawn_local()`,
/// we get back two values:
///
/// 1. an `async_task::Task<()>`, which we refer to as a `Runnable`
/// 2. an `async_task::JoinHandle<T, ()>`, which is wrapped inside a `Task<T>`
///
/// Once a `Runnable` is run, it "vanishes" and only reappears when its future is woken.
type Runnable = async_task::Task<()>;

/// A spawned future.
///
/// Tasks are also futures themselves and yield the output of the spawned future.
///
/// When a task is dropped, its gets canceled and won't be polled again. To cancel a task a bit
/// more gracefully and wait until it is destroyed completely, use the [`cancel()`][Task::cancel()]
/// method.
///
/// Tasks that panic get immediately canceled. Awaiting a canceled task also causes a panic.
///
/// If the future panics, the panic will be unwound into the [`run()`] invocation that polled it.
/// However, this does not apply to the blocking executor - it will simply ignore panics and
/// continue running.
///
/// # Examples
///
/// ```
/// use smol::Task;
///
/// # smol::run(async {
/// // Spawn a task onto the work-stealing executor.
/// let task = Task::spawn(async {
///     println!("Hello from a task!");
///     1 + 2
/// });
///
/// // Wait for the task to complete.
/// assert_eq!(task.await, 3);
/// # });
/// ```
#[must_use = "tasks get canceled when dropped, use `.detach()` to run them in the background"]
#[derive(Debug)]
pub struct Task<T>(Option<async_task::JoinHandle<T, ()>>);

impl<T: 'static> Task<T> {
    /// Spawns a future onto the thread-local executor.
    ///
    /// Panics if the current thread is not inside an invocation of [`run()`].
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::Task;
    ///
    /// smol::run(async {
    ///     let task = Task::local(async { 1 + 2 });
    ///     assert_eq!(task.await, 3);
    /// })
    /// ```
    pub fn local(future: impl Future<Output = T> + 'static) -> Task<T> {
        THREAD_LOCAL_EXECUTOR.with(|ex| ex.spawn(future))
    }
}

impl<T: Send + 'static> Task<T> {
    /// Spawns a future onto the work-stealing executor.
    ///
    /// This future may be stolen and polled by any thread calling [`run()`].
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::Task;
    ///
    /// # smol::run(async {
    /// let task = Task::spawn(async { 1 + 2 });
    /// assert_eq!(task.await, 3);
    /// # });
    /// ```
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        WorkStealingExecutor::get().spawn(future)
    }

    /// Spawns a future onto the blocking executor.
    ///
    /// This future is allowed to block for an indefinite length of time.
    ///
    /// For convenience, there is also the [`blocking!`] macro that spawns a blocking tasks and
    /// immediately awaits it.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Task;
    /// use std::io::stdin;
    ///
    /// # smol::block_on(async {
    /// let line = Task::blocking(async {
    ///     let mut line = String::new();
    ///     std::io::stdin().read_line(&mut line).unwrap();
    ///     line
    /// })
    /// .await;
    /// # });
    /// ```
    pub fn blocking(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        BlockingExecutor::get().spawn(future)
    }
}

impl<T, E> Task<Result<T, E>>
where
    T: Send + 'static,
    E: Debug + Send + 'static,
{
    /// Spawns a new task that awaits and unwraps the result.
    ///
    /// The new task will panic if the original task results in an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::Task;
    ///
    /// TODO
    /// ```
    pub fn unwrap(self) -> Task<T> {
        Task::spawn(async { self.await.unwrap() })
    }

    /// Spawns a new task that awaits and unwraps the result.
    ///
    /// The new task will panic with the provided message if the original task results in an error.
    ///
    /// TODO
    pub fn expect(self, msg: &str) -> Task<T> {
        let msg = msg.to_owned();
        Task::spawn(async move { self.await.expect(&msg) })
    }
}

impl Task<()> {
    /// Detaches the task to keep running in the background.
    ///
    /// TODO
    pub fn detach(mut self) {
        self.0.take().unwrap();
    }
}

impl<T> Task<T> {
    /// TODO
    pub async fn cancel(mut self) -> Option<T> {
        let handle = self.0.take().unwrap();
        handle.cancel();
        handle.await
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.cancel();
        }
    }
}

impl<T> Future for Task<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0.as_mut().unwrap()).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(output) => Poll::Ready(output.expect("task has failed")),
        }
    }
}

/// Blocks on a single future.
///
/// TODO
pub fn block_on<T>(future: impl Future<Output = T>) -> T {
    // The implementation of this function is explained by the following blog post:
    // https://stjepang.github.io/2020/01/25/build-your-own-block-on.html

    thread_local! {
        // Parker and waker associated with the current thread.
        static CACHE: RefCell<(Parker, Waker)> = {
            let parker = Parker::new();
            let unparker = parker.unparker().clone();
            let waker = async_task::waker_fn(move || unparker.unpark());
            RefCell::new((parker, waker))
        };
    }

    CACHE.with(|cache| {
        // Panic if `block_on()` is called recursively.
        let (parker, waker) = &mut *cache.try_borrow_mut().ok().expect("recursive `block_on()`");

        futures::pin_mut!(future);
        let cx = &mut Context::from_waker(&waker);
        loop {
            match future.as_mut().poll(cx) {
                Poll::Ready(output) => return output,
                Poll::Pending => parker.park(),
            }
        }
    })
}

/// Runs executors and the reactor.
///
/// TODO a thread-pool example with num_cpus::get().max(1)
/// TODO a stoppable thread-pool with channels
pub fn run<T>(future: impl Future<Output = T>) -> T {
    // If this thread is already inside an executor, panic.
    if WORKER.is_set() {
        panic!("recursive `run()`");
    }

    // Create a thread-local executor and a worker in the work-stealing executor.
    let local = ThreadLocalExecutor::new();
    let worker = WorkStealingExecutor::get().worker();

    // Create a waker that triggers an I/O event in the thread-local scheduler.
    let ev = local.event.clone();
    let waker = async_task::waker_fn(move || ev.set());
    let cx = &mut Context::from_waker(&waker);
    futures::pin_mut!(future);

    // Set up the thread-locals before execution begins.
    THREAD_LOCAL_EXECUTOR.set(&local, || {
        WORKER.set(&worker, || {
            // We "drive" four components at the same time, treating them all fairly and making
            // sure none of them get starved:
            //
            // 1. `future` - the main future.
            // 2. `local - the thread-local executor.
            // 3. `worker` - the work-stealing executor.
            // 4. `Reactor::get()` - the reactor.
            //
            // When all four components are out of work, we block the current thread on
            // epoll/kevent/WSAPoll. If new work comes in that isn't naturally triggered by an
            // I/O event registered with `Async` handles, we use `IoEvent`s to simulate an I/O
            // event that will unblock the thread:
            //
            // - When the main future is woken, `local.event` is triggered.
            // - When thread-local executor gets new work, `local.event` is triggered.
            // - When work-stealing executor gets new work, `worker.executor.event` is triggered.
            // - When a new earliest timer is registered, `Reactor::get().event` is triggered.
            //
            // This way we make sure that if any changes happen that might give us new work will
            // unblock epoll/kevent/WSAPoll and let us continue the loop.
            loop {
                // 1. Poll the main future.
                if let Poll::Ready(val) = use_throttle(|| future.as_mut().poll(cx)) {
                    return val;
                }
                // 2. Run a batch of tasks in the thread-local executor.
                let more_local = local.execute();
                // 3. Run a batch of tasks in the work-stealing executor.
                let more_worker = worker.execute();
                // 4. Poll the reactor.
                Reactor::get().poll().expect("failure while polling I/O");

                // If there is more work in the thread-local or the work-stealing executor,
                // continue the loop.
                if more_local || more_worker {
                    continue;
                }

                // Prepare for blocking until the reactor is locked or `local.event` is triggered.
                //
                // Note that there is no need to wait for `worker.executor.event`. If the reactor
                // is locked immediately, we'll check for the I/O event right after that anyway.
                //
                // If some other worker is holding the reactor locked, it will be unblocked as soon
                // as the I/O event is triggered. Then, another worker will be allowed to lock the
                // reactor, and will be unblocked if there is more work to do. Every worker
                // triggers `worker.executor.event` every time it finds a runnable task.
                let lock = Reactor::get().lock();
                let ready = local.event.ready();
                futures::pin_mut!(lock);
                futures::pin_mut!(ready);

                // Block until either the reactor is locked or `local.event` is triggered.
                if let Either::Left((mut reactor, _)) = block_on(future::select(lock, ready)) {
                    // Clear the two I/O events.
                    let local_ev = local.event.clear();
                    let worker_ev = worker.executor.event.clear();

                    // If any of the two I/O events has been triggered, continue the loop.
                    if local_ev || worker_ev {
                        continue;
                    }

                    // Block until an I/O event occurs.
                    reactor.wait().expect("failure while waiting on I/O");
                }
            }
        })
    })
}

// Number of times the current task is allowed to poll I/O operations.
//
// When this budget is used up, I/O operations will wake the current task and return
// `Poll::Pending`.
//
// This thread-local is set before running any task.
scoped_thread_local!(static BUDGET: Cell<u32>);

/// Sets an I/O budget for polling a future.
///
/// Once this budget is exceeded, polled I/O operations will always wake the current task and
/// return `Poll::Pending`.
///
/// We throttle I/O this way in order to prevent futures from running for
/// too long and thus starving other futures.
fn use_throttle<T>(poll: impl FnOnce() -> T) -> T {
    // This is a fairly arbitrary number that seems to work well in practice.
    BUDGET.set(&Cell::new(200), poll)
}

/// Returns `Poll::Pending` if the I/O budget has been used up.
fn poll_throttle(cx: &mut Context<'_>) -> Poll<()> {
    // Decrement the budget and check if it was zero.
    if BUDGET.is_set() && BUDGET.with(|b| b.replace(b.get().saturating_sub(1))) == 0 {
        // Make sure to wake the current task. The task is not *really* pending, we're just
        // artificially throttling it to let other tasks be run.
        cx.waker().wake_by_ref();
        return Poll::Pending;
    }
    Poll::Ready(())
}

// ---------- Thread-local executor ----------

// The thread-local executor.
//
// This thread-local is only set while inside `run()`.
scoped_thread_local!(static THREAD_LOCAL_EXECUTOR: ThreadLocalExecutor);

/// An executor for thread-local tasks.
///
/// Thread-local tasks are spawned by calling `Task::local()` and their futures do not have to
/// implement `Send`. They can only be run by the same thread that created them.
struct ThreadLocalExecutor {
    /// The main task queue.
    queue: RefCell<VecDeque<Runnable>>,

    /// When another thread wakes a task belonging to this executor, it goes into this queue.
    injector: Arc<SegQueue<Runnable>>,

    /// An I/O event that is triggered when another thread wakes a task belonging to this executor.
    event: IoEvent,
}

impl ThreadLocalExecutor {
    /// Creates a new thread-local executor.
    fn new() -> ThreadLocalExecutor {
        ThreadLocalExecutor {
            queue: RefCell::new(VecDeque::new()),
            injector: Arc::new(SegQueue::new()),
            event: IoEvent::create().expect("cannot create an `IoEvent`"),
        }
    }

    /// Spawns a future onto this executor.
    ///
    /// Returns a `Task` handle for the spawned task.
    fn spawn<T: 'static>(&self, future: impl Future<Output = T> + 'static) -> Task<T> {
        let id = thread_id();
        let injector = self.injector.clone();
        let event = self.event.clone();

        // The function that schedules a runnable task.
        let schedule = move |runnable| {
            if thread_id() == id {
                // If scheduling from the original thread, push into the main queue.
                THREAD_LOCAL_EXECUTOR.with(|ex| ex.queue.borrow_mut().push_back(runnable));
            } else {
                // If scheduling from a different thread, push into the injector queue.
                injector.push(runnable);
                // Trigger an I/O event to let the original thread know that a task has been
                // scheduled. If that thread is inside epoll/kqueue/WSAPoll, an I/O event will wake
                // it up.
                event.set();
            }
        };

        // Create a task, schedule it, and return its `Task` handle.
        let (runnable, handle) = async_task::spawn_local(future, schedule, ());
        runnable.schedule();
        Task(Some(handle))
    }

    /// Executes a batch of tasks and returns `true` if there are more tasks to run.
    fn execute(&self) -> bool {
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
                        use_throttle(|| r.run());
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

// ---------- Work-stealing executor ----------

// The current thread's worker.
//
// Other threads may steal tasks from this worker through its associated stealer that was
// registered in the work-stealing executor.
//
// This thread-local is only set while inside `run()`.
scoped_thread_local!(static WORKER: Worker);

/// The global work-stealing executor.
///
/// Tasks created by `Task::spawn()` go into this executor. Any calling `run()` initializes a
/// `Worker` that participates in work stealing, which is allowed to run any task in this executor
/// or in other workers.
///
/// Since tasks can be stolen by any worker and thus move from thread to thread, their futures must
/// implement `Send`.
///
/// There is only one global instance of this type, accessible by `Reactor::get()`.
///
/// Work stealing is a strategy that reduces contention in a multi-threaded environment. If all
/// invocations of `run()` used the same global task queue all the time, they would constantly
/// "step on each other's toes", causing a lot of CPU cache traffic and too often waste time
/// retrying queue operations in compare-and-swap loops.
///
/// The solution is to have a separate queue in each invocation of `run()`, called a "worker".
/// Each thread is primarily using its own worker. Once there are no more tasks in the worker, we
/// either grab a batch of tasks from the main global queue, or steal tasks from other workers.
/// Of course, work-stealing still causes contention in some cases, but much less often.
///
/// More about work stealing: https://en.wikipedia.org/wiki/Work_stealing
struct WorkStealingExecutor {
    /// When a thread that is not inside `run()` spawns or wakes a task, it goes into this queue.
    injector: deque::Injector<Runnable>,

    /// Registered handles for stealing tasks from workers.
    stealers: ShardedLock<Slab<deque::Stealer<Runnable>>>,

    /// An I/O event that is triggered whenever there might be available tasks to run.
    event: IoEvent,
}

impl WorkStealingExecutor {
    /// Returns a reference to the global work-stealing executor.
    fn get() -> &'static WorkStealingExecutor {
        static EXECUTOR: Lazy<WorkStealingExecutor> = Lazy::new(|| WorkStealingExecutor {
            injector: deque::Injector::new(),
            stealers: ShardedLock::new(Slab::new()),
            event: IoEvent::create().expect("cannot create an `IoEvent`"),
        });
        &EXECUTOR
    }

    /// Spawns a future onto this executor.
    ///
    /// Returns a `Task` handle for the spawned task.
    fn spawn<T: Send + 'static>(
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
                // Trigger an I/O event to let workers know that a task has been scheduled.
                self.event.set();
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
    fn worker(&'static self) -> Worker {
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
struct Worker {
    /// The ID of this worker obtained during registration.
    key: usize,

    /// A slot into which tasks go before entering the actual queue.
    slot: Cell<Option<Runnable>>,

    /// A queue of tasks.
    ///
    /// Other workers are able to steal tasks from this queue.
    queue: deque::Worker<Runnable>,

    /// The parent work-stealing executor.
    ///
    /// This is the same thing as `WorkStealingExecutor::get()`, but we keep a reference here for
    /// convenience.
    executor: &'static WorkStealingExecutor,
}

impl Worker {
    /// Executes a batch of tasks and returns `true` if there are more tasks to run.
    fn execute(&self) -> bool {
        for _ in 0..4 {
            for _ in 0..50 {
                // Find the next task to run.
                match self.pop() {
                    None => {
                        // There are no more tasks to run.
                        return false;
                    }
                    Some(r) => {
                        // Notify other workers that there may be more tasks.
                        self.executor.event.set();

                        // Run the task.
                        if use_throttle(|| r.run()) {
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

            // Flush the slot, grab some tasks from the global queue, and poll the reactor. We do
            // this occasionally to make execution more fair to all tasks involved.
            self.push(None);
            self.fetch();
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
        // If not, fetch more tasks from the injector queue, the reactor, or other workers.
        self.fetch();
        // Check the slot and the queue again.
        self.slot.take().or_else(|| self.queue.pop())
    }

    /// Steals from the injector and polls the reactor, or steals from other workers if that fails.
    fn fetch(&self) {
        // Try stealing from the global queue.
        if let Some(r) = retry_steal(|| self.executor.injector.steal_batch_and_pop(&self.queue)) {
            // Push the task, but don't return -- let's not forget to poll the reactor.
            self.push(r);
        }

        // Poll the reactor.
        Reactor::get().poll().expect("failure while polling I/O");

        // If there is at least one task in the slot, return.
        if let Some(r) = self.slot.take() {
            self.slot.set(Some(r));
            return;
        }
        // If there is at least one task in the queue, return.
        if !self.queue.is_empty() {
            return;
        }

        // Still no tasks found - our last hope is to steal from other workers.
        let stealers = self.executor.stealers.read().unwrap();

        if let Some(r) = retry_steal(|| {
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
        }) {
            // Push the stolen task.
            self.push(r);
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

impl Drop for Worker {
    fn drop(&mut self) {
        // Unregister the worker.
        self.executor.stealers.write().unwrap().remove(self.key);

        // Flush the slot.
        self.push(None);

        // Move all tasks in this worker's queue into the injector queue.
        while let Some(r) = self.queue.pop() {
            self.executor.injector.push(r);
        }
    }
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

// ---------- Reactor ----------

/// A registered source of I/O events.
#[derive(Debug)]
struct Source {
    /// Raw file descriptor on Unix platforms.
    #[cfg(unix)]
    raw: RawFd,

    /// Raw socket handle on Windows.
    #[cfg(windows)]
    raw: RawSocket,

    /// The ID of this source obtain during registration.
    key: usize,

    /// A list of wakers representing tasks interested in events on this source.
    wakers: piper::Lock<Vec<Waker>>,

    /// Incremented on every I/O notification - this is only used for synchronization.
    tick: AtomicU64,
}

/// The reactor driving I/O events and timers.
///
/// Every async I/O handle ("source") and every timer is registered here. Invocations of `run()`
/// poll the reactor to check for new events every now and then.
///
/// There is only one global instance of this type, accessible by `Reactor::get()`.
struct Reactor {
    /// Raw bindings to epoll/kqueue/WSAPoll.
    sys: sys::Reactor,

    /// Registered sources.
    sources: piper::Lock<Slab<Arc<Source>>>,

    /// Temporary storage for I/O events when polling the reactor.
    events: piper::Mutex<sys::Events>,

    /// An ordered map of registered timers.
    ///
    /// Timers are in the order in which they fire. The `u64` in this type is a unique timer ID
    /// used to distinguish timers that fire at the same time. The `Waker` represents the task
    /// awaiting the timer.
    timers: piper::Lock<BTreeMap<(Instant, u64), Waker>>,

    /// An I/O event that is triggered when a new earliest timer is registered.
    ///
    /// This is used to wake up the thread waiting on the reactor, which would otherwise wait until
    /// the previously earliest timer.
    ///
    /// The reason why this field is lazily created is because `IoEvent`s can be created only after
    /// the reactor is fully initialized.
    event: Lazy<IoEvent>,
}

impl Reactor {
    /// Returns a reference to the reactor.
    fn get() -> &'static Reactor {
        static REACTOR: Lazy<Reactor> = Lazy::new(|| Reactor {
            sys: sys::Reactor::create().expect("cannot initialize I/O event notification"),
            sources: piper::Lock::new(Slab::new()),
            events: piper::Mutex::new(sys::Events::new()),
            timers: piper::Lock::new(BTreeMap::new()),
            event: Lazy::new(|| IoEvent::create().expect("cannot create an `IoEvent`")),
        });
        &REACTOR
    }

    /// Registers an I/O source in the reactor.
    fn register(
        &self,
        #[cfg(unix)] raw: RawFd,
        #[cfg(windows)] raw: RawSocket,
    ) -> io::Result<Arc<Source>> {
        let mut sources = self.sources.lock();
        let vacant = sources.vacant_entry();

        // Create a source and register it.
        let source = Arc::new(Source {
            raw,
            key: vacant.key(),
            wakers: piper::Lock::new(Vec::new()),
            tick: AtomicU64::new(0),
        });
        self.sys.register(raw, source.key)?;

        Ok(vacant.insert(source).clone())
    }

    /// Deregisters an I/O source from the reactor.
    fn deregister(&self, source: &Source) -> io::Result<()> {
        let mut sources = self.sources.lock();
        sources.remove(source.key);
        self.sys.deregister(source.raw)
    }

    /// Processes ready events without blocking.
    ///
    /// This doesn't have strong guarantees. If there are ready events, they may or may not be
    /// processed depending on whether the reactor is locked.
    fn poll(&self) -> io::Result<()> {
        if let Some(events) = self.events.try_lock() {
            let reactor = self;
            let mut lock = ReactorLock { reactor, events };
            // React to events without blocking.
            lock.react(false)?;
        }
        Ok(())
    }

    /// Locks the reactor.
    async fn lock(&self) -> ReactorLock<'_> {
        let reactor = self;
        let events = self.events.lock().await;
        ReactorLock { reactor, events }
    }
}

/// Polls the reactor for I/O events and wakes up tasks.
struct ReactorLock<'a> {
    reactor: &'a Reactor,
    events: piper::MutexGuard<'a, sys::Events>,
}

impl ReactorLock<'_> {
    /// Blocks until at least one event is processed.
    fn wait(&mut self) -> io::Result<()> {
        self.react(true)
    }

    fn react(&mut self, block: bool) -> io::Result<()> {
        // TODO: document this function.......................
        self.reactor.event.clear();

        let next_timer = {
            // Split timers into ready and pending timers.
            let mut timers = self.reactor.timers.lock();
            let pending = timers.split_off(&(Instant::now(), 0));
            let ready = mem::replace(&mut *timers, pending);

            // Wake up tasks waiting on timers.
            for (_, waker) in ready {
                waker.wake();
            }
            // Find when the next timer fires.
            timers.keys().next().map(|(when, _)| *when)
        };

        let timeout = if block {
            // Calculate the timeout till the first timer fires.
            next_timer.map(|when| when.saturating_duration_since(Instant::now()))
        } else {
            // If this poll doesn't block, the timeout is zero.
            Some(Duration::from_secs(0))
        };

        // Block on I/O events.
        loop {
            match self.reactor.sys.wait(&mut self.events, timeout) {
                Ok(0) => return Ok(()),
                Ok(_) => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }

        // Iterate over sources in the event list.
        let sources = self.reactor.sources.lock();
        for source in self.events.iter().filter_map(|i| sources.get(i)) {
            // I/O events may deregister sources, so we need to re-register.
            self.reactor.sys.reregister(source.raw, source.key)?;

            let mut wakers = source.wakers.lock();
            let tick = source.tick.load(Ordering::Acquire);
            source.tick.store(tick.wrapping_add(1), Ordering::Release);

            // Wake up tasks waiting on I/O.
            for w in wakers.drain(..) {
                w.wake();
            }
        }
        Ok(())
    }
}

// ---------- Timer ----------

/// Fires at the chosen point in time.
///
/// TODO
#[derive(Debug)]
pub struct Timer {
    /// A unique ID for this timer.
    ///
    /// When this field is set to `None`, this timer is not registered in the reactor.
    id: Option<u64>,

    /// When this timer fires.
    when: Instant,
}

impl Timer {
    /// Fires after the specified duration of time.
    ///
    /// TODO
    pub fn after(dur: Duration) -> Timer {
        Timer::at(Instant::now() + dur)
    }

    /// Fires at the specified instant in time.
    ///
    /// TODO
    pub fn at(when: Instant) -> Timer {
        let id = None;
        Timer { id, when }
    }

    /// Registers this timer in the reactor, if not already registered.
    ///
    /// A waker associated with the current context will be stored and woken when the timer fires.
    fn register(&mut self, cx: &mut Context<'_>) {
        if self.id.is_none() {
            let mut timers = Reactor::get().timers.lock();

            // If this timer is going to be the earliest one, interrupt the reactor.
            if let Some((first, _)) = timers.keys().next() {
                if self.when < *first {
                    Reactor::get().event.set();
                }
            }

            // Generate a new ID.
            static ID_GENERATOR: AtomicU64 = AtomicU64::new(1);
            let id = ID_GENERATOR.fetch_add(1, Ordering::Relaxed);

            // Insert this timer into the timers map.
            timers.insert((self.when, id), cx.waker().clone());
            self.id = Some(id);
        }
    }

    /// Deregisters this timer from the reactor, if it was registered.
    fn deregister(&mut self) {
        if let Some(id) = self.id {
            Reactor::get().timers.lock().remove(&(self.when, id));
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.deregister();
    }
}

impl Future for Timer {
    type Output = Instant;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Check if this timer has already fired.
        if Instant::now() >= self.when {
            self.deregister();
            Poll::Ready(self.when)
        } else {
            self.register(cx);
            Poll::Pending
        }
    }
}

// ---------- Async I/O ----------

/// Async I/O.
///
/// TODO
#[derive(Debug)]
pub struct Async<T> {
    /// A source registered in the reactor.
    source: Arc<Source>,

    /// The inner I/O handle.
    io: Option<Box<T>>,
}

#[cfg(unix)]
impl<T: AsRawFd> Async<T> {
    /// Converts a non-blocking I/O handle into an async I/O handle.
    ///
    /// TODO: **warning** for unix users: the I/O handle must be compatible with epoll/kqueue!
    /// Most notably, `File`, `Stdin`, `Stdout`, `Stderr` will **not** work.
    pub fn new(io: T) -> io::Result<Async<T>> {
        use nix::fcntl::{fcntl, FcntlArg, OFlag};

        // Put the I/O handle in non-blocking mode.
        let flags = fcntl(io.as_raw_fd(), FcntlArg::F_GETFL).map_err(io_err)?;
        let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
        fcntl(io.as_raw_fd(), FcntlArg::F_SETFL(flags)).map_err(io_err)?;

        // Register the I/O handle in the reactor.
        Ok(Async {
            source: Reactor::get().register(io.as_raw_fd())?,
            io: Some(Box::new(io)),
        })
    }
}

#[cfg(unix)]
impl<T: AsRawFd> AsRawFd for Async<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.source.raw
    }
}

/// Converts a `nix::Error` into `std::io::Error`.
#[cfg(unix)]
fn io_err(err: nix::Error) -> io::Error {
    match err {
        nix::Error::Sys(code) => code.into(),
        err => io::Error::new(io::ErrorKind::Other, Box::new(err)),
    }
}

#[cfg(windows)]
impl<T: AsRawSocket> Async<T> {
    /// Converts a non-blocking I/O handle into an async I/O handle.
    pub fn new(io: T) -> io::Result<Async<T>> {
        // Put the I/O handle in non-blocking mode.
        let socket = unsafe { Socket::from_raw_socket(io.as_raw_socket()) };
        mem::ManuallyDrop::new(socket).set_nonblocking(true)?;

        // Register the I/O handle in the reactor.
        Ok(Async {
            source: Reactor::get().register(io.as_raw_socket())?,
            io: Some(Box::new(io)),
        })
    }
}

#[cfg(windows)]
impl<T: AsRawSocket> AsRawSocket for Async<T> {
    fn as_raw_socket(&self) -> RawSocket {
        self.source.raw
    }
}

impl<T> Async<T> {
    /// Gets a reference to the inner I/O handle.
    ///
    /// TODO
    pub fn get_ref(&self) -> &T {
        self.io.as_ref().unwrap()
    }

    /// Gets a mutable reference to the inner I/O handle.
    ///
    /// TODO
    pub fn get_mut(&mut self) -> &mut T {
        self.io.as_mut().unwrap()
    }

    /// Extracts the inner non-blocking I/O handle.
    ///
    /// TODO
    pub fn into_inner(mut self) -> io::Result<T> {
        let io = *self.io.take().unwrap();
        Reactor::get().deregister(&self.source)?;
        Ok(io)
    }

    /// TODO
    pub async fn with<R>(&self, op: impl FnMut(&T) -> io::Result<R>) -> io::Result<R> {
        let mut op = op;
        let mut io = self.io.as_ref().unwrap();
        let source = &self.source;
        future::poll_fn(|cx| Self::poll_io(cx, || op(&mut io), source)).await
    }

    /// TODO
    pub async fn with_mut<R>(&mut self, op: impl FnMut(&mut T) -> io::Result<R>) -> io::Result<R> {
        let mut op = op;
        let mut io = self.io.as_mut().unwrap();
        let source = &self.source;
        future::poll_fn(|cx| Self::poll_io(cx, || op(&mut io), source)).await
    }

    /// Attempts a non-blocking I/O operation and registers a waker if it errors with `WouldBlock`.
    fn poll_io<R>(
        cx: &mut Context<'_>,
        mut op: impl FnMut() -> io::Result<R>,
        source: &Source,
    ) -> Poll<io::Result<R>> {
        // Throttle if the current task has done too many I/O operations without yielding.
        futures::ready!(poll_throttle(cx));

        loop {
            let tick = source.tick.load(Ordering::Acquire);

            // Attempt the non-blocking operation.
            match op() {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                res => return Poll::Ready(res),
            }

            // Lock the waker list and retry the non-blocking operation.
            let mut wakers = source.wakers.lock();
            if wakers.iter().any(|w| w.will_wake(cx.waker())) {
                break;
            }
            if source.tick.load(Ordering::Acquire) == tick {
                wakers.push(cx.waker().clone());
                break;
            }
        }
        Poll::Pending
    }
}

impl<T> Drop for Async<T> {
    fn drop(&mut self) {
        if self.io.is_some() {
            // Destructors should not panic.
            let _ = Reactor::get().deregister(&self.source);
            // Drop the I/O handle to close it.
            self.io.take();
        }
    }
}

fn poll_future<T>(cx: &mut Context<'_>, fut: impl Future<Output = T>) -> Poll<T> {
    futures::pin_mut!(fut);
    fut.poll(cx)
}

impl<T: Read> AsyncRead for Async<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.with_mut(|io| io.read(buf)))
    }
}

impl<T> AsyncRead for &Async<T>
where
    for<'a> &'a T: Read,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.with(|io| (&*io).read(buf)))
    }
}

impl<T: Write> AsyncWrite for Async<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.with_mut(|io| io.write(buf)))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        poll_future(cx, self.with_mut(|io| io.flush()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

impl<T> AsyncWrite for &Async<T>
where
    for<'a> &'a T: Write,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.with(|io| (&*io).write(buf)))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        poll_future(cx, self.with(|io| (&*io).flush()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

impl Async<TcpListener> {
    /// Creates a listener bound to the specified address.
    pub fn bind<A: ToString>(addr: A) -> io::Result<Async<TcpListener>> {
        let addr = addr
            .to_string()
            .parse::<SocketAddr>()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        TcpListener::bind(addr).and_then(Async::new)
    }

    /// Accepts a new incoming connection.
    pub async fn accept(&self) -> io::Result<(Async<TcpStream>, SocketAddr)> {
        let (stream, addr) = self.with(|io| io.accept()).await?;
        Ok((Async::new(stream)?, addr))
    }

    /// Returns a stream over incoming connections.
    pub fn incoming(&self) -> impl Stream<Item = io::Result<Async<TcpStream>>> + Send + Unpin + '_ {
        Box::pin(stream::unfold(self, |listener| async move {
            let res = listener.accept().await.map(|(stream, _)| stream);
            Some((res, listener))
        }))
    }
}

impl Async<TcpStream> {
    /// Connects to the specified address.
    pub async fn connect<A: ToString>(addr: A) -> io::Result<Async<TcpStream>> {
        let addr = addr.to_string();
        let addr = Task::blocking(async move {
            addr.to_socket_addrs()?.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "could not resolve the address")
            })
        })
        .await?;

        // Create a socket.
        let domain = if addr.is_ipv6() {
            Domain::ipv6()
        } else {
            Domain::ipv4()
        };
        let socket = Socket::new(domain, Type::stream(), Some(Protocol::tcp()))?;

        // Begin async connect and ignore the inevitable "not yet connected" error.
        socket.set_nonblocking(true)?;
        let _ = socket.connect(&addr.into());
        let stream = Async::new(socket.into_tcp_stream())?;

        // Wait for connect to complete.
        let wait_connect = |mut stream: &TcpStream| match stream.write(&[]) {
            Err(err) if err.kind() == io::ErrorKind::NotConnected => {
                Err(io::Error::new(io::ErrorKind::WouldBlock, ""))
            }
            res => res.map(|_| ()),
        };
        // The stream becomes writable when connected.
        stream.with(|io| wait_connect(io)).await?;

        Ok(stream)
    }

    /// Receives data from the stream without removing it from the buffer.
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.with(|io| io.peek(buf)).await
    }
}

impl Async<UdpSocket> {
    /// Creates a socket bound to the specified address.
    pub fn bind<A: ToString>(addr: A) -> io::Result<Async<UdpSocket>> {
        let addr = addr
            .to_string()
            .parse::<SocketAddr>()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        UdpSocket::bind(addr).and_then(Async::new)
    }

    /// Sends data to the specified address.
    pub async fn send_to<A: Into<SocketAddr>>(&self, buf: &[u8], addr: A) -> io::Result<usize> {
        let addr = addr.into();
        self.with(|io| io.send_to(buf, addr)).await
    }

    /// Sends data to the socket's peer.
    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.with(|io| io.send(buf)).await
    }

    /// Receives data from the socket.
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.with(|io| io.recv_from(buf)).await
    }

    /// Receives data from the socket's peer.
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.with(|io| io.recv(buf)).await
    }

    /// Receives data without removing it from the buffer.
    pub async fn peek_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.with(|io| io.peek_from(buf)).await
    }

    /// Receives data from the socket's peer without removing it from the buffer.
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.with(|io| io.peek(buf)).await
    }
}

#[cfg(unix)]
impl Async<UnixListener> {
    /// Creates a listener bound to the specified path.
    pub async fn bind<P: AsRef<Path>>(path: P) -> io::Result<Async<UnixListener>> {
        let path = path.as_ref().to_owned();
        blocking!(UnixListener::bind(path)).and_then(Async::new)
    }

    /// Accepts a new incoming connection.
    pub async fn accept(&self) -> io::Result<(Async<UnixStream>, UnixSocketAddr)> {
        let (stream, addr) = self.with(|io| io.accept()).await?;
        Ok((Async::new(stream)?, addr))
    }

    /// Returns a stream over incoming connections.
    pub fn incoming(
        &self,
    ) -> impl Stream<Item = io::Result<Async<UnixStream>>> + Send + Unpin + '_ {
        Box::pin(stream::unfold(self, |listener| async move {
            let res = listener.accept().await.map(|(stream, _)| stream);
            Some((res, listener))
        }))
    }
}

#[cfg(unix)]
impl Async<UnixStream> {
    /// Connects to the specified path.
    pub async fn connect<P: AsRef<Path>>(path: P) -> io::Result<Async<UnixStream>> {
        let path = path.as_ref().to_owned();
        blocking!(UnixStream::connect(path)).and_then(Async::new)
    }

    /// Creates an unnamed pair of connected streams.
    pub fn pair() -> io::Result<(Async<UnixStream>, Async<UnixStream>)> {
        let (stream1, stream2) = UnixStream::pair()?;
        Ok((Async::new(stream1)?, Async::new(stream2)?))
    }
}

#[cfg(unix)]
impl Async<UnixDatagram> {
    /// Creates a socket bound to the specified path.
    pub async fn bind<P: AsRef<Path>>(path: P) -> io::Result<Async<UnixDatagram>> {
        let path = path.as_ref().to_owned();
        blocking!(UnixDatagram::bind(path)).and_then(Async::new)
    }

    /// Creates a socket not bound to any address.
    pub fn unbound() -> io::Result<Async<UnixDatagram>> {
        UnixDatagram::unbound().and_then(Async::new)
    }

    /// Creates an unnamed pair of connected sockets.
    pub fn pair() -> io::Result<(Async<UnixDatagram>, Async<UnixDatagram>)> {
        let (socket1, socket2) = UnixDatagram::pair()?;
        Ok((Async::new(socket1)?, Async::new(socket2)?))
    }

    /// Sends data to the specified address.
    pub async fn send_to<P: AsRef<Path>>(&self, buf: &[u8], path: P) -> io::Result<usize> {
        self.with(|io| io.send_to(buf, &path)).await
    }

    /// Sends data to the socket's peer.
    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.with(|io| io.send(buf)).await
    }

    /// Receives data from the socket.
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, UnixSocketAddr)> {
        self.with(|io| io.recv_from(buf)).await
    }

    /// Receives data from the socket's peer.
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.with(|io| io.recv(buf)).await
    }
}

// ---------- Self-pipe ----------

/// A boolean flag that is set whenever a thread-local task is woken by another thread.
///
/// Every time this flag's value is changed, an I/O event is triggered.
#[derive(Clone)]
struct IoEvent {
    pipe: Arc<SelfPipe>,
}

impl IoEvent {
    fn create() -> io::Result<IoEvent> {
        Ok(IoEvent {
            pipe: Arc::new(SelfPipe::create()?),
        })
    }

    fn set(&self) {
        self.pipe.set();
    }

    fn clear(&self) -> bool {
        self.pipe.clear()
    }

    async fn ready(&self) {
        self.pipe.ready().await;
    }
}

/// A boolean flag that triggers I/O events whenever changed.
///
/// https://cr.yp.to/docs/selfpipe.html
struct SelfPipe {
    flag: AtomicBool,
    writer: Socket,
    reader: Async<Socket>,
}

impl SelfPipe {
    /// Creates a self-pipe.
    fn create() -> io::Result<SelfPipe> {
        let (writer, reader) = pipe()?;
        writer.set_send_buffer_size(1)?;
        reader.set_recv_buffer_size(1)?;
        Ok(SelfPipe {
            flag: AtomicBool::new(false),
            writer,
            reader: Async::new(reader)?,
        })
    }

    /// Sets the flag to `true`.
    // TODO: rename to raise() as in "raise a signal"?
    fn set(&self) {
        // Publish all in-memory changes before setting the flag.
        atomic::fence(Ordering::SeqCst);

        // If the flag is not set...
        if !self.flag.load(Ordering::SeqCst) {
            // If this thread sets it...
            if !self.flag.swap(true, Ordering::SeqCst) {
                // Trigger an I/O event by writing a byte into the sending socket.
                let _ = (&self.writer).write(&[1]);
                let _ = (&self.writer).flush();
            }
        }
    }

    /// Sets the flag to `false`.
    fn clear(&self) -> bool {
        // Read all available bytes from the receiving socket.
        while self.reader.get_ref().read(&mut [0; 64]).is_ok() {}
        let value = self.flag.swap(false, Ordering::SeqCst);

        // Publish all in-memory changes after clearing the flag.
        atomic::fence(Ordering::SeqCst);
        value
    }

    /// Waits until the flag is changed.
    ///
    /// Note that this method may spuriously report changes when they didn't really happen.
    async fn ready(&self) {
        self.reader
            .with(|_| match self.flag.load(Ordering::SeqCst) {
                true => Ok(()),
                false => Err(io::Error::new(io::ErrorKind::WouldBlock, "")),
            })
            .await
            .expect("failure while waiting on a self-pipe");
    }
}

#[cfg(unix)]
fn pipe() -> io::Result<(Socket, Socket)> {
    let (sock1, sock2) = Socket::pair(Domain::unix(), Type::stream(), None)?;
    sock1.set_nonblocking(true)?;
    sock2.set_nonblocking(true)?;
    Ok((sock1, sock2))
}

#[cfg(windows)]
fn pipe() -> io::Result<(Socket, Socket)> {
    // TODO The only portable way of manually triggering I/O events is to create a socket and
    // send/receive dummy data on it. This pattern is also known as "the self-pipe trick".
    // See the links below for more information.
    //
    // https://github.com/python-trio/trio/blob/master/trio/_core/_wakeup_socketpair.py
    // https://stackoverflow.com/questions/24933411/how-to-emulate-socket-socketpair-on-windows
    // https://gist.github.com/geertj/4325783

    // Create a temporary listener.
    let listener = Socket::new(Domain::ipv4(), Type::stream(), None)?;
    listener.bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())?;
    listener.listen(1)?;

    // First socket: start connecting to the listener.
    let sock1 = Socket::new(Domain::ipv4(), Type::stream(), None)?;
    sock1.set_nonblocking(true)?;
    let _ = sock1.set_nodelay(true)?;
    let _ = sock1.connect(&listener.local_addr()?);

    // Second socket: accept a connection from the listener.
    let (sock2, _) = listener.accept()?;
    sock2.set_nonblocking(true)?;
    let _ = sock2.set_nodelay(true)?;

    Ok((sock1, sock2))
}

// ---------- Blocking executor ----------

/// A thread pool for blocking tasks.
struct BlockingExecutor {
    state: Mutex<State>,
    cvar: Condvar,
}

struct State {
    /// Number of sleeping threads in the pool.
    idle_count: usize,
    /// Total number of thread in the pool.
    thread_count: usize,
    /// Runnable blocking tasks.
    queue: VecDeque<Runnable>,
}

impl BlockingExecutor {
    fn get() -> &'static BlockingExecutor {
        static EXECUTOR: Lazy<BlockingExecutor> = Lazy::new(|| BlockingExecutor {
            state: Mutex::new(State {
                idle_count: 0,
                thread_count: 0,
                queue: VecDeque::new(),
            }),
            cvar: Condvar::new(),
        });
        &EXECUTOR
    }

    /// Spawns a future onto this executor.
    ///
    /// Returns a `Task` handle for the spawned task.
    fn spawn<T: Send + 'static>(
        &'static self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        // Create a task, schedule it, and return its `Task` handle.
        let (runnable, handle) = async_task::spawn(future, move |r| self.schedule(r), ());
        runnable.schedule();
        Task(Some(handle))
    }

    /// Runs the main loop on the current thread.
    fn main_loop(&'static self) {
        let mut state = self.state.lock().unwrap();
        loop {
            state.idle_count -= 1;

            // Run tasks in the queue.
            while let Some(runnable) = state.queue.pop_front() {
                self.grow_pool(state);
                let _ = panic::catch_unwind(|| runnable.run());
                state = self.state.lock().unwrap();
            }

            // Put the thread to sleep until another task is scheduled.
            state.idle_count += 1;
            let timeout = Duration::from_millis(500);
            let (s, res) = self.cvar.wait_timeout(state, timeout).unwrap();
            state = s;

            if res.timed_out() && state.queue.is_empty() {
                // If there are no tasks after a while, stop this thread.
                state.idle_count -= 1;
                state.thread_count -= 1;
                break;
            }
        }
    }

    /// Schedules a runnable task for execution.
    fn schedule(&'static self, runnable: Runnable) {
        let mut state = self.state.lock().unwrap();
        state.queue.push_back(runnable);
        // Notify a sleeping thread and spawn more threads if needed.
        self.cvar.notify_one();
        self.grow_pool(state);
    }

    /// Spawns more blocking threads if the pool is overloaded with work.
    fn grow_pool(&'static self, mut state: MutexGuard<'static, State>) {
        // If runnable tasks greatly outnumber idle threads and there aren't too many threads
        // already, then be aggressive: wake all idle threads and spawn one more thread.
        while state.queue.len() > state.idle_count * 5 && state.thread_count < 500 {
            state.idle_count += 1;
            state.thread_count += 1;
            self.cvar.notify_all();
            thread::spawn(move || self.main_loop());
        }
    }
}

/// Spawns blocking code onto a thread.
#[macro_export]
macro_rules! blocking {
    ($($expr:tt)*) => {
        $crate::Task::blocking(async move { $($expr)* }).await
    };
}

/// Creates an iterator that runs on a thread.
pub fn iter<T: Send + 'static>(
    iter: impl Iterator<Item = T> + Send + 'static,
) -> impl Stream<Item = T> + Send + Unpin + 'static {
    enum State<T, I> {
        Idle(Option<I>),
        Busy(piper::Receiver<T>, Task<I>),
    }

    impl<T, I> Unpin for State<T, I> {}

    impl<T: Send + 'static, I: Iterator<Item = T> + Send + 'static> Stream for State<T, I> {
        type Item = T;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
            // Throttle if the current task has done too many I/O operations without yielding.
            futures::ready!(poll_throttle(cx));

            match &mut *self {
                State::Idle(iter) => {
                    let mut iter = iter.take().unwrap();
                    let (sender, receiver) = piper::chan(8 * 1024);
                    let task = Task::blocking(async move {
                        for item in &mut iter {
                            sender.send(item).await;
                        }
                        iter
                    });
                    *self = State::Busy(receiver, task);
                    self.poll_next(cx)
                }
                State::Busy(receiver, task) => {
                    let opt = futures::ready!(Pin::new(receiver).poll_next(cx));
                    if opt.is_none() {
                        // At the end of stream, retrieve the iterator back.
                        let iter = futures::ready!(Pin::new(task).poll(cx));
                        *self = State::Idle(Some(iter));
                    }
                    Poll::Ready(opt)
                }
            }
        }
    }

    State::Idle(Some(iter))
}

/// Creates a reader that runs on a thread.
pub fn reader(reader: impl Read + Send + 'static) -> impl AsyncRead + Send + Unpin + 'static {
    enum State<T> {
        Idle(Option<T>),
        Busy(piper::Reader, Task<(io::Result<()>, T)>),
    }

    impl<T: AsyncRead + Send + Unpin + 'static> AsyncRead for State<T> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            // Throttle if the current task has done too many I/O operations without yielding.
            futures::ready!(poll_throttle(cx));

            match &mut *self {
                State::Idle(io) => {
                    let mut io = io.take().unwrap();
                    let (reader, mut writer) = piper::pipe(8 * 1024 * 1024); // 8 MB
                    let task = Task::blocking(async move {
                        let res = futures::io::copy(&mut io, &mut writer).await;
                        (res.map(drop), io)
                    });
                    *self = State::Busy(reader, task);
                    self.poll_read(cx, buf)
                }
                State::Busy(reader, task) => {
                    let n = futures::ready!(Pin::new(reader).poll_read(cx, buf))?;
                    if n == 0 {
                        // At the end of stream, retrieve the reader back.
                        let (res, io) = futures::ready!(Pin::new(task).poll(cx));
                        *self = State::Idle(Some(io));
                        res?;
                    }
                    Poll::Ready(Ok(n))
                }
            }
        }
    }

    let io = Box::pin(AllowStdIo::new(reader));
    State::Idle(Some(io))
}

/// Creates a writer that runs on a thread.
///
/// Make sure to flush before dropping the writer.
///
/// TODO
pub fn writer(writer: impl Write + Send + 'static) -> impl AsyncWrite + Send + Unpin + 'static {
    enum State<T> {
        Idle(Option<T>),
        Busy(Option<piper::Writer>, Task<(io::Result<()>, T)>),
    }

    impl<T: AsyncWrite + Send + Unpin + 'static> State<T> {
        fn start(&mut self) {
            if let State::Idle(io) = self {
                let mut io = io.take().unwrap();
                let (reader, writer) = piper::pipe(8 * 1024 * 1024); // 8 MB
                let task = Task::blocking(async move {
                    match futures::io::copy(reader, &mut io).await {
                        Ok(_) => (io.flush().await, io),
                        Err(err) => (Err(err), io),
                    }
                });
                *self = State::Busy(Some(writer), task);
            }
        }
    }

    impl<T: AsyncWrite + Send + Unpin + 'static> AsyncWrite for State<T> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            // Throttle if the current task has done too many I/O operations without yielding.
            futures::ready!(poll_throttle(cx));

            loop {
                match &mut *self {
                    State::Idle(None) => return Poll::Ready(Ok(0)),
                    State::Idle(Some(_)) => self.start(),
                    State::Busy(None, task) => {
                        // The writing end of the pipe is closed, so await the task.
                        let (res, io) = futures::ready!(Pin::new(task).poll(cx));
                        *self = State::Idle(Some(io));
                        res?;
                    }
                    State::Busy(Some(writer), _) => return Pin::new(writer).poll_write(cx, buf),
                }
            }
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            // Throttle if the current task has done too many I/O operations without yielding.
            futures::ready!(poll_throttle(cx));

            loop {
                match &mut *self {
                    State::Idle(None) => return Poll::Ready(Ok(())),
                    State::Idle(Some(_)) => self.start(),
                    State::Busy(writer, task) => {
                        // Close the writing end of the pipe and await the task.
                        writer.take();
                        let (res, io) = futures::ready!(Pin::new(task).poll(cx));
                        *self = State::Idle(Some(io));
                        return Poll::Ready(res);
                    }
                }
            }
        }

        fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            // Flush and then drop the I/O handle.
            futures::ready!(Pin::new(&mut *self).poll_flush(cx))?;
            *self = State::Idle(None);
            Poll::Ready(Ok(()))
        }
    }

    let io = AllowStdIo::new(writer);
    State::Idle(Some(io))
}

// ---------- epoll (Linux, Android) ----------

#[cfg(any(target_os = "linux", target_os = "android"))]
mod sys {
    use std::convert::TryInto;
    use std::io;
    use std::os::unix::io::RawFd;
    use std::time::Duration;

    use nix::sys::epoll::{
        epoll_create1, epoll_ctl, epoll_wait, EpollCreateFlags, EpollEvent, EpollFlags, EpollOp,
    };

    use super::io_err;

    pub struct Reactor(RawFd);
    impl Reactor {
        pub fn create() -> io::Result<Reactor> {
            let epoll_fd = epoll_create1(EpollCreateFlags::EPOLL_CLOEXEC).map_err(io_err)?;
            Ok(Reactor(epoll_fd))
        }
        pub fn register(&self, fd: RawFd, key: usize) -> io::Result<()> {
            let ev = &mut EpollEvent::new(flags(), key as u64);
            epoll_ctl(self.0, EpollOp::EpollCtlAdd, fd, Some(ev)).map_err(io_err)
        }
        pub fn reregister(&self, _raw: RawFd, _key: usize) -> io::Result<()> {
            Ok(())
        }
        pub fn deregister(&self, fd: RawFd) -> io::Result<()> {
            epoll_ctl(self.0, EpollOp::EpollCtlDel, fd, None).map_err(io_err)
        }
        pub fn wait(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            let timeout_ms = timeout
                .and_then(|t| t.as_millis().try_into().ok())
                .unwrap_or(-1);
            events.len = epoll_wait(self.0, &mut events.list, timeout_ms).map_err(io_err)?;
            Ok(events.len)
        }
    }
    fn flags() -> EpollFlags {
        EpollFlags::EPOLLET | EpollFlags::EPOLLIN | EpollFlags::EPOLLOUT | EpollFlags::EPOLLRDHUP
    }

    pub struct Events {
        list: Box<[EpollEvent]>,
        len: usize,
    }
    impl Events {
        pub fn new() -> Events {
            let list = vec![EpollEvent::empty(); 1000].into_boxed_slice();
            let len = 0;
            Events { list, len }
        }
        pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
            self.list[..self.len].iter().map(|ev| ev.data() as usize)
        }
    }
}

// ---------- kqueue (macOS, iOS, FreeBSD, NetBSD, OpenBSD, DragonFly BSD) ----------

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
))]
mod sys {
    use std::convert::TryInto;
    use std::io;
    use std::os::unix::io::RawFd;
    use std::time::Duration;

    use nix::errno::Errno;
    use nix::fcntl::{fcntl, FcntlArg, FdFlag};
    use nix::libc;
    use nix::sys::event::{kevent_ts, kqueue, EventFilter, EventFlag, FilterFlag, KEvent};

    use super::io_err;

    pub struct Reactor(RawFd);
    impl Reactor {
        pub fn create() -> io::Result<Reactor> {
            let fd = kqueue().map_err(io_err)?;
            fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io_err)?;
            Ok(Reactor(fd))
        }
        pub fn register(&self, fd: RawFd, key: usize) -> io::Result<()> {
            let flags = EventFlag::EV_CLEAR | EventFlag::EV_RECEIPT | EventFlag::EV_ADD;
            let udata = key as _;
            let changelist = [
                KEvent::new(fd as _, EventFilter::EVFILT_WRITE, flags, FFLAGS, 0, udata),
                KEvent::new(fd as _, EventFilter::EVFILT_READ, flags, FFLAGS, 0, udata),
            ];
            let mut eventlist = changelist.clone();
            kevent_ts(self.0, &changelist, &mut eventlist, None).map_err(io_err)?;
            for ev in &eventlist {
                // See https://github.com/tokio-rs/mio/issues/582
                let (flags, data) = (ev.flags(), ev.data());
                if flags.contains(EventFlag::EV_ERROR) && data != 0 && data != Errno::EPIPE as _ {
                    return Err(io::Error::from_raw_os_error(data as _));
                }
            }
            Ok(())
        }
        pub fn reregister(&self, _fd: RawFd, _key: usize) -> io::Result<()> {
            Ok(())
        }
        pub fn deregister(&self, fd: RawFd) -> io::Result<()> {
            let flags = EventFlag::EV_RECEIPT | EventFlag::EV_DELETE;
            let changelist = [
                KEvent::new(fd as _, EventFilter::EVFILT_WRITE, flags, FFLAGS, 0, 0),
                KEvent::new(fd as _, EventFilter::EVFILT_READ, flags, FFLAGS, 0, 0),
            ];
            let mut eventlist = changelist.clone();
            kevent_ts(self.0, &changelist, &mut eventlist, None).map_err(io_err)?;
            for ev in &eventlist {
                let (flags, data) = (ev.flags(), ev.data());
                if flags.contains(EventFlag::EV_ERROR) && data != 0 {
                    return Err(io::Error::from_raw_os_error(data as _));
                }
            }
            Ok(())
        }
        pub fn wait(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            let timeout_ms: Option<usize> = timeout.and_then(|t| t.as_millis().try_into().ok());
            let timeout = timeout_ms.map(|ms| libc::timespec {
                tv_sec: (ms / 1000) as libc::time_t,
                tv_nsec: ((ms % 1000) * 1_000_000) as libc::c_long,
            });
            events.len = kevent_ts(self.0, &[], &mut events.list, timeout).map_err(io_err)?;
            Ok(events.len)
        }
    }
    const FFLAGS: FilterFlag = FilterFlag::empty();

    pub struct Events {
        list: Box<[KEvent]>,
        len: usize,
    }
    impl Events {
        pub fn new() -> Events {
            let flags = EventFlag::empty();
            let event = KEvent::new(0, EventFilter::EVFILT_USER, flags, FFLAGS, 0, 0);
            let list = vec![event; 1000].into_boxed_slice();
            let len = 0;
            Events { list, len }
        }
        pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
            self.list[..self.len].iter().map(|ev| ev.udata() as usize)
        }
    }
}

// ---------- WSAPoll (Windows) ----------

#[cfg(target_os = "windows")]
mod sys {
    use std::io;
    use std::os::windows::io::{AsRawSocket, RawSocket};
    use std::time::Duration;

    use wepoll_binding::{Epoll, EventFlag};

    pub struct Reactor(Epoll);
    impl Reactor {
        pub fn create() -> io::Result<Reactor> {
            Ok(Reactor(Epoll::new()?))
        }
        pub fn register(&self, sock: RawSocket, key: usize) -> io::Result<()> {
            self.0.register(&As(sock), flags(), key as u64)
        }
        pub fn reregister(&self, sock: RawSocket, key: usize) -> io::Result<()> {
            // Ignore errors because a concurrent poll can reregister the handle at any point.
            let _ = self.0.reregister(&As(sock), flags(), key as u64);
            Ok(())
        }
        pub fn deregister(&self, sock: RawSocket) -> io::Result<()> {
            // Ignore errors because an event can deregister the handle at any point.
            let _ = self.0.deregister(&As(sock));
            Ok(())
        }
        pub fn wait(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            events.0.clear();
            self.0.poll(&mut events.0, timeout)
        }
    }
    struct As(RawSocket);
    impl AsRawSocket for As {
        fn as_raw_socket(&self) -> RawSocket {
            self.0
        }
    }
    fn flags() -> EventFlag {
        EventFlag::ONESHOT | EventFlag::IN | EventFlag::OUT | EventFlag::RDHUP
    }

    pub struct Events(wepoll_binding::Events);
    impl Events {
        pub fn new() -> Events {
            Events(wepoll_binding::Events::with_capacity(1000))
        }
        pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
            self.0.iter().map(|ev| ev.data() as usize)
        }
    }
}

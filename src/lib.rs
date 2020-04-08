//! A very smol and fast async runtime.

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
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Debug;
use std::io::{self, Read, Write};
use std::mem;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::panic;
use std::pin::Pin;
use std::sync::atomic::{self, AtomicBool, Ordering};
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

// ----- Task -----

/// A runnable future, ready for execution.
type Runnable = async_task::Task<()>;

/// A spawned future.
#[must_use = "tasks are canceled when dropped, use `.detach()` to run in the background"]
#[derive(Debug)]
pub struct Task<T>(Option<async_task::JoinHandle<T, ()>>);

impl<T: Send + 'static> Task<T> {
    /// Spawns a global future.
    ///
    /// This future is allowed to be stolen by another executor.
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        GLOBAL_EXECUTOR.spawn(future)
    }

    /// Spawns a future onto a thread where blocking is allowed.
    pub fn blocking(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        BLOCKING_POOL.spawn(future)
    }
}

impl<T: 'static> Task<T> {
    /// Spawns a future onto the current executor. TODO
    ///
    /// Panics if not called within an executor.
    pub fn local(future: impl Future<Output = T> + 'static) -> Task<T> {
        LOCAL_EXECUTOR.with(|ex| ex.spawn(future))
    }
}

impl Task<()> {
    /// Detaches the task to keep running in the background.
    pub fn detach(mut self) {
        self.0.take().unwrap();
    }
}

impl<T, E> Task<Result<T, E>>
where
    T: Send + 'static,
    E: Debug + Send + 'static,
{
    /// Spawns another task that unwraps the result on the same thread.
    pub fn unwrap(self) -> Task<T> {
        Task::spawn(async { self.await.unwrap() })
    }

    /// Spawns another task that unwraps the result on the same thread.
    pub fn expect(self, msg: &str) -> Task<T> {
        let msg = msg.to_owned();
        Task::spawn(async move { self.await.expect(&msg) })
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if let Some(t) = &self.0 {
            t.cancel();
        }
    }
}

impl<T> Future for Task<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0.as_mut().unwrap()).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(output) => Poll::Ready(output.expect("task failed")),
        }
    }
}

/// Blocks on a single future.
pub fn block_on<T>(future: impl Future<Output = T>) -> T {
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

/// Executes all futures until the main one completes.
pub fn run<T>(future: impl Future<Output = T>) -> T {
    let worker = GLOBAL_EXECUTOR.worker();

    let flag = Arc::new(SelfPipe::create().expect("cannot create a self-pipe"));
    let flag2 = flag.clone();
    let waker = async_task::waker_fn(move || flag2.set());
    let cx = &mut Context::from_waker(&waker);
    futures::pin_mut!(future);

    loop {
        flag.clear();
        match future.as_mut().poll(cx) {
            Poll::Ready(val) => return val,
            Poll::Pending => {}
        }

        while !flag.get() {
            let more_local = LOCAL_EXECUTOR.with(|ex| ex.execute());
            let more_worker = WORKER.set(&worker, || worker.execute());

            if more_local || more_worker {
                REACTOR.poll_quick().expect("failure while polling I/O");
            } else {
                let lock = REACTOR.lock();
                let ready = flag.ready();
                futures::pin_mut!(lock);
                futures::pin_mut!(ready);

                // Block until either the reactor is locked or the flag is set.
                if let Either::Left((mut poller, _)) = block_on(future::select(lock, ready)) {
                    if !flag.get() {
                        poller.poll().expect("failure while polling I/O");
                    }
                }
            }
        }
    }
}

scoped_thread_local!(static BUDGET: Cell<u32>);

/// Runs a task and returns `true` if it was throttled.
fn use_throttle(run: impl FnOnce()) -> bool {
    let b = Cell::new(200);
    BUDGET.set(&b, run);
    b.get() == 0
}

#[inline]
fn poll_throttle(cx: &mut Context<'_>) -> Poll<()> {
    if BUDGET.is_set() && BUDGET.with(|b| b.replace(b.get().saturating_sub(1))) == 0 {
        cx.waker().wake_by_ref();
        return Poll::Pending;
    }
    Poll::Ready(())
}

// ----- Thread-local executor -----

thread_local! {
    /// Holds a queue of thread-local tasks.
    static LOCAL_EXECUTOR: Lazy<LocalExecutor> = Lazy::new(|| LocalExecutor::new());
}

/// A queue of thread-local tasks.
struct LocalExecutor {
    id: ThreadId,
    queue: RefCell<VecDeque<Runnable>>,
    remote: Arc<SegQueue<Runnable>>,
}

impl LocalExecutor {
    fn new() -> LocalExecutor {
        LocalExecutor {
            id: thread::current().id(),
            queue: RefCell::new(VecDeque::new()),
            remote: Arc::new(SegQueue::new()),
        }
    }

    fn spawn<T: 'static>(&self, future: impl Future<Output = T> + 'static) -> Task<T> {
        let id = self.id;
        let remote = self.remote.clone();

        let schedule = move |runnable| {
            LOCAL_EXECUTOR.with(|ex| {
                if ex.id == id {
                    // If scheduling from the original thread, push into the main queue.
                    ex.queue.borrow_mut().push_back(runnable);
                } else {
                    // If scheduling from a remote thread, push into the remote queue.
                    remote.push(runnable);
                    // The original thread may be currently polling so let's interrupt it.
                    INTERRUPT.set();
                }
            });
        };

        let (runnable, handle) = async_task::spawn_local(future, schedule, ());
        runnable.schedule();
        Task(Some(handle))
    }

    /// Performs some work and returns `true` if there is more work to do.
    fn execute(&self) -> bool {
        for _ in 0..100 {
            match self.pop() {
                None => return false,
                Some(runnable) => {
                    use_throttle(|| runnable.run());
                }
            }
        }
        self.fetch();
        true
    }

    /// Pops the next runnable to run.
    fn pop(&self) -> Option<Runnable> {
        if let Some(r) = self.queue.borrow_mut().pop_front() {
            return Some(r);
        }
        self.fetch();
        self.queue.borrow_mut().pop_front()
    }

    /// TODO Moves all tasks from the remote queue into the main queue.
    fn fetch(&self) {
        REACTOR.poll_quick().expect("failure while polling I/O");

        let mut queue = self.queue.borrow_mut();
        while let Ok(r) = self.remote.pop() {
            queue.push_back(r);
        }
    }
}

// ----- Global work-stealing executor -----

/// Holds the global task queue.
static GLOBAL_EXECUTOR: Lazy<GlobalExecutor> = Lazy::new(|| GlobalExecutor::new());

/// Holds the global task queue and registered workers.
struct GlobalExecutor {
    injector: deque::Injector<Runnable>,
    stealers: ShardedLock<HashMap<ThreadId, deque::Stealer<Runnable>>>,
}

impl GlobalExecutor {
    fn new() -> GlobalExecutor {
        GlobalExecutor {
            injector: deque::Injector::new(),
            stealers: ShardedLock::new(HashMap::new()),
        }
    }

    fn spawn<T: Send + 'static>(
        &'static self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        let schedule = move |runnable| {
            if WORKER.is_set() {
                WORKER.with(|w| w.push(runnable));
            } else {
                self.injector.push(runnable);
                // A task has been pushed into the global queue - we need to interrupt.
                INTERRUPT.set();
            }
        };

        let (runnable, handle) = async_task::spawn(future, schedule, ());
        runnable.schedule();
        Task(Some(handle))
    }

    fn worker(&self) -> Worker {
        // Register a new worker.
        let id = thread::current().id();
        match self.stealers.write().unwrap().entry(id) {
            Entry::Occupied(_) => panic!("recursive `run()`"),
            Entry::Vacant(vacant) => {
                let slot = Cell::new(None);
                let worker = deque::Worker::new_fifo();
                vacant.insert(worker.stealer());
                Worker { id, slot, worker }
            }
        }
    }
}

// Holds a queue of some stealable global tasks.
//
// Each thread has its own queue in order to reduce contention on the global task queue.
scoped_thread_local!(static WORKER: Worker);

// TODO: explain that whenever something is pushed into worker/injector, we need to interrupt

/// A queue of some stealable global tasks.
///
/// Each thread has its own worker in order to reduce contention on the global task queue.
struct Worker {
    id: ThreadId,
    slot: Cell<Option<Runnable>>,
    worker: deque::Worker<Runnable>,
}

impl Worker {
    /// Performs some work and returns `true` if there is more work to do.
    fn execute(&self) -> bool {
        let mut step = 0;
        for _ in 0..100 {
            let runnable = match self.pop() {
                None => return false,
                Some(r) => r,
            };

            step += 1;
            if use_throttle(|| runnable.run()) || step >= 10 {
                step = 0;
                self.push(None);
            }
        }

        self.fetch();
        true
    }

    fn push(&self, runnable: impl Into<Option<Runnable>>) {
        match self.slot.replace(runnable.into()) {
            None => {}
            Some(runnable) => {
                self.worker.push(runnable);
                // A task has been pushed into the local queue - we need to interrupt.
                INTERRUPT.set();
            }
        }
    }

    fn pop(&self) -> Option<Runnable> {
        // Take a task from the slot or the local queue.
        if let Some(r) = self.slot.take().or_else(|| self.worker.pop()) {
            return Some(r);
        }

        // Try stealing from the global queue.
        if let Some(r) = retry(|| GLOBAL_EXECUTOR.injector.steal_batch_and_pop(&self.worker)) {
            // A task may have been pushed into the local queue - we need to interrupt.
            INTERRUPT.set();
            return Some(r);
        }

        // Poll the reactor and check if any new tasks were scheduled.
        REACTOR.poll_quick().expect("failure while polling I/O");
        if let Some(r) = self.slot.take().or_else(|| self.worker.pop()) {
            return Some(r);
        }

        // Try stealing from other workers.
        let stealers = GLOBAL_EXECUTOR.stealers.read().unwrap();
        if let Some(r) = retry(|| {
            stealers
                .values()
                .map(|s| s.steal_batch_and_pop(&self.worker))
                .collect()
        }) {
            // A task may have been pushed into the local queue - we need to interrupt.
            INTERRUPT.set();
            return Some(r);
        }
        None
    }

    fn fetch(&self) {
        if let Some(()) = retry(|| GLOBAL_EXECUTOR.injector.steal_batch(&self.worker)) {
            // A task has been pushed into the local queue - we need to interrupt.
            INTERRUPT.set();
        }
        REACTOR.poll_quick().expect("failure while polling I/O");
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        GLOBAL_EXECUTOR.stealers.write().unwrap().remove(&self.id);
        while let Some(r) = self.worker.pop() {
            GLOBAL_EXECUTOR.injector.push(r);
        }
    }
}

fn retry<T>(mut f: impl FnMut() -> deque::Steal<T>) -> Option<T> {
    loop {
        match f() {
            deque::Steal::Success(t) => return Some(t),
            deque::Steal::Empty => return None,
            deque::Steal::Retry => {}
        }
    }
}

// ----- Blocking executor -----

static BLOCKING_POOL: Lazy<BlockingPool> = Lazy::new(|| BlockingPool {
    state: Mutex::new(State {
        idle_count: 0,
        thread_count: 0,
        queue: VecDeque::new(),
    }),
    cvar: Condvar::new(),
});

/// A thread pool for blocking tasks.
struct BlockingPool {
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

impl BlockingPool {
    /// Spawns a blocking task onto the thread pool.
    fn spawn<T: Send + 'static>(
        &'static self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        let (runnable, handle) = async_task::spawn(future, move |r| self.schedule(r), ());
        runnable.schedule();
        Task(Some(handle))
    }

    /// Runs the main loop on the current thread.
    fn execute(&'static self) {
        let mut state = self.state.lock().unwrap();
        loop {
            state.idle_count -= 1;

            // Run tasks in the queue.
            while let Some(runnable) = state.queue.pop_front() {
                self.spawn_more(state);
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
        self.spawn_more(state);
    }

    /// Spawns more blocking threads if the pool is overloaded with work.
    fn spawn_more(&'static self, mut state: MutexGuard<'static, State>) {
        // If runnable tasks greatly outnumber idle threads and there aren't too many threads
        // already, then be aggressive: wake all idle threads and spawn one more thread.
        while state.queue.len() > state.idle_count * 5 && state.thread_count < 500 {
            state.idle_count += 1;
            state.thread_count += 1;
            self.cvar.notify_all();
            thread::spawn(move || self.execute());
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

// ----- Reactor -----

static REACTOR: Lazy<Reactor> = Lazy::new(|| Reactor::create().expect("cannot create a reactor"));

static INTERRUPT: Lazy<SelfPipe> =
    Lazy::new(|| SelfPipe::create().expect("cannot create a self-pipe"));

/// A source of I/O events.
#[derive(Debug)]
struct Source {
    #[cfg(unix)]
    raw: RawFd,

    #[cfg(windows)]
    raw: RawSocket,

    index: usize,

    wakers: piper::Lock<Vec<Waker>>,
}

/// The async I/O and timers driver.
struct Reactor {
    sys: sys::Reactor,
    sources: piper::Lock<Slab<Arc<Source>>>,
    events: piper::Mutex<sys::Events>,
    timers: piper::Lock<BTreeMap<(Instant, usize), Waker>>,
}

impl Reactor {
    /// Creates a new reactor.
    fn create() -> io::Result<Reactor> {
        Ok(Reactor {
            sys: sys::Reactor::create()?,
            sources: piper::Lock::new(Slab::new()),
            events: piper::Mutex::new(sys::Events::new()),
            timers: piper::Lock::new(BTreeMap::new()),
        })
    }

    /// Registers an I/O source in the reactor.
    fn register(
        &self,
        #[cfg(unix)] raw: RawFd,
        #[cfg(windows)] raw: RawSocket,
    ) -> io::Result<Arc<Source>> {
        let mut sources = self.sources.lock();
        let vacant = sources.vacant_entry();
        let source = Arc::new(Source {
            raw,
            index: vacant.key(),
            wakers: piper::Lock::new(Vec::new()),
        });
        self.sys.register(raw, source.index)?;
        Ok(vacant.insert(source).clone())
    }

    /// Deregisters an I/O source from the reactor.
    fn deregister(&self, source: &Source) -> io::Result<()> {
        let mut sources = self.sources.lock();
        sources.remove(source.index);
        self.sys.deregister(source.raw)
    }

    /// Processes ready events without blocking.
    ///
    /// This call provides no guarantees and should only be used for the purpose of optimization.
    fn poll_quick(&self) -> io::Result<()> {
        if let Some(events) = self.events.try_lock() {
            let mut poller = Poller {
                reactor: self,
                events,
            };
            poller.poll_quick()?;
        }
        Ok(())
    }

    /// Locks the reactor for polling.
    async fn lock(&self) -> Poller<'_> {
        Poller {
            reactor: self,
            events: self.events.lock().await,
        }
    }
}

/// Polls the reactor for I/O events and wakes up tasks.
struct Poller<'a> {
    reactor: &'a Reactor,
    events: piper::MutexGuard<'a, sys::Events>,
}

impl Poller<'_> {
    /// Blocks until at least one event is processed.
    fn poll(&mut self) -> io::Result<()> {
        self.poll_internal(true)
    }

    /// Processes ready events without blocking.
    fn poll_quick(&mut self) -> io::Result<()> {
        self.poll_internal(false)
    }

    fn poll_internal(&mut self, block: bool) -> io::Result<()> {
        let next_timer = {
            let now = Instant::now();
            let mut timers = self.reactor.timers.lock();

            // Split timers into ready and pending timers.
            let pending = timers.split_off(&(now, 0));
            let ready = mem::replace(&mut *timers, pending);

            // Wake up tasks waiting on timers.
            for (_, waker) in ready {
                waker.wake();
            }

            // Find when the next timer fires.
            timers.keys().next().map(|(when, _)| *when)
        };

        // If this poll blocks, clear the interrupt flag.
        let timeout = if block && !INTERRUPT.clear() {
            // Calculate the timeout till the first timer fires.
            next_timer.map(|when| when.saturating_duration_since(Instant::now()))
        } else {
            // If this poll doesn't block, the timeout is zero.
            Some(Duration::from_secs(0))
        };

        // Block on I/O events.
        loop {
            match self.reactor.sys.poll(&mut self.events, timeout) {
                Ok(0) => return Ok(()),
                Ok(_) => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }

        // Iterate over sources in the event list.
        let sources = self.reactor.sources.lock();
        for source in self.events.iter().filter_map(|i| sources.get(i)) {
            // I/O events may unregister sources, so we need to re-register.
            self.reactor.sys.reregister(source.raw, source.index)?;

            // Wake up tasks waiting on I/O.
            for w in source.wakers.lock().drain(..) {
                w.wake();
            }
        }
        Ok(())
    }
}

// ----- Timer -----

/// Fires at a certain point in time.
#[derive(Debug)]
pub struct Timer {
    when: Instant,
    inserted: bool,
}

impl Timer {
    /// Fires after the specified duration of time.
    pub fn after(dur: Duration) -> Timer {
        Timer::at(Instant::now() + dur)
    }

    /// Fires at the specified instant in time.
    pub fn at(when: Instant) -> Timer {
        Timer {
            when,
            inserted: false,
        }
    }

    /// Returns an unique identifier for this timer.
    ///
    /// This method assumes the timer is pinned, even though it takes a mutable reference.
    fn key(&mut self) -> (Instant, usize) {
        let address = self as *mut Timer as usize;
        (self.when, address)
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // If this timer is in the timers map, remove it.
        if self.inserted {
            REACTOR.timers.lock().remove(&self.key());
        }
    }
}

impl Future for Timer {
    type Output = Instant;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Check if this timer has already fired.
        if Instant::now() >= self.when {
            if self.inserted {
                REACTOR.timers.lock().remove(&self.key());
                self.inserted = false;
            }
            return Poll::Ready(self.when);
        }

        // Check if this timer has been inserted into the timers map.
        if !self.inserted {
            let mut timers = REACTOR.timers.lock();
            let mut is_earliest = false;
            if let Some((first, _)) = timers.keys().next() {
                if self.when < *first {
                    is_earliest = true;
                }
            }

            // Insert this timer into the timers map.
            let waker = cx.waker().clone();
            timers.insert(self.key(), waker);
            self.inserted = true;

            // If this timer is now the earliest one, interrupt the reactor.
            if is_earliest {
                drop(timers);
                INTERRUPT.set();
            }
        }

        Poll::Pending
    }
}

// ----- Async I/O -----

/// Async I/O.
///
/// TODO: does not work with files!
#[derive(Debug)]
pub struct Async<T> {
    inner: Option<Box<T>>,
    source: Arc<Source>,
}

#[cfg(unix)]
impl<T: AsRawFd> Async<T> {
    /// Converts a non-blocking I/O handle into an async I/O handle.
    pub fn new(inner: T) -> io::Result<Async<T>> {
        use nix::fcntl::{fcntl, FcntlArg, OFlag};

        // Put the I/O handle in non-blocking mode.
        let flags = fcntl(inner.as_raw_fd(), FcntlArg::F_GETFL).map_err(io_err)?;
        let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
        fcntl(inner.as_raw_fd(), FcntlArg::F_SETFL(flags)).map_err(io_err)?;

        // Register the I/O handle in the reactor.
        Ok(Async {
            source: REACTOR.register(inner.as_raw_fd())?,
            inner: Some(Box::new(inner)),
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
    pub fn new(inner: T) -> io::Result<Async<T>> {
        // Put the I/O handle in non-blocking mode.
        let socket = unsafe { Socket::from_raw_socket(inner.as_raw_socket()) };
        mem::ManuallyDrop::new(socket).set_nonblocking(true)?;

        // Register the I/O handle in the reactor.
        Ok(Async {
            source: REACTOR.register(inner.as_raw_socket())?,
            inner: Some(Box::new(inner)),
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
    pub fn get_ref(&self) -> &T {
        self.inner.as_ref().unwrap()
    }

    /// Gets a mutable reference to the inner I/O handle.
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.as_mut().unwrap()
    }

    /// Extracts the inner non-blocking I/O handle.
    pub fn into_inner(mut self) -> io::Result<T> {
        let inner = *self.inner.take().unwrap();
        REACTOR.deregister(&self.source)?;
        Ok(inner)
    }

    pub async fn with<R>(&self, op: impl FnMut(&T) -> io::Result<R>) -> io::Result<R> {
        let mut op = op;
        let mut inner = self.inner.as_ref().unwrap();
        let wakers = &self.source.wakers;
        future::poll_fn(|cx| Self::poll_io(cx, || op(&mut inner), wakers)).await
    }

    pub async fn with_mut<R>(&mut self, op: impl FnMut(&mut T) -> io::Result<R>) -> io::Result<R> {
        let mut op = op;
        let mut inner = self.inner.as_mut().unwrap();
        let wakers = &self.source.wakers;
        future::poll_fn(|cx| Self::poll_io(cx, || op(&mut inner), wakers)).await
    }

    fn poll_io<R>(
        cx: &mut Context<'_>,
        mut op: impl FnMut() -> io::Result<R>,
        wakers: &piper::Lock<Vec<Waker>>,
    ) -> Poll<io::Result<R>> {
        futures::ready!(poll_throttle(cx));

        // Attempt the non-blocking operation.
        match op() {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            res => return Poll::Ready(res),
        }

        // Lock the waker list and retry the non-blocking operation.
        let mut wakers = wakers.lock();
        match op() {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            res => return Poll::Ready(res),
        }

        // If the operation would still block, add the current task to the list.
        if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

impl<T> Drop for Async<T> {
    fn drop(&mut self) {
        if self.inner.is_some() {
            // Destructors should not panic.
            let _ = REACTOR.deregister(&self.source);
            // Drop and close the source.
            self.inner.take();
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
        poll_future(cx, self.with_mut(|inner| inner.read(buf)))
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
        poll_future(cx, self.with(|inner| (&*inner).read(buf)))
    }
}

impl<T: Write> AsyncWrite for Async<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.with_mut(|inner| inner.write(buf)))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        poll_future(cx, self.with_mut(|inner| inner.flush()))
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
        poll_future(cx, self.with(|inner| (&*inner).write(buf)))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        poll_future(cx, self.with(|inner| (&*inner).flush()))
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
        let (stream, addr) = self.with(|inner| inner.accept()).await?;
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
        stream.with(|inner| wait_connect(inner)).await?;

        Ok(stream)
    }

    /// Receives data from the stream without removing it from the buffer.
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.with(|inner| inner.peek(buf)).await
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
        self.with(|inner| inner.send_to(buf, addr)).await
    }

    /// Sends data to the socket's peer.
    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.with(|inner| inner.send(buf)).await
    }

    /// Receives data from the socket.
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.with(|inner| inner.recv_from(buf)).await
    }

    /// Receives data from the socket's peer.
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.with(|inner| inner.recv(buf)).await
    }

    /// Receives data without removing it from the buffer.
    pub async fn peek_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.with(|inner| inner.peek_from(buf)).await
    }

    /// Receives data from the socket's peer without removing it from the buffer.
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.with(|inner| inner.peek(buf)).await
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
        let (stream, addr) = self.with(|inner| inner.accept()).await?;
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
        self.with(|inner| inner.send_to(buf, &path)).await
    }

    /// Sends data to the socket's peer.
    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.with(|inner| inner.send(buf)).await
    }

    /// Receives data from the socket.
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, UnixSocketAddr)> {
        self.with(|inner| inner.recv_from(buf)).await
    }

    /// Receives data from the socket's peer.
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.with(|inner| inner.recv(buf)).await
    }
}

// ----- The self-pipe trick -----

/// A boolean flag that triggers I/O events whenever changed.
struct SelfPipe {
    flag: AtomicBool,
    writer: Socket,
    reader: Async<Socket>,
}

impl SelfPipe {
    /// Creates an I/O flag.
    fn create() -> io::Result<SelfPipe> {
        // The only portable way of manually triggering I/O events is to create a socket and
        // send/receive dummy data on it. This pattern is also known as "the self-pipe trick".
        // See the links below for more information.
        //
        // https://cr.yp.to/docs/selfpipe.html
        // https://github.com/python-trio/trio/blob/master/trio/_core/_wakeup_socketpair.py
        // https://stackoverflow.com/questions/24933411/how-to-emulate-socket-socketpair-on-windows
        // https://gist.github.com/geertj/4325783

        // Create a temporary listener.
        let listener = Socket::new(Domain::ipv4(), Type::stream(), None)?;
        listener.bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())?;
        listener.listen(1)?;

        // First socket: start connecting to the listener.
        let writer = Socket::new(Domain::ipv4(), Type::stream(), None)?;
        writer.set_nonblocking(true)?;
        let _ = writer.connect(&listener.local_addr()?);
        let _ = writer.set_nodelay(true);
        writer.set_send_buffer_size(1)?;

        // Second socket: accept a connection from the listener.
        let (reader, _) = listener.accept()?;
        reader.set_recv_buffer_size(1)?;

        Ok(SelfPipe {
            flag: AtomicBool::new(false),
            writer,
            reader: Async::new(reader)?,
        })
    }

    /// Sets the flag to `true`.
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

    /// Gets the current value of the flag.
    fn get(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Sets the flag to `false`.
    fn clear(&self) -> bool {
        let value = self.flag.swap(false, Ordering::SeqCst);
        if value {
            // Read all available bytes from the receiving socket.
            while self.reader.get_ref().read(&mut [0; 64]).is_ok() {}
        }

        // Publish all in-memory changes after clearing the flag.
        atomic::fence(Ordering::SeqCst);
        value
    }

    /// Waits until the flag is changed.
    ///
    /// Note that this method may spuriously report changes when they didn't really happen.
    async fn ready(&self) {
        self.reader
            .with(|_| match self.get() {
                true => Ok(()),
                false => Err(io::Error::new(io::ErrorKind::WouldBlock, "")),
            })
            .await
            .expect("failure while waiting on an I/O flag")
    }
}

// ----- epoll (Linux, Android) -----

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
        pub fn register(&self, fd: RawFd, index: usize) -> io::Result<()> {
            let ev = &mut EpollEvent::new(flags(), index as u64);
            epoll_ctl(self.0, EpollOp::EpollCtlAdd, fd, Some(ev)).map_err(io_err)
        }
        pub fn reregister(&self, _raw: RawFd, _index: usize) -> io::Result<()> {
            Ok(())
        }
        pub fn deregister(&self, fd: RawFd) -> io::Result<()> {
            epoll_ctl(self.0, EpollOp::EpollCtlDel, fd, None).map_err(io_err)
        }
        pub fn poll(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
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

// ----- kqueue (macOS, iOS, FreeBSD, NetBSD, OpenBSD, DragonFly BSD) -----

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
        pub fn register(&self, fd: RawFd, index: usize) -> io::Result<()> {
            let flags = EventFlag::EV_CLEAR | EventFlag::EV_RECEIPT | EventFlag::EV_ADD;
            let udata = index as _;
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
        pub fn reregister(&self, _fd: RawFd, _index: usize) -> io::Result<()> {
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
        pub fn poll(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            let timeout_ms: Option<usize> = timeout.and_then(|t| t.as_millis().try_into().ok());
            let timeout = timeout_ms.map(|ms| libc::timespec {
                tv_sec: (ms / 1000) as libc::time_t,
                tv_nsec: ((ms % 1000) * 1_000_000) as libc::c_long,
            });
            events.len = kevent_ts(self.0, &[], &mut events.list, timeout).map_err(io_err)?;
            Ok(events.len)
        }
    }
    const FFLAGS: EventFlag = EventFlag::empty();

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

// ----- WSAPoll (Windows) -----

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
        pub fn register(&self, sock: RawSocket, index: usize) -> io::Result<()> {
            self.0.register(&As(sock), flags(), index as u64)
        }
        pub fn reregister(&self, sock: RawSocket, index: usize) -> io::Result<()> {
            // Ignore errors because a concurrent poll can reregister the handle at any point.
            let _ = self.0.reregister(&As(sock), flags(), index as u64);
            Ok(())
        }
        pub fn deregister(&self, sock: RawSocket) -> io::Result<()> {
            // Ignore errors because an event can deregister the handle at any point.
            let _ = self.0.deregister(&As(sock));
            Ok(())
        }
        pub fn poll(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
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

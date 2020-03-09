#![forbid(unsafe_code)]
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
use std::future::Future;
use std::io::{self, Read, Write};
use std::mem;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::panic::catch_unwind;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::{
    os::unix::net::{SocketAddr as UnixSocketAddr, UnixDatagram, UnixListener, UnixStream},
    path::Path,
};

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use crossbeam_queue::SegQueue;
use crossbeam_utils::sync::{Parker, ShardedLock};
use futures_core::stream::Stream;
use futures_io::{AsyncRead, AsyncWrite};
use futures_util::future;
use futures_util::io::AllowStdIo;
use futures_util::lock;
use futures_util::stream;
use io_flag::IoFlag;
use once_cell::sync::Lazy;
use parking_lot::{Condvar, Mutex, MutexGuard};
use slab::Slab;
use socket2::{Domain, Protocol, Socket, Type};

// TODO: fix unwraps
// TODO: catch panics in wake() and Waker::drop()

// ----- Executor -----

static EXECUTOR: Lazy<Executor> = Lazy::new(|| Executor::new());

/// A runnable future, ready for execution.
type Runnable = async_task::Task<()>;

thread_local! {
    static LOCAL: Lazy<Local> = Lazy::new(|| Local::new());
    static PROCESSOR: Lazy<Processor> = Lazy::new(|| Processor::new());
}

/// A queue of thread-local tasks.
struct Local {
    id: ThreadId,
    queue: RefCell<VecDeque<Runnable>>,
    remote: Arc<SegQueue<Runnable>>,
}

impl Local {
    fn new() -> Local {
        Local {
            id: thread::current().id(),
            queue: RefCell::new(VecDeque::new()),
            remote: Arc::new(SegQueue::new()),
        }
    }

    fn spawn<T: 'static>(&self, future: impl Future<Output = T> + 'static) -> Task<T> {
        let id = self.id;
        let remote = self.remote.clone();

        let schedule = move |runnable| {
            LOCAL.with(|local| {
                if local.id == id {
                    // If scheduling from the original thread, push into the main queue.
                    local.queue.borrow_mut().push_back(runnable);
                } else {
                    // If scheduling from a remote thread, push into the remote queue.
                    remote.push(runnable);
                    // The original thread may be currently polling so let's interrupt it.
                    REACTOR.interrupt();
                }
            });
        };

        let (runnable, handle) = async_task::spawn_local(future, schedule, ());
        runnable.schedule();
        Task(Some(handle))
    }

    /// Performs some work and returns `true` if there is more work to do.
    fn tick(&self) -> bool {
        for _ in 0..100 {
            match self.pop() {
                None => return false,
                Some(runnable) => runnable.run(),
            }
        }
        self.drain_remote();
        true
    }

    /// Pops the next runnable to run.
    fn pop(&self) -> Option<Runnable> {
        if let Some(r) = self.queue.borrow_mut().pop_front() {
            return Some(r);
        }
        self.drain_remote();
        self.queue.borrow_mut().pop_front()
    }

    /// Moves all tasks from the remote queue into the main queue.
    fn drain_remote(&self) {
        let mut queue = self.queue.borrow_mut();
        while let Ok(r) = self.remote.pop() {
            queue.push_back(r);
        }
    }
}

struct Processor {
    is_running: Cell<bool>,
    slot: Cell<Option<Runnable>>,
    worker: Worker<Runnable>,
}

impl Processor {
    fn new() -> Processor {
        Processor {
            is_running: Cell::new(false),
            slot: Cell::new(None),
            worker: Worker::new_fifo(),
        }
    }

    /// Performs some work and returns `true` if there is more work to do.
    fn tick(&self) -> bool {
        let more_local = LOCAL.with(|local| local.tick());

        for _ in 0..100 {
            match self.pop() {
                None => return more_local,
                Some(runnable) => {
                    REACTOR.interrupt();
                    runnable.run();
                }
            }
        }

        while EXECUTOR.injector.steal_batch(&self.worker).is_retry() {}
        true
    }

    fn pop(&self) -> Option<Runnable> {
        if let Some(r) = self.slot.take().or_else(|| self.worker.pop()) {
            return Some(r);
        }

        // Then look into the global queue.
        loop {
            match EXECUTOR.injector.steal_batch_and_pop(&self.worker) {
                Steal::Success(r) => return Some(r),
                Steal::Empty => break,
                Steal::Retry => {}
            }
        }

        REACTOR.quick_poll().expect("failure while polling I/O");

        if let Some(r) = self.slot.take().or_else(|| self.worker.pop()) {
            return Some(r);
        }

        let stealers = EXECUTOR.stealers.read().unwrap();
        loop {
            // Try stealing a batch of tasks from each queue.
            match stealers
                .values()
                .map(|s| s.steal_batch_and_pop(&self.worker))
                .collect()
            {
                Steal::Success(r) => return Some(r),
                Steal::Empty => break,
                Steal::Retry => {}
            }
        }
        None
    }
}

struct Executor {
    injector: Injector<Runnable>,
    stealers: ShardedLock<HashMap<ThreadId, Stealer<Runnable>>>,
}

impl Executor {
    fn new() -> Executor {
        Executor {
            injector: Injector::new(),
            stealers: ShardedLock::new(HashMap::new()),
        }
    }

    fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        let schedule = |runnable| {
            PROCESSOR.with(|proc| {
                if proc.is_running.get() {
                    match proc.slot.replace(Some(runnable)) {
                        None => {}
                        Some(runnable) => proc.worker.push(runnable),
                    }
                } else {
                    EXECUTOR.injector.push(runnable);
                }
            });
            REACTOR.interrupt();
        };

        let (runnable, handle) = async_task::spawn(future, schedule, ());
        runnable.schedule();
        Task(Some(handle))
    }

    fn run<T>(&self, future: impl Future<Output = T>) -> T {
        PROCESSOR.with(|proc| {
            // Register a new worker.
            let id = thread::current().id();
            match self.stealers.write().unwrap().entry(id) {
                Entry::Occupied(_) => panic!("recursive `run()`"),
                Entry::Vacant(e) => {
                    e.insert(proc.worker.stealer());
                }
            }
            proc.is_running.set(true);

            // Unregister the worker at the end of this scope.
            scopeguard::defer! {{
                self.stealers.write().unwrap().remove(&id);
                while let Some(r) = proc.worker.pop() {
                    self.injector.push(r);
                }
                proc.is_running.set(false);
            }}

            let waker = Arc::new(
                IoFlag::create()
                    .and_then(Async::nonblocking)
                    .expect("cannot create waker flag"),
            );

            let flag = waker.clone();
            let w = async_task::waker_fn(move || flag.get_ref().set());
            let cx = &mut Context::from_waker(&w);

            pin_utils::pin_mut!(future);

            loop {
                waker.get_ref().clear();
                match future.as_mut().poll(cx) {
                    Poll::Ready(val) => return val,
                    Poll::Pending => {}
                }

                while proc.tick() && !waker.get_ref().get() {
                    REACTOR.quick_poll().expect("failure whikle polling I/O");
                }

                let waker_ready = waker.ready();
                pin_utils::pin_mut!(waker_ready);

                let poller_ready = REACTOR.lock();
                pin_utils::pin_mut!(poller_ready);

                if let future::Either::Right((mut poller, _)) =
                    block_on(future::select(waker_ready, poller_ready))
                {
                    if !waker.get_ref().get() {
                        poller.poll().expect("failure while polling I/O");
                    }
                }

                // TODO: use piper::select! here
                // block_on(piper::select! {
                //     waker.ready() => {}
                //     poller = REACTOR.lock() => poller.poll().expect("failure while polling I/O"),
                // });
            }
        })
    }
}

/// Executes all futures until the main one completes.
pub fn run<T>(future: impl Future<Output = T>) -> T {
    EXECUTOR.run(future)
}

/// A spawned future.
#[must_use = "tasks are canceled when dropped, use `.forget()` to run in the background"]
pub struct Task<T>(Option<async_task::JoinHandle<T, ()>>);

impl<T: Send + 'static> Task<T> {
    /// Spawns a global future.
    ///
    /// This future is allowed to be stolen by another executor.
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        EXECUTOR.spawn(future)
    }

    /// Spawns a future onto a thread where blocking is allowed.
    pub fn blocking(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        THREAD_POOL.spawn(future)
    }
}

impl<T: 'static> Task<T> {
    /// Spawns a future onto the current executor.
    ///
    /// Panics if not called within an executor.
    pub fn local(future: impl Future<Output = T> + 'static) -> Task<T>
    where
        T: 'static,
    {
        LOCAL.with(|local| local.spawn(future))
    }
}

impl Task<()> {
    /// Moves the task into the background.
    pub fn forget(mut self) {
        self.0.take().unwrap();
    }
}

impl<T, E> Task<Result<T, E>>
where
    T: Send + 'static,
    E: Debug + Send + 'static,
{
    /// Spawns another task that unwraps the result.
    pub fn unwrap(self) -> Task<T> {
        Task::spawn(async { self.await.unwrap() })
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

// ----- Blocking thread pool -----

static THREAD_POOL: Lazy<ThreadPool> = Lazy::new(|| ThreadPool {
    state: Mutex::new(State {
        idle_count: 0,
        thread_count: 0,
        queue: VecDeque::new(),
    }),
    cvar: Condvar::new(),
});

struct ThreadPool {
    state: Mutex<State>,
    cvar: Condvar,
}

struct State {
    idle_count: usize,
    thread_count: usize,
    queue: VecDeque<Runnable>,
}

impl ThreadPool {
    fn spawn<T: Send + 'static>(
        &'static self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        let (runnable, handle) = async_task::spawn(future, move |r| self.schedule(r), ());
        runnable.schedule();
        Task(Some(handle))
    }

    fn run(&'static self) {
        let mut state = self.state.lock();
        loop {
            state.idle_count -= 1;

            while let Some(runnable) = state.queue.pop_front() {
                self.spawn_more(state);
                let _ = catch_unwind(|| runnable.run());
                state = self.state.lock();
            }

            state.idle_count += 1;
            let timeout = Duration::from_millis(500);

            if self.cvar.wait_for(&mut state, timeout).timed_out() {
                state.idle_count -= 1;
                state.thread_count -= 1;
                self.spawn_more(state);
                break;
            }
        }
    }

    fn schedule(&'static self, runnable: Runnable) {
        let mut state = self.state.lock();
        state.queue.push_back(runnable);
        self.cvar.notify_one();
        self.spawn_more(state);
    }

    fn spawn_more(&'static self, mut state: MutexGuard<'static, State>) {
        // If runnable tasks greatly outnumber idle threads and there aren't too many threads
        // already, then be aggressive: wake all idle threads and spawn one more thread.
        while state.queue.len() > state.idle_count * 5 && state.thread_count < 500 {
            state.idle_count += 1;
            state.thread_count += 1;
            self.cvar.notify_all();
            thread::spawn(move || self.run());
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

        pin_utils::pin_mut!(future);
        let cx = &mut Context::from_waker(&waker);
        loop {
            match future.as_mut().poll(cx) {
                Poll::Ready(output) => return output,
                Poll::Pending => parker.park(),
            }
        }
    })
}

/// Spawns blocking code onto a thread.
#[macro_export]
macro_rules! blocking {
    ($($expr:tt)*) => {
        $crate::Task::blocking(async move { $($expr)* }).await
    };
}

/// Spawns a blocking iterator onto a thread.
pub fn iter<T>(t: T) -> impl Stream<Item = T::Item> + Send + Unpin + 'static
where
    T: Iterator + Send + 'static,
    T::Item: Send,
{
    let (s, r) = piper::chan(1);
    Task::blocking(async move {
        for item in t {
            s.send(item).await;
        }
    })
    .forget();
    r
}

const PIPE_CAP: usize = 256 * 1024;

/// Spawns a blocking reader onto a thread.
pub fn reader(t: impl Read + Send + 'static) -> impl AsyncRead + Send + Unpin + 'static {
    let t = AllowStdIo::new(t);
    let (r, mut w) = piper::pipe(PIPE_CAP);
    Task::blocking(async move { drop(futures_util::io::copy(t, &mut w).await) }).forget();
    r
}

/// Spawns a blocking writer onto a thread.
pub fn writer(t: impl Write + Send + 'static) -> impl AsyncWrite + Send + Unpin + 'static {
    let mut t = AllowStdIo::new(t);
    let (r, w) = piper::pipe(PIPE_CAP);
    Task::blocking(async move { drop(futures_util::io::copy(r, &mut t).await) }).forget();
    w
}

// ----- Async I/O and timers -----

static REACTOR: Lazy<Reactor> = Lazy::new(|| Reactor::create().expect("cannot create reactor"));

static INTERRUPT: Lazy<Async<IoFlag>> = Lazy::new(|| {
    IoFlag::create()
        .and_then(Async::nonblocking)
        .expect("cannot create interrupt flag")
});

struct Source {
    raw: sys::Raw,
    index: usize,
    readers: Mutex<Vec<Waker>>,
    writers: Mutex<Vec<Waker>>,
}

struct Reactor {
    sys: sys::Reactor,
    sources: Mutex<Slab<Arc<Source>>>,
    events: lock::Mutex<sys::Events>,
    timers: Mutex<BTreeMap<(Instant, usize), Waker>>,
}

impl Reactor {
    fn create() -> io::Result<Reactor> {
        Ok(Reactor {
            sys: sys::Reactor::create()?,
            sources: Mutex::new(Slab::new()),
            events: lock::Mutex::new(sys::Events::new()),
            timers: Mutex::new(BTreeMap::new()),
        })
    }

    fn register(&self, raw: sys::Raw) -> io::Result<Arc<Source>> {
        let mut sources = self.sources.lock();
        let vacant = sources.vacant_entry();
        let index = vacant.key();
        self.sys.register(raw, index)?;

        let source = Arc::new(Source {
            raw,
            index,
            readers: Mutex::new(Vec::new()),
            writers: Mutex::new(Vec::new()),
        });
        vacant.insert(source.clone());

        Ok(source)
    }

    fn deregister(&self, source: &Source) -> io::Result<()> {
        let mut sources = self.sources.lock();
        sources.remove(source.index);
        self.sys.deregister(source.raw)
    }

    fn quick_poll(&self) -> io::Result<()> {
        if let Some(events) = self.events.try_lock() {
            let mut poller = Poller {
                reactor: self,
                events,
            };
            poller.quick_poll()?;
        }
        Ok(())
    }

    async fn lock(&self) -> Poller<'_> {
        Poller {
            reactor: self,
            events: self.events.lock().await,
        }
    }

    fn interrupt(&self) {
        INTERRUPT.get_ref().set();
    }
}

struct Poller<'a> {
    reactor: &'a Reactor,
    events: lock::MutexGuard<'a, sys::Events>,
}

impl Poller<'_> {
    fn poll(&mut self) -> io::Result<()> {
        self.poll_internal(true)
    }

    fn quick_poll(&mut self) -> io::Result<()> {
        self.poll_internal(false)
    }

    fn poll_internal(&mut self, block: bool) -> io::Result<()> {
        let next_timer = {
            let now = Instant::now();
            let mut timers = self.reactor.timers.lock();

            let pending = timers.split_off(&(now, 0));
            let ready = mem::replace(&mut *timers, pending);

            for (_, waker) in ready {
                waker.wake();
            }

            timers.keys().next().map(|(when, _)| *when)
        };

        let interrupted = INTERRUPT.get_ref().clear();
        let timeout = if block && !interrupted {
            next_timer.map(|when| when.saturating_duration_since(Instant::now()))
        } else {
            Some(Duration::from_secs(0))
        };

        if self.reactor.sys.poll(&mut self.events, timeout)? == 0 {
            return Ok(());
        }

        let mut wakers = VecDeque::new();
        let sources = self.reactor.sources.lock();

        for ev in self.events.iter() {
            if let Some(source) = sources.get(ev.index) {
                self.reactor.sys.reregister(source.raw, source.index)?;

                // In order to minimize worst-case latency, wake writers before readers.
                // See https://twitter.com/kingprotty/status/1222152589405384705?s=19
                if ev.is_write {
                    for w in source.writers.lock().drain(..) {
                        wakers.push_front(w);
                    }
                }
                if ev.is_read {
                    for w in source.readers.lock().drain(..) {
                        wakers.push_back(w);
                    }
                }
            }
        }
        drop(sources);

        // Wake up ready I/O.
        for waker in wakers {
            waker.wake();
        }

        Ok(())
    }
}

/// Fires at a certain point in time.
pub struct Timer {
    when: Instant,
    inserted: bool,
}

impl Timer {
    /// Fires after the specified duration of time.
    pub fn after(dur: Duration) -> Timer {
        Timer {
            when: Instant::now() + dur,
            inserted: false,
        }
    }

    /// Fires at the specified instant in time.
    pub fn at(dur: Duration) -> Timer {
        Timer {
            when: Instant::now() + dur,
            inserted: false,
        }
    }

    fn key(&mut self) -> (Instant, usize) {
        let id = self as *mut Timer as usize;
        (self.when, id)
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if self.inserted {
            REACTOR.timers.lock().remove(&self.key());
        }
    }
}

impl Future for Timer {
    type Output = Instant;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut timers = REACTOR.timers.lock();

        if Instant::now() >= self.when {
            timers.remove(&self.key());
            return Poll::Ready(self.when);
        }

        if !self.inserted {
            let mut is_earliest = false;
            if let Some((first, _)) = timers.keys().next() {
                if self.when < *first {
                    is_earliest = true;
                }
            }

            let waker = cx.waker().clone();
            timers.insert(self.key(), waker);
            self.inserted = true;

            if is_earliest {
                drop(timers);
                REACTOR.interrupt();
            }
        }

        Poll::Pending
    }
}

/// Async I/O.
pub struct Async<T> {
    inner: Option<Box<T>>,
    source: Arc<Source>,
}

#[cfg(unix)]
impl<T: std::os::unix::io::AsRawFd> Async<T> {
    /// Converts a non-blocking I/O handle into an async I/O handle.
    pub fn nonblocking(inner: T) -> io::Result<Async<T>> {
        Ok(Async {
            source: REACTOR.register(sys::Raw::new(&inner))?,
            inner: Some(Box::new(inner)),
        })
    }
}

#[cfg(windows)]
impl<T: std::os::windows::io::AsRawSocket> Async<T> {
    /// Converts a non-blocking I/O handle into an async I/O handle.
    pub fn nonblocking(inner: T) -> io::Result<Async<T>> {
        Ok(Async {
            source: REACTOR.register(sys::Raw::new(&inner))?,
            inner: Some(Box::new(inner)),
        })
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

    /// Converts a non-blocking read into an async operation.
    pub async fn read_with<R>(&self, f: impl FnMut(&T) -> io::Result<R>) -> io::Result<R> {
        let mut f = f;
        let mut inner = self.inner.as_ref().unwrap();
        let wakers = &self.source.readers;
        future::poll_fn(|cx| Self::poll_io(cx, |s| f(s), &mut inner, wakers)).await
    }

    /// Converts a non-blocking read into an async operation.
    pub async fn read_with_mut<R>(
        &mut self,
        f: impl FnMut(&mut T) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut f = f;
        let mut inner = self.inner.as_mut().unwrap();
        let wakers = &self.source.readers;
        future::poll_fn(|cx| Self::poll_io(cx, |s| f(s), &mut inner, wakers)).await
    }

    /// Converts a non-blocking write into an async operation.
    pub async fn write_with<R>(&self, f: impl FnMut(&T) -> io::Result<R>) -> io::Result<R> {
        let mut f = f;
        let mut inner = self.inner.as_ref().unwrap();
        let wakers = &self.source.writers;
        future::poll_fn(|cx| Self::poll_io(cx, |s| f(s), &mut inner, wakers)).await
    }

    /// Converts a non-blocking write into an async operation.
    pub async fn write_with_mut<R>(
        &mut self,
        f: impl FnMut(&mut T) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut f = f;
        let mut inner = self.inner.as_mut().unwrap();
        let wakers = &self.source.writers;
        future::poll_fn(|cx| Self::poll_io(cx, |s| f(s), &mut inner, wakers)).await
    }

    fn poll_io<I, R>(
        cx: &mut Context<'_>,
        mut f: impl FnMut(&mut I) -> io::Result<R>,
        inner: &mut I,
        wakers: &Mutex<Vec<Waker>>,
    ) -> Poll<io::Result<R>> {
        // Attempt the non-blocking operation.
        match f(inner) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            res => return Poll::Ready(res),
        }

        // Acquire a lock on the waker list.
        let mut wakers = wakers.lock();

        // Attempt the non-blocking operation again.
        match f(inner) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            res => return Poll::Ready(res),
        }

        // If it would still block, register the curent waker and return.
        if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

impl<T> Drop for Async<T> {
    fn drop(&mut self) {
        if self.inner.take().is_some() {
            // Destructors should not panic.
            let _ = REACTOR.deregister(&self.source);
        }
    }
}

impl<T: Read> AsyncRead for Async<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let fut = self.read_with_mut(|inner| inner.read(buf));
        pin_utils::pin_mut!(fut);
        fut.poll(cx)
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
        let fut = self.read_with(|inner| (&*inner).read(buf));
        pin_utils::pin_mut!(fut);
        fut.poll(cx)
    }
}

impl<T: Write> AsyncWrite for Async<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let fut = self.write_with_mut(|inner| inner.write(buf));
        pin_utils::pin_mut!(fut);
        fut.poll(cx)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let fut = self.write_with_mut(|inner| inner.flush());
        pin_utils::pin_mut!(fut);
        fut.poll(cx)
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
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
        let fut = self.write_with(|inner| (&*inner).write(buf));
        pin_utils::pin_mut!(fut);
        fut.poll(cx)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let fut = self.write_with(|inner| (&*inner).flush());
        pin_utils::pin_mut!(fut);
        fut.poll(cx)
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Async<TcpListener> {
    /// Creates a listener bound to the specified address.
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Async<TcpListener>> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Async::nonblocking(listener)?)
    }

    /// Accepts a new incoming connection.
    pub async fn accept(&self) -> io::Result<(Async<TcpStream>, SocketAddr)> {
        let (stream, addr) = self.read_with(|inner| inner.accept()).await?;
        stream.set_nonblocking(true)?;
        Ok((Async::nonblocking(stream)?, addr))
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
    pub async fn connect<T: ToSocketAddrs + Send + 'static>(
        addr: T,
    ) -> io::Result<Async<TcpStream>> {
        let addrs = Task::blocking(async move {
            let iter = addr.to_socket_addrs()?;
            io::Result::Ok(iter.collect::<Vec<_>>())
        })
        .await?;

        let mut last_err = None;

        // Try connecting to each address one by one.
        for addr in addrs {
            match Self::connect_to(addr).await {
                Ok(stream) => return Ok(stream),
                Err(err) => last_err = Some(err),
            }
        }

        // Return the last error if at least one address was tried.
        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve to any address",
            )
        }))
    }

    /// Attempts connecting to a single address.
    async fn connect_to(addr: SocketAddr) -> io::Result<Async<TcpStream>> {
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
        let stream = Async::nonblocking(socket.into_tcp_stream())?;

        // Wait for connect to complete.
        let wait_connect = |mut stream: &TcpStream| match stream.write(&[]) {
            Err(err) if err.kind() == io::ErrorKind::NotConnected => {
                Err(io::Error::new(io::ErrorKind::WouldBlock, ""))
            }
            res => res.map(|_| ()),
        };
        // The stream becomes writable when connected.
        stream.write_with(|inner| wait_connect(inner)).await?;

        Ok(stream)
    }

    /// Receives data from the stream without removing it from the buffer.
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(|inner| inner.peek(buf)).await
    }
}

impl Async<UdpSocket> {
    /// Creates a socket bound to the specified address.
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Async<UdpSocket>> {
        let socket = UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        Ok(Async::nonblocking(socket)?)
    }

    /// Sends data to the specified address.
    pub async fn send_to<A: ToSocketAddrs>(&self, buf: &[u8], addr: A) -> io::Result<usize> {
        self.write_with(|inner| inner.send_to(buf, &addr)).await
    }

    /// Sends data to the socket's peer.
    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.write_with(|inner| inner.send(buf)).await
    }

    /// Receives data from the socket.
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.read_with(|inner| inner.recv_from(buf)).await
    }

    /// Receives data from the socket's peer.
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(|inner| inner.recv(buf)).await
    }

    /// Receives data without removing it from the buffer.
    pub async fn peek_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.read_with(|inner| inner.peek_from(buf)).await
    }

    /// Receives data from the socket's peer without removing it from the buffer.
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(|inner| inner.peek(buf)).await
    }
}

#[cfg(unix)]
impl Async<UnixListener> {
    /// Creates a listener bound to the specified path.
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Async<UnixListener>> {
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Async::nonblocking(listener)?)
    }

    /// Accepts a new incoming connection.
    pub async fn accept(&self) -> io::Result<(Async<UnixStream>, UnixSocketAddr)> {
        let (stream, addr) = self.read_with(|inner| inner.accept()).await?;
        stream.set_nonblocking(true)?;
        Ok((Async::nonblocking(stream)?, addr))
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
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<Async<UnixStream>> {
        let stream = UnixStream::connect(path)?;
        stream.set_nonblocking(true)?;
        Ok(Async::nonblocking(stream)?)
    }

    /// Creates an unnamed pair of connected streams.
    pub fn pair() -> io::Result<(Async<UnixStream>, Async<UnixStream>)> {
        let (stream1, stream2) = UnixStream::pair()?;
        stream1.set_nonblocking(true)?;
        stream2.set_nonblocking(true)?;
        Ok((Async::nonblocking(stream1)?, Async::nonblocking(stream2)?))
    }
}

#[cfg(unix)]
impl Async<UnixDatagram> {
    /// Creates a socket bound to the specified path.
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Async<UnixDatagram>> {
        let socket = UnixDatagram::bind(path)?;
        socket.set_nonblocking(true)?;
        Ok(Async::nonblocking(socket)?)
    }

    /// Creates a socket not bound to any address.
    pub fn unbound() -> io::Result<Async<UnixDatagram>> {
        let socket = UnixDatagram::unbound()?;
        socket.set_nonblocking(true)?;
        Ok(Async::nonblocking(socket)?)
    }

    /// Creates an unnamed pair of connected sockets.
    pub fn pair() -> io::Result<(Async<UnixDatagram>, Async<UnixDatagram>)> {
        let (socket1, socket2) = UnixDatagram::pair()?;
        socket1.set_nonblocking(true)?;
        socket2.set_nonblocking(true)?;
        Ok((Async::nonblocking(socket1)?, Async::nonblocking(socket2)?))
    }

    /// Sends data to the specified address.
    pub async fn send_to<P: AsRef<Path>>(&self, buf: &[u8], path: P) -> io::Result<usize> {
        self.write_with(|inner| inner.send_to(buf, &path)).await
    }

    /// Sends data to the socket's peer.
    pub async fn send<A: ToSocketAddrs>(&self, buf: &[u8]) -> io::Result<usize> {
        self.write_with(|inner| inner.send(buf)).await
    }

    /// Receives data from the socket.
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, UnixSocketAddr)> {
        self.read_with(|inner| inner.recv_from(buf)).await
    }

    /// Receives data from the socket's peer.
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(|inner| inner.recv(buf)).await
    }
}

impl Async<IoFlag> {
    async fn ready(&self) {
        self.read_with(|inner| match inner.get() {
            true => Ok(()),
            false => Err(io::Error::new(io::ErrorKind::WouldBlock, "")),
        })
        .await
        .unwrap()
    }
}

// ----- Linux / Android (epoll) -----

#[cfg(any(target_os = "linux", target_os = "android"))]
mod sys {
    use std::convert::TryInto;
    use std::io;
    use std::os::unix::io::{AsRawFd, RawFd};
    use std::time::Duration;

    use nix::{
        errno::Errno,
        sys::epoll::{
            epoll_create1, epoll_ctl, epoll_wait, EpollCreateFlags, EpollEvent, EpollFlags, EpollOp,
        },
    };

    #[derive(Clone, Copy)]
    pub struct Raw(RawFd);
    impl Raw {
        pub fn new(s: &impl AsRawFd) -> Raw {
            Raw(s.as_raw_fd())
        }
    }

    pub struct Reactor(RawFd);
    impl Reactor {
        pub fn create() -> io::Result<Reactor> {
            Ok(Reactor(
                epoll_create1(EpollCreateFlags::EPOLL_CLOEXEC).map_err(io_err)?,
            ))
        }
        pub fn register(&self, raw: Raw, index: usize) -> io::Result<()> {
            let ev = &mut EpollEvent::new(flags(), index as u64);
            epoll_ctl(self.0, EpollOp::EpollCtlAdd, raw.0, Some(ev)).map_err(io_err)
        }
        pub fn reregister(&self, _raw: Raw, _index: usize) -> io::Result<()> {
            Ok(())
        }
        pub fn deregister(&self, raw: Raw) -> io::Result<()> {
            epoll_ctl(self.0, EpollOp::EpollCtlDel, raw.0, None).map_err(io_err)
        }
        pub fn poll(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            let timeout_ms = timeout
                .and_then(|t| t.as_millis().try_into().ok())
                .unwrap_or(-1);
            events.len = match epoll_wait(self.0, &mut events.list, timeout_ms) {
                Ok(len) => len,
                Err(nix::Error::Sys(Errno::EINTR)) => 0,
                Err(err) => return Err(io_err(err)),
            };
            Ok(events.len)
        }
    }
    fn flags() -> EpollFlags {
        EpollFlags::EPOLLET | EpollFlags::EPOLLIN | EpollFlags::EPOLLOUT | EpollFlags::EPOLLRDHUP
    }
    fn io_err(err: impl std::error::Error + Send + Sync + 'static) -> io::Error {
        io::Error::new(io::ErrorKind::Other, Box::new(err))
    }

    pub struct Events {
        list: Box<[EpollEvent]>,
        len: usize,
    }
    impl Events {
        pub fn new() -> Events {
            Events {
                list: vec![EpollEvent::empty(); 1000].into_boxed_slice(),
                len: 0,
            }
        }
        pub fn iter(&self) -> impl Iterator<Item = Event> + '_ {
            self.list[..self.len].iter().map(|ev| Event {
                is_read: ev.events() != EpollFlags::EPOLLOUT,
                is_write: ev.events() != EpollFlags::EPOLLIN,
                index: ev.data() as usize,
            })
        }
    }
    pub struct Event {
        pub is_read: bool,
        pub is_write: bool,
        pub index: usize,
    }
}

// ----- macOS / iOS / FreeBSD / NetBSD / OpenBSD / DragonFly BSD (kqueue) -----

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
    use std::os::unix::io::{AsRawFd, RawFd};
    use std::time::Duration;

    use nix::{
        errno::Errno,
        fcntl::{fcntl, FcntlArg, FdFlag},
        sys::event::{kevent_ts, kqueue, EventFilter, EventFlag, FilterFlag, KEvent},
    };

    #[derive(Clone, Copy)]
    pub struct Raw(RawFd);
    impl Raw {
        pub fn new(s: &impl AsRawFd) -> Raw {
            Raw(s.as_raw_fd())
        }
    }

    pub struct Reactor(RawFd);
    impl Reactor {
        pub fn create() -> io::Result<Reactor> {
            let fd = kqueue().map_err(io_err)?;
            fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io_err)?;
            Ok(Reactor(fd))
        }
        pub fn register(&self, raw: Raw, index: usize) -> io::Result<()> {
            let ident = raw.0 as _;
            let flags = EventFlag::EV_CLEAR | EventFlag::EV_RECEIPT | EventFlag::EV_ADD;
            let fflags = FilterFlag::empty();
            let udata = index as _;
            let changelist = [
                KEvent::new(ident, EventFilter::EVFILT_WRITE, flags, fflags, 0, udata),
                KEvent::new(ident, EventFilter::EVFILT_READ, flags, fflags, 0, udata),
            ];
            let mut eventlist = changelist.clone();
            match kevent_ts(self.0, &changelist, &mut eventlist, None) {
                Ok(_) | Err(nix::Error::Sys(Errno::EINTR)) => {}
                Err(err) => return Err(io_err(err)),
            }
            for ev in &eventlist {
                // See https://github.com/tokio-rs/mio/issues/582
                if ev.data() != 0
                    && ev.data() != Errno::EPIPE as _
                    && ev.flags().contains(EventFlag::EV_ERROR)
                {
                    return Err(io::Error::from_raw_os_error(ev.data() as _));
                }
            }
            Ok(())
        }
        pub fn reregister(&self, _raw: Raw, _index: usize) -> io::Result<()> {
            Ok(())
        }
        pub fn deregister(&self, raw: Raw) -> io::Result<()> {
            let ident = raw.0 as _;
            let flags = EventFlag::EV_RECEIPT | EventFlag::EV_DELETE;
            let fflags = FilterFlag::empty();
            let changelist = [
                KEvent::new(ident, EventFilter::EVFILT_WRITE, flags, fflags, 0, 0),
                KEvent::new(ident, EventFilter::EVFILT_READ, flags, fflags, 0, 0),
            ];
            let mut eventlist = changelist.clone();
            match kevent_ts(self.0, &changelist, &mut eventlist, None) {
                Ok(_) | Err(nix::Error::Sys(Errno::EINTR)) => {}
                Err(err) => return Err(io_err(err)),
            }
            for ev in &eventlist {
                if ev.data() != 0 && ev.flags().contains(EventFlag::EV_ERROR) {
                    return Err(io::Error::from_raw_os_error(ev.data() as _));
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
            events.len = match kevent_ts(self.0, &[], &mut events.list, timeout) {
                Ok(n) => n,
                Err(nix::Error::Sys(Errno::EINTR)) => 0,
                Err(err) => return Err(io_err(err)),
            };
            Ok(events.len)
        }
    }
    fn io_err(err: impl std::error::Error + Send + Sync + 'static) -> io::Error {
        io::Error::new(io::ErrorKind::Other, Box::new(err))
    }

    pub struct Events {
        list: Box<[KEvent]>,
        len: usize,
    }
    impl Events {
        pub fn new() -> Events {
            let flags = EventFlag::empty();
            let fflags = FilterFlag::empty();
            let event = KEvent::new(0, EventFilter::EVFILT_USER, flags, fflags, 0, 0);
            Events {
                list: vec![event; 1000].into_boxed_slice(),
                len: 0,
            }
        }
        pub fn iter(&self) -> impl Iterator<Item = Event> + '_ {
            self.list[..self.len].iter().map(|ev| Event {
                is_read: ev.filter() != EventFilter::EVFILT_WRITE,
                is_write: ev.filter() != EventFilter::EVFILT_READ,
                index: ev.udata() as usize,
            })
        }
    }
    pub struct Event {
        pub is_read: bool,
        pub is_write: bool,
        pub index: usize,
    }
}

// ----- Windows (WSAPoll) -----

#[cfg(target_os = "windows")]
mod sys {
    use std::io;
    use std::os::windows::io::{AsRawSocket, RawSocket};
    use std::time::Duration;

    use wepoll::{Epoll, EventFlag};
    use wepoll_binding as wepoll;

    #[derive(Clone, Copy)]
    pub struct Raw(RawSocket);
    impl Raw {
        pub fn new(s: &impl AsRawSocket) -> Raw {
            Raw(s.as_raw_socket())
        }
    }
    impl AsRawSocket for Raw {
        fn as_raw_socket(&self) -> RawSocket {
            self.0
        }
    }

    pub struct Reactor(Epoll);
    impl Reactor {
        pub fn create() -> io::Result<Reactor> {
            Ok(Reactor(Epoll::new()?))
        }
        pub fn register(&self, raw: Raw, index: usize) -> io::Result<()> {
            self.0.register(&raw, flags(), index as u64)
        }
        pub fn reregister(&self, raw: Raw, index: usize) -> io::Result<()> {
            // Ignore errors because a concurrent poll can reregister the handle at any point.
            let _ = self.0.reregister(&raw, flags(), index as u64);
            Ok(())
        }
        pub fn deregister(&self, raw: Raw) -> io::Result<()> {
            // Ignore errors because an event can deregister the handle at any point.
            let _ = self.0.deregister(&raw);
            Ok(())
        }
        pub fn poll(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            events.0.clear();
            let n = match self.0.poll(&mut events.0, timeout) {
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => 0,
                Err(err) => return Err(err),
            };
            Ok(n)
        }
    }
    fn flags() -> EventFlag {
        EventFlag::ONESHOT | EventFlag::IN | EventFlag::OUT | EventFlag::RDHUP
    }

    pub struct Events(wepoll::Events);
    impl Events {
        pub fn new() -> Events {
            Events(wepoll::Events::with_capacity(1000))
        }
        pub fn iter(&self) -> impl Iterator<Item = Event> + '_ {
            self.0.iter().map(|ev| Event {
                is_read: ev.flags() != EventFlag::OUT,
                is_write: ev.flags() != EventFlag::IN,
                index: ev.data() as usize,
            })
        }
    }
    pub struct Event {
        pub is_read: bool,
        pub is_write: bool,
        pub index: usize,
    }
}

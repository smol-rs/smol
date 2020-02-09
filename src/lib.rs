#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]
// TODO: #![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "windows")))]
compile_error!("smol does not support this target OS");

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::convert::TryInto;
use std::fmt::Debug;
use std::future::Future;
use std::io::{self, Read, Write};
use std::mem;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::panic::catch_unwind;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::{
    io::{AsRawFd, RawFd},
    net::{SocketAddr as UnixSocketAddr, UnixDatagram, UnixListener, UnixStream},
};

#[cfg(target_os = "windows")]
use std::os::windows::io::{AsRawSocket, RawSocket};

use crossbeam_channel as channel;
use crossbeam_utils::sync::Parker;
use futures_core::stream::Stream;
use futures_io::{AsyncBufRead, AsyncRead, AsyncWrite};
use futures_util::future;
use futures_util::io::{AsyncReadExt, AsyncWriteExt};
use futures_util::stream::{self, StreamExt};
use once_cell::sync::Lazy;
use parking_lot::{Condvar, Mutex};
use slab::Slab;
use socket2::{Domain, Protocol, Socket, Type};

#[cfg(target_os = "linux")]
use nix::sys::epoll::{
    epoll_create1, epoll_ctl, epoll_wait, EpollCreateFlags, EpollEvent, EpollFlags, EpollOp,
};

#[cfg(target_os = "windows")]
use wepoll_binding::{Epoll, EpollFlags, Events};

// TODO: fix unwraps
// TODO: if epoll/kqueue/wepoll gets EINTR, then retry - or maybe just call notify()
// TODO: catch panics in wake() and Waker::drop()
// TODO: readme for inspiration: https://github.com/piscisaureus/wepoll

// ----- Poller -----

struct Poller {
    registry: Registry,
    flag: AtomicBool,
    socket_notify: Socket,
    socket_wakeup: Socket,
}

static POLLER: Lazy<Poller> = Lazy::new(|| Poller::create().expect("cannot create poller"));

impl Poller {
    fn create() -> io::Result<Poller> {
        // https://stackoverflow.com/questions/24933411/how-to-emulate-socket-socketpair-on-windows
        // https://github.com/mhils/backports.socketpair/blob/master/backports/socketpair/__init__.py
        // https://github.com/python-trio/trio/blob/master/trio/_core/_wakeup_socketpair.py
        // https://gist.github.com/geertj/4325783

        // Create a temporary listener.
        let listener = Socket::new(Domain::ipv4(), Type::stream(), None)?;
        listener.bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())?;
        listener.listen(1)?;
        let addr = listener.local_addr()?;

        // First socket: connect to the listener.
        let sock1 = Socket::new(Domain::ipv4(), Type::stream(), None)?;
        sock1.set_nonblocking(true)?;
        let _ = sock1.connect(&addr);
        let _ = sock1.set_nodelay(true)?;
        sock1.set_send_buffer_size(1)?;

        // Second socket: accept a client from the listener.
        let (sock2, _) = listener.accept()?;
        sock2.set_nonblocking(true)?;
        sock2.set_recv_buffer_size(1)?;

        let registry = Registry::create()?;
        registry.register(&sock2)?;

        Ok(Poller {
            registry,
            flag: AtomicBool::new(false),
            socket_notify: sock1,
            socket_wakeup: sock2,
        })
    }

    fn poll(&self) -> io::Result<()> {
        let interrupted = self.reset();
        let next_timer = self.registry.poll_timers();

        let timeout = if interrupted {
            Some(Duration::from_secs(0))
        } else {
            next_timer.map(|when| Instant::now().saturating_duration_since(when))
        };
        self.registry.wait_io(timeout)?;
        Ok(())
    }

    fn poll_quick(&self) -> io::Result<()> {
        self.registry.poll_timers();
        self.registry.wait_io(Some(Duration::from_secs(0)))?;
        Ok(())
    }

    /// Sets the interrupt flag and writes to the wakeup socket.
    fn interrupt(&self) {
        if !self.flag.load(Ordering::SeqCst) {
            if !self.flag.swap(true, Ordering::SeqCst) {
                loop {
                    match (&self.socket_notify).write(&[1]) {
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                        _ => {
                            let _ = (&self.socket_notify).flush();
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Clears the interrupt flag and drains the wakeup socket.
    fn reset(&self) -> bool {
        let value = self.flag.swap(false, Ordering::SeqCst);
        if value {
            loop {
                match (&self.socket_wakeup).read(&mut [0; 64]) {
                    Ok(n) if n > 0 => {}
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    _ => break,
                }
            }
        }
        value
    }
}

// ----- Registry -----

struct Entry {
    #[cfg(unix)]
    fd: RawFd,

    #[cfg(windows)]
    socket: RawSocket,

    index: usize,
    readers: Mutex<Vec<Waker>>,
    writers: Mutex<Vec<Waker>>,
}

#[cfg(unix)]
impl AsRawFd for Entry {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

#[cfg(windows)]
impl AsRawSocket for Entry {
    fn as_raw_socket(&self) -> RawSocket {
        self.socket
    }
}

struct Registry {
    #[cfg(target_os = "linux")]
    epoll: RawFd,
    #[cfg(target_os = "linux")]
    events: Mutex<Box<[EpollEvent]>>,

    #[cfg(target_os = "windows")]
    epoll: Epoll,
    #[cfg(target_os = "windows")]
    events: Mutex<Events>,

    io_handles: Mutex<Slab<Arc<Entry>>>,
    timers: Mutex<BTreeMap<(Instant, usize), Waker>>,
}

impl Registry {
    fn create() -> io::Result<Registry> {
        Ok(Registry {
            #[cfg(target_os = "linux")]
            epoll: epoll_create1(EpollCreateFlags::EPOLL_CLOEXEC).map_err(io_err)?,
            #[cfg(target_os = "linux")]
            events: Mutex::new(vec![EpollEvent::empty(); 1000].into_boxed_slice()),

            #[cfg(target_os = "windows")]
            epoll: Epoll::new()?,
            #[cfg(target_os = "windows")]
            events: Mutex::new(Events::with_capacity(1000)),

            io_handles: Mutex::new(Slab::new()),
            timers: Mutex::new(BTreeMap::new()),
        })
    }

    fn register(
        &self,
        #[cfg(unix)] source: &dyn AsRawFd,
        #[cfg(windows)] source: &dyn RawSocket,
    ) -> io::Result<Arc<Entry>> {
        let mut io_handles = self.io_handles.lock();
        let vacant = io_handles.vacant_entry();
        let index = vacant.key();

        #[cfg(target_os = "linux")]
        epoll_ctl(
            self.epoll,
            EpollOp::EpollCtlAdd,
            source.as_raw_fd(),
            Some(&mut EpollEvent::new(
                EpollFlags::EPOLLONESHOT
                    | EpollFlags::EPOLLIN
                    | EpollFlags::EPOLLOUT
                    | EpollFlags::EPOLLRDHUP,
                index as u64,
            )),
        )
        .map_err(io_err)?;

        #[cfg(target_os = "windows")]
        self.epoll.register(
            source,
            EventFlag::ONESHOT | EventFlag::IN | EventFlag::OUT | EventFlag::RDHUP,
            index as u64,
        )?;

        let entry = Arc::new(Entry {
            #[cfg(unix)]
            fd: source.as_raw_fd(),

            #[cfg(windows)]
            socket: source.as_raw_socket(),

            index,
            readers: Mutex::new(Vec::new()),
            writers: Mutex::new(Vec::new()),
        });
        vacant.insert(entry.clone());

        Ok(entry)
    }

    fn deregister(&self, entry: &Entry) -> io::Result<()> {
        let mut io_handles = self.io_handles.lock();
        io_handles.remove(entry.index);

        #[cfg(target_os = "linux")]
        epoll_ctl(self.epoll, EpollOp::EpollCtlDel, entry.as_raw_fd(), None).map_err(io_err)?;

        #[cfg(target_os = "windows")]
        self.epoll.deregister(entry)?;

        Ok(())
    }

    fn wait_io(&self, timeout: Option<Duration>) -> io::Result<()> {
        let mut events = if timeout == Some(Duration::from_secs(0)) {
            match self.events.try_lock() {
                None => return Ok(()),
                Some(e) => e,
            }
        } else {
            self.events.lock()
        };

        #[cfg(target_os = "linux")]
        let (n, iter) = {
            let timeout_ms = timeout
                .and_then(|t| t.as_millis().try_into().ok())
                .unwrap_or(-1);
            let n = epoll_wait(self.epoll, &mut events, timeout_ms).map_err(io_err)?;
            (n, &events[..n])
        };

        #[cfg(target_os = "windows")]
        let (n, iter) = {
            events.clear();
            let n = self.epoll.poll(&mut events, timeout)?;
            (n, events.iter())
        };

        let mut wakers = VecDeque::new();
        if n > 0 {
            let io_handles = self.io_handles.lock();

            for ev in iter {
                #[cfg(target_os = "linux")]
                let (is_read, is_write, index) = (
                    ev.events() != EpollFlags::EPOLLOUT,
                    ev.events() != EpollFlags::EPOLLIN,
                    ev.data() as usize,
                );

                #[cfg(target_os = "windows")]
                let (is_read, is_write, index) = (
                    ev.flags() != EventFlag::OUT,
                    ev.flags() != EventFlag::IN,
                    ev.data() as usize,
                );

                // In order to minimize latencies, wake writers before readers.
                // Source: https://twitter.com/kingprotty/status/1222152589405384705?s=19
                if let Some(entry) = io_handles.get(index) {
                    if is_read {
                        for w in entry.readers.lock().drain(..) {
                            wakers.push_back(w);
                        }
                    }
                    if is_write {
                        for w in entry.writers.lock().drain(..) {
                            wakers.push_front(w);
                        }
                    }

                    #[cfg(target_os = "linux")]
                    epoll_ctl(
                        self.epoll,
                        EpollOp::EpollCtlMod,
                        entry.fd,
                        Some(&mut EpollEvent::new(
                            EpollFlags::EPOLLONESHOT
                                | EpollFlags::EPOLLIN
                                | EpollFlags::EPOLLOUT
                                | EpollFlags::EPOLLRDHUP,
                            entry.index as u64,
                        )),
                    )
                    .map_err(io_err)?;

                    #[cfg(target_os = "windows")]
                    self.epoll.reregister(
                        entry,
                        EventFlag::ONESHOT | EventFlag::IN | EventFlag::OUT | EventFlag::RDHUP,
                        entry.index as u64
                    )?;
                }
            }
        }

        // Wake up ready I/O.
        for waker in wakers {
            waker.wake();
        }

        Ok(())
    }

    fn poll_timers(&self) -> Option<Instant> {
        let now = Instant::now();
        let (ready, next_timer) = {
            let mut timers = self.timers.lock();
            let pending = timers.split_off(&(now, 0));
            let ready = mem::replace(&mut *timers, pending);
            let next_timer = timers.keys().next().map(|(when, _)| *when);
            (ready, next_timer)
        };

        // Wake up ready timers.
        for (_, waker) in ready {
            waker.wake();
        }

        next_timer
    }
}

/// Converts any error into an I/O error.
#[cfg(unix)]
fn io_err(err: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::Other, Box::new(err))
}

// ----- Executor -----

struct Executor {
    receiver: channel::Receiver<Runnable>,
    queue: channel::Sender<Runnable>,
    cvar: Condvar,
    mutex: Mutex<bool>,
}

static EXECUTOR: Lazy<Executor> = Lazy::new(|| {
    let (sender, receiver) = channel::unbounded::<Runnable>();
    Executor {
        receiver,
        queue: sender,
        cvar: Condvar::new(),
        mutex: Mutex::new(false),
    }
});

/// A runnable future, ready for execution.
type Runnable = async_task::Task<()>;

/// Executes all futures until the main one completes.
pub fn run<T>(future: impl Future<Output = T>) -> T {
    pin_utils::pin_mut!(future);

    // TODO: panic on nested run()
    // TODO Optimization: use thread-local cache for ready and queue
    let ready = Arc::new(AtomicBool::new(true));

    let waker = async_task::waker_fn({
        let ready = ready.clone();
        move || {
            if !ready.swap(true, Ordering::SeqCst) {
                POLLER.interrupt();
                let _m = EXECUTOR.mutex.lock();
                EXECUTOR.cvar.notify_all();
            }
        }
    });

    // The number of times the thread found work in a row.
    let mut runs = 0;
    // The number of times the thread didn't find work in a row.
    let mut fails = 0;

    loop {
        while !ready.load(Ordering::SeqCst) {
            if runs >= 64 {
                runs = 0;
                POLLER.poll_quick().unwrap();
            }

            match EXECUTOR.receiver.try_recv() {
                Ok(runnable) => {
                    runs += 1;
                    fails = 0;
                    let _ = catch_unwind(|| runnable.run());
                }
                Err(_) => {
                    runs = 0;
                    fails += 1;
                    POLLER.poll_quick().unwrap();

                    if fails <= 1 {
                        continue;
                    }
                    if fails <= 3 {
                        std::thread::yield_now();
                        continue;
                    }

                    let mut m = EXECUTOR.mutex.lock();

                    if *m {
                        if !ready.load(Ordering::SeqCst) {
                            EXECUTOR.cvar.wait(&mut m);
                        }
                        continue;
                    }

                    *m = true;
                    drop(m);

                    let _guard = {
                        struct OnDrop<F: FnMut()>(F);
                        impl<F: FnMut()> Drop for OnDrop<F> {
                            fn drop(&mut self) {
                                (self.0)();
                            }
                        }
                        OnDrop(|| {
                            let mut m = EXECUTOR.mutex.lock();
                            *m = false;
                            EXECUTOR.cvar.notify_one();
                        })
                    };

                    POLLER.poll().unwrap();
                }
            }
        }

        runs += 1;
        fails = 0;

        ready.store(false, Ordering::SeqCst);
        match future.as_mut().poll(&mut Context::from_waker(&waker)) {
            Poll::Pending => {}
            Poll::Ready(val) => return val,
        }
    }
}

/// A spawned future.
pub struct Task<T>(async_task::JoinHandle<T, ()>);

impl<T: Send + 'static> Task<T> {
    /// Spawns a global future.
    ///
    /// This future is allowed to be stolen by another executor.
    #[must_use]
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        // Create a runnable and schedule it for execution.
        let schedule = |runnable| {
            EXECUTOR.queue.send(runnable).unwrap();
            POLLER.interrupt();
        };
        let (runnable, handle) = async_task::spawn(future, schedule, ());
        runnable.schedule();

        // Return a join handle that retrieves the output of the future.
        Task(handle)
    }

    /// Spawns a future onto the blocking thread pool.
    #[must_use]
    pub fn blocking(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        let (runnable, handle) =
            async_task::spawn(future, |r| THREAD_POOL.sender.send(r).unwrap(), ());
        runnable.schedule();
        Task(handle)
    }
}

impl<T: 'static> Task<T> {
    /// Spawns a future onto the current executor.
    ///
    /// Panics if not called within an executor.
    #[must_use]
    pub fn local(future: impl Future<Output = T> + 'static) -> Task<T>
    where
        T: 'static,
    {
        // TODO: panic if not called inside a worker started with run()
        todo!()
    }
}

impl<E> Task<Result<(), E>>
where
    E: Debug + Send + 'static,
{
    /// Spawns a global future that unwraps the result.
    pub fn unwrap(self) -> Task<()> {
        Task::spawn(async { self.await.unwrap() })
    }
}

impl<T> Future for Task<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(output) => Poll::Ready(output.expect("task failed")),
        }
    }
}

// ----- Blocking -----

struct ThreadPool {
    sender: channel::Sender<Runnable>,
    receiver: channel::Receiver<Runnable>,
}

static THREAD_POOL: Lazy<ThreadPool> = Lazy::new(|| {
    // Start a single worker thread waiting for the first task.
    start_thread();

    let (sender, receiver) = channel::unbounded();
    ThreadPool { sender, receiver }
});

fn start_thread() {
    use std::sync::atomic::*;
    static SLEEPING: AtomicUsize = AtomicUsize::new(0);

    SLEEPING.fetch_add(1, Ordering::SeqCst);
    let timeout = Duration::from_secs(1);

    thread::Builder::new()
        .name("async-std/blocking".to_string())
        .spawn(move || {
            loop {
                let mut runnable = match THREAD_POOL.receiver.recv_timeout(timeout) {
                    Ok(runnable) => runnable,
                    Err(_) => {
                        // Check whether this is the last sleeping thread.
                        if SLEEPING.fetch_sub(1, Ordering::SeqCst) == 1 {
                            // If so, then restart the thread to make sure there is always at least
                            // one sleeping thread.
                            if SLEEPING.compare_and_swap(0, 1, Ordering::SeqCst) == 0 {
                                continue;
                            }
                        }

                        // Stop the thread.
                        return;
                    }
                };

                // If there are no sleeping threads, then start one to make sure there is always at
                // least one sleeping thread.
                if SLEEPING.fetch_sub(1, Ordering::SeqCst) == 1 {
                    start_thread();
                }

                loop {
                    let _ = catch_unwind(|| runnable.run());

                    // Try taking another runnable if there are any available.
                    runnable = match THREAD_POOL.receiver.try_recv() {
                        Ok(runnable) => runnable,
                        Err(_) => break,
                    };
                }

                // If there is at least one sleeping thread, stop this thread instead of putting it
                // to sleep.
                if SLEEPING.load(Ordering::SeqCst) > 0 {
                    return;
                }

                SLEEPING.fetch_add(1, Ordering::SeqCst);
            }
        })
        .expect("cannot start a blocking thread");
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
        let (parker, waker) = &mut *cache.try_borrow_mut().ok().expect("recursive `block_on`");

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

/// Moves blocking code onto a thread.
#[macro_export]
macro_rules! blocking {
    ($($expr:tt)*) => {
        $crate::Task::blocking(async move { $($expr)* }).await
    };
}

/// Converts a blocking iterator into a stream.
pub fn iter<T>(t: T) -> impl Stream<Item = T::Item> + Send + Unpin + 'static
where
    T: Iterator + Send + 'static,
    T::Item: Send,
{
    // NOTE: stop task if the returned handle is dropped
    todo!();
    stream::empty()
}

/// Converts a blocking reader into an async reader.
pub fn reader(t: impl Read + Send + 'static) -> impl AsyncBufRead + Send + Unpin + 'static {
    // NOTE: stop task if the returned handle is dropped
    todo!();
    futures_util::io::empty()
}

/// Converts a blocking writer into an async writer.
pub fn writer(t: impl Write + Send + 'static) -> impl AsyncWrite + Send + Unpin + 'static {
    // TODO: should we simply return Writer here?
    // NOTE: stop task if the returned handle is dropped
    todo!();
    futures_util::io::sink()
}

/// Blocks on async I/O.
pub struct BlockOn<T>(pub T);

impl<T: Stream + Unpin> Iterator for BlockOn<T> {
    type Item = T::Item;

    fn next(&mut self) -> Option<Self::Item> {
        block_on(Pin::new(&mut self.0).next())
    }
}

impl<T: AsyncRead + Unpin> Read for BlockOn<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        block_on(Pin::new(&mut self.0).read(buf))
    }
}

impl<T: AsyncWrite + Unpin> Write for BlockOn<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        block_on(Pin::new(&mut self.0).write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        block_on(Pin::new(&mut self.0).flush())
    }
}

// ----- Timer -----

/// Fires at an instant in time.
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
}

impl Drop for Timer {
    fn drop(&mut self) {
        let id = self as *mut Timer as usize;
        POLLER.registry.timers.lock().remove(&(self.when, id));
        self.inserted = false;
    }
}

impl Future for Timer {
    type Output = Instant;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let id = &mut *self as *mut Timer as usize;
        let mut timers = POLLER.registry.timers.lock();

        if Instant::now() >= self.when {
            timers.remove(&(self.when, id));
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
            timers.insert((self.when, id), waker);
            self.inserted = true;

            if is_earliest {
                drop(timers);
                POLLER.interrupt();
            }
        }

        Poll::Pending
    }
}

// ----- Async I/O -----

/// Asynchronous I/O.
pub struct Async<T> {
    source: Box<T>,
    entry: Arc<Entry>,
}

#[cfg(any(unix, docsrs))]
#[cfg_attr(docsrs, doc(cfg(unix)))]
impl<T: AsRawFd> Async<T> {
    /// Converts a non-blocking I/O handle into an async I/O handle.
    pub fn nonblocking(source: T) -> io::Result<Async<T>> {
        Ok(Async {
            entry: POLLER.registry.register(&source)?,
            source: Box::new(source),
        })
    }
}

#[cfg(any(windows, docsrs))]
#[cfg_attr(docsrs, doc(cfg(windows)))]
impl<T: AsRawSocket> Async<T> {
    /// Converts a non-blocking I/O handle into an async I/O handle.
    pub fn nonblocking(source: T) -> io::Result<Async<T>> {
        Ok(Async {
            entry: POLLER.registry.register(&source)?,
            source: Box::new(source),
        })
    }
}

impl<T> Async<T> {
    /// Gets a reference to the I/O source.
    pub fn source(&self) -> &T {
        &self.source
    }

    /// Gets a mutable reference to the I/O source.
    pub fn source_mut(&mut self) -> &mut T {
        &mut self.source
    }

    /// Converts a non-blocking read into an async operation.
    pub async fn read_with<R>(&self, f: impl FnMut(&T) -> io::Result<R>) -> io::Result<R> {
        let mut f = f;
        let mut source = &self.source;
        let wakers = &self.entry.readers;
        future::poll_fn(|cx| Self::poll_io(cx, |s| f(s), &mut source, wakers)).await
    }

    /// Converts a non-blocking read into an async operation.
    pub async fn read_with_mut<R>(
        &mut self,
        f: impl FnMut(&mut T) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut f = f;
        let mut source = &mut self.source;
        let wakers = &self.entry.readers;
        future::poll_fn(|cx| Self::poll_io(cx, |s| f(s), &mut source, wakers)).await
    }

    /// Converts a non-blocking write into an async operation.
    pub async fn write_with<R>(&self, f: impl FnMut(&T) -> io::Result<R>) -> io::Result<R> {
        let mut f = f;
        let mut source = &self.source;
        let wakers = &self.entry.writers;
        future::poll_fn(|cx| Self::poll_io(cx, |s| f(s), &mut source, wakers)).await
    }

    /// Converts a non-blocking write into an async operation.
    pub async fn write_with_mut<R>(
        &mut self,
        f: impl FnMut(&mut T) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut f = f;
        let mut source = &mut self.source;
        let wakers = &self.entry.writers;
        future::poll_fn(|cx| Self::poll_io(cx, |s| f(s), &mut source, wakers)).await
    }

    fn poll_io<S, R>(
        cx: &mut Context<'_>,
        mut f: impl FnMut(&mut S) -> io::Result<R>,
        source: &mut S,
        wakers: &Mutex<Vec<Waker>>,
    ) -> Poll<io::Result<R>> {
        // Attempt the non-blocking operation.
        match f(source) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            res => return Poll::Ready(res),
        }

        // Acquire a lock on the waker list.
        let mut wakers = wakers.lock();

        // Attempt the non-blocking operation again.
        match f(source) {
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
        // Ignore errors because an event in oneshot mode may deregister the fd before we do.
        let _ = POLLER.registry.deregister(&self.entry);
    }
}

impl<T: Read> AsyncRead for Async<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let fut = self.read_with_mut(|source| source.read(buf));
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
        let fut = self.read_with(|source| (&*source).read(buf));
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
        let fut = self.write_with_mut(|source| source.write(buf));
        pin_utils::pin_mut!(fut);
        fut.poll(cx)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let fut = self.write_with_mut(|source| source.flush());
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
        let fut = self.write_with(|source| (&*source).write(buf));
        pin_utils::pin_mut!(fut);
        fut.poll(cx)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let fut = self.write_with(|source| (&*source).flush());
        pin_utils::pin_mut!(fut);
        fut.poll(cx)
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

// ----- Networking -----

impl Async<TcpListener> {
    /// Creates a listener bound to the specified address.
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Async<TcpListener>> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Async::nonblocking(listener)?)
    }

    /// Accepts a new incoming connection.
    pub async fn accept(&self) -> io::Result<(Async<TcpStream>, SocketAddr)> {
        let (stream, addr) = self.read_with(|source| source.accept()).await?;
        stream.set_nonblocking(true)?;
        Ok((Async::nonblocking(stream)?, addr))
    }

    /// Returns a stream over incoming connections.
    pub fn incoming(&self) -> impl Stream<Item = io::Result<Async<TcpStream>>> + Unpin + '_ {
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
        let wait_connect = |stream: &TcpStream| match stream.peer_addr() {
            Err(err) if err.kind() == io::ErrorKind::NotConnected => {
                Err(io::Error::new(io::ErrorKind::WouldBlock, ""))
            }
            res => res.map(|_| ()),
        };
        // The stream becomes writable when connected.
        stream.write_with(|source| wait_connect(source)).await?;

        // Check for connect errors.
        match stream.source().take_error()? {
            None => Ok(stream),
            Some(err) => Err(err),
        }
    }

    /// Receives data from the stream without removing it from the buffer.
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(|source| source.peek(buf)).await
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
        self.write_with(|source| source.send_to(buf, &addr)).await
    }

    /// Sends data to the socket's peer.
    pub async fn send<A: ToSocketAddrs>(&self, buf: &[u8]) -> io::Result<usize> {
        self.write_with(|source| source.send(buf)).await
    }

    /// Receives data from the socket.
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.read_with(|source| source.recv_from(buf)).await
    }

    /// Receives data from the socket's peer.
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(|source| source.recv(buf)).await
    }

    /// Receives data without removing it from the buffer.
    pub async fn peek_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.read_with(|source| source.peek_from(buf)).await
    }

    /// Receives data from the socket's peer without removing it from the buffer.
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(|source| source.peek(buf)).await
    }
}

#[cfg(any(unix, docsrs))]
#[cfg_attr(docsrs, doc(cfg(unix)))]
impl Async<UnixListener> {
    /// Creates a listener bound to the specified path.
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Async<UnixListener>> {
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Async::nonblocking(listener)?)
    }

    /// Accepts a new incoming connection.
    pub async fn accept(&self) -> io::Result<(Async<UnixStream>, UnixSocketAddr)> {
        let (stream, addr) = self.read_with(|source| source.accept()).await?;
        stream.set_nonblocking(true)?;
        Ok((Async::nonblocking(stream)?, addr))
    }

    /// Returns a stream over incoming connections.
    pub fn incoming(&self) -> impl Stream<Item = io::Result<Async<UnixStream>>> + Unpin + '_ {
        Box::pin(stream::unfold(self, |listener| async move {
            let res = listener.accept().await.map(|(stream, _)| stream);
            Some((res, listener))
        }))
    }
}

#[cfg(any(unix, docsrs))]
#[cfg_attr(docsrs, doc(cfg(unix)))]
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

#[cfg(any(unix, docsrs))]
#[cfg_attr(docsrs, doc(cfg(unix)))]
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
        self.write_with(|source| source.send_to(buf, &path)).await
    }

    /// Sends data to the socket's peer.
    pub async fn send<A: ToSocketAddrs>(&self, buf: &[u8]) -> io::Result<usize> {
        self.write_with(|source| source.send(buf)).await
    }

    /// Receives data from the socket.
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, UnixSocketAddr)> {
        self.read_with(|source| source.recv_from(buf)).await
    }

    /// Receives data from the socket's peer.
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(|source| source.recv(buf)).await
    }
}

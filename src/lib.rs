#![forbid(unsafe_code)]
// TODO: #![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

use std::collections::BTreeMap;
use std::convert::TryInto;
use std::error::Error;
use std::fs;
use std::future::Future;
use std::io::{self, Read, Write};
use std::mem;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::os;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{SocketAddr as UnixSocketAddr, UnixDatagram, UnixListener, UnixStream};
use std::panic::catch_unwind;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, ExitStatus, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel as channel;
use futures_core::stream::Stream;
use futures_io::{AsyncBufRead, AsyncRead, AsyncWrite};
use futures_util::{future, stream};
use nix::sys::epoll::{
    epoll_create1, epoll_ctl, epoll_wait, EpollCreateFlags, EpollEvent, EpollFlags, EpollOp,
};
use once_cell::sync::Lazy;
use parking_lot::{Condvar, Mutex};
use slab::Slab;
use socket2::{Domain, Protocol, Socket, Type};

#[cfg(not(any(target_os = "linux", target_os = "android")))]
compile_error!("smol does not support this target OS");

// TODO: fix unwraps
// TODO: if epoll/kqueue/wepoll gets EINTR, then retry - or maybe just call notify()
// TODO: catch panics in wake() and Waker::drop()
// TODO: readme for inspiration: https://github.com/piscisaureus/wepoll
// TODO: implement FusedFuture for Task and Timer?

// ----- Event loop -----

struct Runtime {
    // TODO: rename Runtime to Reactor?
    // TODO: move executor into separate Executor struct (and maybe also have Blocking)
    // TODO: also make Reactor::poll() and Reactor::interrupt()
    // Executor
    receiver: channel::Receiver<Runnable>,
    queue: channel::Sender<Runnable>,
    cvar: Condvar,
    mutex: Mutex<bool>,

    // Timers
    timers: Mutex<BTreeMap<(Instant, usize), Waker>>,

    // Polling I/O events
    poller: Mutex<Poller>,
    registry: Registry,

    // Interrupting epoll/kqueue/wepoll
    notified: AtomicBool,
    socket_notify: Socket,
    socket_wakeup: Async<Socket>,
}

struct Registry {
    epoll: RawFd,
    entries: Mutex<Slab<Arc<Entry>>>,
}

struct Poller {
    epoll_events: Box<[EpollEvent]>,
    bytes: Box<[u8]>,
    wakers: std::collections::VecDeque<Waker>,
}

/// Converts any error into an I/O error.
fn io_err(err: impl Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::Other, Box::new(err))
}

fn initialize() -> io::Result<Runtime> {
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

    let registry = Registry {
        epoll: epoll_create1(EpollCreateFlags::EPOLL_CLOEXEC).map_err(io_err)?,
        entries: Mutex::new(Slab::new()),
    };
    let sock2 = registry.register(sock2)?;

    let (sender, receiver) = channel::unbounded::<Runnable>();
    Ok(Runtime {
        receiver,
        queue: sender,
        cvar: Condvar::new(),
        mutex: Mutex::new(false),

        timers: Mutex::new(BTreeMap::new()),

        // TODO: convert Result to io::Result
        poller: Mutex::new(Poller {
            epoll_events: vec![EpollEvent::empty(); 1000].into_boxed_slice(),
            bytes: vec![0; 1000].into_boxed_slice(),
            wakers: Default::default(),
        }),
        registry,

        notified: AtomicBool::new(false),
        socket_notify: sock1,
        socket_wakeup: sock2,
    })
}

impl Registry {
    fn register<T: AsRawFd>(&self, source: T) -> io::Result<Async<T>> {
        let fd = source.as_raw_fd();
        let entry = {
            let mut entries = self.entries.lock();
            let vacant = entries.vacant_entry();
            let entry = Arc::new(Entry {
                fd,
                index: vacant.key(),
                readers: Mutex::new(Vec::new()),
                writers: Mutex::new(Vec::new()),
            });
            vacant.insert(entry.clone());
            entry
        };

        // TODO: handle epoll errors
        epoll_ctl(
            self.epoll,
            EpollOp::EpollCtlAdd,
            fd,
            Some(&mut EpollEvent::new(
                EpollFlags::EPOLLET
                    | EpollFlags::EPOLLIN
                    | EpollFlags::EPOLLOUT
                    | EpollFlags::EPOLLRDHUP,
                entry.index as u64,
            )),
        )
        .map_err(io_err)?;

        // TODO: if epoll fails, remove the entry
        Ok(Async {
            source: Box::new(source),
            entry,
        })
    }

    // TODO: we probably don't need to pass fd because it's in the entry
    fn unregister(&self, fd: RawFd, index: usize) {
        self.entries.lock().remove(index);
        // Ignore errors because an event in oneshot mode may unregister the fd before we do.
        let _ = epoll_ctl(self.epoll, EpollOp::EpollCtlDel, fd, None);
    }
}

static RT: Lazy<Runtime> = Lazy::new(|| initialize().expect("cannot initialize smol runtime"));

fn poll(block: bool) -> bool {
    let mut poller = match RT.poller.try_lock() {
        None => return false,
        Some(poller) => poller,
    };

    // Reset the interrupt flag and the wakeup socket.
    let notified = RT.notified.swap(false, Ordering::SeqCst);
    if notified {
        loop {
            match RT.socket_wakeup.source().read(&mut poller.bytes) {
                Ok(n) if n > 0 => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                _ => break,
            }
        }
    }

    let mut timeout = poll_timers(&mut poller);
    if notified || !block {
        timeout = Some(Duration::from_secs(0));
    }
    poll_io(&mut poller, timeout);

    true
}

fn poll_timers(poller: &mut Poller) -> Option<Duration> {
    let now = Instant::now();

    let (ready, next_event) = {
        let mut timers = RT.timers.lock();
        let pending = timers.split_off(&(now, 0));
        let ready = mem::replace(&mut *timers, pending);
        let next_event = timers.keys().next().map(|(when, _)| *when - now);
        (ready, next_event)
    };

    // Wake up ready timers.
    for (_, waker) in ready {
        waker.wake();
    }

    next_event
}

fn poll_io(poller: &mut Poller, timeout: Option<Duration>) {
    poller.wakers.clear();

    let timeout_ms = match timeout {
        None => -1,
        Some(t) => t.as_millis().try_into().expect("timer duration overflow"),
    };
    let n = epoll_wait(RT.registry.epoll, &mut poller.epoll_events, timeout_ms).unwrap();

    if n > 0 {
        let entries = RT.registry.entries.lock();
        for ev in &poller.epoll_events[..n] {
            let is_read = ev.events() != EpollFlags::EPOLLOUT;
            let is_write = ev.events() != EpollFlags::EPOLLIN;
            let index = ev.data() as usize;

            if let Some(entry) = entries.get(index) {
                if is_read {
                    for w in entry.readers.lock().drain(..) {
                        poller.wakers.push_back(w);
                    }
                }
                if is_write {
                    for w in entry.writers.lock().drain(..) {
                        poller.wakers.push_front(w);
                    }
                }
            }
        }
    }

    for waker in poller.wakers.drain(..) {
        waker.wake();
    }
}

fn interrupt() {
    if !RT.notified.load(Ordering::SeqCst) {
        if !RT.notified.swap(true, Ordering::SeqCst) {
            loop {
                match (&RT.socket_notify).write(&[1]) {
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    _ => break,
                }
            }
        }
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
        RT.timers.lock().remove(&(self.when, id));
        self.inserted = false;
    }
}

impl Future for Timer {
    type Output = Instant;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let id = &mut *self as *mut Timer as usize;
        let mut timers = RT.timers.lock();

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
                interrupt();
            }
        }

        Poll::Pending
    }
}

// ----- Executor -----

/// A runnable future, ready for execution.
type Runnable = async_task::Task<()>;

/// A spawned future.
#[must_use]
pub struct Task<T>(async_task::JoinHandle<T, ()>);

impl<T: Send + 'static> Task<T> {
    /// Spawns a global future.
    ///
    /// This future is allowed to be stolen by another executor.
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        // Create a runnable and schedule it for execution.
        let schedule = |runnable| {
            RT.queue.send(runnable).unwrap();
            interrupt();
        };
        let (runnable, handle) = async_task::spawn(future, schedule, ());
        runnable.schedule();

        // Return a join handle that retrieves the output of the future.
        Task(handle)
    }

    /// Spawns a future onto the blocking thread pool.
    pub fn blocking(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        crate::blocking(future)
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
        // let (runnable, handle) = async_task::spawn_local(future, |t| todo!(), ());
        // runnable.schedule();
        // TODO: panic if not called inside a worker started with worker()
        // TODO
        todo!()
    }
}

impl Task<()> {
    pub fn detach(self) {}
}

impl<E: std::fmt::Debug + Send + 'static> Task<Result<(), E>> {
    pub fn detach(self) {
        Task::spawn(async { self.await.unwrap() }).detach();
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

/// Executes all futures until the main one completes.
pub fn run<T>(future: impl Future<Output = T>) -> T {
    pin_utils::pin_mut!(future);

    // TODO: what is the behavior of nested run()s
    //  - maybe just panic

    // TODO Optimization: use thread-local cache for ready and queue
    let ready = Arc::new(AtomicBool::new(true));

    let waker = async_task::waker_fn({
        let ready = ready.clone();
        move || {
            if !ready.swap(true, Ordering::SeqCst) {
                interrupt();
                let _m = RT.mutex.lock();
                RT.cvar.notify_all();
            }
        }
    });

    loop {
        while !ready.load(Ordering::SeqCst) {
            match RT.receiver.try_recv() {
                Ok(runnable) => {
                    let _ = catch_unwind(|| runnable.run());
                }
                Err(_) => {
                    let mut m = RT.mutex.lock();

                    if *m {
                        if !ready.load(Ordering::SeqCst) {
                            RT.cvar.wait(&mut m);
                        }
                        continue;
                    }

                    *m = true;
                    drop(m);
                    // TODO: if this panics, set m to false and notify

                    if !ready.load(Ordering::SeqCst) {
                        poll(true);
                    }

                    m = RT.mutex.lock();
                    *m = false;
                    RT.cvar.notify_one();
                }
            }
        }

        ready.store(false, Ordering::SeqCst);
        match future.as_mut().poll(&mut Context::from_waker(&waker)) {
            Poll::Pending => {}
            Poll::Ready(val) => return val,
        }
    }
}

// ----- Blocking -----

/// Moves blocking code onto a thread.
#[macro_export]
macro_rules! blocking {
    ($($expr:tt)*) => {
        smol::Task::blocking(async move { $($expr)* }).await
    };
}

/// Blocks on a single future.
pub fn block_on<T>(future: impl Future<Output = T>) -> T {
    todo!()
}

/// Converts a blocking iterator into a stream.
pub fn iter<T>(t: T) -> impl Stream<Item = T::Item> + Unpin + 'static
where
    T: Iterator + Send + 'static,
{
    // NOTE: stop task if the returned handle is dropped
    todo!();
    stream::empty()
}

/// Converts a blocking reader into an async reader.
pub fn reader(t: impl Read + Send + 'static) -> impl AsyncBufRead + Unpin + 'static {
    // TODO: should we simply return Reader here?
    // NOTE: stop task if the returned handle is dropped
    todo!();
    futures_util::io::empty()
}

/// Converts a blocking writer into an async writer.
pub fn writer(t: impl Write + Send + 'static) -> impl AsyncWrite + Unpin + 'static {
    // TODO: should we simply return Writer here?
    // NOTE: stop task if the returned handle is dropped
    todo!();
    futures_util::io::sink()
}

/// Blocks on async I/O.
pub struct BlockOn; // TODO

// TODO: struct ThreadPool and method fn spawn()
//   - Task::blocking(fut) then calls THREAD_POOL.spawn(fut)

/// Spawns a future onto the blocking thread pool.
fn blocking<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
    // TODO: ignore panics

    use std::sync::atomic::*;
    static SLEEPING: AtomicUsize = AtomicUsize::new(0);

    struct Pool {
        sender: channel::Sender<Runnable>,
        receiver: channel::Receiver<Runnable>,
    }

    static POOL: Lazy<Pool> = Lazy::new(|| {
        // Start a single worker thread waiting for the first task.
        start_thread();

        let (sender, receiver) = channel::unbounded();
        Pool { sender, receiver }
    });

    fn start_thread() {
        SLEEPING.fetch_add(1, Ordering::SeqCst);
        let timeout = Duration::from_secs(1);

        thread::Builder::new()
            .name("async-std/blocking".to_string())
            .spawn(move || {
                loop {
                    let mut runnable = match POOL.receiver.recv_timeout(timeout) {
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
                        runnable = match POOL.receiver.try_recv() {
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

    let (runnable, handle) = async_task::spawn(future, |r| POOL.sender.send(r).unwrap(), ());
    runnable.schedule();
    Task(handle)
}

// ----- Async I/O -----

/// Asynchronous I/O.
pub struct Async<T> {
    source: Box<T>,
    entry: Arc<Entry>,
}

impl<T> Drop for Async<T> {
    fn drop(&mut self) {
        // TODO: call entry.unregister();
        // TODO: what about this unwrap?
        RT.registry.unregister(self.entry.fd, self.entry.index);
    }
}

struct Entry {
    fd: RawFd,
    index: usize,
    readers: Mutex<Vec<Waker>>,
    writers: Mutex<Vec<Waker>>,
}

impl<T: AsRawFd> Async<T> {
    /// Converts a non-blocking I/O handle into an async I/O handle.
    pub fn nonblocking(source: T) -> io::Result<Async<T>> {
        RT.registry.register(source)
    }

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

impl<T: AsRawFd + Read> AsyncRead for Async<T> {
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

impl<T: AsRawFd> AsyncRead for &Async<T>
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

impl<T: AsRawFd + Write> AsyncWrite for Async<T> {
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

impl<T: AsRawFd> AsyncWrite for &Async<T>
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

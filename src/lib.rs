#![forbid(unsafe_code)]
// TODO: #![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

use std::collections::BTreeMap;
use std::convert::TryInto;
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
use futures_io::{AsyncRead, AsyncWrite};
use futures_util::{future, stream};
use nix::sys::epoll::{
    epoll_create1, epoll_ctl, epoll_wait, EpollCreateFlags, EpollEvent, EpollFlags, EpollOp,
};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use slab::Slab;
use socket2::{Domain, Protocol, Socket, Type};

#[cfg(not(any(target_os = "linux", target_os = "android")))]
compile_error!("smol does not support this target OS");

// TODO: hello world example
// TODO: example with uds_windows crate
// TODO: example with stdin
// TODO: example with pinned threads
// TODO: example with process spawn and output
// TODO: example with process child I/O
// TODO: example with timeout
// TODO: example with OS timers
// TODO: example with Async::reader(std::io::stdin())
// TODO: example with Async::writer(std::io::stdout())
// TODO: example with signal-hook
//  - Async::iter(Signals::new(&[SIGINT])?)
// TODO: example with ctrl-c
// TODO: example with filesystem
// TODO: example that prints a file
// TODO: example with hyper
// TODO: generate OS-specific docs with --cfg docsrs
//       #[cfg(any(windows, docsrs))]
//       #[cfg_attr(docsrs, doc(cfg(windows)))]
// TODO: task cancellation?
// TODO: fix unwraps
// TODO: if epoll/kqueue/wepoll gets EINTR, then retry - or maybe just call notify()
// TODO: catch panics in wake() and Waker::drop()
// TODO: readme for inspiration: https://github.com/piscisaureus/wepoll
// TODO: constructors like Async::<TcpStream>::new(std_tcpstream)

// ----- Event loop -----

struct Runtime {
    // Executor
    queue: channel::Sender<Runnable>,

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
    wakers: Vec<Waker>,
}

fn initialize() -> io::Result<Runtime> {
    thread::spawn(|| loop {
        poll(true);
    });

    let (sender, receiver) = channel::unbounded::<Runnable>();
    for _ in 0..num_cpus::get().max(1) {
        let receiver = receiver.clone();
        thread::spawn(move || {
            receiver.iter().for_each(|runnable| {
                let _ = catch_unwind(|| runnable.run());
            })
        });
    }

    // https://stackoverflow.com/questions/24933411/how-to-emulate-socket-socketpair-on-windows
    // https://github.com/mhils/backports.socketpair/blob/master/backports/socketpair/__init__.py
    // https://github.com/python-trio/trio/blob/master/trio/_core/_wakeup_socketpair.py
    // https://gist.github.com/geertj/4325783
    //
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
        epoll: epoll_create1(EpollCreateFlags::EPOLL_CLOEXEC).expect("cannot create epoll"),
        entries: Mutex::new(Slab::new()),
    };
    let sock2 = registry.register(sock2);

    Ok(Runtime {
        registry,
        // TODO: convert Result to io::Result
        timers: Mutex::new(BTreeMap::new()),
        queue: sender,
        poller: Mutex::new(Poller {
            epoll_events: vec![EpollEvent::empty(); 1000].into_boxed_slice(),
            bytes: vec![0; 1000].into_boxed_slice(),
            wakers: Vec::new(),
        }),
        notified: AtomicBool::new(false),
        socket_notify: sock1,
        socket_wakeup: sock2,
    })
}

impl Registry {
    fn register<T: AsRawFd>(&self, source: T) -> Async<T> {
        let fd = source.as_raw_fd();
        let entry = {
            let mut entries = self.entries.lock();
            let vacant = entries.vacant_entry();
            let entry = Arc::new(Entry {
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
        .unwrap();

        // TODO: if epoll fails, remove the entry
        Async {
            source: Box::new(source),
            flavor: Flavor::Socket(Registration { fd, entry }),
        }
    }
}

static RT: Lazy<Runtime> = Lazy::new(|| initialize().expect("cannot initialize smol runtime"));

fn poll(block: bool) -> bool {
    let mut poller = match RT.poller.try_lock() {
        None => return false,
        Some(poller) => poller,
    };

    // Reset the interrupt flag and the wakeup socket.
    if RT.notified.swap(false, Ordering::SeqCst) {
        loop {
            match RT.socket_wakeup.source().read(&mut poller.bytes) {
                Ok(n) if n > 0 => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                _ => break,
            }
        }
    }

    let mut timeout = poll_timers(&mut poller);
    if !block {
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
                    poller.wakers.append(&mut entry.readers.lock());
                }
                if is_write {
                    poller.wakers.append(&mut entry.writers.lock());
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

// ----- Executor -----

/// A runnable future, ready for execution.
type Runnable = async_task::Task<()>;

/// A scheduled future.
pub struct Task<T>(async_task::JoinHandle<T, ()>);

impl<T> Task<T> {
    /// Starts an executor and runs a future on it.
    pub fn run<F>(future: F) -> T
    where
        F: Future<Output = T>,
    {
        // TODO: run() should propagate panics into caller
        // TODO: when run() finishes, we need to wake another worker()
        //   - because all workers could be stuck on the parker and nobody on epoll
        //   - should we also poll to clear up timers, pending operations, and so on?

        // let handle = spawn(future);
        todo!()
    }

    /// Schedules a future for execution.
    ///
    /// This future is allowed to be stolen by another executor.
    pub fn schedule<F>(future: F) -> Task<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        // Create a runnable and schedule it for execution.
        let (runnable, handle) = async_task::spawn(future, |r| RT.queue.send(r).unwrap(), ());
        runnable.schedule();

        // Return a join handle that retrieves the output of the future.
        Task(handle)
    }

    /// Schedules a future on the current executor.
    ///
    /// Panics if not called within an executor.
    pub fn local<F>(future: F) -> Task<T>
    where
        F: Future<Output = T> + 'static,
        T: 'static,
    {
        // let (runnable, handle) = async_task::spawn_local(future, |t| todo!(), ());
        // runnable.schedule();
        // TODO: panic if not called inside a worker started with run()
        todo!()
    }

    /// Schedules a future on the blocking thread pool.
    pub fn blocking<F>(future: F) -> Task<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
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

// ----- Async I/O -----

struct Entry {
    index: usize,
    readers: Mutex<Vec<Waker>>,
    writers: Mutex<Vec<Waker>>,
}

struct Registration {
    fd: RawFd,
    entry: Arc<Entry>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        RT.registry.entries.lock().remove(self.entry.index);
        epoll_ctl(RT.registry.epoll, EpollOp::EpollCtlDel, self.fd, None).unwrap();
    }
}

/// Asynchronous I/O.
pub struct Async<T: ?Sized> {
    flavor: Flavor,
    source: Box<T>,
}

enum Flavor {
    Socket(Registration),
    Timer(Instant, bool),
}

impl<T> Async<T> {
    /// Turns a blocking iterator into an async stream.
    pub fn iter(t: T) -> Async<dyn Iterator<Item = T::Item>>
    where
        T: Iterator + 'static,
    {
        // NOTE: stop task if the returned handle is dropped
        todo!()
    }

    /// Turns a blocking reader into an async reader.
    pub fn reader(t: T) -> Async<dyn Read>
    where
        T: Read + 'static,
    {
        // NOTE: stop task if the returned handle is dropped
        todo!()
    }

    /// Turns a blocking writer into an async writer.
    pub fn writer(t: T) -> Async<dyn Write>
    where
        T: Write + 'static,
    {
        // NOTE: stop task if the returned handle is dropped
        todo!()
    }

    /// Turns a non-blocking I/O handle into an async I/O handle.
    pub fn nonblocking(source: T) -> Async<T>
    where
        T: AsRawFd,
    {
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

    pub fn into_source(self) -> T {
        // TODO: take from the Option<Box> and then Drop ignores it
        todo!()
    }

    /// Turns a non-blocking read into an async operation.
    pub async fn read_with<'a, R>(
        &'a self,
        f: impl FnMut(&'a T) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut f = f;
        // future::poll_fn(|cx| self.poll_with(cx, &self.entry().readers, &mut f)).await
        todo!()
    }

    pub async fn read_with_mut<'a, R>(
        &'a mut self,
        f: impl FnMut(&'a mut T) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut f = f;
        // future::poll_fn(|cx| self.poll_with(cx, &self.entry().readers, &mut f)).await
        todo!()
    }

    /// Turns a non-blocking write into an async operation.
    pub async fn write_with<'a, R>(
        &'a self,
        f: impl FnMut(&'a T) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut f = f;
        // future::poll_fn(|cx| self.poll_with(cx, &self.entry().writers, &mut f)).await
        todo!()
    }

    pub async fn write_with_mut<'a, R>(
        &'a mut self,
        f: impl FnMut(&'a mut T) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut f = f;
        // future::poll_fn(|cx| self.poll_with(cx, &self.entry().writers, &mut f)).await
        todo!()
    }

    fn poll_with<'a, R>(
        &'a self,
        cx: &mut Context<'_>,
        dir: Direction,
        f: impl FnMut(&'a T) -> io::Result<R>,
    ) -> Poll<io::Result<R>> {
        // let mut f = f;
        //
        // // Attempt the non-blocking operation.
        // match f(&mut self.source) {
        //     Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
        //     res => return Poll::Ready(res),
        // }
        //
        // // Acquire a lock on the waker list.
        // let mut wakers = wakers.lock();
        //
        // // Attempt the non-blocking operation again.
        // match f(&mut self.source) {
        //     Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
        //     res => return Poll::Ready(res),
        // }
        //
        // // If it would still block, register the curent waker and return.
        // if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
        //     wakers.push(cx.waker().clone());
        // }
        // Poll::Pending
        todo!()
    }

    fn poll_with_mut<'a, R>(
        &'a mut self,
        cx: &mut Context<'_>,
        dir: Direction,
        // wakers: &Mutex<Vec<Waker>>,
        f: impl FnMut(&'a mut T) -> io::Result<R>,
    ) -> Poll<io::Result<R>> {
        todo!()
    }

    fn entry(&self) -> &Entry {
        match &self.flavor {
            Flavor::Socket(reg) => &reg.entry,
            Flavor::Timer { .. } => unreachable!(),
        }
    }

    fn entry_mut(&mut self) -> &Entry {
        match &mut self.flavor {
            Flavor::Socket(reg) => &mut reg.entry,
            Flavor::Timer { .. } => unreachable!(),
        }
    }
}

impl Async<Instant> {
    /// Completes after the specified duration of time.
    pub fn timer(dur: Duration) -> Async<Instant> {
        Async {
            source: Box::new(todo!()), // TODO: have Box<Option>, set to None?
            flavor: Flavor::Timer(Instant::now() + dur, false),
        }
    }
}

enum Direction {
    Read,
    Write,
}

impl<T: ?Sized> Drop for Async<T> {
    fn drop(&mut self) {
        let id = self as *mut Async<T> as usize;
        if let Flavor::Timer(when, inserted) = &self.flavor {
            if *inserted {
                RT.timers.lock().remove(&(*when, id));
            }
        }
    }
}

impl Future for Async<Instant> {
    type Output = Instant;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let id = &mut *self as *mut Async<Instant> as usize;
        let mut timers = RT.timers.lock();

        match &mut self.flavor {
            Flavor::Socket(..) => Poll::Pending,
            Flavor::Timer(when, inserted) => {
                if Instant::now() >= *when {
                    timers.remove(&(*when, id));
                    return Poll::Ready(*when);
                }

                if !*inserted {
                    let mut is_earliest = false;
                    if let Some((first, _)) = timers.keys().next() {
                        if *when < *first {
                            is_earliest = true;
                        }
                    }

                    let waker = cx.waker().clone();
                    timers.insert((*when, id), waker);
                    *inserted = true;

                    if is_earliest {
                        drop(timers);
                        interrupt();
                    }
                }

                Poll::Pending
            }
        }
    }
}

// ----- Async trait impls -----

impl<T> Stream for Async<dyn Iterator<Item = T>> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        todo!()
    }
}

impl AsyncRead for Async<dyn Read> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        todo!()
    }
}

impl AsyncWrite for Async<dyn Write> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        todo!()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        todo!()
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
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

// ----- Stdio -----

// NOTE: we can return Async<Stdin> because in AsyncRead we can read from the byte channel
// - but wait! Async<Stdin> doesn't impl AsyncRead because &Stdin doesn't impl Read
// - so we have to use Async<dyn Read> or impl AsyncRead manually for all types
// - if we do it manually, what about uds_windows?
// NOTE: Async<TcpStream> can be converted to Async<dyn Read>

// impl Async<Stdin> {
// Returns an async writer into stdin.
// TODO: Async::stdin() -> Async<Stdin>
// TODO: Async::read_line()
// }

// NOTE: what happens if we drop stdout? how do we know when data is flushed
//   - solution: there is poll_flush()

// Returns an async reader from stdout.
// TODO: Async::stdout() -> Async<Stdout>

// Returns an async reader from stderr.
// TODO: Async::stderr() -> Async<Stderr>
// }

// ----- Process -----

impl Async<Command> {
    /// Executes a command and returns its output.
    pub async fn output(cmd: Command) -> io::Result<Output> {
        let mut cmd = cmd;
        Task::blocking(async move { cmd.output() }).await
    }

    /// Executes a command and returns its exit status.
    pub async fn status(cmd: Command) -> io::Result<ExitStatus> {
        let mut cmd = cmd;
        Task::blocking(async move { cmd.status() }).await
    }
}

impl Async<Child> {
    /// Waits for a child process to exit and returns its exit status.
    pub async fn wait(child: Child) -> io::Result<ExitStatus> {
        let mut child = child;
        Task::blocking(async move { child.wait() }).await
    }

    /// Waits for a child process to exit and returns its output.
    pub async fn wait_with_output(child: Child) -> io::Result<Output> {
        Task::blocking(async move { child.wait_with_output() }).await
    }
}

// ----- Networking -----

impl Async<TcpListener> {
    /// Creates a listener bound to the specified address.
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Async<TcpListener>> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Async::nonblocking(listener))
    }

    /// Accepts a new incoming connection.
    pub async fn accept(&self) -> io::Result<(Async<TcpStream>, SocketAddr)> {
        let (stream, addr) = self.read_with(|source| source.accept()).await?;
        stream.set_nonblocking(true)?;
        Ok((Async::nonblocking(stream), addr))
    }

    /// Returns a stream over incoming connections.
    pub fn incoming(&self) -> impl Stream<Item = io::Result<Async<TcpStream>>> + Unpin + '_ {
        // TODO: can we return Async<Incoming>?
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
        let stream = Async::nonblocking(socket.into_tcp_stream());

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
        Ok(Async::nonblocking(socket))
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
        Ok(Async::nonblocking(listener))
    }

    /// Accepts a new incoming connection.
    pub async fn accept(&self) -> io::Result<(Async<UnixStream>, UnixSocketAddr)> {
        let (stream, addr) = self.read_with(|source| source.accept()).await?;
        stream.set_nonblocking(true)?;
        Ok((Async::nonblocking(stream), addr))
    }

    /// Returns a stream over incoming connections.
    pub fn incoming(&self) -> impl Stream<Item = io::Result<Async<UnixStream>>> + Unpin + '_ {
        // TODO: can we return Async<Incoming>?
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
        Ok(Async::nonblocking(stream))
    }

    /// Creates an unnamed pair of connected streams.
    pub fn pair() -> io::Result<(Async<UnixStream>, Async<UnixStream>)> {
        let (stream1, stream2) = UnixStream::pair()?;
        stream1.set_nonblocking(true)?;
        stream2.set_nonblocking(true)?;
        Ok((Async::nonblocking(stream1), Async::nonblocking(stream2)))
    }
}

impl Async<UnixDatagram> {
    /// Creates a socket bound to the specified path.
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Async<UnixDatagram>> {
        let socket = UnixDatagram::bind(path)?;
        socket.set_nonblocking(true)?;
        Ok(Async::nonblocking(socket))
    }

    /// Creates a socket not bound to any address.
    pub fn unbound() -> io::Result<Async<UnixDatagram>> {
        let socket = UnixDatagram::unbound()?;
        socket.set_nonblocking(true)?;
        Ok(Async::nonblocking(socket))
    }

    /// Creates an unnamed pair of connected sockets.
    pub fn pair() -> io::Result<(Async<UnixDatagram>, Async<UnixDatagram>)> {
        let (socket1, socket2) = UnixDatagram::pair()?;
        socket1.set_nonblocking(true)?;
        socket2.set_nonblocking(true)?;
        Ok((Async::nonblocking(socket1), Async::nonblocking(socket2)))
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

// ----- Filesystem -----

impl Async<()> {
    pub async fn canonicalize<P: AsRef<Path>>(path: P) -> io::Result<PathBuf> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::canonicalize(path) }).await
    }

    pub async fn copy<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> io::Result<u64> {
        let src = src.as_ref().to_owned();
        let dst = dst.as_ref().to_owned();
        Task::blocking(async move { fs::copy(src, dst) }).await
    }

    pub async fn create_dir<P: AsRef<Path>>(path: P) -> io::Result<()> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::create_dir(path) }).await
    }

    pub async fn create_dir_all<P: AsRef<Path>>(path: P) -> io::Result<()> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::create_dir_all(path) }).await
    }

    pub async fn hard_link<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> io::Result<()> {
        let src = src.as_ref().to_owned();
        let dst = dst.as_ref().to_owned();
        Task::blocking(async move { fs::hard_link(src, dst) }).await
    }

    pub async fn metadata<P: AsRef<Path>>(path: P) -> io::Result<fs::Metadata> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::metadata(path) }).await
    }

    pub async fn read<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::read(path) }).await
    }

    pub async fn read_dir<P: AsRef<Path>>(path: P) -> io::Result<Async<fs::ReadDir>> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { todo!() }).await
    }

    pub async fn read_link<P: AsRef<Path>>(path: P) -> io::Result<PathBuf> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::read_link(path) }).await
    }

    pub async fn read_to_string<P: AsRef<Path>>(path: P) -> io::Result<String> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::read_to_string(path) }).await
    }

    pub async fn remove_dir<P: AsRef<Path>>(path: P) -> io::Result<()> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::remove_dir(path) }).await
    }

    pub async fn remove_dir_all<P: AsRef<Path>>(path: P) -> io::Result<()> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::remove_dir_all(path) }).await
    }

    pub async fn remove_file<P: AsRef<Path>>(path: P) -> io::Result<()> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::remove_file(path) }).await
    }

    pub async fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<()> {
        let from = from.as_ref().to_owned();
        let to = to.as_ref().to_owned();
        Task::blocking(async move { fs::rename(from, to) }).await
    }

    pub async fn set_permissions<P: AsRef<Path>>(path: P, perm: fs::Permissions) -> io::Result<()> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::set_permissions(path, perm) }).await
    }

    pub async fn symlink_metadata<P: AsRef<Path>>(path: P) -> io::Result<fs::Metadata> {
        let path = path.as_ref().to_owned();
        Task::blocking(async move { fs::symlink_metadata(path) }).await
    }

    pub async fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) -> io::Result<()> {
        let path = path.as_ref().to_owned();
        let contents = contents.as_ref().to_owned();
        Task::blocking(async move { fs::write(path, contents) }).await
    }

    // TODO: unix-only
    pub async fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> io::Result<()> {
        let src = src.as_ref().to_owned();
        let dst = dst.as_ref().to_owned();
        Task::blocking(async move { os::unix::fs::symlink(src, dst) }).await
    }

    // TODO: windows-only
    // pub async fn symlink_dir<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> io::Result<()> {
    //     let src = src.as_ref().to_owned();
    //     let dst = dst.as_ref().to_owned();
    //     Task::blocking(async move { os::windows::fs::symlink_dir(src, dst) }).await
    // }

    // TODO: windows-only
    // pub async fn symlink_file<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> io::Result<()> {
    //     let src = src.as_ref().to_owned();
    //     let dst = dst.as_ref().to_owned();
    //     Task::blocking(async move { os::windows::fs::symlink_dir(src, dst) }).await
    // }
}

impl Stream for Async<fs::ReadDir> {
    type Item = io::Result<fs::DirEntry>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        todo!()
    }
}

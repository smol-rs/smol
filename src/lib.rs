#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::convert::TryInto;
use std::future::Future;
use std::io::{self, Read, Write};
use std::mem;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::io::{AsRawFd, RawFd};
use std::panic::catch_unwind;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel as channel;
use futures_io::{AsyncRead, AsyncWrite};
use futures_util::future;
use nix::sys::epoll::{
    epoll_create1, epoll_ctl, epoll_wait, EpollCreateFlags, EpollEvent, EpollFlags, EpollOp,
};
use once_cell::sync::Lazy;
use slab::Slab;
use socket2::{Domain, Protocol, Socket, Type};

#[cfg(not(any(target_os = "linux", target_os = "android")))]
compile_error!("smol does not support this target OS");

// TODO: hello world example
// TODO: example with stdin
// TODO: example with process spawn and output
// TODO: example with timeout
// TODO: example with OS timers
// TODO: do we need a crate like async-pipe, which is like async byte channel?
//   - impls AsyncRead and AsyncWrite
//   - impls Stream
//   - that allows us to "pipe" stdin into main task
// TODO: example with filesystem
// TODO: example that prints a file
// TODO: example with ctrl-c
// TODO: example with hyper
// TODO: generate OS-specific docs with --cfg docsrs
//       #[cfg(any(windows, docsrs))]
//       #[cfg_attr(docsrs, doc(cfg(windows)))]
// TODO: task cancellation?
// TODO: fix unwraps
// TODO: if epoll/kqueue/wepoll gets EINTR, then retry - maybe just call notify()
// TODO: catch panics in wake()
// TODO: use async mutex for locking timer and poller?

// ----- Globals -----

struct Runtime {
    epoll: RawFd,
    entries: Mutex<Slab<Arc<Entry>>>,
    timers: Mutex<BTreeMap<(Instant, usize), Waker>>,
    queue: channel::Sender<Runnable>,

    poller: Mutex<Poller>,

    notified: AtomicBool,
    // sock_notify: Socket,
    // sock_wakeup: Async<Socket>,
}

static SOCKETS: Lazy<(Socket, Async<Socket>)> = Lazy::new(|| {
    &RT;
    wakeup_sockets().unwrap()
});

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

    Ok(Runtime {
        epoll: epoll_create1(EpollCreateFlags::EPOLL_CLOEXEC).expect("cannot create epoll"),
        entries: Mutex::new(Slab::new()),
        timers: Mutex::new(BTreeMap::new()),
        queue: sender,
        poller: Mutex::new(Poller {
            epoll_events: vec![EpollEvent::empty(); 1000].into_boxed_slice(),
        }),
        notified: AtomicBool::new(false),
        // sock_notify: sock1,
        // sock_wakeup: sock2,
    })
}

static RT: Lazy<Runtime> = Lazy::new(|| initialize().expect("cannot initialize smol runtime"));

// ----- Poller -----

struct Poller {
    epoll_events: Box<[EpollEvent]>,
}

fn poll(block: bool) -> bool {
    let mut poller = match RT.poller.try_lock() {
        Ok(poller) => poller,
        Err(_) => return false,
    };

    if RT.notified.swap(false, Ordering::SeqCst) {
        // TODO: cache this buffer somewhere in Runtime
        let mut tmp = [0; 1024];
        loop {
            match (&*SOCKETS.1.source()).read(&mut tmp) {
                Ok(n) if n > 0 => {}
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

    let mut timers = RT.timers.lock().unwrap();
    let pending = timers.split_off(&(now, 0));
    let ready = mem::replace(&mut *timers, pending);
    let next_timer = timers.keys().next().map(|(when, _)| *when - now);
    drop(timers);

    // Wake up ready timers.
    for (_, waker) in ready {
        waker.wake();
    }

    next_timer
}

fn poll_io(poller: &mut Poller, timeout: Option<Duration>) {
    let timeout_ms = match timeout {
        None => -1,
        Some(t) => t.as_millis().try_into().expect("timer duration overflow"),
    };
    let n = epoll_wait(RT.epoll, &mut poller.epoll_events, timeout_ms).unwrap();

    if n == 0 {
        return;
    }

    let entries = RT.entries.lock().unwrap();

    for ev in &poller.epoll_events[..n] {
        let events = ev.events();
        let index = ev.data() as usize;

        if let Some(entry) = entries.get(index) {
            if events != EpollFlags::EPOLLOUT {
                for waker in entry.readers.lock().unwrap().drain(..) {
                    waker.wake();
                }
            }
            if events != EpollFlags::EPOLLIN {
                for waker in entry.writers.lock().unwrap().drain(..) {
                    waker.wake();
                }
            }
        }
    }
}

fn notify() {
    if !RT.notified.load(Ordering::SeqCst) {
        if !RT.notified.swap(true, Ordering::SeqCst) {
            loop {
                match (&SOCKETS.0).write(&[1]) {
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    _ => break,
                }
            }
        }
    }
}

/// Returns a (writer, reader) pair of sockets for waking up the poller.
///
/// https://stackoverflow.com/questions/24933411/how-to-emulate-socket-socketpair-on-windows
/// https://github.com/mhils/backports.socketpair/blob/master/backports/socketpair/__init__.py
/// https://github.com/python-trio/trio/blob/master/trio/_core/_wakeup_socketpair.py
/// https://gist.github.com/geertj/4325783
fn wakeup_sockets() -> io::Result<(Socket, Async<Socket>)> {
    // Create a temporary listener.
    let listener = Socket::new(Domain::ipv4(), Type::stream(), None)?;
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into();
    listener.bind(&addr)?;
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

    Ok((sock1, Async::register(sock2)))
}

// ----- Executor -----

// Runs the future to completion on a new worker (have Mutex<Vec<Stealer>>)
// there will be no hidden threadpool!!
/// Starts an executor and runs a future on it.
pub fn run<F, T>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    // TODO: run() should propagate panics into caller
    // TODO: when run() finishes, we need to wake another worker()
    //   - because all workers could be stuck on the parker and nobody on epoll
    //   - should we also poll to clear up timers, pending operations, and so on?

    // let handle = spawn(future);
    todo!("run tasks from the queue until handle completes")

    // Start a threadpool.
    // for _ in 0..num_cpus::get().max(1) {
    //     thread::spawn(|| smol::run(future::pending()));
    // }

    // Start a stoppable threadpool.
    // let mut pool = vec![];
    // for _ in 0..num_cpus::get().max(1) {
    //     let (s, r) = oneshot::channel<()>();
    //     pool.push(s);
    //     thread::spawn(|| smol::run(async move { drop(r.await) }));
    // }
    // drop(pool); // stops the threadpool!
}

/// A spawned future and its current state.
type Runnable = async_task::Task<()>;

/// Awaits the output of a scheduled future.
pub struct Task<T>(async_task::JoinHandle<T, ()>);

impl<T> Task<T> {
    /// Schedules a future for execution.
    pub fn schedule<F>(future: F) -> Task<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        // Create a runnable and schedule it for execution.
        let (runnable, handle) = async_task::spawn(future, |t| RT.queue.send(t).unwrap(), ());
        runnable.schedule();

        // Return a join handle that retrieves the output of the future.
        Task(handle)
    }

    pub fn local<F>(future: F) -> Task<T>
    where
        F: Future<Output = T>,
    {
        todo!()
    }

    pub fn blocking<F>(future: F) -> Task<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        todo!()
    }
}

impl<T> Future for Task<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // NOTE: always resume panics into Task with a "task cancelled" message on cancel
        match Pin::new(&mut self.0).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(output) => Poll::Ready(output.expect("task failed")),
        }
    }
}

// ----- Blocking -----

// TODO

// ----- Timer -----

/// Completes at a certain point in time.
pub struct Timer {
    when: Instant,
    inserted: bool,
}

impl Timer {
    pub fn at(when: Instant) -> Timer {
        Timer {
            when,
            inserted: false,
        }
    }

    pub fn after(dur: Duration) -> Timer {
        Timer::at(Instant::now() + dur)
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if self.inserted {
            let id = self as *mut Timer as usize;
            RT.timers.lock().unwrap().remove(&(self.when, id));
        }
    }
}

impl Future for Timer {
    type Output = Instant;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let id = &mut *self as *mut Timer as usize;
        let mut timers = RT.timers.lock().unwrap();

        if Instant::now() >= self.when {
            timers.remove(&(self.when, id));
            return Poll::Ready(self.when);
        }

        if !self.inserted {
            if let Some((first, _)) = timers.keys().next() {
                if self.when < *first {
                    todo!("notify epoller");
                }
            }

            let waker = cx.waker().clone();
            timers.insert((self.when, id), waker);
            self.inserted = true;
            notify();
        }

        Poll::Pending
    }
}

// ----- Async I/O -----

struct Entry {
    index: usize,
    readers: Mutex<Vec<Waker>>,
    writers: Mutex<Vec<Waker>>,
}

struct Registration<T> {
    fd: RawFd,
    source: T,
    entry: Arc<Entry>,
}

impl<T> Drop for Registration<T> {
    fn drop(&mut self) {
        epoll_ctl(RT.epoll, EpollOp::EpollCtlDel, self.fd, None).unwrap();
        RT.entries.lock().unwrap().remove(self.entry.index);
    }
}

/// An async I/O handle.
pub struct Async<T>(Arc<Registration<T>>);

impl<T: AsRawFd> Async<T> {
    // note: make sure source is in non-blocking mode
    pub fn register(source: T) -> Async<T> {
        let mut entries = RT.entries.lock().unwrap();
        let vacant = entries.vacant_entry();
        let index = vacant.key();
        let entry = Arc::new(Entry {
            index,
            readers: Mutex::new(Vec::new()),
            writers: Mutex::new(Vec::new()),
        });
        vacant.insert(entry.clone());
        drop(entries);

        // TODO: handle epoll errors
        epoll_ctl(
            RT.epoll,
            EpollOp::EpollCtlAdd,
            source.as_raw_fd(),
            Some(&mut EpollEvent::new(
                EpollFlags::EPOLLET
                    | EpollFlags::EPOLLIN
                    | EpollFlags::EPOLLOUT
                    | EpollFlags::EPOLLRDHUP,
                index as u64,
            )),
        )
        .unwrap();

        let fd = source.as_raw_fd();
        Async(Arc::new(Registration { fd, source, entry }))
    }
}

impl<T> Async<T> {
    /// Gets a reference to the source I/O handle.
    pub fn source(&self) -> &T {
        &self.0.source
    }

    /// Turns a non-blocking read into an async operation.
    pub async fn read_with<'a, R>(
        &'a self,
        mut f: impl FnMut(&'a T) -> io::Result<R>,
    ) -> io::Result<R> {
        future::poll_fn(|cx| self.poll_with(cx, &self.0.entry.readers, &mut f)).await
    }

    /// Turns a non-blocking write into an async operation.
    pub async fn write_with<'a, R>(
        &'a self,
        mut f: impl FnMut(&'a T) -> io::Result<R>,
    ) -> io::Result<R> {
        future::poll_fn(|cx| self.poll_with(cx, &self.0.entry.writers, &mut f)).await
    }

    fn poll_with<'a, R>(
        &'a self,
        cx: &mut Context<'_>,
        wakers: &Mutex<Vec<Waker>>,
        mut f: impl FnMut(&'a T) -> io::Result<R>,
    ) -> Poll<io::Result<R>> {
        // Attempt the non-blocking operation.
        match f(self.source()) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            res => return Poll::Ready(res),
        }

        // Acquire a lock on the waker list.
        let mut wakers = wakers.lock().unwrap();

        // Attempt the non-blocking operation again.
        match f(self.source()) {
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

impl<T> Clone for Async<T> {
    fn clone(&self) -> Async<T> {
        Async(self.0.clone())
    }
}

// ----- Networking -----

impl<T> AsyncRead for Async<T>
where
    for<'a> &'a T: Read,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_with(cx, &self.0.entry.readers, |mut source| source.read(buf))
    }
}

impl<T> AsyncWrite for Async<T>
where
    for<'a> &'a T: Write,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_with(cx, &self.0.entry.writers, |mut source| source.write(buf))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_with(cx, &self.0.entry.writers, |mut source| source.flush())
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Async<TcpStream> {
    pub async fn connect<T: ToSocketAddrs>(addr: T) -> io::Result<Async<TcpStream>> {
        let mut last_err = None;

        // Try connecting to each address one by one.
        // TODO: use blocking pool to resolve
        for addr in addr.to_socket_addrs()? {
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

    // TODO: extract into each_addr()
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
        let stream = Async::register(socket.into_tcp_stream());

        // Wait for connect to complete.
        let check_connected = |stream: &TcpStream| match stream.peer_addr() {
            Err(err) if err.kind() == io::ErrorKind::NotConnected => {
                Err(io::Error::new(io::ErrorKind::WouldBlock, ""))
            }
            res => res,
        };
        stream.write_with(check_connected).await?;
        Ok(stream)
    }
}

impl Async<TcpListener> {
    /// TODO
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Async<TcpListener>> {
        // Bind and make the listener async.
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Async::register(listener))
    }

    /// TODO
    pub async fn accept(&self) -> io::Result<(Async<TcpStream>, SocketAddr)> {
        // Accept and make the stream async.
        let (stream, addr) = self.read_with(TcpListener::accept).await?;
        stream.set_nonblocking(true)?;
        let stream = Async::register(stream);
        Ok((stream, addr))
    }

    // TODO: incoming()
}

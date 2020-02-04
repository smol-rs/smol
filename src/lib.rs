#![deny(unsafe_code)]

use std::collections::BTreeMap;
use std::future::Future;
use std::io::{self, Read, Write};
use std::mem;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::io::{AsRawFd, RawFd};
use std::panic::catch_unwind;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam::channel;
use futures::future;
use futures::io::{AsyncRead, AsyncWrite};
use nix::sys::epoll::{
    epoll_create1, epoll_ctl, epoll_wait, EpollCreateFlags, EpollEvent, EpollFlags, EpollOp,
};
use once_cell::sync::Lazy;
use slab::Slab;
use socket2::{Domain, Protocol, Socket, Type};

#[cfg(not(any(target_os = "linux", target_os = "android")))]
compile_error!("smol does not support this target OS");

// ----- Globals -----

struct Runtime {
    epoll: RawFd,
    entries: Mutex<Slab<Arc<Entry>>>,
    timers: Mutex<BTreeMap<(Instant, usize), Waker>>,
}

static RT: Lazy<Runtime> = Lazy::new(|| {
    thread::spawn(|| {
        let mut buffer = vec![EpollEvent::empty(); 1000];
        loop {
            let ready = {
                let mut timers = RT.timers.lock().unwrap();
                let pending = timers.split_off(&(Instant::now(), 0));
                mem::replace(&mut *timers, pending)
            };
            for (_, waker) in ready {
                waker.wake();
            }

            // todo: use a timeout
            let n = epoll_wait(RT.epoll, &mut buffer, -1).unwrap();
            let entries = RT.entries.lock().unwrap();

            for ev in &buffer[..n] {
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
    });

    Runtime {
        epoll: epoll_create1(EpollCreateFlags::EPOLL_CLOEXEC).unwrap(),
        entries: Mutex::new(Slab::new()),
        timers: Mutex::new(BTreeMap::new()),
    }
});

// ----- Executor -----

// Runs the future to completion on a new worker (have Mutex<Vec<Stealer>>)
// there will be no hidden threadpool!!
pub fn run<F, R>(future: F) -> JoinHandle<R>
where
    F: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    // TODO: run() should propagate panics into caller

    let handle = spawn(future);
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

/// A queue that holds scheduled tasks.
static QUEUE: Lazy<channel::Sender<Task>> = Lazy::new(|| {
    // Create a queue.
    let (sender, receiver) = channel::unbounded::<Task>();

    // Spawn executor threads the first time the queue is created.
    for _ in 0..num_cpus::get().max(1) {
        let receiver = receiver.clone();
        thread::spawn(move || {
            receiver.iter().for_each(|task| {
                let _ = catch_unwind(|| task.run());
            })
        });
    }

    sender
});

/// A spawned future and its current state.
type Task = async_task::Task<()>;

/// Spawns a future on the executor.
pub fn spawn<F, R>(future: F) -> JoinHandle<R>
where
    F: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    // Create a task and schedule it for execution.
    let (task, handle) = async_task::spawn(future, |t| QUEUE.send(t).unwrap(), ());
    task.schedule();

    // Return a join handle that retrieves the output of the future.
    JoinHandle(handle)
}

/// Awaits the output of a spawned future.
pub struct JoinHandle<R>(async_task::JoinHandle<R, ()>);

impl<R> Future for JoinHandle<R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(output) => Poll::Ready(output.expect("task failed")),
        }
    }
}

// ----- Timer -----

pub struct Timer {
    when: Instant,
    inserted: bool,
}

impl Timer {
    pub fn after(dur: Duration) -> Timer {
        Timer {
            when: Instant::now() + dur,
            inserted: false,
        }
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
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let id = &mut *self as *mut Timer as usize;
        let mut timers = RT.timers.lock().unwrap();

        if Instant::now() >= self.when {
            timers.remove(&(self.when, id));
            return Poll::Ready(());
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

pub struct Registration<T> {
    fd: RawFd,
    source: T,
    entry: Arc<Entry>,
}

pub struct Async<T>(Arc<Registration<T>>);

impl<T> Async<T> {
    // note: make sure source is in non-blocking mode
    pub fn register(source: T) -> Async<T>
    where
        T: AsRawFd,
    {
        let mut entries = RT.entries.lock().unwrap();
        let vacant = entries.vacant_entry();
        let index = vacant.key();
        let entry = Arc::new(Entry {
            index,
            readers: Mutex::new(Vec::new()),
            writers: Mutex::new(Vec::new()),
        });
        vacant.insert(entry.clone());

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
        m: &Mutex<Vec<Waker>>,
        mut f: impl FnMut(&'a T) -> io::Result<R>,
    ) -> Poll<io::Result<R>> {
        // Attempt the non-blocking operation.
        match f(self.source()) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            res => return Poll::Ready(res),
        }

        // Acquire a lock on the waker list.
        let mut wakers = m.lock().unwrap();

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

impl<T> Drop for Registration<T> {
    fn drop(&mut self) {
        epoll_ctl(RT.epoll, EpollOp::EpollCtlDel, self.fd, None).unwrap();
        RT.entries.lock().unwrap().remove(self.entry.index);
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
}
